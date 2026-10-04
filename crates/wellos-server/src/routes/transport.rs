//! Staff transport coordination (`transport.coordinate`).
//!
//! Logistics only: a coordinator sees appointment time, facility,
//! requirements, pickup window, vehicle, operator and status — never the
//! chart. The pickup address and live positions are sensitive reads,
//! decrypted on demand and audited individually.

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, facility_scope, ResourceCtx};
use crate::ratelimit;
use crate::routes::guard;
use crate::state::AppState;
use crate::transport::{self, TransportRow};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/transport", get(list_requests).post(create_request))
        .route("/transport/:id", get(get_request))
        .route("/transport/:id/transition", post(transition_request))
        .route("/transport/:id/address", get(read_address))
        .route(
            "/transport/:id/location",
            get(read_locations).post(share_location),
        )
        .route("/transport/vehicles", get(list_vehicles))
}

fn resource_ctx(ctx: &AuthContext, t: &TransportRow) -> Option<ResourceCtx> {
    Some(ResourceCtx {
        tenant_id: ctx.tenant_id,
        patient_id: Some(t.patient_id),
        facility_id: Some(t.facility_id),
    })
}

/// Load and authorize; an out-of-scope request is indistinguishable from a
/// missing one.
async fn scoped(
    state: &AppState,
    ctx: &AuthContext,
    conn: &mut sqlx::PgConnection,
    id: Uuid,
    resource_type: &str,
) -> Result<(TransportRow, crate::routes::Allowed), ApiError> {
    guard(
        state,
        ctx,
        actions::TRANSPORT_COORDINATE,
        resource_type,
        None,
    )
    .await?;
    let t = transport::load(conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        state,
        ctx,
        actions::TRANSPORT_COORDINATE,
        resource_type,
        resource_ctx(ctx, &t),
    )
    .await
    .map_err(|_| ApiError::not_found())?;
    Ok((t, allowed))
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub facility_id: Option<Uuid>,
    pub appointment_id: Option<Uuid>,
    pub status: Option<String>,
    pub mine: Option<bool>,
    pub limit: Option<i64>,
}

async fn list_requests(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::TRANSPORT_COORDINATE,
        "transport_request",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: q.facility_id,
        }),
    )
    .await?;
    allowed.record_on_pool(&state, &ctx).await?;
    let facilities = match (
        facility_scope(&ctx, actions::TRANSPORT_COORDINATE),
        q.facility_id,
    ) {
        (None, None) => None,
        (None, Some(f)) => Some(vec![f]),
        (Some(ids), None) => Some(ids),
        (Some(ids), Some(f)) => {
            if !ids.contains(&f) {
                return Err(ApiError::forbidden("facility is outside your scope"));
            }
            Some(vec![f])
        }
    };
    let statuses: Vec<String> = q
        .status
        .as_deref()
        .map(|s| {
            s.split(',')
                .map(str::trim)
                .filter(|s| transport::STATUSES.contains(s))
                .map(String::from)
                .collect()
        })
        .unwrap_or_default();
    let mut conn = state.pool.acquire().await?;
    let rows = transport::list(
        &mut conn,
        ctx.tenant_id,
        transport::ListFilter {
            facility_ids: facilities.as_deref(),
            patient_id: None,
            appointment_id: q.appointment_id,
            statuses: &statuses,
            operator_user_id: q.mine.unwrap_or(false).then_some(ctx.user_id),
            limit: q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT),
        },
    )
    .await?;
    let mut items: Vec<Value> = rows.iter().map(transport::transport_json).collect();
    // Logistics need a name to meet the patient; the record number stays out.
    crate::scheduling::attach_patient_summaries(&state.pool, ctx.tenant_id, &mut items, false)
        .await?;
    Ok(Json(json!({
        "items": items,
        "statuses": transport::STATUSES,
        "location_encryption_configured": state.runtime.location.keyring.is_some(),
    })))
}

async fn create_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<transport::CreateInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    guard(
        &state,
        &ctx,
        actions::TRANSPORT_COORDINATE,
        "transport_request",
        None,
    )
    .await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let mut conn = state.pool.acquire().await?;
    let a = crate::routes::access::load_appointment_scoped(
        &mut conn,
        ctx.tenant_id,
        body.appointment_id,
    )
    .await?;
    drop(conn);
    let allowed = guard(
        &state,
        &ctx,
        actions::TRANSPORT_COORDINATE,
        "transport_request",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: Some(a.patient_id),
            facility_id: Some(a.facility_id),
        }),
    )
    .await
    .map_err(|_| ApiError::not_found())?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let t = transport::create(&mut tx, &ctx, &state, body, true).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(transport::transport_json(&t))))
}

