//! Consultation scribe: recording consent, in-memory transcription into a
//! structured draft, and per-section application under clinician control.
//!
//! Safety boundaries:
//! - raw audio lives in memory for the duration of one request and is never
//!   written to the database, disk, or logs;
//! - the output is an unsigned `scribe_draft` AIArtifact bound to the exact
//!   note version it was proposed against; nothing enters the note without
//!   an explicit per-section application that re-checks the live version;
//! - authorization is the same centralized guard used by every
//!   documentation write (tenant, facility, care relationship, own active
//!   consultation), plus a dedicated transcription rate-limit family;
//! - provider credentials are server-side configuration only.

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::actions;
use crate::ratelimit;
use crate::routes::encounter_docs::{
    exceeds_chars, load_encounter, lock_encounter, require_own_active, resource_ctx,
    validate_sections, write_draft_note, SaveNote, NOTE_SECTION_MAX_CHARS,
};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::Json;
use base64::Engine;
use chrono::Utc;
use dmind_gateway::scribe::{extract_sections, extraction_info, ScribeError, TranscriptionRequest};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use sqlx::Row;
use uuid::Uuid;
use wellos_domain::ai::{ArtifactStatus, ScribeDraftV1, NOTE_SECTIONS, SCRIBE_DRAFT_SCHEMA};

/// Largest accepted decoded recording (about 20 minutes of Opus speech).
pub const MAX_AUDIO_BYTES: usize = 6 * 1024 * 1024;
/// Request-body ceiling for the transcription route (base64 + envelope).
pub const MAX_BODY_BYTES: usize = 9 * 1024 * 1024;
pub const MIN_DURATION_MS: u64 = 1_000;
pub const MAX_DURATION_MS: u64 = 20 * 60 * 1_000;
/// Container types browsers produce with `MediaRecorder`, plus the common
/// upload formats an OpenAI-compatible endpoint accepts.
pub const ALLOWED_MIME_TYPES: &[&str] = &[
    "audio/webm",
    "audio/ogg",
    "audio/mp4",
    "audio/mpeg",
    "audio/wav",
];

const SCRIBE_TEMPLATE: &str = "consultation-scribe@1.0.0";

/// Retire every scribe draft that could still be applied: those awaiting
/// review and those partially applied (`approved`, whose remaining sections
/// stay insertable). Called when a new recording replaces the current draft
/// and when the encounter closes (sign/cancel), since a closed record can
/// never receive them.
pub(crate) async fn supersede_applicable_scribe_drafts(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    tenant_id: Uuid,
    encounter_id: Uuid,
) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE ai_artifacts SET status = $1
         WHERE tenant_id = $2 AND encounter_id = $3 AND artifact_type = 'scribe_draft'
           AND status IN ($4, $5)",
    )
    .bind(ArtifactStatus::Superseded.as_str())
    .bind(tenant_id)
    .bind(encounter_id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .bind(ArtifactStatus::Approved.as_str())
    .execute(&mut **tx)
    .await?;
    Ok(())
}

/// The note version a scribe draft is currently bound to: the version read
/// under the encounter lock when it was proposed, advanced by each of its own
/// applications (which bump the note). Any other write to the note leaves the
/// draft bound to a version that no longer exists, so it can no longer be
/// applied.
pub(crate) fn bound_note_version(
    source_note_version: Option<i64>,
    review_detail: &Value,
) -> Option<i64> {
    review_detail["applied"]
        .as_array()
        .and_then(|applied| applied.last())
        .and_then(|last| last["note_version"].as_i64())
        .map_or(source_note_version, Some)
}

// ---------------------------------------------------------------------------
// POST /api/v1/encounters/:id/recording-consent
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RecordingConsent {
    pub granted: bool,
}

