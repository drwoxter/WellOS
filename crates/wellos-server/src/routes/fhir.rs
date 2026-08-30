//! Minimal FHIR R4 read boundary for the vertical slice.
//!
//! Maps internal resources to FHIR R4 JSON. This is a facade, not a full FHIR
//! server (see ADR-0005): a dedicated standards server/library is the roadmap
//! path for full conformance.

use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, ResourceCtx};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

pub async fn patient(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query(
        "SELECT id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier
         FROM patients WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)?;
    guard(
        &state,
        &ctx,
        actions::PATIENT_READ,
        "fhir_patient",
        Some(ResourceCtx {
            tenant_id: row.get("tenant_id"),
            patient_id: Some(id),
            facility_id: Some(row.get("facility_id")),
        }),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;
    Ok(Json(json!({
        "resourceType": "Patient",
        "id": id,
        "identifier": [{ "system": "urn:wellos:mrn", "value": row.get::<String,_>("identifier") }],
        "name": [{ "family": row.get::<String,_>("family_name"), "given": [row.get::<String,_>("given_name")] }],
        "birthDate": row.get::<chrono::NaiveDate,_>("birth_date").to_string(),
        "gender": row.get::<String,_>("sex"),
    })))
}

pub async fn observation(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query(
        "SELECT o.id, o.tenant_id, o.patient_id, o.code_loinc, o.value_num::text AS value_num,
                o.unit, o.reference_range, o.status, o.effective_at, p.facility_id,
                EXISTS(SELECT 1 FROM observations o2
                       WHERE o2.tenant_id = o.tenant_id AND o2.amends = o.id)
                    AS superseded
         FROM observations o JOIN patients p ON p.id = o.patient_id
         WHERE o.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let patient_id: Uuid = row.get("patient_id");
    guard(
        &state,
        &ctx,
        actions::PATIENT_READ,
        "fhir_observation",
        Some(ResourceCtx {
            tenant_id: row.get("tenant_id"),
            patient_id: Some(patient_id),
            facility_id: Some(row.get("facility_id")),
        }),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;
    let status: String = row.get("status");
    // Observation rows are append-only: supersession is derived from the
    // amendment relationship, never from mutating the historical row.
    let superseded: bool = row.get("superseded");
    Ok(Json(json!({
        "resourceType": "Observation",
        "id": id,
        "status": if superseded { "amended" } else if status == "corrected" { "corrected" } else { "final" },
        "code": { "coding": [{ "system": "http://loinc.org", "code": row.get::<String,_>("code_loinc") }] },
        "subject": { "reference": format!("Patient/{patient_id}") },
        "effectiveDateTime": row.get::<chrono::DateTime<chrono::Utc>,_>("effective_at").to_rfc3339(),
        "valueQuantity": {
            // Exact decimal: never round clinical values through f64.
            "value": row.get::<String,_>("value_num").parse::<serde_json::Number>()
                .map(Value::Number)
                .map_err(|_| ApiError::internal("invalid numeric observation value"))?,
            "unit": row.get::<String,_>("unit"),
            "system": "http://unitsofmeasure.org",
            "code": row.get::<String,_>("unit"),
        },
    })))
}

pub async fn service_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query(
        "SELECT sr.id, sr.tenant_id, sr.patient_id, sr.encounter_id, sr.code_loinc, sr.display,
                sr.status, sr.loop_state, p.facility_id
         FROM service_requests sr JOIN patients p ON p.id = sr.patient_id
         WHERE sr.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let patient_id: Uuid = row.get("patient_id");
    guard(
        &state,
        &ctx,
        actions::PATIENT_READ,
        "fhir_service_request",
        Some(ResourceCtx {
            tenant_id: row.get("tenant_id"),
            patient_id: Some(patient_id),
            facility_id: Some(row.get("facility_id")),
        }),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;
    let loop_state: String = row.get("loop_state");
    Ok(Json(json!({
        "resourceType": "ServiceRequest",
        "id": id,
        "status": if loop_state == "closed" { "completed" } else { "active" },
        "intent": "order",
        "code": {
            "coding": [{ "system": "http://loinc.org", "code": row.get::<String,_>("code_loinc") }],
            "text": row.get::<String,_>("display"),
        },
        "subject": { "reference": format!("Patient/{patient_id}") },
        "encounter": { "reference": format!("Encounter/{}", row.get::<Uuid,_>("encounter_id")) },
    })))
}
