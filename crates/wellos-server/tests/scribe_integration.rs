//! Integration tests for the consultation scribe: consent, transcription
//! validation and rate limiting, deterministic structured output bound to the
//! note version, per-section application that never overwrites clinician
//! text, closed-encounter and stale-version rejection, authorization
//! boundaries, the dashboard cockpit, the patient brief / diagnostic history
//! payload, and create-or-resume consultations. Also proves no raw audio is
//! ever persisted.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use dmind_gateway::scribe::FakeTranscription;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use wellos_server::state::{AppState, AuthConfig};

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wellos:wellos_dev@localhost:5432/wellos".to_string())
}

async fn state_with(auth: AuthConfig) -> (AppState, Arc<FakeTranscription>) {
    let pool = wellos_server::connect_pool(&database_url()).await.unwrap();
    wellos_server::run_migrations(&pool).await.unwrap();
    let seeded: Option<(i64,)> = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_optional(&pool)
        .await
        .unwrap();
    if seeded.map(|(n,)| n).unwrap_or(0) == 0 {
        wellos_server::seeddata::seed(&pool).await.unwrap();
    }
    let gateway = Arc::new(dmind_gateway::fake::FakeProvider::new());
    let scribe = Arc::new(FakeTranscription::new());
    let mut state = AppState::with_auth(pool, gateway, auth);
    state.scribe = scribe.clone();
    (state, scribe)
}

