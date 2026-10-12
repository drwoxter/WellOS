//! Care Operations & Command Center API. Every handler authorizes through the
//! central policy (`care_ops.*` actions), scopes to the caller's facilities,
//! records the allow decision transactionally with the mutation, and returns
//! typed `409` conflicts for stale versions and hard-constraint changes.

use crate::auth::AuthContext;
use crate::care_ops::{self as ops, flow, forecast, theatre, workforce};
use crate::error::ApiError;
use crate::policy::{actions, facility_scope, ResourceCtx};
use crate::ratelimit;
use crate::routes::extract::OptionalJson;
use crate::routes::{guard, Allowed};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;
use wellos_domain::ai::ReviewDecision;

const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/operations/snapshot", get(snapshot))
        .route(
            "/operations/locations",
            get(list_locations).post(create_location),
        )
        .route("/operations/locations/:id/state", post(set_position_state))
        .route("/operations/locations/:id/periods", post(create_period))
        .route(
            "/operations/locations/:id/periods/:period_id/deactivate",
            post(deactivate_period),
        )
        .route(
            "/operations/episodes",
            get(list_episodes).post(create_episode),
        )
        .route(
            "/operations/episodes/:id",
            get(get_episode).patch(update_episode),
        )
        .route(
            "/operations/episodes/:id/transition",
            post(transition_episode),
        )
        .route("/operations/episodes/:id/links", post(add_link))
        .route(
            "/operations/episodes/:id/handoffs",
            get(list_handoffs).post(create_handoff),
        )
        .route("/operations/handoffs/:id/decision", post(decide_handoff))
        .route(
            "/operations/placement-requests",
            get(list_requests).post(create_request),
        )
        .route("/operations/placement-requests/:id", get(get_request))
        .route(
            "/operations/placement-requests/:id/candidates",
            post(generate_candidates),
        )
        .route("/operations/placement-requests/:id/hold", post(hold))
        .route(
            "/operations/placement-requests/:id/release-hold",
            post(release_hold),
        )
        .route("/operations/placement-requests/:id/confirm", post(confirm))
        .route(
            "/operations/placement-requests/:id/cancel",
            post(cancel_request),
        )
        .route(
            "/operations/placements/:id/release",
            post(release_placement),
        )
        .route("/operations/tasks", get(list_tasks).post(create_task))
        .route("/operations/tasks/:id/transition", post(transition_task))
        .route("/operations/tasks/:id/reassign", post(reassign_task))
        .route("/operations/escalations", get(list_escalations))
        .route("/operations/escalations/:id", post(update_escalation))
        .route(
            "/operations/theatre/sessions",
            get(list_sessions).post(create_session),
        )
        .route(
            "/operations/theatre/cases",
            get(list_cases).post(create_case),
        )
        .route("/operations/theatre/cases/:id", get(get_case))
        .route(
            "/operations/theatre/cases/:id/schedule",
            post(schedule_case),
        )
        .route(
            "/operations/theatre/cases/:id/transition",
            post(transition_case),
        )
        .route(
            "/operations/workforce/shifts",
            get(list_shifts).post(create_shift),
        )
        .route("/operations/workforce/shifts/:id", get(get_shift))
        .route(
            "/operations/workforce/shifts/:id/assignments",
            post(add_assignment),
        )
        .route(
            "/operations/workforce/assignments/:id",
            post(change_assignment),
        )
        .route("/operations/workforce/absences", post(record_absence))
        .route(
            "/operations/forecasts",
            get(list_forecasts).post(create_forecast),
        )
        .route("/operations/forecasts/:id", get(get_forecast))
        .route(
            "/operations/forecasts/:id/explanation",
            post(forecast_explanation),
        )
        .route("/operations/snapshot/brief", post(flow_brief))
        .route(
            "/operations/placement-requests/:id/ranking",
            post(placement_ranking),
        )
        .route(
            "/operations/episodes/:id/discharge-coordination",
            post(discharge_coordination),
        )
        .route(
            "/operations/episodes/:id/handoffs/draft",
            post(handoff_draft),
        )
        .route(
            "/operations/theatre/sessions/:id/explanation",
            post(theatre_explanation),
        )
        .route("/operations/artifacts/:id/review", post(review_artifact))
        .route("/operations/jobs/tick", post(tick))
}

fn visible(
    ctx: &AuthContext,
    action: &str,
    requested: Option<Uuid>,
) -> Result<Option<Vec<Uuid>>, ApiError> {
    match (facility_scope(ctx, action), requested) {
        (None, None) => Ok(None),
        (None, Some(f)) => Ok(Some(vec![f])),
        (Some(ids), None) => Ok(Some(ids)),
        (Some(ids), Some(f)) if ids.contains(&f) => Ok(Some(vec![f])),
        (Some(_), Some(_)) => Err(ApiError::forbidden("facility is outside your scope")),
    }
}

fn res(
    ctx: &AuthContext,
    facility_id: Option<Uuid>,
    patient_id: Option<Uuid>,
) -> Option<ResourceCtx> {
    Some(ResourceCtx {
        tenant_id: ctx.tenant_id,
        patient_id,
        facility_id,
    })
}

async fn allow(
    state: &AppState,
    ctx: &AuthContext,
    action: &str,
    rt: &str,
    facility_id: Option<Uuid>,
    patient_id: Option<Uuid>,
) -> Result<Allowed, ApiError> {
    guard(state, ctx, action, rt, res(ctx, facility_id, patient_id)).await
}

