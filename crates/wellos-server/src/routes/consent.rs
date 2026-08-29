use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, ResourceCtx};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

const KNOWN_PURPOSES: &[&str] = &["care_delivery", "ai_external_processing", "research"];

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
    let patient = sqlx::query("SELECT tenant_id FROM patients WHERE id = $1")
        .bind(body.patient_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let tenant_id: Uuid = patient.get("tenant_id");

    guard(
        &state,
        &ctx,
        actions::CONSENT_WRITE,
        "consent",
        Some(ResourceCtx {
            tenant_id,
            patient_id: Some(body.patient_id),
        }),
    )
    .await?;

    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "INSERT INTO consents (id, tenant_id, patient_id, purpose, status)
         VALUES ($1,$2,$3,$4,$5)
         ON CONFLICT (tenant_id, patient_id, purpose)
         DO UPDATE SET status = EXCLUDED.status, version = consents.version + 1, recorded_at = now()",
    )
    .bind(Uuid::now_v7())
    .bind(tenant_id)
    .bind(body.patient_id)
    .bind(&body.purpose)
    .bind(&body.status)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "consent.changed",
        &state.cell,
        json!({ "patient_id": body.patient_id, "purpose": body.purpose, "status": body.status }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(
        json!({ "patient_id": body.patient_id, "purpose": body.purpose, "status": body.status }),
    ))
}