async fn test_state() -> (AppState, Arc<FakeTranscription>) {
    state_with(AuthConfig::development()).await
}

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .body(match body {
            Some(v) => Body::from(v.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    let res = wellos_server::app(state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn uniq(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::now_v7().simple())
}

/// A recognisable synthetic "recording": never real speech, but bytes that
/// would be trivially findable if anything persisted them.
const SYNTHETIC_AUDIO_MARKER: &[u8] = b"WELLOS-SYNTHETIC-AUDIO-MARKER-";

fn synthetic_audio(len: usize) -> Vec<u8> {
    let mut v = Vec::with_capacity(len);
    while v.len() < len {
        v.extend_from_slice(SYNTHETIC_AUDIO_MARKER);
    }
    v.truncate(len);
    v
}

fn b64(bytes: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(bytes)
}

fn transcribe_body(audio: &[u8], duration_ms: u64) -> Value {
    json!({
        "audio_base64": b64(audio),
        "mime_type": "audio/webm;codecs=opus",
        "duration_ms": duration_ms,
        "language": "en",
    })
}

/// Registers a fresh synthetic patient; returns (facility, id, identifier).
async fn register_patient(state: &AppState) -> (String, String, String) {
    let (st, meta) = call(state, "GET", "/api/v1/meta/tenant", "dev-reg.rivera", None).await;
    assert_eq!(st, StatusCode::OK);
    let facility = meta["facilities"][0]["id"].as_str().unwrap().to_string();
    let identifier = uniq("MRN-SCR");
    let (st, patient) = call(
        state,
        "POST",
        "/api/v1/patients",
        "dev-reg.rivera",
        Some(json!({
            "facility_id": facility,
            "family_name": "Scribe",
            "given_name": "Test",
            "birth_date": "1975-05-05",
            "sex": "male",
            "identifier": identifier,
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{patient}");
    (
        facility,
        patient["id"].as_str().unwrap().to_string(),
        identifier,
    )
}

/// Fresh patient + in-progress consultation owned by dr.garcia.
async fn start_consultation(state: &AppState) -> (String, String) {
    let (_, patient_id, _) = register_patient(state).await;
    let (st, enc) = call(
        state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{enc}");
    (patient_id, enc["id"].as_str().unwrap().to_string())
}

async fn grant_consent(state: &AppState, enc: &str) {
    let (st, body) = call(
        state,
        "POST",
        &format!("/api/v1/encounters/{enc}/recording-consent"),
        "dev-dr.garcia",
        Some(json!({ "granted": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["granted"], json!(true));
}

async fn transcribe(state: &AppState, enc: &str, token: &str) -> (StatusCode, Value) {
    call(
        state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe"),
        token,
        Some(transcribe_body(&synthetic_audio(4_096), 90_000)),
    )
    .await
}

// ---------------------------------------------------------------------------
// Consent
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transcription_requires_recorded_consent_and_consent_is_audited() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;

    let (st, err) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("consent_required"));

    // Declined consent is recorded but does not unlock transcription.
    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/recording-consent"),
        "dev-dr.garcia",
        Some(json!({ "granted": false })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let (st, err) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("consent_required"));

    grant_consent(&state, &enc).await;
    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");

    // Consent decisions are append-only and mirrored to the outbox.
    let consents: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM encounter_recording_consents WHERE encounter_id = $1::uuid",
    )
    .bind(&enc)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(consents, 2);
    let events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outbox_events
         WHERE event_type = 'encounter.recording.consent_recorded'
           AND resource_refs->>'encounter_id' = $1",
    )
    .bind(&enc)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(events, 2);

    // The workspace reports the latest decision so the UI can skip re-asking.
    let (st, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ws["recording_consent"]["granted"], json!(true));
    assert_eq!(ws["capabilities"]["can_record"], json!(true));
}

// ---------------------------------------------------------------------------
// Validation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transcription_validates_mime_size_duration_and_language() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let path = format!("/api/v1/encounters/{enc}/scribe");
    let audio = synthetic_audio(2_048);

    let cases: Vec<(Value, StatusCode, &str)> = vec![
        (
            json!({ "audio_base64": b64(&audio), "mime_type": "video/mp4", "duration_ms": 5_000 }),
            StatusCode::BAD_REQUEST,
            "unsupported_media_type",
        ),
        (
            json!({ "audio_base64": b64(&audio), "mime_type": "text/plain", "duration_ms": 5_000 }),
            StatusCode::BAD_REQUEST,
            "unsupported_media_type",
        ),
        (
            json!({ "audio_base64": b64(&audio), "mime_type": "audio/webm", "duration_ms": 200 }),
            StatusCode::BAD_REQUEST,
            "recording_too_short",
        ),
        (
            json!({ "audio_base64": b64(&audio), "mime_type": "audio/webm", "duration_ms": 21 * 60 * 1000 }),
            StatusCode::BAD_REQUEST,
            "recording_too_long",
        ),
        (
            json!({ "audio_base64": b64(&audio), "mime_type": "audio/webm", "duration_ms": 5_000, "language": "fr" }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            json!({ "audio_base64": "not*base64!", "mime_type": "audio/webm", "duration_ms": 5_000 }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
        (
            json!({ "audio_base64": "", "mime_type": "audio/webm", "duration_ms": 5_000 }),
            StatusCode::BAD_REQUEST,
            "validation_failed",
        ),
    ];
    for (body, expected, code) in cases {
        let (st, err) = call(&state, "POST", &path, "dev-dr.garcia", Some(body.clone())).await;
        assert_eq!(st, expected, "{body} -> {err}");
        assert_eq!(err["error"]["code"], json!(code), "{body} -> {err}");
    }

    // Oversize recordings are refused before decoding.
    let oversize = synthetic_audio(wellos_server::routes::scribe::MAX_AUDIO_BYTES + 1);
    let (st, err) = call(
        &state,
        "POST",
        &path,
        "dev-dr.garcia",
        Some(json!({ "audio_base64": b64(&oversize), "mime_type": "audio/webm", "duration_ms": 60_000 })),
    )
    .await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE, "{err}");
    assert_eq!(err["error"]["code"], json!("recording_too_large"));

    // Nothing above created an artifact.
    let artifacts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ai_artifacts WHERE encounter_id = $1::uuid AND artifact_type = 'scribe_draft'",
    )
    .bind(&enc)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(artifacts, 0);
}

#[tokio::test]
async fn transcription_has_its_own_rate_limit_family() {
    // dr.lopez is used here because dr.garcia's scribe window is shared
    // with the other tests in this file running concurrently.
    let mut cfg = AuthConfig::development();
    cfg.rate.scribe_per_min = 3;
    let (state, _) = state_with(cfg).await;
    // Windows are fixed-minute and persisted, so a previous run within the
    // same minute would otherwise leave this principal already exhausted.
    sqlx::query("DELETE FROM rate_limit_windows WHERE key LIKE '%:scribe'")
        .execute(&state.pool)
        .await
        .unwrap();
    let (_, patient_id, _) = register_patient(&state).await;
    let (st, enc) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.lopez",
        Some(json!({ "patient_id": patient_id })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{enc}");
    let enc = enc["id"].as_str().unwrap().to_string();
    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/recording-consent"),
        "dev-dr.lopez",
        Some(json!({ "granted": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");

    let mut statuses = Vec::new();
    for _ in 0..5 {
        let (st, _) = transcribe(&state, &enc, "dev-dr.lopez").await;
        statuses.push(st);
    }
    assert!(statuses.contains(&StatusCode::OK), "{statuses:?}");
    assert!(
        statuses.contains(&StatusCode::TOO_MANY_REQUESTS),
        "{statuses:?}"
    );
    let first_429 = statuses
        .iter()
        .position(|s| *s == StatusCode::TOO_MANY_REQUESTS)
        .unwrap();
    assert!(
        statuses[first_429..]
            .iter()
            .all(|s| *s == StatusCode::TOO_MANY_REQUESTS),
        "{statuses:?}"
    );

    // The general API family is unaffected: the workspace still loads.
    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.lopez",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Deterministic structured output, provenance, no raw audio
// ---------------------------------------------------------------------------

#[tokio::test]
async fn transcription_yields_valid_structured_draft_bound_to_note_version() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;

    // A note at version 1 exists when the recording is processed.
    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "reason_for_encounter": "Follow-up (typed by clinician)" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");
    assert_eq!(note["version"], json!(1));

    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");
    assert_eq!(draft["status"], json!("awaiting_review"));
    assert_eq!(draft["note_version"], json!(1));
    assert_eq!(draft["route"], json!("dmind-fake"));
    let output = &draft["output"];
    assert_eq!(output["schema_version"], json!("scribe-draft.v1"));
    assert_eq!(output["source_note_version"], json!(1));
    assert_eq!(output["language"], json!("en"));

    // The structured contract round-trips through the strict validator.
    let parsed: wellos_domain::ai::ScribeDraftV1 =
        serde_json::from_value(output.clone()).expect("draft parses");
    parsed.validate().expect("draft validates");
    assert!(!parsed.transcript.is_empty());
    assert!(parsed.sections.len() >= 5, "{:?}", parsed.sections.len());
    assert!(
        parsed.sections.iter().any(|s| s.review_needed),
        "low-confidence sections must be marked for review"
    );
    assert!(
        !parsed.flags.is_empty(),
        "the fake script contains a contradiction"
    );
    // Every proposed section is assembled from cited transcript segments,
    // never invented: each section's text is exactly its segments joined.
    for section in &parsed.sections {
        assert!(!section.segments.is_empty(), "{section:?}");
        let quoted: Vec<&str> = section
            .segments
            .iter()
            .map(|i| parsed.transcript[*i as usize].text.trim_end_matches('.'))
            .collect();
        assert_eq!(
            section.text,
            format!("{}.", quoted.join(". ")),
            "{section:?}"
        );
    }
    assert!(!parsed.limitations.is_empty());

    // Determinism: a second recording produces identical structured text.
    let (st, draft2) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft2}");
    assert_eq!(draft2["output"]["sections"], output["sections"]);
    assert_eq!(draft2["output"]["transcript"], output["transcript"]);

    // Only the newest draft is reviewable.
    let statuses: Vec<String> = sqlx::query_scalar(
        "SELECT status FROM ai_artifacts WHERE encounter_id = $1::uuid AND artifact_type = 'scribe_draft'
         ORDER BY created_at",
    )
    .bind(&enc)
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        statuses,
        vec!["superseded".to_string(), "awaiting_review".to_string()]
    );

    // Provenance: recording hash + note version, and outbox events.
    let citations: Value =
        sqlx::query_scalar("SELECT citations FROM ai_artifacts WHERE id = $1::uuid")
            .bind(draft["id"].as_str().unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let cites: Vec<String> = serde_json::from_value(citations).unwrap();
    assert!(
        cites.iter().any(|c| c.starts_with("recording:sha256:")),
        "{cites:?}"
    );
    assert!(
        cites.contains(&format!("encounter_note:{enc}:v1")),
        "{cites:?}"
    );
    let transcribed: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outbox_events WHERE event_type = 'encounter.scribe.transcribed'
           AND resource_refs->>'encounter_id' = $1",
    )
    .bind(&enc)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(transcribed, 2);
}

#[tokio::test]
async fn raw_audio_is_never_persisted_anywhere_in_the_database() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let audio = synthetic_audio(3_000);
    let (st, draft) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe"),
        "dev-dr.garcia",
        Some(transcribe_body(&audio, 30_000)),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{draft}");

    // Neither the raw bytes nor their base64 form appear in any column of
    // any user table: the only trace is a one-way hash.
    let marker = String::from_utf8_lossy(SYNTHETIC_AUDIO_MARKER).to_string();
    let encoded_prefix: String = b64(&audio).chars().take(32).collect();
    let tables: Vec<String> = sqlx::query_scalar(
        "SELECT table_name FROM information_schema.tables
         WHERE table_schema = 'public' AND table_type = 'BASE TABLE'",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert!(!tables.is_empty());
    for table in &tables {
        let hits: i64 = sqlx::query_scalar(&format!(
            "SELECT COUNT(*) FROM \"{table}\" t WHERE t::text LIKE '%' || $1 || '%' OR t::text LIKE '%' || $2 || '%'"
        ))
        .bind(&marker)
        .bind(&encoded_prefix)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(hits, 0, "raw audio found in table {table}");
    }
    assert!(!tables
        .iter()
        .any(|t| t.contains("audio") || t.contains("recording_blob")));

    let input_hash: String =
        sqlx::query_scalar("SELECT input_hash FROM ai_artifacts WHERE id = $1::uuid")
            .bind(draft["id"].as_str().unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(input_hash.len(), 64);
}

#[tokio::test]
async fn provider_failure_is_safe_bounded_and_retryable() {
    let (state, fake) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;

    fake.set_unavailable(true);
    let (st, err) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{err}");
    assert_eq!(err["error"]["code"], json!("scribe_unavailable"));
    let message = err["error"]["message"].as_str().unwrap_or("");
    assert!(
        !message.contains("offline"),
        "provider detail must not leak: {message}"
    );
    let artifacts: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM ai_artifacts WHERE encounter_id = $1::uuid AND artifact_type = 'scribe_draft'",
    )
    .bind(&enc)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(artifacts, 0);
    let failures: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outbox_events WHERE event_type = 'ai.generation.failed'
           AND resource_refs->>'encounter_id' = $1",
    )
    .bind(&enc)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(failures, 1);

    // The browser keeps the recording; a retry after recovery succeeds.
    fake.set_unavailable(false);
    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");
}

// ---------------------------------------------------------------------------
// Authorization boundaries
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scribe_requires_own_active_consultation_in_scope() {
    let (state, _) = test_state().await;
    let (patient_id, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;

    // Another physician at the same facility: not their encounter.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/recording-consent"),
        "dev-dr.lopez",
        Some(json!({ "granted": true })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{err}");
    let (st, err) = transcribe(&state, &enc, "dev-dr.lopez").await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{err}");

    // A physician scoped to another facility gets the non-enumerating 404.
    let (st, _) = transcribe(&state, &enc, "dev-dr.annex").await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Registration staff have no documentation right at all.
    let (st, _) = transcribe(&state, &enc, "dev-reg.rivera").await;
    assert_ne!(st, StatusCode::OK);

    // Order-only encounters never accept recordings.
    let (st, order_only) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id, "encounter_type": "order_only" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{order_only}");
    let oo = order_only["id"].as_str().unwrap();
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{oo}/recording-consent"),
        "dev-dr.garcia",
        Some(json!({ "granted": true })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("not_a_consultation"));
}

#[tokio::test]
async fn closed_encounters_reject_transcription_and_application() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");
    let artifact = draft["id"].as_str().unwrap().to_string();

    let (st, cancelled) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/cancel"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{cancelled}");

    let (st, err) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("encounter_not_active"));

    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe/{artifact}/review"),
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "sections": [{ "section": "plan", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("encounter_not_active"));

    // Closing retired the awaiting draft.
    let status: String = sqlx::query_scalar("SELECT status FROM ai_artifacts WHERE id = $1::uuid")
        .bind(&artifact)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(status, "superseded");
}

// ---------------------------------------------------------------------------
// Application: safe merge, per-section, version binding
// ---------------------------------------------------------------------------

#[tokio::test]
async fn application_fills_empty_sections_appends_to_filled_and_never_overwrites() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "assessment": "Clinician's own assessment." })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");
    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");
    let artifact = draft["id"].as_str().unwrap().to_string();
    let proposed = |section: &str| -> String {
        draft["output"]["sections"]
            .as_array()
            .unwrap()
            .iter()
            .find(|s| s["section"] == json!(section))
            .and_then(|s| s["text"].as_str())
            .unwrap()
            .to_string()
    };
    let review_path = format!("/api/v1/encounters/{enc}/scribe/{artifact}/review");

    // Filling a section the clinician already wrote is refused: their text
    // is never replaced.
    let (st, err) = call(
        &state,
        "POST",
        &review_path,
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 1,
            "assessment": "Clinician's own assessment.",
            "sections": [{ "section": "assessment", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("section_not_empty"));

    // Fill an empty section and append to the filled one in one atomic
    // application; unsaved clinician text in a third section is kept.
    let (st, applied) = call(
        &state,
        "POST",
        &review_path,
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 1,
            "assessment": "Clinician's own assessment.",
            "follow_up": "Typed while the recording processed (unsaved).",
            "sections": [
                { "section": "history_present_illness", "mode": "fill" },
                { "section": "assessment", "mode": "append" },
            ],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{applied}");
    assert_eq!(applied["status"], json!("approved"));
    assert_eq!(applied["note"]["version"], json!(2));
    let (st, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ws["note"]["version"], json!(2));
    assert_eq!(
        ws["note"]["history_present_illness"],
        json!(proposed("history_present_illness"))
    );
    assert_eq!(
        ws["note"]["assessment"],
        json!(format!(
            "Clinician's own assessment.\n\n{}",
            proposed("assessment")
        ))
    );
    assert_eq!(
        ws["note"]["follow_up"],
        json!("Typed while the recording processed (unsaved).")
    );
    let applied_sections: Vec<&str> = ws["scribe_draft"]["review_detail"]["applied"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["section"].as_str().unwrap())
        .collect();
    assert_eq!(
        applied_sections,
        vec!["history_present_illness", "assessment"]
    );

    // Remaining sections can still be applied one at a time against the new
    // version; a section already applied cannot be applied twice.
    let (st, err) = call(
        &state,
        "POST",
        &review_path,
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 2,
            "history_present_illness": proposed("history_present_illness"),
            "assessment": format!("Clinician's own assessment.\n\n{}", proposed("assessment")),
            "follow_up": "Typed while the recording processed (unsaved).",
            "sections": [{ "section": "history_present_illness", "mode": "append" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("section_already_applied"));
    let (st, applied) = call(
        &state,
        "POST",
        &review_path,
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 2,
            "history_present_illness": proposed("history_present_illness"),
            "assessment": format!("Clinician's own assessment.\n\n{}", proposed("assessment")),
            "follow_up": "Typed while the recording processed (unsaved).",
            "sections": [{ "section": "plan", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{applied}");
    assert_eq!(applied["note"]["version"], json!(3));

    // Sections the draft never proposed, or unknown modes, are rejected.
    let (st, err) = call(
        &state,
        "POST",
        &review_path,
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 3,
            "sections": [{ "section": "plan", "mode": "replace" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{err}");
    // An approved (partially applied) draft can no longer be dismissed.
    let (st, err) = call(
        &state,
        "POST",
        &review_path,
        "dev-dr.garcia",
        Some(json!({ "decision": "dismiss" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("artifact_not_reviewable"));

    // Application and approval are recorded in the outbox.
    let applied_events: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outbox_events WHERE event_type = 'encounter.scribe.applied'
           AND resource_refs->>'encounter_id' = $1",
    )
    .bind(&enc)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(applied_events, 2);
}

#[tokio::test]
async fn application_against_a_changed_note_version_is_refused() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");
    let artifact = draft["id"].as_str().unwrap().to_string();
    assert_eq!(draft["note_version"], Value::Null);

    // The note changes underneath (another tab saved).
    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "plan": "Saved elsewhere." })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");

    // Applying with the version the clinician was looking at (none) fails
    // without touching the note.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe/{artifact}/review"),
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "sections": [{ "section": "plan", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("version_required"));
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe/{artifact}/review"),
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 7,
            "plan": "Saved elsewhere.",
            "sections": [{ "section": "plan", "mode": "append" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("version_conflict"));
    let (_, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(ws["note"]["version"], json!(1));
    assert_eq!(ws["note"]["plan"], json!("Saved elsewhere."));
    assert_eq!(ws["scribe_draft"]["status"], json!("awaiting_review"));

    // Reloading and applying against the live version works, and the
    // dismissed path is decision-only.
    let (st, applied) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe/{artifact}/review"),
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 1,
            "plan": "Saved elsewhere.",
            "sections": [{ "section": "plan", "mode": "append" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{applied}");
    assert_eq!(applied["note"]["version"], json!(2));

    let (st, draft2) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft2}");
    let (st, dismissed) = call(
        &state,
        "POST",
        &format!(
            "/api/v1/encounters/{enc}/scribe/{}/review",
            draft2["id"].as_str().unwrap()
        ),
        "dev-dr.garcia",
        Some(json!({ "decision": "dismiss" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{dismissed}");
    assert_eq!(dismissed["status"], json!("rejected"));
    let (_, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(ws["note"]["version"], json!(2));
}

#[tokio::test]
async fn signed_notes_cannot_receive_scribe_text() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");
    let artifact = draft["id"].as_str().unwrap().to_string();
    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "reason_for_encounter": "Visit", "assessment": "Stable" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");
    let (st, signed) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/sign"),
        "dev-dr.garcia",
        Some(json!({ "version": 1 })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{signed}");

    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/scribe/{artifact}/review"),
        "dev-dr.garcia",
        Some(json!({
            "decision": "apply",
            "version": 1,
            "reason_for_encounter": "Visit",
            "assessment": "Stable",
            "sections": [{ "section": "plan", "mode": "fill" }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("encounter_not_active"));
}

// ---------------------------------------------------------------------------
// Workspace brief + diagnostics, dashboard cockpit, create-or-resume
// ---------------------------------------------------------------------------

async fn seeded_patient(state: &AppState, given: &str) -> Value {
    let (st, res) = call(
        state,
        "GET",
        &format!("/api/v1/patients?query={given}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{res}");
    res["patients"]
        .as_array()
        .and_then(|items| {
            items
                .iter()
                .find(|p| p["given_name"] == json!(given))
                .cloned()
        })
        .unwrap_or_else(|| panic!("seeded patient {given} not found: {res}"))
}

#[tokio::test]
async fn workspace_carries_patient_brief_and_grouped_diagnostic_history() {
    let (state, _) = test_state().await;
    let carlos = seeded_patient(&state, "Carlos").await;
    let carlos_id = carlos["id"].as_str().unwrap().to_string();
    let (st, enc) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": carlos_id, "resume": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{enc}");
    let enc_id = enc["id"].as_str().unwrap().to_string();

    let (st, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc_id}?lang=es"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ws}");

    let brief = &ws["brief"];
    assert!(brief["generated_at"].is_string());
    assert!(brief["recent_notes"].is_array());
    assert!(brief["open_tasks"].is_array());
    assert!(brief["open_requests"].is_array(), "{brief}");
    assert!(
        brief["recent_abnormal"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["code"] == json!("2345-7") && o["abnormal"] == json!("high")),
        "{brief}"
    );
    assert!(
        brief["open_requests"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["loop_state"] == json!("ordered")
                && r["display"].as_str().unwrap_or("").starts_with("Glucose")),
        "pending glucose order should be listed: {brief}"
    );

    let dx = &ws["diagnostics"];
    let tests = dx["tests"].as_array().expect("tests grouped");
    let glucose = tests
        .iter()
        .find(|t| t["code"] == json!("2345-7"))
        .unwrap_or_else(|| panic!("glucose group missing: {dx}"));
    assert_eq!(glucose["unit"], json!("mg/dL"));
    assert_eq!(glucose["reference_range"], json!("70-99 mg/dL"));
    assert_eq!(glucose["direction"], json!("rising"), "{glucose}");
    assert_eq!(glucose["latest_abnormal"], json!("high"));
    assert!(glucose["pending_count"].as_u64().unwrap() >= 1, "{glucose}");
    let values: Vec<String> = glucose["results"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| r["value"].to_string().trim_matches('"').to_string())
        .collect();
    assert!(
        values.iter().any(|v| v == "118") && values.iter().any(|v| v == "134"),
        "{values:?}"
    );
    for r in glucose["results"].as_array().unwrap() {
        assert!(r["effective_at"].is_string());
        assert!(r["abnormal"].is_string() || r["abnormal"].is_null());
    }
    let potassium = tests.iter().find(|t| t["code"] == json!("2823-3")).unwrap();
    assert_eq!(potassium["latest_abnormal"], json!("high"));
    assert!(potassium["results"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r["critical"] == json!(true)));

    // Assistive dMind commentary: Spanish, cites facts, labelled, bounded.
    let analysis = &dx["analysis"];
    assert_eq!(analysis["schema_version"], json!("diagnostic-trends.v1"));
    assert_eq!(analysis["language"], json!("es"));
    assert_eq!(analysis["provider"]["provider"], json!("dmind-fake"));
    let statements = analysis["statements"].as_array().unwrap();
    assert!(!statements.is_empty());
    for s in statements {
        assert!(!s["facts"].as_array().unwrap().is_empty(), "{s}");
        let text = s["text"].as_str().unwrap().to_lowercase();
        for banned in ["recomend", "recommend", "prescrib", "diagnos"] {
            assert!(!text.contains(banned), "{text}");
        }
    }
    assert!(!analysis["limitations"].as_array().unwrap().is_empty());

    // Default language is English.
    let (_, ws_en) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc_id}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(ws_en["diagnostics"]["analysis"]["language"], json!("en"));

    // Clean up so the seeded worklist counts stay stable for other tests.
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc_id}/cancel"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn start_consultation_resumes_own_in_progress_consultation() {
    let (state, _) = test_state().await;
    let (_, patient_id, identifier) = register_patient(&state).await;

    let (st, first) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id, "resume": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{first}");
    assert_eq!(first["resumed"], json!(false));
    let first_id = first["id"].as_str().unwrap().to_string();

    // Same clinician, same patient: the open consultation is returned.
    let (st, again) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id, "resume": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{again}");
    assert_eq!(again["resumed"], json!(true));
    assert_eq!(again["id"], json!(first_id));

    // Search surfaces the same hint without exposing anything else.
    let (st, res) = call(
        &state,
        "GET",
        &format!("/api/v1/patients?query={identifier}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{res}");
    let items = res["patients"].as_array().unwrap();
    let me = items
        .iter()
        .find(|p| p["id"] == json!(patient_id))
        .unwrap_or_else(|| panic!("{res}"));
    assert_eq!(me["open_consultation_id"], json!(first_id));

    // Another clinician does not resume someone else's consultation.
    let (st, other) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.lopez",
        Some(json!({ "patient_id": patient_id, "resume": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{other}");
    assert_eq!(other["resumed"], json!(false));
    assert_ne!(other["id"], json!(first_id));

    // Without the flag, or for order-only contexts, a new encounter opens.
    let (st, fresh) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{fresh}");
    assert_eq!(fresh["resumed"], json!(false));
    assert_ne!(fresh["id"], json!(first_id));

    // After the consultation closes it is no longer resumable.
    for id in [first_id.as_str(), fresh["id"].as_str().unwrap()] {
        let (st, _) = call(
            &state,
            "POST",
            &format!("/api/v1/encounters/{id}/cancel"),
            "dev-dr.garcia",
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }
    let (st, after) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id, "resume": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{after}");
    assert_eq!(after["resumed"], json!(false));
}

#[tokio::test]
async fn dashboard_cockpit_is_scoped_and_free_of_generated_text() {
    let (state, _) = test_state().await;
    let (_, enc) = start_consultation(&state).await;
    grant_consent(&state, &enc).await;
    let (st, draft) = transcribe(&state, &enc, "dev-dr.garcia").await;
    assert_eq!(st, StatusCode::OK, "{draft}");

    let (st, cockpit) = call(
        &state,
        "GET",
        "/api/v1/dashboard/cockpit",
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{cockpit}");
    assert!(cockpit["generated_at"].is_string());
    let drafts = cockpit["draft_consultations"].as_array().unwrap();
    assert!(
        drafts.iter().any(|d| d["id"] == json!(enc)),
        "own in-progress consultation must be listed: {cockpit}"
    );
    for d in drafts {
        assert!(d["patient"]["given_name"].is_string(), "{d}");
    }
    let attention = cockpit["attention"].as_array().unwrap();
    assert!(
        attention
            .iter()
            .any(|a| a["open_alerts"].as_u64().unwrap_or(0) > 0),
        "seeded critical alert should draw attention: {cockpit}"
    );
    assert!(!cockpit["pending_tasks"].as_array().unwrap().is_empty());
    let activity = cockpit["ai_activity"].as_array().unwrap();
    assert!(activity
        .iter()
        .any(|a| a["artifact_type"] == json!("scribe_draft")));
    for a in activity {
        assert!(
            a.get("output").is_none(),
            "no generated text on the dashboard: {a}"
        );
        assert!(a.get("summary").is_none(), "{a}");
    }

    // Another physician does not see dr.garcia's draft consultation.
    let (st, other) = call(
        &state,
        "GET",
        "/api/v1/dashboard/cockpit",
        "dev-dr.lopez",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{other}");
    assert!(
        !other["draft_consultations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["id"] == json!(enc)),
        "{other}"
    );

    // The out-of-facility physician sees nothing from facility A.
    let (st, annex) = call(
        &state,
        "GET",
        "/api/v1/dashboard/cockpit",
        "dev-dr.annex",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{annex}");
    assert!(annex["attention"].as_array().unwrap().is_empty(), "{annex}");
    assert!(
        !annex["draft_consultations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|d| d["id"] == json!(enc)),
        "{annex}"
    );
    assert!(
        annex["pending_tasks"].as_array().unwrap().is_empty(),
        "{annex}"
    );

    // Registration staff have no worklist right.
    let (st, _) = call(
        &state,
        "GET",
        "/api/v1/dashboard/cockpit",
        "dev-reg.rivera",
        None,
    )
    .await;
    assert_ne!(st, StatusCode::OK);

    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/cancel"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}