/// Authorize against a loaded resource; denial reads as not-found (anti-probing).
async fn allow_existing(
    state: &AppState,
    ctx: &AuthContext,
    action: &str,
    rt: &str,
    facility_id: Uuid,
    patient_id: Option<Uuid>,
) -> Result<Allowed, ApiError> {
    guard(
        state,
        ctx,
        action,
        rt,
        res(ctx, Some(facility_id), patient_id),
    )
    .await
    .map_err(|_| ApiError::not_found())
}

fn limit(l: Option<i64>) -> i64 {
    l.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

#[derive(Debug, Deserialize)]
pub struct FacilityQuery {
    pub facility_id: Option<Uuid>,
    pub episode_id: Option<Uuid>,
    pub patient_id: Option<Uuid>,
    pub open_only: Option<bool>,
    pub status: Option<String>,
    pub horizon: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
}

// ---------------------------------------------------------------- snapshot

async fn snapshot(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<FacilityQuery>,
) -> Result<Json<Value>, ApiError> {
    let facility_id = q
        .facility_id
        .ok_or_else(|| ApiError::bad_request("facility_required", "facility_id is required"))?;
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "care_ops_snapshot",
        Some(facility_id),
        None,
    )
    .await?;
    visible(&ctx, actions::CARE_OPS_READ, Some(facility_id))?;
    let mut conn = state.pool.acquire().await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let snap = flow::snapshot(&mut conn, ctx.tenant_id, facility_id, Utc::now()).await?;
    crate::audit::emit(
        &mut *conn,
        &ctx,
        "care_ops.snapshot.read",
        &state.cell,
        json!({"facility_id": facility_id}),
        None,
    )
    .await?;
    Ok(Json(snap))
}

// ---------------------------------------------------------------- locations

async fn list_locations(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<FacilityQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "operational_location",
        q.facility_id,
        None,
    )
    .await?;
    let facilities = visible(&ctx, actions::CARE_OPS_READ, q.facility_id)?;
    let mut conn = state.pool.acquire().await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let rows = ops::list_locations(&mut conn, ctx.tenant_id, facilities.as_deref()).await?;
    Ok(Json(json!({"locations": rows})))
}

async fn create_location(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<ops::LocationInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "operational_location",
        Some(body.facility_id),
        None,
    )
    .await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = ops::create_location(&mut tx, &ctx, &body).await?;
    crate::audit::emit(
        &mut *tx,
        &ctx,
        "care_ops.location.created",
        &state.cell,
        json!({"location_id": row.id, "facility_id": row.facility_id, "level": row.level}),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!(row))))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StateBody {
    pub expected_version: i64,
    pub state: String,
    pub reason_code: Option<String>,
    pub note: Option<String>,
}

async fn set_position_state(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<StateBody>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let loc = ops::load_location(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "operational_location",
        loc.facility_id,
        None,
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let to = wellos_domain::care_ops::PositionState::parse(&body.state)
        .ok_or_else(|| ApiError::bad_request("invalid_state", "unknown position state"))?;
    let row = ops::set_position_state(
        &mut tx,
        &ctx,
        id,
        body.expected_version,
        to,
        body.reason_code.as_deref(),
        body.note.as_deref(),
    )
    .await?;
    crate::audit::emit(&mut *tx, &ctx, "care_ops.location.state_changed", &state.cell, json!({"location_id": id, "from": loc.position_state, "to": body.state, "reason_code": body.reason_code}), None).await?;
    tx.commit().await?;
    Ok(Json(json!(row)))
}

async fn create_period(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ops::PeriodInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = state.pool.begin().await?;
    let loc = ops::load_location(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "operational_location",
        loc.facility_id,
        None,
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let pid = ops::create_period(&mut tx, &ctx, id, &body).await?;
    crate::audit::emit(
        &mut *tx,
        &ctx,
        "care_ops.location.period_created",
        &state.cell,
        json!({"location_id": id, "period_id": pid, "kind": body.kind}),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"period_id": pid}))))
}

async fn deactivate_period(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((id, period_id)): Path<(Uuid, Uuid)>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let loc = ops::load_location(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "operational_location",
        loc.facility_id,
        None,
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    ops::deactivate_period(&mut tx, ctx.tenant_id, period_id).await?;
    crate::audit::emit(
        &mut *tx,
        &ctx,
        "care_ops.location.period_deactivated",
        &state.cell,
        json!({"location_id": id, "period_id": period_id}),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"ok": true})))
}

// ---------------------------------------------------------------- episodes

async fn list_episodes(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<FacilityQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "care_episode",
        q.facility_id,
        q.patient_id,
    )
    .await?;
    let facilities = visible(&ctx, actions::CARE_OPS_READ, q.facility_id)?;
    let statuses: Vec<String> = q
        .status
        .iter()
        .flat_map(|s| s.split(',').map(|x| x.trim().to_string()))
        .filter(|s| !s.is_empty())
        .collect();
    let mut conn = state.pool.acquire().await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let rows = ops::list_episodes(
        &mut conn,
        ctx.tenant_id,
        ops::EpisodeFilter {
            facility_ids: facilities.as_deref(),
            patient_id: q.patient_id,
            statuses: &statuses,
            open_only: q.open_only.unwrap_or(false),
            limit: limit(q.limit),
        },
    )
    .await?;
    Ok(Json(json!({"episodes": rows})))
}

