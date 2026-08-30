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

#[derive(Deserialize)]
pub struct StartEncounter {
    pub patient_id: Uuid,
}

pub async fn start(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<StartEncounter>,
) -> Result<Json<Value>, ApiError> {
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
        actions::ENCOUNTER_START,
        "encounter",
        Some(ResourceCtx {
            tenant_id,
            patient_id: None, // starting an encounter establishes the relationship
        }),
    )
    .await?;

    let id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    sqlx::query(
        "INSERT INTO encounters (id, tenant_id, facility_id, patient_id, practitioner_id)
         VALUES ($1,$2,$3,$4,$5)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(facility_id)
    .bind(body.patient_id)
    .bind(ctx.user_id)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "encounter.started",
        &state.cell,
        json!({ "encounter_id": id, "patient_id": body.patient_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
pub struct CreateServiceRequest {
    pub encounter_id: Uuid,
    pub code_loinc: String,
    pub display: String,
}

pub async fn create_service_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<CreateServiceRequest>,
) -> Result<Json<Value>, ApiError> {
    if body.code_loinc.trim().is_empty() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "code_loinc is required",
        ));
    }
    let enc = sqlx::query("SELECT tenant_id, patient_id FROM encounters WHERE id = $1")
        .bind(body.encounter_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let tenant_id: Uuid = enc.get("tenant_id");
    let patient_id: Uuid = enc.get("patient_id");

    let allowed = guard(
        &state,
        &ctx,
        actions::SERVICE_REQUEST_CREATE,
        "service_request",
        Some(ResourceCtx {
            tenant_id,
            patient_id: Some(patient_id),
        }),
    )
    .await?;

    let id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    sqlx::query(
        "INSERT INTO service_requests (id, tenant_id, encounter_id, patient_id, requester_id, code_loinc, display)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(body.encounter_id)
    .bind(patient_id)
    .bind(ctx.user_id)
    .bind(&body.code_loinc)
    .bind(&body.display)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "service_request.created",
        &state.cell,
        json!({ "service_request_id": id, "patient_id": patient_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(
        json!({ "id": id, "loop_state": "ordered", "version": 1 }),
    ))
}
