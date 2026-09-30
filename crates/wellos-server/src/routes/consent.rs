use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, ResourceCtx};
use crate::routes::guard;
use crate::scheduling;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

/// Purposes a patient or representative may grant or revoke themselves
/// through `/api/v1/me/consents`; each gates one scheduling data use.
pub const SELF_SERVICE_PURPOSES: &[&str] = &[
    scheduling::CONSENT_CALENDAR,
    scheduling::CONSENT_LOCATION,
    scheduling::CONSENT_TRANSPORT,
];

const KNOWN_PURPOSES: &[&str] = &[
    "care_delivery",
    "ai_external_processing",
    "research",
    scheduling::CONSENT_CALENDAR,
    scheduling::CONSENT_LOCATION,
    scheduling::CONSENT_TRANSPORT,
    scheduling::CONSENT_WAITLIST,
];

/// Append one immutable consent version and audit it. Consent decisions are
/// append-only: readers select the highest version per purpose. Locking the
/// patient row serializes version allocation per patient, and a unique
/// (tenant, patient, purpose, version) index backstops it.
pub async fn append_consent(
    tx: &mut PgConnection,
    ctx: &AuthContext,
    cell: &str,
    tenant_id: Uuid,
    patient_id: Uuid,
    purpose: &str,
    status: &str,
) -> Result<(), ApiError> {
    sqlx::query("SELECT id FROM patients WHERE id = $1 FOR UPDATE")
        .bind(patient_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO consents (id, tenant_id, patient_id, purpose, status, version)
         VALUES ($1,$2,$3,$4,$5,
                 COALESCE((SELECT MAX(version) FROM consents
                           WHERE tenant_id=$2 AND patient_id=$3 AND purpose=$4), 0) + 1)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant_id)
    .bind(patient_id)
    .bind(purpose)
    .bind(status)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        ctx,
        "consent.changed",
        cell,
        json!({ "patient_id": patient_id, "purpose": purpose, "status": status }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(())
}

#[derive(Deserialize)]
pub struct SetConsent {
    pub patient_id: Uuid,
    pub purpose: String,
    /// "active" or "revoked"
    pub status: String,
}

pub async fn set_consent(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<SetConsent>,
) -> Result<Json<Value>, ApiError> {
    if !KNOWN_PURPOSES.contains(&body.purpose.as_str()) {
        return Err(ApiError::bad_request(
            "unknown_purpose",
            "unknown consent purpose",
        ));
    }
    if !matches!(body.status.as_str(), "active" | "revoked") {
        return Err(ApiError::bad_request(
            "validation_failed",
            "status must be 'active' or 'revoked'",
        ));
    }
    let patient = sqlx::query("SELECT tenant_id, facility_id FROM patients WHERE id = $1")
        .bind(body.patient_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let tenant_id: Uuid = patient.get("tenant_id");
    let facility_id: Uuid = patient.get("facility_id");

    let allowed = guard(
        &state,
        &ctx,
        actions::CONSENT_WRITE,
        "consent",
        Some(ResourceCtx {
            tenant_id,
            patient_id: Some(body.patient_id),
            facility_id: Some(facility_id),
        }),
    )
    .await?;

    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    append_consent(
        &mut tx,
        &ctx,
        &state.cell,
        tenant_id,
        body.patient_id,
        &body.purpose,
        &body.status,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(
        json!({ "patient_id": body.patient_id, "purpose": body.purpose, "status": body.status }),
    ))
}
