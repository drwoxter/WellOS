//! Staff capacity panel: deterministic `capacity-forecast.v1` runs and the
//! governed `capacity-explanation.v1` explanation.
//!
//! Forecasts are facility-scoped operational data (`capacity.review`); no
//! patient identifiers are returned. A forecast never changes schedules,
//! availability or priority — it is evidence for a human decision.

use crate::auth::AuthContext;
use crate::capacity;
use crate::error::ApiError;
use crate::policy::{actions, facility_scope, ResourceCtx};
use crate::ratelimit;
use crate::routes::extract::OptionalJson;
use crate::routes::guard;
use crate::scheduling;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{NaiveDate, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

const DEFAULT_LIMIT: i64 = 20;
const MAX_LIMIT: i64 = 100;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route(
            "/capacity/forecasts",
            get(list_forecasts).post(create_forecast),
        )
        .route("/capacity/forecasts/:id", get(get_forecast))
        .route("/capacity/forecasts/:id/explain", post(explain_forecast))
}

#[derive(Debug, Deserialize)]
pub struct ListQuery {
    pub facility_id: Option<Uuid>,
    pub service_code: Option<String>,
    pub limit: Option<i64>,
}

/// Facilities the caller may review, intersected with an optional filter.
/// Returns `None` for tenant-wide reviewers without a filter.
fn visible_facilities(
    ctx: &AuthContext,
    requested: Option<Uuid>,
) -> Result<Option<Vec<Uuid>>, ApiError> {
    match (facility_scope(ctx, actions::CAPACITY_REVIEW), requested) {
        (None, None) => Ok(None),
        (None, Some(f)) => Ok(Some(vec![f])),
        (Some(ids), None) => Ok(Some(ids)),
        (Some(ids), Some(f)) => {
            if ids.contains(&f) {
                Ok(Some(vec![f]))
            } else {
                Err(ApiError::forbidden("facility is outside your scope"))
            }
        }
    }
}

async fn list_forecasts(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::CAPACITY_REVIEW,
        "capacity_forecast",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: q.facility_id,
        }),
    )
    .await?;
    allowed.record_on_pool(&state, &ctx).await?;
    let facilities = visible_facilities(&ctx, q.facility_id)?;
    let service = match q.service_code.as_deref() {
        Some(code) if !wellos_domain::access::is_valid_code(code) => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "service_code has an invalid format",
            ))
        }
        other => other,
    };
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let mut conn = state.pool.acquire().await?;
    let rows = capacity::list_forecasts(
        &mut conn,
        ctx.tenant_id,
        facilities.as_deref(),
        service,
        limit,
    )
    .await?;
    Ok(Json(json!({
        "forecasts": rows.iter().map(capacity::forecast_json).collect::<Vec<_>>(),
        "forecast_version": wellos_domain::capacity::FORECAST_VERSION,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CreateBody {
    pub facility_id: Uuid,
    pub service_code: String,
    pub horizon_start: Option<NaiveDate>,
    pub horizon_days: Option<i64>,
}

async fn create_forecast(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<CreateBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::CAPACITY_REVIEW,
        "capacity_forecast",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: Some(body.facility_id),
        }),
    )
    .await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let horizon_start = body
        .horizon_start
        .unwrap_or_else(|| Utc::now().date_naive());
    let horizon_days = body.horizon_days.unwrap_or(capacity::DEFAULT_HORIZON_DAYS);
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let row = capacity::create_forecast(
        &mut tx,
        &ctx,
        &state,
        body.facility_id,
        &body.service_code,
        horizon_start,
        horizon_days,
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(capacity::forecast_json(&row))))
}

async fn get_forecast(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let row = capacity::load_forecast(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::CAPACITY_REVIEW,
        "capacity_forecast",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: Some(row.facility_id),
        }),
    )
    .await
    .map_err(|_| ApiError::not_found())?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let explanation = match row.explanation_artifact_id {
        Some(aid) => sqlx::query(
            "SELECT id, output, synthetic, status, model, route, reused_from
             FROM ai_artifacts WHERE id = $1 AND tenant_id = $2",
        )
        .bind(aid)
        .bind(ctx.tenant_id)
        .fetch_optional(&mut *conn)
        .await?
        .map(|r| {
            use sqlx::Row;
            json!({
                "artifact_id": r.get::<Uuid, _>("id"),
                "explanation": r.get::<Value, _>("output"),
                "synthetic": r.get::<bool, _>("synthetic"),
                "status": r.get::<String, _>("status"),
                "model": r.get::<String, _>("model"),
                "provider": r.get::<String, _>("route"),
                "reused": r.get::<Option<Uuid>, _>("reused_from").is_some(),
            })
        }),
        None => None,
    };
    let mut v = capacity::forecast_json(&row);
    v["explanation"] = explanation.unwrap_or(Value::Null);
    Ok(Json(v))
}

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ExplainBody {
    pub language: Option<String>,
}

async fn explain_forecast(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<ExplainBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let language = scheduling::clean_text(body.language, "language", 35)?
        .map(|l| l.to_lowercase())
        .unwrap_or_else(|| "en".into());
    let mut conn = state.pool.acquire().await?;
    let row = capacity::load_forecast(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::CAPACITY_REVIEW,
        "capacity_forecast",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: Some(row.facility_id),
        }),
    )
    .await
    .map_err(|_| ApiError::not_found())?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let outcome = capacity::explain(&state, &ctx, row.id, &language).await?;
    let mut conn = state.pool.acquire().await?;
    let row = capacity::load_forecast(&mut conn, ctx.tenant_id, row.id).await?;
    Ok(Json(json!({
        "explanation": outcome,
        "forecast": capacity::forecast_json(&row),
    })))
}