async fn create_episode(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<ops::EpisodeInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "care_episode",
        Some(body.facility_id),
        Some(body.patient_id),
    )
    .await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = ops::create_episode(&mut tx, &ctx, &body).await?;
    crate::audit::emit(&mut *tx, &ctx, "care_ops.episode.created", &state.cell, json!({"episode_id": row.id, "facility_id": row.facility_id, "patient_id": row.patient_id, "episode_type_code": row.episode_type_code, "status": row.status}), None).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!(row))))
}

async fn episode_detail(
    conn: &mut sqlx::PgConnection,
    ep: &ops::EpisodeRow,
) -> Result<Value, ApiError> {
    let links = ops::episode_links(conn, ep.tenant_id, ep.id).await?;
    let placements = ops::episode_placements(conn, ep.tenant_id, ep.id).await?;
    let tasks = ops::list_tasks(conn, ep.tenant_id, Some(ep.id), None, false, MAX_LIMIT).await?;
    let handoffs = ops::episode_handoffs(conn, ep.tenant_id, ep.id).await?;
    let escalations = ops::list_escalations(conn, ep.tenant_id, None, Some(ep.id), false).await?;
    let requests =
        ops::list_requests(conn, ep.tenant_id, None, Some(ep.id), false, MAX_LIMIT).await?;
    let cases =
        theatre::list_cases(conn, ep.tenant_id, None, Some(ep.id), false, MAX_LIMIT).await?;
    let history = ops::episode_history(conn, ep.tenant_id, ep.id).await?;
    let artifacts = sqlx::query(
        "SELECT id, artifact_type, status, synthetic, created_at FROM ai_artifacts
         WHERE tenant_id = $1 AND care_episode_id = $2
         ORDER BY created_at DESC LIMIT 50",
    )
    .bind(ep.tenant_id)
    .bind(ep.id)
    .fetch_all(&mut *conn)
    .await?;
    let artifacts: Vec<Value> = artifacts
        .iter()
        .map(|r| {
            use sqlx::Row;
            json!({
                "id": r.get::<Uuid, _>("id"),
                "artifact_type": r.get::<String, _>("artifact_type"),
                "status": r.get::<String, _>("status"),
                "synthetic": r.get::<bool, _>("synthetic"),
                "created_at": r.get::<DateTime<Utc>, _>("created_at"),
            })
        })
        .collect();
    Ok(json!({
        "episode": ep,
        "links": links,
        "placements": placements,
        "placement_requests": requests,
        "tasks": tasks,
        "handoffs": handoffs,
        "escalations": escalations,
        "theatre_cases": cases,
        "history": history,
        "ai_artifacts": artifacts,
    }))
}

async fn get_episode(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let ep = ops::load_episode(&mut conn, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "care_episode",
        ep.facility_id,
        Some(ep.patient_id),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    Ok(Json(episode_detail(&mut conn, &ep).await?))
}

async fn update_episode(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ops::EpisodeUpdateInput>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let ep = ops::load_episode(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "care_episode",
        ep.facility_id,
        Some(ep.patient_id),
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = ops::update_episode(&mut tx, &ctx, id, &body).await?;
    crate::audit::emit(
        &mut *tx,
        &ctx,
        "care_ops.episode.updated",
        &state.cell,
        json!({"episode_id": id, "version": row.version}),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!(row)))
}

async fn transition_episode(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ops::EpisodeTransitionInput>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let ep = ops::load_episode(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "care_episode",
        ep.facility_id,
        Some(ep.patient_id),
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = ops::transition_episode(&mut tx, &ctx, id, &body).await?;
    if !row.status_enum().is_open() {
        ops::resolve_escalations(
            &mut tx,
            ctx.tenant_id,
            Some(id),
            None,
            &["boarding", "placement_delay", "discharge_delay"],
        )
        .await?;
    }
    crate::audit::emit(&mut *tx, &ctx, "care_ops.episode.transitioned", &state.cell, json!({"episode_id": id, "from": ep.status, "to": row.status, "transition": body.transition, "reason_code": body.reason_code}), None).await?;
    tx.commit().await?;
    Ok(Json(json!(row)))
}

async fn add_link(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ops::LinkInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = state.pool.begin().await?;
    let ep = ops::load_episode(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "care_episode",
        ep.facility_id,
        Some(ep.patient_id),
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    ops::add_link(&mut tx, &ctx, &ep, &body).await?;
    crate::audit::emit(
        &mut *tx,
        &ctx,
        "care_ops.episode.linked",
        &state.cell,
        json!({"episode_id": id, "link_type": body.link_type, "link_id": body.link_id}),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"ok": true}))))
}

// ---------------------------------------------------------------- handoffs

async fn list_handoffs(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let ep = ops::load_episode(&mut conn, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "care_handoff",
        ep.facility_id,
        Some(ep.patient_id),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    Ok(Json(
        json!({"handoffs": ops::episode_handoffs(&mut conn, ctx.tenant_id, id).await?}),
    ))
}

async fn create_handoff(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ops::HandoffInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = state.pool.begin().await?;
    let ep = ops::load_episode(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "care_handoff",
        ep.facility_id,
        Some(ep.patient_id),
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = ops::create_handoff(&mut tx, &ctx, ep.id, &body).await?;
    crate::audit::emit(
        &mut *tx,
        &ctx,
        "care_ops.handoff.created",
        &state.cell,
        json!({"episode_id": id, "handoff_id": row.id, "origin": row.origin}),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!(row))))
}