/// Record that the treating practitioner obtained (or withdrew) the
/// patient's consent to record this consultation. Append-only; audited as
/// `encounter.recording.consent_recorded` with identifiers only.
pub async fn record_consent(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<RecordingConsent>,
) -> Result<Json<Value>, ApiError> {
    let enc = load_encounter(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::ENCOUNTER_DOCUMENT,
        "recording_consent",
        Some(resource_ctx(&enc)),
    )
    .await?;
    require_own_active(&enc, &ctx)?;

    let consent_id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    let enc = lock_encounter(&mut tx, id).await?;
    require_own_active(&enc, &ctx)?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let recorded_at: chrono::DateTime<Utc> = sqlx::query_scalar(
        "INSERT INTO encounter_recording_consents
         (id, tenant_id, encounter_id, patient_id, practitioner_id, granted)
         VALUES ($1,$2,$3,$4,$5,$6) RETURNING recorded_at",
    )
    .bind(consent_id)
    .bind(enc.tenant_id)
    .bind(id)
    .bind(enc.patient_id)
    .bind(ctx.user_id)
    .bind(body.granted)
    .fetch_one(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "encounter.recording.consent_recorded",
        &state.cell,
        json!({ "encounter_id": id, "consent_id": consent_id, "granted": body.granted }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({
        "id": consent_id,
        "granted": body.granted,
        "recorded_at": recorded_at,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/encounters/:id/scribe — transcribe + structure one recording
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct TranscribeRequest {
    /// Standard base64 of the recorded container bytes.
    pub audio_base64: String,
    /// Container type as reported by the recorder, e.g. `audio/webm;codecs=opus`.
    pub mime_type: String,
    pub duration_ms: u64,
    /// "en" or "es".
    pub language: Option<String>,
}

fn normalize_mime(raw: &str) -> Option<&'static str> {
    let base = raw
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    ALLOWED_MIME_TYPES.iter().copied().find(|m| *m == base)
}

fn validate_request(
    body: &TranscribeRequest,
) -> Result<(&'static str, &'static str, Vec<u8>), ApiError> {
    let language = match body.language.as_deref() {
        None | Some("en") => "en",
        Some("es") => "es",
        Some(_) => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "language must be 'en' or 'es'",
            ))
        }
    };
    let mime = normalize_mime(&body.mime_type).ok_or_else(|| {
        ApiError::bad_request(
            "unsupported_media_type",
            "recording format not supported; use webm, ogg, mp4, mpeg or wav audio",
        )
    })?;
    if body.duration_ms < MIN_DURATION_MS {
        return Err(ApiError::bad_request(
            "recording_too_short",
            "the recording is too short to transcribe",
        ));
    }
    if body.duration_ms > MAX_DURATION_MS {
        return Err(ApiError::bad_request(
            "recording_too_long",
            "the recording exceeds the maximum supported duration",
        ));
    }
    // Reject oversize payloads before decoding: base64 expands 3 bytes to 4.
    if body.audio_base64.len() > MAX_AUDIO_BYTES / 3 * 4 + 4 {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "recording_too_large",
            "the recording exceeds the maximum supported size",
        ));
    }
    let audio = base64::engine::general_purpose::STANDARD
        .decode(body.audio_base64.trim())
        .map_err(|_| ApiError::bad_request("validation_failed", "audio must be standard base64"))?;
    if audio.is_empty() {
        return Err(ApiError::bad_request("validation_failed", "audio is empty"));
    }
    if audio.len() > MAX_AUDIO_BYTES {
        return Err(ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "recording_too_large",
            "the recording exceeds the maximum supported size",
        ));
    }
    Ok((language, mime, audio))
}

