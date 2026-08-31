//! Encounter documentation: structured consultation notes, vital signs,
//! diagnoses, and the governed dMind documentation aid.
//!
//! Integrity model: drafts are editable under optimistic concurrency; signing
//! creates an immutable record; corrections after signature are dated addenda
//! linked to the original note. Every read and write passes through the
//! central policy guard and is audited.

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, facility_scope, ResourceCtx};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::Json;
use dmind_gateway::SummaryRequest;
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;
use wellos_domain::ai::ArtifactStatus;

struct EncounterCtx {
    tenant_id: Uuid,
    patient_id: Uuid,
    facility_id: Uuid,
    practitioner_id: Uuid,
    status: String,
}

async fn load_encounter(state: &AppState, id: Uuid) -> Result<EncounterCtx, ApiError> {
    let row = sqlx::query(
        "SELECT tenant_id, patient_id, facility_id, practitioner_id, status
         FROM encounters WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok(EncounterCtx {
        tenant_id: row.get("tenant_id"),
        patient_id: row.get("patient_id"),
        facility_id: row.get("facility_id"),
        practitioner_id: row.get("practitioner_id"),
        status: row.get("status"),
    })
}

fn resource_ctx(enc: &EncounterCtx) -> ResourceCtx {
    ResourceCtx {
        tenant_id: enc.tenant_id,
        patient_id: Some(enc.patient_id),
        facility_id: Some(enc.facility_id),
    }
}

/// Documentation writes attach to the practitioner's own active encounter.
fn require_own_active(enc: &EncounterCtx, ctx: &AuthContext) -> Result<(), ApiError> {
    if enc.practitioner_id != ctx.user_id {
        return Err(ApiError::forbidden(
            "documentation requires the practitioner's own encounter",
        ));
    }
    if enc.status != "in_progress" {
        return Err(ApiError::conflict(
            "encounter_not_active",
            "this encounter is no longer in progress",
        ));
    }
    Ok(())
}

const NOTE_SECTIONS: &[&str] = &[
    "reason_for_encounter",
    "history_present_illness",
    "medical_history",
    "review_of_systems",
    "physical_exam",
    "assessment",
    "plan",
    "follow_up",
];

// ---------------------------------------------------------------------------
// GET /api/v1/encounters/:id — consultation workspace payload
// ---------------------------------------------------------------------------

pub async fn workspace(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let enc = load_encounter(&state, id).await?;
    guard(
        &state,
        &ctx,
        actions::PATIENT_READ,
        "encounter",
        Some(resource_ctx(&enc)),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;

    let row = sqlx::query(
        "SELECT e.status, e.encounter_type, e.started_at, e.completed_at,
                u.display_name AS practitioner, f.name AS facility_name,
                p.family_name, p.given_name, p.birth_date, p.sex, p.identifier
         FROM encounters e
         JOIN users u ON u.id = e.practitioner_id
         JOIN facilities f ON f.id = e.facility_id
         JOIN patients p ON p.id = e.patient_id
         WHERE e.id = $1",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;

    let note = sqlx::query(
        "SELECT n.id, n.status, n.version, n.reason_for_encounter, n.history_present_illness,
                n.medical_history, n.review_of_systems, n.physical_exam, n.assessment,
                n.plan, n.follow_up, n.updated_at, n.signed_at,
                a.display_name AS author, s.display_name AS signed_by_name
         FROM encounter_notes n
         JOIN users a ON a.id = n.author_id
         LEFT JOIN users s ON s.id = n.signed_by
         WHERE n.tenant_id = $1 AND n.encounter_id = $2",
    )
    .bind(enc.tenant_id)
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    let (note_json, note_id) = match &note {
        Some(n) => {
            let nid: Uuid = n.get("id");
            (
                json!({
                    "id": nid,
                    "status": n.get::<String,_>("status"),
                    "version": n.get::<i64,_>("version"),
                    "reason_for_encounter": n.get::<Option<String>,_>("reason_for_encounter"),
                    "history_present_illness": n.get::<Option<String>,_>("history_present_illness"),
                    "medical_history": n.get::<Option<String>,_>("medical_history"),
                    "review_of_systems": n.get::<Option<String>,_>("review_of_systems"),
                    "physical_exam": n.get::<Option<String>,_>("physical_exam"),
                    "assessment": n.get::<Option<String>,_>("assessment"),
                    "plan": n.get::<Option<String>,_>("plan"),
                    "follow_up": n.get::<Option<String>,_>("follow_up"),
                    "author": n.get::<String,_>("author"),
                    "updated_at": n.get::<chrono::DateTime<chrono::Utc>,_>("updated_at"),
                    "signed_at": n.get::<Option<chrono::DateTime<chrono::Utc>>,_>("signed_at"),
                    "signed_by": n.get::<Option<String>,_>("signed_by_name"),
                }),
                Some(nid),
            )
        }
        None => (Value::Null, None),
    };

    let addenda = match note_id {
        Some(nid) => sqlx::query(
            "SELECT a.body, a.created_at, u.display_name AS author
             FROM encounter_note_addenda a JOIN users u ON u.id = a.author_id
             WHERE a.tenant_id = $1 AND a.note_id = $2 ORDER BY a.created_at",
        )
        .bind(enc.tenant_id)
        .bind(nid)
        .fetch_all(&state.pool)
        .await?
        .iter()
        .map(|r| {
            json!({
                "body": r.get::<String,_>("body"),
                "author": r.get::<String,_>("author"),
                "created_at": r.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
            })
        })
        .collect::<Vec<_>>(),
        None => Vec::new(),
    };

    let vitals_row = |r: &sqlx::postgres::PgRow| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "encounter_id": r.get::<Uuid,_>("encounter_id"),
            "systolic_mmhg": r.get::<Option<Decimal>,_>("systolic_mmhg"),
            "diastolic_mmhg": r.get::<Option<Decimal>,_>("diastolic_mmhg"),
            "heart_rate_bpm": r.get::<Option<Decimal>,_>("heart_rate_bpm"),
            "respiratory_rate_bpm": r.get::<Option<Decimal>,_>("respiratory_rate_bpm"),
            "temperature_c": r.get::<Option<Decimal>,_>("temperature_c"),
            "spo2_percent": r.get::<Option<Decimal>,_>("spo2_percent"),
            "weight_kg": r.get::<Option<Decimal>,_>("weight_kg"),
            "height_cm": r.get::<Option<Decimal>,_>("height_cm"),
            "bmi": r.get::<Option<Decimal>,_>("bmi"),
            "recorded_at": r.get::<chrono::DateTime<chrono::Utc>,_>("recorded_at"),
        })
    };
    let vitals = sqlx::query(
        "SELECT * FROM vital_signs WHERE tenant_id = $1 AND encounter_id = $2
         ORDER BY recorded_at DESC",
    )
    .bind(enc.tenant_id)
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(&vitals_row)
    .collect::<Vec<_>>();
    let previous_vitals = sqlx::query(
        "SELECT * FROM vital_signs WHERE tenant_id = $1 AND patient_id = $2
           AND encounter_id <> $3
         ORDER BY recorded_at DESC LIMIT 5",
    )
    .bind(enc.tenant_id)
    .bind(enc.patient_id)
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(&vitals_row)
    .collect::<Vec<_>>();

    let diagnoses = sqlx::query(
        "SELECT id, code, display, clinical_status, recorded_at, encounter_id
         FROM conditions WHERE tenant_id = $1 AND patient_id = $2
         ORDER BY recorded_at DESC",
    )
    .bind(enc.tenant_id)
    .bind(enc.patient_id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "code": r.get::<String,_>("code"),
            "display": r.get::<String,_>("display"),
            "status": r.get::<String,_>("clinical_status"),
            "recorded_at": r.get::<chrono::DateTime<chrono::Utc>,_>("recorded_at"),
            "this_encounter": r.get::<Option<Uuid>,_>("encounter_id") == Some(id),
        })
    })
    .collect::<Vec<_>>();

    let allergies = sqlx::query(
        "SELECT substance, criticality FROM allergies
         WHERE tenant_id = $1 AND patient_id = $2 ORDER BY recorded_at",
    )
    .bind(enc.tenant_id)
    .bind(enc.patient_id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "substance": r.get::<String,_>("substance"),
            "criticality": r.get::<String,_>("criticality"),
        })
    })
    .collect::<Vec<_>>();

    let medications = sqlx::query(
        "SELECT name, status FROM medications
         WHERE tenant_id = $1 AND patient_id = $2 ORDER BY recorded_at",
    )
    .bind(enc.tenant_id)
    .bind(enc.patient_id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "name": r.get::<String,_>("name"),
            "status": r.get::<String,_>("status"),
        })
    })
    .collect::<Vec<_>>();

    let alerts = sqlx::query(
        "SELECT severity, message, created_at FROM alerts
         WHERE tenant_id = $1 AND patient_id = $2 AND status = 'open'
         ORDER BY created_at DESC",
    )
    .bind(enc.tenant_id)
    .bind(enc.patient_id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "severity": r.get::<String,_>("severity"),
            "message": r.get::<String,_>("message"),
            "created_at": r.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
        })
    })
    .collect::<Vec<_>>();

    let service_requests = sqlx::query(
        "SELECT id, display, loop_state, created_at FROM service_requests
         WHERE tenant_id = $1 AND encounter_id = $2 ORDER BY created_at DESC",
    )
    .bind(enc.tenant_id)
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "display": r.get::<String,_>("display"),
            "loop_state": r.get::<String,_>("loop_state"),
            "created_at": r.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
        })
    })
    .collect::<Vec<_>>();

    // Latest dMind documentation draft for this encounter (assistive only).
    let ai_draft = sqlx::query(
        "SELECT id, status, output, limitations, citations, model, model_version,
                generated_at, review_decision
         FROM ai_artifacts
         WHERE tenant_id = $1 AND encounter_id = $2 AND artifact_type = 'encounter_summary'
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(enc.tenant_id)
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "status": r.get::<String,_>("status"),
            "output": r.get::<Option<Value>,_>("output"),
            "limitations": r.get::<Value,_>("limitations"),
            "citations": r.get::<Value,_>("citations"),
            "model": r.get::<Option<String>,_>("model"),
            "model_version": r.get::<Option<String>,_>("model_version"),
            "generated_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("generated_at"),
            "review_decision": r.get::<Option<String>,_>("review_decision"),
        })
    })
    .unwrap_or(Value::Null);

    // Display-only capability hints mirroring central policy semantics; the
    // backend guards remain authoritative for every write.
    let covers = |action: &str| match facility_scope(&ctx, action) {
        None => true,
        Some(ids) => ids.contains(&enc.facility_id),
    };
    let own = enc.practitioner_id == ctx.user_id;
    let active = enc.status == "in_progress";
    let note_signed = note
        .as_ref()
        .is_some_and(|n| n.get::<String, _>("status") == "signed");
    let capabilities = json!({
        "can_document": own && active && covers(actions::ENCOUNTER_DOCUMENT),
        "can_sign": own && active && !note_signed && covers(actions::ENCOUNTER_SIGN),
        "can_add_addendum": own && note_signed && covers(actions::ENCOUNTER_DOCUMENT),
        "can_order_lab": own && active && covers(actions::SERVICE_REQUEST_CREATE),
    });

    Ok(Json(json!({
        "encounter": {
            "id": id,
            "status": row.get::<String,_>("status"),
            "encounter_type": row.get::<String,_>("encounter_type"),
            "started_at": row.get::<chrono::DateTime<chrono::Utc>,_>("started_at"),
            "completed_at": row.get::<Option<chrono::DateTime<chrono::Utc>>,_>("completed_at"),
            "practitioner": row.get::<String,_>("practitioner"),
            "facility_name": row.get::<String,_>("facility_name"),
            "own": own,
        },
        "patient": {
            "id": enc.patient_id,
            "family_name": row.get::<String,_>("family_name"),
            "given_name": row.get::<String,_>("given_name"),
            "birth_date": row.get::<chrono::NaiveDate,_>("birth_date"),
            "sex": row.get::<String,_>("sex"),
            "identifier": row.get::<String,_>("identifier"),
        },
        "allergies": allergies,
        "medications": medications,
        "alerts": alerts,
        "note": note_json,
        "addenda": addenda,
        "vitals": vitals,
        "previous_vitals": previous_vitals,
        "diagnoses": diagnoses,
        "service_requests": service_requests,
        "ai_draft": ai_draft,
        "capabilities": capabilities,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/encounters/:id/note — create or update the draft note
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SaveNote {
    /// Expected note version for optimistic concurrency; omit when creating.
    pub version: Option<i64>,
    pub reason_for_encounter: Option<String>,
    pub history_present_illness: Option<String>,
    pub medical_history: Option<String>,
    pub review_of_systems: Option<String>,
    pub physical_exam: Option<String>,
    pub assessment: Option<String>,
    pub plan: Option<String>,
    pub follow_up: Option<String>,
}

impl SaveNote {
    fn sections(&self) -> [&Option<String>; 8] {
        [
            &self.reason_for_encounter,
            &self.history_present_illness,
            &self.medical_history,
            &self.review_of_systems,
            &self.physical_exam,
            &self.assessment,
            &self.plan,
            &self.follow_up,
        ]
    }
}

pub async fn save_note(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<SaveNote>,
) -> Result<Json<Value>, ApiError> {
    for (name, value) in NOTE_SECTIONS.iter().zip(body.sections()) {
        if value.as_deref().is_some_and(|v| v.len() > 20_000) {
            return Err(ApiError::bad_request(
                "validation_failed",
                format!("{name} exceeds 20000 characters"),
            ));
        }
    }
    let enc = load_encounter(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::ENCOUNTER_DOCUMENT,
        "encounter_note",
        Some(resource_ctx(&enc)),
    )
    .await?;
    require_own_active(&enc, &ctx)?;

    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let existing = sqlx::query(
        "SELECT id, status, version FROM encounter_notes
         WHERE tenant_id = $1 AND encounter_id = $2 FOR UPDATE",
    )
    .bind(enc.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;

    let (note_id, new_version) = match existing {
        None => {
            let note_id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO encounter_notes
                 (id, tenant_id, encounter_id, patient_id, author_id, status, version,
                  reason_for_encounter, history_present_illness, medical_history,
                  review_of_systems, physical_exam, assessment, plan, follow_up)
                 VALUES ($1,$2,$3,$4,$5,'draft',1,$6,$7,$8,$9,$10,$11,$12,$13)",
            )
            .bind(note_id)
            .bind(enc.tenant_id)
            .bind(id)
            .bind(enc.patient_id)
            .bind(ctx.user_id)
            .bind(&body.reason_for_encounter)
            .bind(&body.history_present_illness)
            .bind(&body.medical_history)
            .bind(&body.review_of_systems)
            .bind(&body.physical_exam)
            .bind(&body.assessment)
            .bind(&body.plan)
            .bind(&body.follow_up)
            .execute(&mut *tx)
            .await?;
            (note_id, 1i64)
        }
        Some(row) => {
            let note_id: Uuid = row.get("id");
            let status: String = row.get("status");
            let current: i64 = row.get("version");
            if status != "draft" {
                return Err(ApiError::conflict(
                    "note_signed",
                    "a signed note is immutable; add an addendum instead",
                ));
            }
            let Some(expected) = body.version else {
                return Err(ApiError::conflict(
                    "version_required",
                    "the note already exists; provide its current version",
                ));
            };
            if expected != current {
                return Err(ApiError::conflict(
                    "version_conflict",
                    "the note was updated by someone else; reload before saving",
                ));
            }
            sqlx::query(
                "UPDATE encounter_notes SET version = version + 1, updated_at = now(),
                        reason_for_encounter=$1, history_present_illness=$2, medical_history=$3,
                        review_of_systems=$4, physical_exam=$5, assessment=$6, plan=$7, follow_up=$8
                 WHERE id = $9 AND status = 'draft' AND version = $10",
            )
            .bind(&body.reason_for_encounter)
            .bind(&body.history_present_illness)
            .bind(&body.medical_history)
            .bind(&body.review_of_systems)
            .bind(&body.physical_exam)
            .bind(&body.assessment)
            .bind(&body.plan)
            .bind(&body.follow_up)
            .bind(note_id)
            .bind(current)
            .execute(&mut *tx)
            .await?;
            (note_id, current + 1)
        }
    };
    audit::emit(
        &mut *tx,
        &ctx,
        "encounter.note.saved",
        &state.cell,
        json!({ "encounter_id": id, "note_id": note_id, "version": new_version }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(
        json!({ "id": note_id, "status": "draft", "version": new_version }),
    ))
}

// ---------------------------------------------------------------------------
// POST /api/v1/encounters/:id/sign — sign the note, complete the encounter
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SignNote {
    pub version: i64,
}

pub async fn sign(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<SignNote>,
) -> Result<Json<Value>, ApiError> {
    let enc = load_encounter(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::ENCOUNTER_SIGN,
        "encounter_note",
        Some(resource_ctx(&enc)),
    )
    .await?;
    require_own_active(&enc, &ctx)?;

    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let note = sqlx::query(
        "SELECT id, status, version, reason_for_encounter, assessment, plan
         FROM encounter_notes WHERE tenant_id = $1 AND encounter_id = $2 FOR UPDATE",
    )
    .bind(enc.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        ApiError::conflict("note_missing", "document the consultation before signing")
    })?;
    let note_id: Uuid = note.get("id");
    if note.get::<String, _>("status") != "draft" {
        return Err(ApiError::conflict(
            "note_signed",
            "this note is already signed",
        ));
    }
    if note.get::<i64, _>("version") != body.version {
        return Err(ApiError::conflict(
            "version_conflict",
            "the note was updated by someone else; reload before signing",
        ));
    }
    let filled = |v: Option<String>| v.is_some_and(|s| !s.trim().is_empty());
    if !filled(note.get("reason_for_encounter")) {
        return Err(ApiError::bad_request(
            "sign_requires_reason",
            "a reason for encounter is required before signing",
        ));
    }
    if !filled(note.get("assessment")) && !filled(note.get("plan")) {
        return Err(ApiError::bad_request(
            "sign_requires_assessment_or_plan",
            "an assessment or plan is required before signing",
        ));
    }
    sqlx::query(
        "UPDATE encounter_notes SET status='signed', version = version + 1,
                signed_at = now(), signed_by = $1, updated_at = now()
         WHERE id = $2 AND status = 'draft' AND version = $3",
    )
    .bind(ctx.user_id)
    .bind(note_id)
    .bind(body.version)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE encounters SET status='completed', completed_at = now()
         WHERE id = $1 AND status = 'in_progress'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "encounter.note.signed",
        &state.cell,
        json!({ "encounter_id": id, "note_id": note_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(
        json!({ "id": note_id, "status": "signed", "encounter_status": "completed" }),
    ))
}

// ---------------------------------------------------------------------------
// POST /api/v1/encounters/:id/addenda — dated correction on a signed note
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AddAddendum {
    pub body: String,
}

pub async fn add_addendum(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<AddAddendum>,
) -> Result<Json<Value>, ApiError> {
    let text = body.body.trim();
    if text.is_empty() || text.len() > 8000 {
        return Err(ApiError::bad_request(
            "validation_failed",
            "addendum body must be between 1 and 8000 characters",
        ));
    }
    let enc = load_encounter(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::ENCOUNTER_DOCUMENT,
        "encounter_note",
        Some(resource_ctx(&enc)),
    )
    .await?;
    // Addenda attach to the practitioner's own signed encounter; the
    // encounter itself is already completed.
    if enc.practitioner_id != ctx.user_id {
        return Err(ApiError::forbidden(
            "addenda require the practitioner's own encounter",
        ));
    }

    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let note = sqlx::query(
        "SELECT id, status FROM encounter_notes
         WHERE tenant_id = $1 AND encounter_id = $2 FOR UPDATE",
    )
    .bind(enc.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| ApiError::conflict("note_missing", "there is no note for this encounter"))?;
    let note_id: Uuid = note.get("id");
    if note.get::<String, _>("status") != "signed" {
        return Err(ApiError::conflict(
            "note_not_signed",
            "addenda apply to signed notes; edit the draft instead",
        ));
    }
    let addendum_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO encounter_note_addenda (id, tenant_id, note_id, author_id, body)
         VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(addendum_id)
    .bind(enc.tenant_id)
    .bind(note_id)
    .bind(ctx.user_id)
    .bind(text)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "encounter.note.addendum",
        &state.cell,
        json!({ "encounter_id": id, "note_id": note_id, "addendum_id": addendum_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({ "id": addendum_id })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/encounters/:id/vitals — structured vital signs with validation
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct RecordVitals {
    pub systolic_mmhg: Option<Decimal>,
    pub diastolic_mmhg: Option<Decimal>,
    pub heart_rate_bpm: Option<Decimal>,
    pub respiratory_rate_bpm: Option<Decimal>,
    pub temperature_c: Option<Decimal>,
    pub spo2_percent: Option<Decimal>,
    pub weight_kg: Option<Decimal>,
    pub height_cm: Option<Decimal>,
    /// Acknowledge values outside the usual range after an explicit warning.
    #[serde(default)]
    pub confirm_unusual: bool,
}

/// Per-measure bounds: values outside the hard range are rejected outright;
/// values outside the usual range require explicit confirmation.
struct VitalBounds {
    field: &'static str,
    hard: (i64, i64),
    usual: (i64, i64),
}

const VITAL_BOUNDS: &[VitalBounds] = &[
    VitalBounds {
        field: "systolic_mmhg",
        hard: (30, 400),
        usual: (70, 220),
    },
    VitalBounds {
        field: "diastolic_mmhg",
        hard: (15, 300),
        usual: (40, 130),
    },
    VitalBounds {
        field: "heart_rate_bpm",
        hard: (10, 350),
        usual: (40, 180),
    },
    VitalBounds {
        field: "respiratory_rate_bpm",
        hard: (2, 120),
        usual: (8, 40),
    },
    VitalBounds {
        field: "temperature_c",
        hard: (25, 45),
        usual: (34, 41),
    },
    VitalBounds {
        field: "spo2_percent",
        hard: (10, 100),
        usual: (85, 100),
    },
    VitalBounds {
        field: "weight_kg",
        hard: (1, 500),
        usual: (2, 250),
    },
    VitalBounds {
        field: "height_cm",
        hard: (20, 280),
        usual: (40, 220),
    },
];

pub async fn record_vitals(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<RecordVitals>,
) -> Result<Json<Value>, ApiError> {
    let values: [(&str, Option<Decimal>); 8] = [
        ("systolic_mmhg", body.systolic_mmhg),
        ("diastolic_mmhg", body.diastolic_mmhg),
        ("heart_rate_bpm", body.heart_rate_bpm),
        ("respiratory_rate_bpm", body.respiratory_rate_bpm),
        ("temperature_c", body.temperature_c),
        ("spo2_percent", body.spo2_percent),
        ("weight_kg", body.weight_kg),
        ("height_cm", body.height_cm),
    ];
    if values.iter().all(|(_, v)| v.is_none()) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "at least one vital sign value is required",
        ));
    }
    let mut unusual: Vec<&str> = Vec::new();
    for bounds in VITAL_BOUNDS {
        let Some(value) = values
            .iter()
            .find(|(f, _)| *f == bounds.field)
            .and_then(|(_, v)| *v)
        else {
            continue;
        };
        if value < Decimal::from(bounds.hard.0) || value > Decimal::from(bounds.hard.1) {
            return Err(ApiError::bad_request(
                "value_out_of_range",
                format!(
                    "{} must be between {} and {}",
                    bounds.field, bounds.hard.0, bounds.hard.1
                ),
            ));
        }
        if value < Decimal::from(bounds.usual.0) || value > Decimal::from(bounds.usual.1) {
            unusual.push(bounds.field);
        }
    }
    if !unusual.is_empty() && !body.confirm_unusual {
        return Err(ApiError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "unusual_values",
            format!(
                "values outside the usual range require confirmation: {}",
                unusual.join(", ")
            ),
        ));
    }

    let enc = load_encounter(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::ENCOUNTER_DOCUMENT,
        "vital_signs",
        Some(resource_ctx(&enc)),
    )
    .await?;
    require_own_active(&enc, &ctx)?;

    // BMI is always server-calculated (kg / m²), never client-supplied.
    let bmi = match (body.weight_kg, body.height_cm) {
        (Some(w), Some(h)) if h > Decimal::ZERO => {
            let meters = h / Decimal::from(100);
            Some((w / (meters * meters)).round_dp(1))
        }
        _ => None,
    };

    let vitals_id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    sqlx::query(
        "INSERT INTO vital_signs
         (id, tenant_id, encounter_id, patient_id, recorded_by, systolic_mmhg, diastolic_mmhg,
          heart_rate_bpm, respiratory_rate_bpm, temperature_c, spo2_percent, weight_kg,
          height_cm, bmi)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
    )
    .bind(vitals_id)
    .bind(enc.tenant_id)
    .bind(id)
    .bind(enc.patient_id)
    .bind(ctx.user_id)
    .bind(body.systolic_mmhg)
    .bind(body.diastolic_mmhg)
    .bind(body.heart_rate_bpm)
    .bind(body.respiratory_rate_bpm)
    .bind(body.temperature_c)
    .bind(body.spo2_percent)
    .bind(body.weight_kg)
    .bind(body.height_cm)
    .bind(bmi)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "encounter.vitals.recorded",
        &state.cell,
        json!({ "encounter_id": id, "vital_signs_id": vitals_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({ "id": vitals_id, "bmi": bmi })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/encounters/:id/diagnoses — encounter-linked diagnosis
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AddDiagnosis {
    pub display: String,
    pub code: Option<String>,
    /// active | provisional | resolved
    pub status: Option<String>,
}

pub async fn add_diagnosis(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<AddDiagnosis>,
) -> Result<Json<Value>, ApiError> {
    let display = body.display.trim();
    if display.is_empty() || display.len() > 200 {
        return Err(ApiError::bad_request(
            "validation_failed",
            "display must be between 1 and 200 characters",
        ));
    }
    let code = body.code.as_deref().map(str::trim).unwrap_or("");
    if code.len() > 32 {
        return Err(ApiError::bad_request(
            "validation_failed",
            "code exceeds 32 characters",
        ));
    }
    let status = body.status.as_deref().unwrap_or("active");
    if !matches!(status, "active" | "provisional" | "resolved") {
        return Err(ApiError::bad_request(
            "validation_failed",
            "status must be active, provisional or resolved",
        ));
    }
    let enc = load_encounter(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::ENCOUNTER_DOCUMENT,
        "condition",
        Some(resource_ctx(&enc)),
    )
    .await?;
    require_own_active(&enc, &ctx)?;

    let dx_id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    sqlx::query(
        "INSERT INTO conditions
         (id, tenant_id, patient_id, code, display, clinical_status, encounter_id, recorded_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(dx_id)
    .bind(enc.tenant_id)
    .bind(enc.patient_id)
    .bind(code)
    .bind(display)
    .bind(status)
    .bind(id)
    .bind(ctx.user_id)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "encounter.diagnosis.added",
        &state.cell,
        json!({ "encounter_id": id, "condition_id": dx_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({ "id": dx_id })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/encounters/:id/cancel
// ---------------------------------------------------------------------------

pub async fn cancel(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let enc = load_encounter(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::ENCOUNTER_DOCUMENT,
        "encounter",
        Some(resource_ctx(&enc)),
    )
    .await?;
    require_own_active(&enc, &ctx)?;

    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let signed: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM encounter_notes
         WHERE tenant_id = $1 AND encounter_id = $2 AND status = 'signed'",
    )
    .bind(enc.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    if signed.is_some() {
        return Err(ApiError::conflict(
            "note_signed",
            "an encounter with a signed note cannot be cancelled",
        ));
    }
    let updated = sqlx::query(
        "UPDATE encounters SET status='cancelled', completed_at = now()
         WHERE id = $1 AND status = 'in_progress'",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "encounter_not_active",
            "this encounter is no longer in progress",
        ));
    }
    audit::emit(
        &mut *tx,
        &ctx,
        "encounter.cancelled",
        &state.cell,
        json!({ "encounter_id": id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({ "id": id, "status": "cancelled" })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/encounters/:id/ai-draft — governed dMind documentation aid
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct AiDraftRequest {
    /// "en" or "es".
    pub language: Option<String>,
}

/// Generate an assistive draft summary from facts already recorded in this
/// encounter. The output is an AIArtifact awaiting explicit clinician review;
/// it never modifies the note, places orders, or introduces new facts. This
/// milestone routes only to the local deterministic provider — no external
/// processing.
pub async fn ai_draft(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<AiDraftRequest>,
) -> Result<Json<Value>, ApiError> {
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
    let enc = load_encounter(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::ENCOUNTER_DOCUMENT,
        "ai_artifact",
        Some(resource_ctx(&enc)),
    )
    .await?;
    require_own_active(&enc, &ctx)?;

    // Facts are restricted to information already recorded in this
    // encounter: note sections, the latest vital-sign set, and diagnoses.
    let mut facts: Vec<(String, String)> = Vec::new();
    let mut missing: Vec<&str> = Vec::new();
    let note = sqlx::query(
        "SELECT id, reason_for_encounter, history_present_illness, medical_history,
                review_of_systems, physical_exam, assessment, plan, follow_up
         FROM encounter_notes WHERE tenant_id = $1 AND encounter_id = $2",
    )
    .bind(enc.tenant_id)
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    match &note {
        Some(n) => {
            let note_id: Uuid = n.get("id");
            for section in NOTE_SECTIONS {
                match n
                    .get::<Option<String>, _>(*section)
                    .filter(|v| !v.trim().is_empty())
                {
                    Some(text) => facts.push((
                        format!("encounter_note:{note_id}:{section}"),
                        format!("{}: {}", section.replace('_', " "), text.trim()),
                    )),
                    None => missing.push(section),
                }
            }
        }
        None => missing.extend(NOTE_SECTIONS),
    }
    let latest_vitals = sqlx::query(
        "SELECT id, systolic_mmhg, diastolic_mmhg, heart_rate_bpm, respiratory_rate_bpm,
                temperature_c, spo2_percent, weight_kg, height_cm, bmi
         FROM vital_signs WHERE tenant_id = $1 AND encounter_id = $2
         ORDER BY recorded_at DESC LIMIT 1",
    )
    .bind(enc.tenant_id)
    .bind(id)
    .fetch_optional(&state.pool)
    .await?;
    match &latest_vitals {
        Some(v) => {
            let vid: Uuid = v.get("id");
            let mut parts: Vec<String> = Vec::new();
            let mut push = |label: &str, value: Option<Decimal>, unit: &str| {
                if let Some(value) = value {
                    parts.push(format!("{label} {value} {unit}"));
                }
            };
            push("BP systolic", v.get("systolic_mmhg"), "mmHg");
            push("BP diastolic", v.get("diastolic_mmhg"), "mmHg");
            push("heart rate", v.get("heart_rate_bpm"), "bpm");
            push("respiratory rate", v.get("respiratory_rate_bpm"), "/min");
            push("temperature", v.get("temperature_c"), "°C");
            push("SpO2", v.get("spo2_percent"), "%");
            push("weight", v.get("weight_kg"), "kg");
            push("height", v.get("height_cm"), "cm");
            push("BMI", v.get("bmi"), "kg/m²");
            if !parts.is_empty() {
                facts.push((format!("vital_signs:{vid}"), parts.join(", ")));
            }
        }
        None => missing.push("vital_signs"),
    }
    let diagnoses = sqlx::query(
        "SELECT id, display, code, clinical_status FROM conditions
         WHERE tenant_id = $1 AND encounter_id = $2 ORDER BY recorded_at",
    )
    .bind(enc.tenant_id)
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    if diagnoses.is_empty() {
        missing.push("diagnoses");
    }
    for d in &diagnoses {
        let dx_id: Uuid = d.get("id");
        let code: String = d.get("code");
        let code_part = if code.is_empty() {
            String::new()
        } else {
            format!(" ({code})")
        };
        facts.push((
            format!("condition:{dx_id}"),
            format!(
                "diagnosis: {}{} [{}]",
                d.get::<String, _>("display"),
                code_part,
                d.get::<String, _>("clinical_status"),
            ),
        ));
    }
    if facts.is_empty() {
        return Err(ApiError::conflict(
            "nothing_to_summarize",
            "record documentation or vital signs before requesting a draft",
        ));
    }

    let req = SummaryRequest {
        template: "encounter-summary@1.0.0".into(),
        facts,
        language: language.into(),
    };
    let resp = state
        .gateway
        .summarize_result(&req)
        .await
        .map_err(|e| match e {
            dmind_gateway::GatewayError::Unavailable(_) => ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "ai_unavailable",
                "the documentation assistant is unavailable; care continues without it",
            ),
            other => ApiError::internal(other),
        })?;

    // Deterministically identified documentation gaps are limitations the
    // clinician sees alongside the provider's own limitations.
    let mut limitations = resp.output.limitations.clone();
    if !missing.is_empty() {
        limitations.push(format!(
            "Incomplete documentation sections: {}",
            missing.join(", ")
        ));
    }

    let artifact_id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    // Any previous unreviewed draft for this encounter is superseded so only
    // one draft awaits review at a time.
    sqlx::query(
        "UPDATE ai_artifacts SET status = $1
         WHERE tenant_id = $2 AND encounter_id = $3 AND artifact_type = 'encounter_summary'
           AND status = $4",
    )
    .bind(ArtifactStatus::Superseded.as_str())
    .bind(enc.tenant_id)
    .bind(id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, encounter_id, artifact_type, autonomy_level, status,
          model, model_version, route, template, input_hash, output, output_schema,
          citations, limitations, generated_at)
         VALUES ($1,$2,$3,$4,'encounter_summary','A1',$5,$6,$7,$8,$9,$10,$11,
                 'result-summary.v1',$12,$13, now())",
    )
    .bind(artifact_id)
    .bind(enc.tenant_id)
    .bind(enc.patient_id)
    .bind(id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .bind(&resp.model)
    .bind(&resp.model_version)
    .bind(&resp.route)
    .bind(&req.template)
    .bind(&resp.input_hash)
    .bind(serde_json::to_value(&resp.output).map_err(ApiError::internal)?)
    .bind(serde_json::to_value(&resp.output.cited_sources).map_err(ApiError::internal)?)
    .bind(serde_json::to_value(&limitations).map_err(ApiError::internal)?)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "ai.artifact.generated",
        &state.cell,
        json!({ "artifact_id": artifact_id, "encounter_id": id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;

    Ok(Json(json!({
        "id": artifact_id,
        "status": ArtifactStatus::AwaitingReview.as_str(),
        "output": resp.output,
        "limitations": limitations,
        "citations": resp.output.cited_sources,
        "model": resp.model,
        "model_version": resp.model_version,
    })))
}