async fn decide_handoff(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ops::HandoffDecisionInput>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let (facility_id, patient_id): (Uuid, Uuid) = sqlx::query_as(
        "SELECT facility_id, patient_id FROM care_handoffs WHERE tenant_id = $1 AND id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_REVIEW_AI,
        "care_handoff",
        facility_id,
        Some(patient_id),
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = ops::decide_handoff(&mut tx, &ctx, id, &body).await?;
    crate::audit::emit(
        &mut *tx,
        &ctx,
        "care_ops.handoff.decided",
        &state.cell,
        json!({"handoff_id": id, "decision": body.decision, "artifact_id": row.artifact_id}),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!(row)))
}

// ---------------------------------------------------------------- placements

async fn list_requests(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<FacilityQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "placement_request",
        q.facility_id,
        None,
    )
    .await?;
    let facilities = visible(&ctx, actions::CARE_OPS_READ, q.facility_id)?;
    let mut conn = state.pool.acquire().await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let rows = ops::list_requests(
        &mut conn,
        ctx.tenant_id,
        facilities.as_deref(),
        q.episode_id,
        q.open_only.unwrap_or(true),
        limit(q.limit),
    )
    .await?;
    Ok(Json(json!({"requests": rows})))
}

async fn create_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<ops::PlacementRequestInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = state.pool.begin().await?;
    let ep = ops::load_episode(&mut tx, ctx.tenant_id, body.episode_id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "placement_request",
        ep.facility_id,
        Some(ep.patient_id),
    )
    .await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = ops::create_request(&mut tx, &ctx, &body).await?;
    ops::arm_escalation(
        &mut tx,
        &ctx,
        ops::ArmEscalation {
            facility_id: row.facility_id,
            patient_id: row.patient_id,
            episode_id: Some(row.episode_id),
            visit_id: None,
            placement_request_id: Some(row.id),
            kind: "placement_delay",
            priority: &row.priority,
            started_at: row.created_at,
            threshold_minutes: None,
        },
    )
    .await?;
    crate::audit::emit(&mut *tx, &ctx, "care_ops.placement.requested", &state.cell, json!({"request_id": row.id, "episode_id": row.episode_id, "facility_id": row.facility_id, "kind": row.kind, "priority": row.priority}), None).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!(row))))
}

async fn request_detail(
    conn: &mut sqlx::PgConnection,
    req: &ops::PlacementRequestRow,
) -> Result<Value, ApiError> {
    let candidates = ops::latest_candidates(conn, req.tenant_id, req.id).await?;
    let placements = ops::episode_placements(conn, req.tenant_id, req.episode_id).await?;
    let history = ops::placement_history(conn, req.tenant_id, req.id).await?;
    Ok(
        json!({"request": req, "candidates": candidates, "placements": placements.into_iter().filter(|p| p.request_id == Some(req.id)).collect::<Vec<_>>(), "history": history}),
    )
}

async fn get_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let req = ops::load_request(&mut conn, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "placement_request",
        req.facility_id,
        Some(req.patient_id),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    Ok(Json(request_detail(&mut conn, &req).await?))
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CandidatesBody {
    pub expected_version: Option<i64>,
}

async fn generate_candidates(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<CandidatesBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let mut tx = state.pool.begin().await?;
    let req = ops::load_request(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "placement_request",
        req.facility_id,
        Some(req.patient_id),
    )
    .await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let (req, m, considered) =
        ops::generate_candidates(&mut tx, &ctx, id, body.expected_version).await?;
    crate::audit::emit(&mut *tx, &ctx, "care_ops.placement.candidates_generated", &state.cell, json!({"request_id": id, "matcher_version": m.matcher_version, "facts_hash": m.facts_hash, "feasible": m.feasible.len(), "rejected": m.rejected.len(), "considered": considered}), None).await?;
    tx.commit().await?;
    Ok(Json(
        json!({"request": req, "match": m, "considered": considered}),
    ))
}

async fn hold(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ops::HoldInput>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let req = ops::load_request(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "placement_request",
        req.facility_id,
        Some(req.patient_id),
    )
    .await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let (req, p) = ops::hold(&mut tx, &ctx, id, &body).await?;
    crate::audit::emit(&mut *tx, &ctx, "care_ops.placement.held", &state.cell, json!({"request_id": id, "placement_id": p.id, "location_id": p.location_id, "expires_at": p.expires_at, "facts_hash": p.facts_hash}), None).await?;
    tx.commit().await?;
    Ok(Json(json!({"request": req, "placement": p})))
}

async fn release_hold(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ops::ReleaseHoldInput>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let req = ops::load_request(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "placement_request",
        req.facility_id,
        Some(req.patient_id),
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let req = ops::release_hold(&mut tx, &ctx, id, &body).await?;
    crate::audit::emit(
        &mut *tx,
        &ctx,
        "care_ops.placement.hold_released",
        &state.cell,
        json!({"request_id": id, "reason_code": body.reason_code}),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"request": req})))
}

async fn confirm(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ops::ConfirmInput>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let req = ops::load_request(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "placement_request",
        req.facility_id,
        Some(req.patient_id),
    )
    .await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let (req, p, ep) = match ops::confirm(&mut tx, &ctx, id, &body).await {
        Ok(v) => v,
        Err(e) if ops::refusal_is_persisted(&e) => {
            tx.commit().await?;
            return Err(e);
        }
        Err(e) => return Err(e),
    };
    ops::resolve_escalations(
        &mut tx,
        ctx.tenant_id,
        Some(ep.id),
        None,
        &["placement_delay", "boarding"],
    )
    .await?;
    let event = if req.kind == "transfer" {
        "care_ops.placement.transferred"
    } else {
        "care_ops.placement.confirmed"
    };
    crate::audit::emit(&mut *tx, &ctx, event, &state.cell, json!({"request_id": id, "placement_id": p.id, "location_id": p.location_id, "episode_id": ep.id, "facts_hash": p.facts_hash}), None).await?;
    tx.commit().await?;
    Ok(Json(json!({"request": req, "placement": p, "episode": ep})))
}