async fn get_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (t, allowed) = scoped(&state, &ctx, &mut conn, id, "transport_request").await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let history = transport::history_json(&mut conn, t.id).await?;
    let mut body = transport::transport_json(&t);
    body["history"] = json!(history);
    body["allowed_transitions"] = json!(transport::STATUSES
        .iter()
        .filter(|s| transport::can_transition(&t.status, s))
        .collect::<Vec<_>>());
    Ok(Json(body))
}

async fn transition_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<transport::TransitionInput>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (_, allowed) = scoped(&state, &ctx, &mut conn, id, "transport_request").await?;
    drop(conn);
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let t = transport::transition(&mut tx, &ctx, &state, id, body, true).await?;
    tx.commit().await?;
    Ok(Json(transport::transport_json(&t)))
}

/// Decrypt the pickup address for the coordinator; every read is audited.
async fn read_address(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (t, allowed) = scoped(&state, &ctx, &mut conn, id, "transport_address").await?;
    if !transport::is_active(&t.status) {
        return Err(ApiError::conflict(
            "episode_closed",
            "the pickup address is only available during an active transport episode",
        ));
    }
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let address = transport::open_address(&state, &t)?;
    audit::emit(
        &mut *conn,
        &ctx,
        "transport.address.read",
        &state.cell,
        json!({ "transport_request_id": t.id, "present": address.is_some() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "transport_request_id": t.id,
        "pickup_address": address,
        "origin_area_code": t.origin_area_code,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocationBody {
    pub latitude: f64,
    pub longitude: f64,
}

/// Operator shares the vehicle position during the episode.
async fn share_location(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<LocationBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (t, allowed) = scoped(&state, &ctx, &mut conn, id, "transport_location").await?;
    drop(conn);
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let expires_at =
        transport::share_location(&mut tx, &ctx, &state, &t, body.latitude, body.longitude).await?;
    tx.commit().await?;
    Ok(Json(json!({
        "transport_request_id": t.id,
        "expires_at": expires_at,
        "ttl_seconds": state.runtime.location.live_location_ttl.as_secs(),
    })))
}

async fn read_locations(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (t, allowed) = scoped(&state, &ctx, &mut conn, id, "transport_location").await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let positions = transport::live_positions(&mut conn, &state, &t).await?;
    audit::emit(
        &mut *conn,
        &ctx,
        "transport.location.read",
        &state.cell,
        json!({ "transport_request_id": t.id, "positions": positions.len() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "transport_request_id": t.id,
        "status": t.status,
        "sharing_open": transport::location_sharing_open(&t.status),
        "positions": positions,
    })))
}

#[derive(Debug, Deserialize)]
pub struct VehicleQuery {
    pub facility_id: Option<Uuid>,
}

/// Transport-capable resources the coordinator may assign.
async fn list_vehicles(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<VehicleQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::TRANSPORT_COORDINATE,
        "transport_resource",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: q.facility_id,
        }),
    )
    .await?;
    allowed.record_on_pool(&state, &ctx).await?;
    let facilities = match (
        facility_scope(&ctx, actions::TRANSPORT_COORDINATE),
        q.facility_id,
    ) {
        (None, None) => None,
        (None, Some(f)) => Some(vec![f]),
        (Some(ids), None) => Some(ids),
        (Some(ids), Some(f)) => {
            if !ids.contains(&f) {
                return Err(ApiError::forbidden("facility is outside your scope"));
            }
            Some(vec![f])
        }
    };
    let rows = sqlx::query(
        "SELECT r.id, r.name, r.resource_type_code, r.facility_id, r.capacity,
                r.accessibility_codes, r.time_zone
         FROM schedulable_resources r
         LEFT JOIN catalog_entries c
           ON c.tenant_id = r.tenant_id AND c.kind = 'resource_type'
          AND c.code = r.resource_type_code AND c.active
         WHERE r.tenant_id = $1 AND r.active
           AND ($2::uuid[] IS NULL OR r.facility_id = ANY($2))
           AND (r.resource_type_code IN ('vehicle','accessible_vehicle','ambulance','home_visit_team')
                OR (c.config->>'transport')::boolean IS TRUE)
         ORDER BY r.facility_id, r.name
         LIMIT 200",
    )
    .bind(ctx.tenant_id)
    .bind(facilities.as_deref())
    .fetch_all(&state.pool)
    .await?;
    use sqlx::Row;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid, _>("id"),
                "name": r.get::<String, _>("name"),
                "resource_type_code": r.get::<String, _>("resource_type_code"),
                "facility_id": r.get::<Uuid, _>("facility_id"),
                "capacity": r.get::<i32, _>("capacity"),
                "accessibility_codes": r.get::<Vec<String>, _>("accessibility_codes"),
                "time_zone": r.get::<String, _>("time_zone"),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}
