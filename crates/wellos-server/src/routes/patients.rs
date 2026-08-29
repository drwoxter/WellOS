use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, ResourceCtx};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::NaiveDate;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

#[derive(Deserialize)]
pub struct RegisterPatient {
    pub facility_id: Uuid,
    pub family_name: String,
    pub given_name: String,
    pub birth_date: NaiveDate,
    pub sex: String,
    pub identifier: String,
}

pub async fn register(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<RegisterPatient>,
) -> Result<Json<Value>, ApiError> {
    if body.family_name.trim().is_empty() || body.identifier.trim().is_empty() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "family_name and identifier are required",
        ));
    }
    guard(
        &state,
        &ctx,
        actions::PATIENT_REGISTER,
        "patient",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
        }),
    )
    .await?;

    let id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    sqlx::query(
        "INSERT INTO patients (id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(body.facility_id)
    .bind(&body.family_name)
    .bind(&body.given_name)
    .bind(body.birth_date)
    .bind(&body.sex)
    .bind(&body.identifier)
    .execute(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(db) if db.is_unique_violation() => {
            ApiError::conflict("duplicate_identifier", "patient identifier already exists")
        }
        _ => e.into(),
    })?;
    audit::emit(
        &mut *tx,
        &ctx,
        "patient.registered",
        &state.cell,
        json!({ "patient_id": id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({ "id": id })))
}

#[derive(Deserialize)]
pub struct SearchParams {
    #[serde(default)]
    pub query: String,
}

pub async fn search(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(params): Query<SearchParams>,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::PATIENT_SEARCH,
        "patient",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
        }),
    )
    .await?;
    let like = format!("%{}%", params.query);
    let rows = sqlx::query(
        "SELECT id, family_name, given_name, birth_date, sex, identifier
         FROM patients
         WHERE tenant_id = $1
           AND (family_name ILIKE $2 OR given_name ILIKE $2 OR identifier ILIKE $2)
         ORDER BY family_name, given_name LIMIT 50",
    )
    .bind(ctx.tenant_id)
    .bind(&like)
    .fetch_all(&state.pool)
    .await?;
    let patients: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid, _>("id"),
                "family_name": r.get::<String, _>("family_name"),
                "given_name": r.get::<String, _>("given_name"),
                "birth_date": r.get::<NaiveDate, _>("birth_date"),
                "sex": r.get::<String, _>("sex"),
                "identifier": r.get::<String, _>("identifier"),
            })
        })
        .collect();
    Ok(Json(json!({ "patients": patients })))
}

pub async fn chart(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let patient = sqlx::query(
        "SELECT id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier
         FROM patients WHERE id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let patient_tenant: Uuid = patient.get("tenant_id");

    guard(
        &state,
        &ctx,
        actions::PATIENT_READ,
        "patient",
        Some(ResourceCtx {
            tenant_id: patient_tenant,
            patient_id: Some(id),
        }),
    )
    .await?;

    let allergies = fetch_list(
        &state,
        "SELECT substance AS a, criticality AS b FROM allergies WHERE tenant_id=$1 AND patient_id=$2 ORDER BY recorded_at",
        ctx.tenant_id, id, |r| json!({"substance": r.get::<String,_>("a"), "criticality": r.get::<String,_>("b")}),
    ).await?;
    let medications = fetch_list(
        &state,
        "SELECT name AS a, status AS b FROM medications WHERE tenant_id=$1 AND patient_id=$2 ORDER BY recorded_at",
        ctx.tenant_id, id, |r| json!({"name": r.get::<String,_>("a"), "status": r.get::<String,_>("b")}),
    ).await?;
    let conditions = fetch_list(
        &state,
        "SELECT code AS a, display AS b FROM conditions WHERE tenant_id=$1 AND patient_id=$2 ORDER BY recorded_at",
        ctx.tenant_id, id, |r| json!({"code": r.get::<String,_>("a"), "display": r.get::<String,_>("b")}),
    ).await?;

    let observations = sqlx::query(
        "SELECT id, code_loinc, value_num::text AS value_num, unit, status, effective_at
         FROM observations WHERE tenant_id=$1 AND patient_id=$2
         ORDER BY effective_at DESC LIMIT 50",
    )
    .bind(ctx.tenant_id)
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "code_loinc": r.get::<String,_>("code_loinc"),
            "value": r.get::<String,_>("value_num"),
            "unit": r.get::<String,_>("unit"),
            "status": r.get::<String,_>("status"),
            "effective_at": r.get::<chrono::DateTime<chrono::Utc>,_>("effective_at"),
        })
    })
    .collect::<Vec<_>>();

    let requests = sqlx::query(
        "SELECT id, code_loinc, display, loop_state, version, created_at
         FROM service_requests WHERE tenant_id=$1 AND patient_id=$2 ORDER BY created_at DESC",
    )
    .bind(ctx.tenant_id)
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "code_loinc": r.get::<String,_>("code_loinc"),
            "display": r.get::<String,_>("display"),
            "loop_state": r.get::<String,_>("loop_state"),
            "version": r.get::<i64,_>("version"),
            "created_at": r.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
        })
    })
    .collect::<Vec<_>>();

    let encounters = sqlx::query(
        "SELECT e.id, e.status, e.started_at, u.display_name AS practitioner
         FROM encounters e JOIN users u ON u.id = e.practitioner_id
         WHERE e.tenant_id=$1 AND e.patient_id=$2 ORDER BY e.started_at DESC",
    )
    .bind(ctx.tenant_id)
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "status": r.get::<String,_>("status"),
            "started_at": r.get::<chrono::DateTime<chrono::Utc>,_>("started_at"),
            "practitioner": r.get::<String,_>("practitioner"),
        })
    })
    .collect::<Vec<_>>();

    let consents = fetch_list(
        &state,
        "SELECT purpose AS a, status AS b FROM consents WHERE tenant_id=$1 AND patient_id=$2 ORDER BY purpose",
        ctx.tenant_id, id, |r| json!({"purpose": r.get::<String,_>("a"), "status": r.get::<String,_>("b")}),
    ).await?;

    Ok(Json(json!({
        "patient": {
            "id": patient.get::<Uuid,_>("id"),
            "facility_id": patient.get::<Uuid,_>("facility_id"),
            "family_name": patient.get::<String,_>("family_name"),
            "given_name": patient.get::<String,_>("given_name"),
            "birth_date": patient.get::<NaiveDate,_>("birth_date"),
            "sex": patient.get::<String,_>("sex"),
            "identifier": patient.get::<String,_>("identifier"),
        },
        "allergies": allergies,
        "medications": medications,
        "conditions": conditions,
        "observations": observations,
        "service_requests": requests,
        "encounters": encounters,
        "consents": consents,
    })))
}

async fn fetch_list(
    state: &AppState,
    sql: &str,
    tenant_id: Uuid,
    patient_id: Uuid,
    map: impl Fn(&sqlx::postgres::PgRow) -> Value,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(sql)
        .bind(tenant_id)
        .bind(patient_id)
        .fetch_all(&state.pool)
        .await?;
    Ok(rows.iter().map(map).collect())
}