async fn cancel_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ops::CancelRequestInput>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let req = ops::load_request(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "placement_request",
        req.facility_id,
        Some(req.patient_id),
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let req = ops::cancel_request(&mut tx, &ctx, id, &body).await?;
    ops::resolve_escalations(
        &mut tx,
        ctx.tenant_id,
        Some(req.episode_id),
        None,
        &["placement_delay"],
    )
    .await?;
    crate::audit::emit(
        &mut *tx,
        &ctx,
        "care_ops.placement.request_cancelled",
        &state.cell,
        json!({"request_id": id, "reason_code": body.reason_code}),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({"request": req})))
}

async fn release_placement(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ops::ReleaseInput>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let p = ops::load_placement(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "placement",
        p.facility_id,
        Some(p.patient_id),
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let (p, ep) = ops::release(&mut tx, &ctx, id, &body).await?;
    crate::audit::emit(&mut *tx, &ctx, "care_ops.placement.released", &state.cell, json!({"placement_id": id, "location_id": p.location_id, "episode_id": ep.id, "reason_code": body.reason_code}), None).await?;
    tx.commit().await?;
    Ok(Json(json!({"placement": p, "episode": ep})))
}

// ---------------------------------------------------------------- tasks

async fn list_tasks(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<FacilityQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "care_coordination_task",
        q.facility_id,
        None,
    )
    .await?;
    let facilities = visible(&ctx, actions::CARE_OPS_READ, q.facility_id)?;
    let mut conn = state.pool.acquire().await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let rows = ops::list_tasks(
        &mut conn,
        ctx.tenant_id,
        q.episode_id,
        facilities.as_deref(),
        q.open_only.unwrap_or(true),
        limit(q.limit),
    )
    .await?;
    Ok(Json(json!({"tasks": rows})))
}

async fn create_task(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<ops::TaskInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = state.pool.begin().await?;
    let ep = ops::load_episode(&mut tx, ctx.tenant_id, body.episode_id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "care_coordination_task",
        ep.facility_id,
        Some(ep.patient_id),
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = ops::create_task(&mut tx, &ctx, &body).await?;
    crate::audit::emit(&mut *tx, &ctx, "care_ops.task.created", &state.cell, json!({"task_id": row.id, "episode_id": row.episode_id, "kind": row.kind, "barrier_category_code": row.barrier_category_code, "patient_visible": row.patient_visible}), None).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!(row))))
}

async fn transition_task(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ops::TaskTransitionInput>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let t = ops::load_task(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "care_coordination_task",
        t.facility_id,
        Some(t.patient_id),
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = ops::transition_task(&mut tx, &ctx, id, &body).await?;
    crate::audit::emit(
        &mut *tx,
        &ctx,
        "care_ops.task.transitioned",
        &state.cell,
        json!({"task_id": id, "from": t.status, "to": row.status, "transition": body.transition}),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!(row)))
}

async fn reassign_task(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ops::ReassignInput>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let t = ops::load_task(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "care_coordination_task",
        t.facility_id,
        Some(t.patient_id),
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = ops::reassign_task(&mut tx, &ctx, id, &body).await?;
    crate::audit::emit(&mut *tx, &ctx, "care_ops.task.reassigned", &state.cell, json!({"task_id": id, "accountable_user_id": row.accountable_user_id, "accountable_queue_id": row.accountable_queue_id}), None).await?;
    tx.commit().await?;
    Ok(Json(json!(row)))
}

// ---------------------------------------------------------------- escalations

async fn list_escalations(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<FacilityQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "care_escalation",
        q.facility_id,
        None,
    )
    .await?;
    let facilities = visible(&ctx, actions::CARE_OPS_READ, q.facility_id)?;
    let mut conn = state.pool.acquire().await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let rows = ops::list_escalations(
        &mut conn,
        ctx.tenant_id,
        facilities.as_deref(),
        q.episode_id,
        q.open_only.unwrap_or(true),
    )
    .await?;
    Ok(Json(json!({"escalations": rows})))
}

async fn update_escalation(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ops::EscalationUpdateInput>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let (facility_id, patient_id): (Uuid, Uuid) = sqlx::query_as(
        "SELECT facility_id, patient_id FROM care_escalations WHERE tenant_id = $1 AND id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "care_escalation",
        facility_id,
        Some(patient_id),
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = ops::update_escalation(&mut tx, &ctx, id, &body).await?;
    crate::audit::emit(
        &mut *tx,
        &ctx,
        "care_ops.escalation.resolved",
        &state.cell,
        json!({"escalation_id": id, "status": row.status}),
        None,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!(row)))
}

// ---------------------------------------------------------------- theatre

async fn list_sessions(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<FacilityQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "theatre_session",
        q.facility_id,
        None,
    )
    .await?;
    let facilities = visible(&ctx, actions::CARE_OPS_READ, q.facility_id)?;
    let from = q.from.unwrap_or_else(|| Utc::now() - Duration::hours(12));
    let to = q.to.unwrap_or(from + Duration::days(7));
    let mut conn = state.pool.acquire().await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    Ok(Json(
        json!({"sessions": theatre::list_sessions(&mut conn, ctx.tenant_id, facilities.as_deref(), from, to).await?}),
    ))
}