/// Whether the practitioner's latest recorded consent decision for this
/// encounter is a grant. Consent rows are append-only, so the newest row
/// (by `recorded_at`, then the time-ordered `id`) is the current decision.
async fn has_recording_consent<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    tenant_id: Uuid,
    encounter_id: Uuid,
    practitioner_id: Uuid,
) -> Result<bool, ApiError> {
    let latest: Option<bool> = sqlx::query_scalar(
        "SELECT granted FROM encounter_recording_consents
         WHERE tenant_id = $1 AND encounter_id = $2 AND practitioner_id = $3
         ORDER BY recorded_at DESC, id DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(encounter_id)
    .bind(practitioner_id)
    .fetch_optional(executor)
    .await?;
    Ok(latest == Some(true))
}

fn consent_required() -> ApiError {
    ApiError::conflict(
        "consent_required",
        "record the patient's consent before transcribing a consultation",
    )
}

async fn record_generation_failure(
    state: &AppState,
    ctx: &AuthContext,
    encounter_id: Uuid,
    stage: &str,
) -> Result<(), ApiError> {
    audit::emit(
        &state.pool,
        ctx,
        "ai.generation.failed",
        &state.cell,
        json!({ "encounter_id": encounter_id, "artifact_type": "scribe_draft", "stage": stage }),
        None,
    )
    .await
    .map_err(ApiError::internal)
}

/// Transcribe one in-memory recording and structure it into proposed note
/// sections. The audio is handed to the configured provider and dropped; the
/// persisted artifact contains only the structured output, bound to the note
/// version current at insertion (read under the encounter lock).
pub async fn transcribe(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<TranscribeRequest>,
) -> Result<Json<Value>, ApiError> {
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scribe).await?;
    let (language, mime, audio) = validate_request(&body)?;
    let enc = load_encounter(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::ENCOUNTER_DOCUMENT,
        "scribe_draft",
        Some(resource_ctx(&enc)),
    )
    .await?;
    require_own_active(&enc, &ctx)?;
    if !has_recording_consent(&state.pool, enc.tenant_id, id, ctx.user_id).await? {
        return Err(consent_required());
    }

    // Provenance for the recording without retaining it: a one-way hash.
    let audio_sha256 = hex::encode(Sha256::digest(&audio));
    let duration_ms = body.duration_ms;
    let request = TranscriptionRequest {
        audio,
        mime_type: mime.to_string(),
        duration_ms,
        language: language.to_string(),
    };
    // No database lock is held while the (possibly external) provider runs.
    let transcription = match state.scribe.transcribe(&request).await {
        Ok(t) => t,
        Err(err) => {
            record_generation_failure(&state, &ctx, id, "transcription").await?;
            return Err(match err {
                ScribeError::Unavailable(_) => ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "scribe_unavailable",
                    "transcription is unavailable right now; your recording is kept in this browser so you can retry",
                ),
                ScribeError::InvalidOutput(_) => ApiError::new(
                    StatusCode::BAD_GATEWAY,
                    "scribe_invalid_output",
                    "the transcription service returned unusable output; you can retry",
                ),
                ScribeError::Rejected(_) => ApiError::new(
                    StatusCode::UNPROCESSABLE_ENTITY,
                    "audio_rejected",
                    "the recording could not be processed; check the microphone and record again",
                ),
            });
        }
    };
    drop(request);

    let (sections, flags) = extract_sections(&transcription.segments, language);
    let mut limitations = dmind_gateway::scribe::limitations(language);
    if sections.is_empty() {
        limitations.push(if language == "es" {
            "No se pudo asignar ninguna parte de la conversación a una sección de la nota."
                .to_string()
        } else {
            "No part of the conversation could be mapped to a note section.".to_string()
        });
    }

    let artifact_id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    let enc = lock_encounter(&mut tx, id).await?;
    require_own_active(&enc, &ctx)?;
    // Consent decisions are written under the same encounter lock, so a
    // withdrawal that committed while the provider was running is visible
    // here; the transcript is then discarded and nothing is persisted.
    if !has_recording_consent(&mut *tx, enc.tenant_id, id, ctx.user_id).await? {
        drop(tx);
        record_generation_failure(&state, &ctx, id, "consent_withdrawn").await?;
        return Err(consent_required());
    }
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let note_version: Option<i64> = sqlx::query_scalar(
        "SELECT version FROM encounter_notes WHERE tenant_id = $1 AND encounter_id = $2",
    )
    .bind(enc.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;

    let draft = ScribeDraftV1 {
        schema_version: SCRIBE_DRAFT_SCHEMA.to_string(),
        encounter_id: id,
        source_note_version: note_version,
        language: language.to_string(),
        transcript: transcription.segments,
        sections,
        flags,
        transcription: transcription.provider.clone(),
        extraction: extraction_info(),
        generated_at: Utc::now(),
        limitations: limitations.clone(),
    };
    if let Err(reason) = draft.validate() {
        drop(tx);
        tracing::warn!(stage = "structure", "scribe output failed validation");
        let _ = reason;
        record_generation_failure(&state, &ctx, id, "structure").await?;
        return Err(ApiError::new(
            StatusCode::BAD_GATEWAY,
            "scribe_invalid_output",
            "the structured draft failed validation; you can retry",
        ));
    }

    // One applicable scribe draft per encounter; the encounter lock
    // serializes concurrent recordings.
    supersede_applicable_scribe_drafts(&mut tx, enc.tenant_id, id).await?;
    let citations: Vec<String> = vec![
        format!("recording:sha256:{audio_sha256}"),
        match note_version {
            Some(v) => format!("encounter_note:{id}:v{v}"),
            None => format!("encounter_note:{id}:none"),
        },
    ];
    sqlx::query(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, encounter_id, artifact_type, autonomy_level, status,
          model, model_version, route, template, input_hash, output, output_schema,
          citations, limitations, note_version, generated_at)
         VALUES ($1,$2,$3,$4,'scribe_draft','A1',$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)",
    )
    .bind(artifact_id)
    .bind(enc.tenant_id)
    .bind(enc.patient_id)
    .bind(id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .bind(&draft.transcription.model)
    .bind(&draft.transcription.model_version)
    .bind(&draft.transcription.provider)
    .bind(SCRIBE_TEMPLATE)
    .bind(&audio_sha256)
    .bind(serde_json::to_value(&draft).map_err(ApiError::internal)?)
    .bind(SCRIBE_DRAFT_SCHEMA)
    .bind(serde_json::to_value(&citations).map_err(ApiError::internal)?)
    .bind(serde_json::to_value(&limitations).map_err(ApiError::internal)?)
    .bind(note_version)
    .bind(draft.generated_at)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "encounter.scribe.transcribed",
        &state.cell,
        json!({
            "encounter_id": id,
            "artifact_id": artifact_id,
            "duration_ms": duration_ms,
            "mime_type": mime,
            "segments": draft.transcript.len(),
            "sections": draft.sections.len(),
            "provider": draft.transcription.provider,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    audit::emit(
        &mut *tx,
        &ctx,
        "ai.artifact.generated",
        &state.cell,
        json!({
            "artifact_id": artifact_id,
            "encounter_id": id,
            "artifact_type": "scribe_draft",
            "note_version": note_version,
            "input_hash": audio_sha256,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;

    Ok(Json(json!({
        "id": artifact_id,
        "status": ArtifactStatus::AwaitingReview.as_str(),
        "output": draft,
        "limitations": limitations,
        "model": draft.transcription.model,
        "model_version": draft.transcription.model_version,
        "route": draft.transcription.provider,
        "generated_at": draft.generated_at,
        "review_decision": Value::Null,
        "review_detail": json!({ "applied": [] }),
        "note_version": note_version,
        "stale": false,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/encounters/:id/scribe/:artifact_id/review
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SectionApplication {
    pub section: String,
    /// `fill` places the suggestion into a section the clinician left empty;
    /// `append` adds it below existing clinician text. Neither ever replaces
    /// what the clinician wrote.
    pub mode: String,
}

#[derive(Deserialize)]
pub struct ScribeReview {
    /// `apply` merges the listed sections into the draft note; `dismiss`
    /// rejects the whole draft.
    pub decision: String,
    #[serde(default)]
    pub sections: Vec<SectionApplication>,
    /// The clinician's current draft exactly as displayed (including unsaved
    /// text) with the server version it is based on. The merged result is
    /// written as the next note version in the same transaction.
    #[serde(flatten)]
    pub note: SaveNote,
}

fn section_mut<'a>(note: &'a mut SaveNote, section: &str) -> Option<&'a mut Option<String>> {
    Some(match section {
        "reason_for_encounter" => &mut note.reason_for_encounter,
        "history_present_illness" => &mut note.history_present_illness,
        "medical_history" => &mut note.medical_history,
        "review_of_systems" => &mut note.review_of_systems,
        "physical_exam" => &mut note.physical_exam,
        "assessment" => &mut note.assessment,
        "plan" => &mut note.plan,
        "follow_up" => &mut note.follow_up,
        _ => return None,
    })
}

/// Merge one proposed section into the clinician's draft without ever
/// discarding clinician text: an empty section takes the proposal, a filled
/// one only accepts an explicit `append`.
pub(crate) fn merge_section(
    current: Option<&str>,
    proposal: &str,
    mode: &str,
) -> Result<String, ApiError> {
    let current = current.unwrap_or("");
    if current.trim().is_empty() {
        return Ok(proposal.to_string());
    }
    if mode != "append" {
        return Err(ApiError::conflict(
            "section_not_empty",
            "this section already contains clinician text; append instead of filling",
        ));
    }
    Ok(format!("{}\n\n{proposal}", current.trim_end()))
}

/// Apply or dismiss a scribe draft. Application is one transaction with the
/// note write: the encounter is locked, the artifact must still be
/// reviewable, the clinician's submitted draft must be based on the current
/// note version, the chosen sections are merged (never overwriting clinician
/// text), the note version advances, and the per-section decision is
/// recorded — all or nothing.
pub async fn review(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((id, artifact_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ScribeReview>,
) -> Result<Json<Value>, ApiError> {
    let apply = match body.decision.as_str() {
        "apply" => true,
        "dismiss" => false,
        _ => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "decision must be 'apply' or 'dismiss'",
            ))
        }
    };
    if apply {
        validate_sections(&body.note)?;
        if body.sections.is_empty() || body.sections.len() > NOTE_SECTIONS.len() {
            return Err(ApiError::bad_request(
                "validation_failed",
                "apply requires between one and eight sections",
            ));
        }
        let mut seen = std::collections::HashSet::new();
        for s in &body.sections {
            if !NOTE_SECTIONS.contains(&s.section.as_str()) || !seen.insert(s.section.as_str()) {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    "sections must be distinct note sections",
                ));
            }
            if s.mode != "fill" && s.mode != "append" {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    "mode must be 'fill' or 'append'",
                ));
            }
        }
    } else if !body.sections.is_empty() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "dismiss does not take sections",
        ));
    }

    let enc = load_encounter(&state, id).await?;
    let document = guard(
        &state,
        &ctx,
        actions::ENCOUNTER_DOCUMENT,
        "scribe_draft",
        Some(resource_ctx(&enc)),
    )
    .await?;
    let ai_review = guard(
        &state,
        &ctx,
        actions::AI_REVIEW,
        "ai_artifact",
        Some(resource_ctx(&enc)),
    )
    .await?;
    require_own_active(&enc, &ctx)?;

    let mut tx = state.pool.begin().await?;
    let enc = lock_encounter(&mut tx, id).await?;
    require_own_active(&enc, &ctx)?;
    document.record(&mut tx, &ctx, &state.cell).await?;
    ai_review.record(&mut tx, &ctx, &state.cell).await?;

    let artifact = sqlx::query(
        "SELECT status, output, review_detail, note_version FROM ai_artifacts
         WHERE id = $1 AND tenant_id = $2 AND encounter_id = $3 AND artifact_type = 'scribe_draft'
         FOR UPDATE",
    )
    .bind(artifact_id)
    .bind(enc.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let status = ArtifactStatus::parse(artifact.get::<String, _>("status").as_str())
        .ok_or_else(|| ApiError::internal("invalid artifact status"))?;
    // Sections may be applied one at a time, so an approved draft stays
    // applicable; dismissal only makes sense while nothing was applied.
    let reviewable = matches!(
        (apply, status),
        (
            true,
            ArtifactStatus::AwaitingReview | ArtifactStatus::Approved
        ) | (false, ArtifactStatus::AwaitingReview)
    );
    if !reviewable {
        return Err(ApiError::conflict(
            "artifact_not_reviewable",
            "this draft was already dismissed or superseded",
        ));
    }

    let current_note_version: Option<i64> = sqlx::query_scalar(
        "SELECT version FROM encounter_notes WHERE tenant_id = $1 AND encounter_id = $2",
    )
    .bind(enc.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;

    let mut review_detail: Value = artifact.get("review_detail");
    let mut note_result: Option<(Uuid, i64)> = None;
    let mut merged_sections: Option<Value> = None;
    let next_status;
    let event;
    if apply {
        match (body.note.version, current_note_version) {
            (None, Some(_)) => {
                return Err(ApiError::conflict(
                    "version_required",
                    "the note already exists; provide its current version",
                ));
            }
            (expected, current) if expected != current => {
                return Err(ApiError::conflict(
                    "version_conflict",
                    "the note was updated by someone else; reload before applying suggestions",
                ));
            }
            _ => {}
        }
        let source_note_version: Option<i64> = artifact.get("note_version");
        if bound_note_version(source_note_version, &review_detail) != current_note_version {
            return Err(ApiError::conflict(
                "artifact_stale",
                "the note changed since this draft was prepared; record again for a new draft",
            ));
        }
        let output: Value = artifact
            .get::<Option<Value>, _>("output")
            .unwrap_or(Value::Null);
        let already: Vec<String> = review_detail["applied"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|a| a["section"].as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default();
        let mut merged = SaveNote {
            version: body.note.version,
            reason_for_encounter: body.note.reason_for_encounter.clone(),
            history_present_illness: body.note.history_present_illness.clone(),
            medical_history: body.note.medical_history.clone(),
            review_of_systems: body.note.review_of_systems.clone(),
            physical_exam: body.note.physical_exam.clone(),
            assessment: body.note.assessment.clone(),
            plan: body.note.plan.clone(),
            follow_up: body.note.follow_up.clone(),
        };
        for s in &body.sections {
            if already.contains(&s.section) {
                return Err(ApiError::conflict(
                    "section_already_applied",
                    "this suggestion was already applied to the note",
                ));
            }
            let proposal = output["sections"]
                .as_array()
                .and_then(|arr| arr.iter().find(|p| p["section"] == json!(s.section)))
                .and_then(|p| p["text"].as_str())
                .map(str::trim)
                .filter(|t| !t.is_empty())
                .ok_or_else(|| {
                    ApiError::bad_request(
                        "validation_failed",
                        "section was not proposed by this draft",
                    )
                })?;
            let slot = section_mut(&mut merged, &s.section)
                .ok_or_else(|| ApiError::internal("unknown note section"))?;
            let text = merge_section(slot.as_deref(), proposal, &s.mode)?;
            if exceeds_chars(&text, NOTE_SECTION_MAX_CHARS) {
                return Err(ApiError::bad_request(
                    "section_limit_exceeded",
                    format!(
                        "applying this suggestion would push {} past {NOTE_SECTION_MAX_CHARS} characters",
                        s.section
                    ),
                ));
            }
            *slot = Some(text);
        }
        let written = write_draft_note(&mut tx, &state, &ctx, &enc, id, &merged).await?;
        merged_sections = Some(json!({
            "reason_for_encounter": merged.reason_for_encounter,
            "history_present_illness": merged.history_present_illness,
            "medical_history": merged.medical_history,
            "review_of_systems": merged.review_of_systems,
            "physical_exam": merged.physical_exam,
            "assessment": merged.assessment,
            "plan": merged.plan,
            "follow_up": merged.follow_up,
        }));
        let now = Utc::now();
        let applied = review_detail
            .as_object_mut()
            .and_then(|o| {
                o.entry("applied")
                    .or_insert_with(|| Value::Array(Vec::new()))
                    .as_array_mut()
            })
            .ok_or_else(|| ApiError::internal("invalid review_detail"))?;
        for s in &body.sections {
            applied.push(json!({
                "section": s.section,
                "mode": s.mode,
                "note_version": written.1,
                "at": now,
            }));
        }
        note_result = Some(written);
        next_status = ArtifactStatus::Approved;
        event = "encounter.scribe.applied";
    } else {
        next_status = ArtifactStatus::Rejected;
        event = "encounter.scribe.dismissed";
    }
    let decision = if apply { "approved" } else { "rejected" };
    let updated = sqlx::query(
        "UPDATE ai_artifacts SET status = $1, reviewer_id = $2, review_decision = $3,
                reviewed_at = COALESCE(reviewed_at, now()), review_detail = $4
         WHERE id = $5 AND status = $6",
    )
    .bind(next_status.as_str())
    .bind(ctx.user_id)
    .bind(decision)
    .bind(&review_detail)
    .bind(artifact_id)
    .bind(status.as_str())
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "review_conflict",
            "the draft was reviewed or superseded concurrently",
        ));
    }
    let sections: Vec<&str> = body.sections.iter().map(|s| s.section.as_str()).collect();
    let note_version = note_result.map(|(_, v)| v).or(current_note_version);
    audit::emit(
        &mut *tx,
        &ctx,
        "ai.artifact.reviewed",
        &state.cell,
        json!({ "artifact_id": artifact_id, "encounter_id": id, "decision": decision,
                "sections": sections, "note_version": note_version }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    audit::emit(
        &mut *tx,
        &ctx,
        event,
        &state.cell,
        json!({ "artifact_id": artifact_id, "encounter_id": id, "sections": sections,
                "note_version": note_version }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;

    let note = note_result.map(|(note_id, version)| {
        let mut n = json!({ "id": note_id, "status": "draft", "version": version });
        if let (Some(obj), Some(Value::Object(sections))) = (n.as_object_mut(), merged_sections) {
            obj.extend(sections);
        }
        n
    });
    Ok(Json(json!({
        "id": artifact_id,
        "status": next_status.as_str(),
        "review_decision": decision,
        "review_detail": review_detail,
        "note": note,
        "note_version": note_version,
    })))
}
