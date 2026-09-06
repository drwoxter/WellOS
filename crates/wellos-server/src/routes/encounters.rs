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
    /// `consultation` (default) accepts clinical documentation;
    /// `order_only` is a laboratory-order context that never holds a note.
    #[serde(default)]
    pub encounter_type: Option<String>,
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
            facility_id: Some(facility_id),
        }),
    )
    .await?;

    let encounter_type = match body.encounter_type.as_deref() {
        None | Some("consultation") => "consultation",
        Some("order_only") => "order_only",
        Some(_) => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "encounter_type must be 'consultation' or 'order_only'",
            ))
        }
    };

    let id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    sqlx::query(
        "INSERT INTO encounters (id, tenant_id, facility_id, patient_id, practitioner_id, encounter_type)
         VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(facility_id)
    .bind(body.patient_id)
    .bind(ctx.user_id)
    .bind(encounter_type)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "encounter.started",
        &state.cell,
        json!({
            "encounter_id": id,
            "patient_id": body.patient_id,
            "encounter_type": encounter_type,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({ "id": id })))
}

fn require_order_context(
    status: &str,
    practitioner_id: Uuid,
    ctx: &AuthContext,
) -> Result<(), ApiError> {
    if status != "in_progress" {
        return Err(ApiError::conflict(
            "encounter_not_active",
            "orders require an active (in progress) encounter",
        ));
    }
    if practitioner_id != ctx.user_id {
        return Err(ApiError::forbidden(
            "orders require the requester's own encounter",
        ));
    }
    Ok(())
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
    let enc = sqlx::query(
        "SELECT e.tenant_id, e.patient_id, e.facility_id, e.status, e.practitioner_id
         FROM encounters e WHERE e.id = $1",
    )
    .bind(body.encounter_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let tenant_id: Uuid = enc.get("tenant_id");
    let patient_id: Uuid = enc.get("patient_id");
    let facility_id: Uuid = enc.get("facility_id");
    let enc_status: String = enc.get("status");
    let practitioner_id: Uuid = enc.get("practitioner_id");

    let allowed = guard(
        &state,
        &ctx,
        actions::SERVICE_REQUEST_CREATE,
        "service_request",
        Some(ResourceCtx {
            tenant_id,
            patient_id: Some(patient_id),
            facility_id: Some(facility_id),
        }),
    )
    .await?;

    // Orders attach only to the requester's own active encounter; a closed
    // encounter or one owned by another practitioner is not a valid order
    // context.
    require_order_context(&enc_status, practitioner_id, &ctx)?;

    let id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    // The encounter row is locked for the rest of the transaction so ordering
    // serializes with cancellation and signing (which take the same lock);
    // eligibility is decided on the locked row, not the earlier read.
    let locked = sqlx::query(
        "SELECT tenant_id, patient_id, facility_id, status, practitioner_id
         FROM encounters WHERE id = $1 FOR UPDATE",
    )
    .bind(body.encounter_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    if locked.get::<Uuid, _>("tenant_id") != tenant_id
        || locked.get::<Uuid, _>("patient_id") != patient_id
        || locked.get::<Uuid, _>("facility_id") != facility_id
    {
        return Err(ApiError::not_found());
    }
    require_order_context(
        &locked.get::<String, _>("status"),
        locked.get("practitioner_id"),
        &ctx,
    )?;
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