async fn create_session(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<theatre::SessionInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = state.pool.begin().await?;
    let loc = ops::load_location(&mut tx, ctx.tenant_id, body.theatre_location_id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_THEATRE,
        "theatre_session",
        loc.facility_id,
        None,
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = theatre::create_session(&mut tx, &ctx, &body).await?;
    crate::audit::emit(&mut *tx, &ctx, "care_ops.theatre.session_created", &state.cell, json!({"session_id": row.id, "theatre_location_id": row.theatre_location_id, "facility_id": row.facility_id}), None).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!(row))))
}

async fn list_cases(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<FacilityQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "perioperative_case",
        q.facility_id,
        None,
    )
    .await?;
    let facilities = visible(&ctx, actions::CARE_OPS_READ, q.facility_id)?;
    let mut conn = state.pool.acquire().await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let rows = theatre::list_cases(
        &mut conn,
        ctx.tenant_id,
        facilities.as_deref(),
        q.episode_id,
        q.open_only.unwrap_or(true),
        limit(q.limit),
    )
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for c in &rows {
        let readiness = theatre::readiness(&mut conn, c, None, None).await?;
        out.push(json!({"case": c, "readiness": readiness}));
    }
    Ok(Json(json!({"cases": out})))
}

async fn create_case(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<theatre::CaseInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = state.pool.begin().await?;
    let ep = ops::load_episode(&mut tx, ctx.tenant_id, body.episode_id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_THEATRE,
        "perioperative_case",
        ep.facility_id,
        Some(ep.patient_id),
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = theatre::create_case(&mut tx, &ctx, &body).await?;
    crate::audit::emit(&mut *tx, &ctx, "care_ops.theatre.case_created", &state.cell, json!({"case_id": row.id, "episode_id": row.episode_id, "procedure_code": row.procedure_code, "priority": row.priority}), None).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!(row))))
}

#[derive(Debug, Deserialize)]
pub struct CaseQuery {
    pub theatre_location_id: Option<Uuid>,
    pub scheduled_start: Option<DateTime<Utc>>,
}

async fn get_case(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Query(q): Query<CaseQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let c = theatre::load_case(&mut conn, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "perioperative_case",
        c.facility_id,
        Some(c.patient_id),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let readiness =
        theatre::readiness(&mut conn, &c, q.theatre_location_id, q.scheduled_start).await?;
    let history = theatre::case_history(&mut conn, ctx.tenant_id, id).await?;
    Ok(Json(
        json!({"case": c, "readiness": readiness, "history": history}),
    ))
}

async fn schedule_case(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<theatre::ScheduleInput>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let c = theatre::load_case(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_THEATRE,
        "perioperative_case",
        c.facility_id,
        Some(c.patient_id),
    )
    .await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let (row, check) = theatre::schedule(&mut tx, &ctx, id, &body).await?;
    if !check.conflicts.is_empty() {
        crate::audit::emit(
            &mut *tx,
            &ctx,
            "care_ops.theatre.conflict_detected",
            &state.cell,
            json!({"case_id": id, "conflicts": check.conflicts, "overridden": !check.feasible}),
            None,
        )
        .await?;
    }
    crate::audit::emit(&mut *tx, &ctx, "care_ops.theatre.case_scheduled", &state.cell, json!({"case_id": id, "theatre_location_id": row.theatre_location_id, "scheduled_start": row.scheduled_start, "scheduled_end": row.scheduled_end, "facts_hash": check.facts_hash}), None).await?;
    tx.commit().await?;
    Ok(Json(json!({"case": row, "readiness": check})))
}

async fn transition_case(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<theatre::CaseTransitionInput>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let c = theatre::load_case(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_THEATRE,
        "perioperative_case",
        c.facility_id,
        Some(c.patient_id),
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = theatre::transition_case(&mut tx, &ctx, id, &body).await?;
    crate::audit::emit(&mut *tx, &ctx, "care_ops.theatre.case_transitioned", &state.cell, json!({"case_id": id, "from": c.status, "to": row.status, "transition": body.transition, "reason_code": body.reason_code}), None).await?;
    tx.commit().await?;
    Ok(Json(json!(row)))
}

// ---------------------------------------------------------------- workforce

async fn list_shifts(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<FacilityQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "workforce_shift",
        q.facility_id,
        None,
    )
    .await?;
    let facilities = visible(&ctx, actions::CARE_OPS_READ, q.facility_id)?;
    let from = q.from.unwrap_or_else(|| Utc::now() - Duration::hours(12));
    let to = q.to.unwrap_or(from + Duration::days(7));
    let mut conn = state.pool.acquire().await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let shifts =
        workforce::list_shifts(&mut conn, ctx.tenant_id, facilities.as_deref(), from, to).await?;
    let mut out = Vec::with_capacity(shifts.len());
    for s in &shifts {
        let cov = workforce::coverage(&mut conn, ctx.tenant_id, s.id).await?;
        out.push(json!({"shift": s, "requirements": cov.requirements, "coverage": cov.report, "reallocation_candidates": cov.reallocations}));
    }
    Ok(Json(json!({"shifts": out})))
}

async fn create_shift(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<workforce::ShiftInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_WORKFORCE,
        "workforce_shift",
        Some(body.facility_id),
        None,
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = workforce::create_shift(&mut tx, &ctx, &body).await?;
    crate::audit::emit(&mut *tx, &ctx, "care_ops.workforce.shift_created", &state.cell, json!({"shift_id": row.id, "facility_id": row.facility_id, "label": row.label, "starts_at": row.starts_at, "ends_at": row.ends_at}), None).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!(row))))
}

async fn get_shift(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let s = workforce::load_shift(&mut conn, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "workforce_shift",
        s.facility_id,
        None,
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let cov = workforce::coverage(&mut conn, ctx.tenant_id, id).await?;
    Ok(Json(
        json!({"shift": cov.shift, "requirements": cov.requirements, "assignments": cov.assignments, "coverage": cov.report, "reallocation_candidates": cov.reallocations}),
    ))
}

async fn add_assignment(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<workforce::AssignmentInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut tx = state.pool.begin().await?;
    let s = workforce::load_shift(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_WORKFORCE,
        "workforce_shift",
        s.facility_id,
        None,
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = workforce::add_assignment(&mut tx, &ctx, id, &body).await?;
    crate::audit::emit(&mut *tx, &ctx, "care_ops.workforce.assignment_changed", &state.cell, json!({"shift_id": id, "assignment_id": row.id, "profession_code": row.profession_code, "role_in_shift": row.role_in_shift, "override": row.override_reason.is_some()}), None).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!(row))))
}

async fn change_assignment(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<workforce::AssignmentChangeInput>,
) -> Result<Json<Value>, ApiError> {
    let mut tx = state.pool.begin().await?;
    let facility_id: Uuid = sqlx::query_scalar(
        "SELECT s.facility_id FROM workforce_shift_assignments a JOIN workforce_shifts s ON s.id = a.shift_id WHERE a.tenant_id = $1 AND a.id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_WORKFORCE,
        "workforce_shift",
        facility_id,
        None,
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    if let Some(target) = body.target_shift_id {
        let target_facility: Uuid = sqlx::query_scalar(
            "SELECT facility_id FROM workforce_shifts WHERE tenant_id = $1 AND id = $2",
        )
        .bind(ctx.tenant_id)
        .bind(target)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(ApiError::not_found)?;
        if target_facility != facility_id {
            allow_existing(
                &state,
                &ctx,
                actions::CARE_OPS_MANAGE_WORKFORCE,
                "workforce_shift",
                target_facility,
                None,
            )
            .await?
            .record(&mut tx, &ctx, &state.cell)
            .await?;
        }
    }
    let out = workforce::change_assignment(&mut tx, &ctx, id, &body).await?;
    crate::audit::emit(&mut *tx, &ctx, "care_ops.workforce.assignment_changed", &state.cell, json!({"assignment_id": id, "status": body.status, "target_shift_id": body.target_shift_id}), None).await?;
    tx.commit().await?;
    Ok(Json(out))
}

async fn record_absence(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<workforce::AbsenceInput>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_WORKFORCE,
        "workforce_absence",
        None,
        None,
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let id = workforce::record_absence(&mut tx, &ctx, &body).await?;
    crate::audit::emit(&mut *tx, &ctx, "care_ops.workforce.absence_recorded", &state.cell, json!({"absence_id": id, "reason_code": body.reason_code, "starts_at": body.starts_at, "ends_at": body.ends_at}), None).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(json!({"absence_id": id}))))
}

// ---------------------------------------------------------------- forecasts

async fn list_forecasts(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<FacilityQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "care_operations_forecast",
        q.facility_id,
        None,
    )
    .await?;
    let facilities = visible(&ctx, actions::CARE_OPS_READ, q.facility_id)?;
    let mut conn = state.pool.acquire().await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let rows = forecast::list_forecasts(
        &mut conn,
        ctx.tenant_id,
        facilities.as_deref(),
        q.horizon.as_deref(),
        limit(q.limit),
    )
    .await?;
    Ok(Json(
        json!({"forecasts": rows.iter().map(|r| forecast::forecast_json(r, false)).collect::<Vec<_>>(), "forecast_version": wellos_domain::care_ops::FORECAST_VERSION}),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ForecastBody {
    pub facility_id: Uuid,
    pub horizon: String,
    pub horizon_start: Option<NaiveDate>,
}

async fn create_forecast(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<ForecastBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "care_operations_forecast",
        Some(body.facility_id),
        None,
    )
    .await?;
    visible(&ctx, actions::CARE_OPS_READ, Some(body.facility_id))?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = forecast::create_forecast(
        &mut tx,
        &ctx,
        &state,
        body.facility_id,
        &body.horizon,
        body.horizon_start,
    )
    .await?;
    tx.commit().await?;
    Ok((
        StatusCode::CREATED,
        Json(forecast::forecast_json(&row, false)),
    ))
}

async fn get_forecast(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let row = forecast::load_forecast(&mut conn, ctx.tenant_id, id).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "care_operations_forecast",
        row.facility_id,
        None,
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let include_facts = guard(
        &state,
        &ctx,
        actions::CARE_OPS_AUDIT,
        "care_operations_forecast",
        res(&ctx, Some(row.facility_id), None),
    )
    .await
    .is_ok();
    Ok(Json(forecast::forecast_json(&row, include_facts)))
}

// ---------------------------------------------------------------- jobs

/// Periodic tick: expire overdue holds and fire due escalation timers. Idempotent.
async fn tick(State(state): State<AppState>, ctx: AuthContext) -> Result<Json<Value>, ApiError> {
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "care_ops_job",
        None,
        None,
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let expired = ops::expire_holds(&mut tx, &ctx, ctx.tenant_id).await?;
    let fired = ops::fire_due_escalations(&mut tx, &ctx, ctx.tenant_id, Utc::now()).await?;
    for e in &fired {
        crate::audit::emit(&mut *tx, &ctx, "care_ops.escalation.fired", &state.cell, json!({"escalation_id": e.id, "kind": e.kind, "facility_id": e.facility_id, "internal_alert_id": e.internal_alert_id}), None).await?;
    }
    tx.commit().await?;
    Ok(Json(
        json!({"expired_holds": expired, "fired_escalations": fired.iter().map(|e| e.id).collect::<Vec<_>>()}),
    ))
}

// ---------------------------------------------------------------- dMind (advisory)

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct AiBody {
    pub facility_id: Option<Uuid>,
    pub language: Option<String>,
    pub shift_label: Option<String>,
}

async fn flow_brief(
    State(state): State<AppState>,
    ctx: AuthContext,
    body: OptionalJson<AiBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let facility_id = body
        .facility_id
        .ok_or_else(|| ApiError::bad_request("facility_required", "facility_id is required"))?;
    let allowed = allow(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "care_ops_snapshot",
        Some(facility_id),
        None,
    )
    .await?;
    visible(&ctx, actions::CARE_OPS_READ, Some(facility_id))?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Api).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let (snapshot_id, snapshot, outcome) =
        ops::ai::flow_brief(&state, &ctx, &mut tx, facility_id, body.language.as_deref()).await?;
    tx.commit().await?;
    Ok(Json(json!({
        "snapshot_id": snapshot_id,
        "snapshot": snapshot,
        "brief": outcome,
    })))
}

async fn placement_ranking(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<AiBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let mut tx = state.pool.begin().await?;
    let req = ops::load_request(&mut tx, ctx.tenant_id, id, true).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "placement_request",
        req.facility_id,
        Some(req.patient_id),
    )
    .await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Api).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let outcome =
        ops::ai::placement_ranking(&state, &ctx, &mut tx, &req, body.language.as_deref()).await?;
    tx.commit().await?;
    Ok(Json(json!({ "request_id": id, "ranking": outcome })))
}

async fn discharge_coordination(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<AiBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let mut tx = state.pool.begin().await?;
    let ep = ops::load_episode(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "care_episode",
        ep.facility_id,
        Some(ep.patient_id),
    )
    .await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Api).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let outcome =
        ops::ai::discharge_coordination(&state, &ctx, &mut tx, &ep, body.language.as_deref())
            .await?;
    tx.commit().await?;
    Ok(Json(json!({ "episode_id": id, "coordination": outcome })))
}

async fn theatre_explanation(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<AiBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let facility_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT facility_id FROM theatre_sessions WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .fetch_optional(&state.pool)
    .await?;
    let facility_id = facility_id.ok_or_else(ApiError::not_found)?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_THEATRE,
        "theatre_session",
        facility_id,
        None,
    )
    .await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Api).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let (_, outcome) =
        ops::ai::theatre_plan(&state, &ctx, &mut tx, id, body.language.as_deref()).await?;
    tx.commit().await?;
    Ok(Json(json!({ "session_id": id, "explanation": outcome })))
}

async fn forecast_explanation(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<AiBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let mut tx = state.pool.begin().await?;
    let row = forecast::load_forecast(&mut tx, ctx.tenant_id, id).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_READ,
        "care_operations_forecast",
        row.facility_id,
        None,
    )
    .await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Api).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let outcome =
        ops::ai::forecast_explanation(&state, &ctx, &mut tx, &row, body.language.as_deref())
            .await?;
    tx.commit().await?;
    Ok(Json(json!({ "forecast_id": id, "explanation": outcome })))
}

async fn handoff_draft(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<AiBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let shift_label = body
        .shift_label
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ApiError::bad_request("validation_failed", "shift_label is required"))?;
    let mut tx = state.pool.begin().await?;
    let ep = ops::load_episode(&mut tx, ctx.tenant_id, id, false).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_MANAGE_FLOW,
        "care_episode",
        ep.facility_id,
        Some(ep.patient_id),
    )
    .await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Api).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let (outcome, handoff) = ops::ai::handoff_draft(
        &state,
        &ctx,
        &mut tx,
        &ep,
        shift_label,
        body.language.as_deref(),
    )
    .await?;
    tx.commit().await?;
    let code = if handoff.is_some() {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((
        code,
        Json(json!({ "episode_id": id, "draft": outcome, "handoff": handoff })),
    ))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ArtifactReviewBody {
    pub decision: String,
    pub note: Option<String>,
}

async fn review_artifact(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ArtifactReviewBody>,
) -> Result<Json<Value>, ApiError> {
    let decision = match body.decision.as_str() {
        "approved" | "accepted" => ReviewDecision::Approved,
        "rejected" => ReviewDecision::Rejected,
        _ => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "decision must be 'approved' or 'rejected'",
            ))
        }
    };
    if body.note.as_deref().is_some_and(|n| n.len() > 4000) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "note exceeds 4000 characters",
        ));
    }
    let mut tx = state.pool.begin().await?;
    let target = ops::ai::load_review_target(&mut tx, ctx.tenant_id, id).await?;
    let allowed = allow_existing(
        &state,
        &ctx,
        actions::CARE_OPS_REVIEW_AI,
        "ai_artifact",
        target.facility_id,
        target.patient_id,
    )
    .await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let next = ops::ai::review(
        &mut tx,
        &ctx,
        &state.cell,
        id,
        &target,
        decision,
        body.note.as_deref(),
    )
    .await?;
    tx.commit().await?;
    Ok(Json(json!({
        "artifact_id": id,
        "artifact_type": target.artifact_type,
        "status": next.as_str(),
    })))
}
