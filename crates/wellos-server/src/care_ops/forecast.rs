//! Deterministic `care-operations-forecast.v1`: aggregate 24-hour and 7-day
//! operational pressure from persisted synthetic/operational history,
//! calendar effects and current occupancy. Patient-level prediction is never
//! produced; dMind may only explain a stored forecast.

use super::workforce::facility_tz;
use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::state::AppState;
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use uuid::Uuid;
use wellos_domain::care_ops::{
    self as domain, DailyOpsHistory, OpsCalendarEffect, OpsForecast, OpsForecastInputs,
    FORECAST_VERSION,
};

pub const HISTORY_DAYS: i64 = 56;

#[derive(Debug, Clone, Serialize)]
pub struct ForecastRow {
    pub id: Uuid,
    pub facility_id: Uuid,
    pub forecast_version: String,
    pub horizon: String,
    pub generated_at: DateTime<Utc>,
    pub horizon_start: DateTime<Utc>,
    pub horizon_end: DateTime<Utc>,
    pub status: String,
    pub source_facts: Value,
    pub facts_hash: String,
    pub output: Value,
    pub explanation_artifact_id: Option<Uuid>,
}

const COLS: &str =
    "id, facility_id, forecast_version, horizon, generated_at, horizon_start, horizon_end, status,
    source_facts, facts_hash, output, explanation_artifact_id";

fn from_row(r: &PgRow) -> ForecastRow {
    ForecastRow {
        id: r.get("id"),
        facility_id: r.get("facility_id"),
        forecast_version: r.get("forecast_version"),
        horizon: r.get("horizon"),
        generated_at: r.get("generated_at"),
        horizon_start: r.get("horizon_start"),
        horizon_end: r.get("horizon_end"),
        status: r.get("status"),
        source_facts: r.get("source_facts"),
        facts_hash: r.get("facts_hash"),
        output: r.get("output"),
        explanation_artifact_id: r.get("explanation_artifact_id"),
    }
}

pub fn horizon_days(h: &str) -> Option<usize> {
    match h {
        "24h" => Some(1),
        "7d" => Some(7),
        _ => None,
    }
}

async fn daily_counts(
    conn: &mut PgConnection,
    sql: &str,
    tenant_id: Uuid,
    facility_id: Uuid,
    tz: &str,
    from: DateTime<Utc>,
    to: DateTime<Utc>,
) -> Result<BTreeMap<NaiveDate, i32>, ApiError> {
    let rows = sqlx::query(sql)
        .bind(tenant_id)
        .bind(facility_id)
        .bind(tz)
        .bind(from)
        .bind(to)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows
        .iter()
        .map(|r| (r.get::<NaiveDate, _>("day"), r.get::<i32, _>("n")))
        .collect())
}

pub async fn assemble_inputs(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_id: Uuid,
    horizon_start: NaiveDate,
    days: usize,
) -> Result<(OpsForecastInputs, DateTime<Utc>, DateTime<Utc>), ApiError> {
    let tz = facility_tz(conn, tenant_id, facility_id).await?;
    let tz_name = tz.name().to_string();
    let midnight = NaiveTime::from_hms_opt(0, 0, 0).unwrap_or_default();
    let to_utc = |d: NaiveDate| {
        tz.from_local_datetime(&d.and_time(midnight))
            .earliest()
            .map(|t| t.with_timezone(&Utc))
            .unwrap_or_else(|| Utc.from_utc_datetime(&d.and_time(midnight)))
    };
    let history_start = horizon_start - Duration::days(HISTORY_DAYS);
    let hist_from = to_utc(history_start);
    let hist_to = to_utc(horizon_start);
    let horizon_end_date = horizon_start + Duration::days(days as i64);
    let h_start = hist_to;
    let h_end = to_utc(horizon_end_date);

    let arrivals = daily_counts(
        conn,
        "SELECT (arrived_at AT TIME ZONE $3)::date AS day, count(*)::int AS n FROM visits
         WHERE tenant_id = $1 AND facility_id = $2 AND arrived_at >= $4 AND arrived_at < $5 GROUP BY 1",
        tenant_id, facility_id, &tz_name, hist_from, hist_to,
    )
    .await?;
    let admissions = daily_counts(
        conn,
        "SELECT (started_at AT TIME ZONE $3)::date AS day, count(*)::int AS n FROM care_episodes
         WHERE tenant_id = $1 AND facility_id = $2 AND started_at >= $4 AND started_at < $5 GROUP BY 1",
        tenant_id, facility_id, &tz_name, hist_from, hist_to,
    )
    .await?;
    let discharges = daily_counts(
        conn,
        "SELECT (ended_at AT TIME ZONE $3)::date AS day, count(*)::int AS n FROM care_episodes
         WHERE tenant_id = $1 AND facility_id = $2 AND status = 'completed' AND ended_at >= $4 AND ended_at < $5 GROUP BY 1",
        tenant_id, facility_id, &tz_name, hist_from, hist_to,
    )
    .await?;
    let placements = daily_counts(
        conn,
        "SELECT (created_at AT TIME ZONE $3)::date AS day, count(*)::int AS n FROM placement_requests
         WHERE tenant_id = $1 AND facility_id = $2 AND created_at >= $4 AND created_at < $5 GROUP BY 1",
        tenant_id, facility_id, &tz_name, hist_from, hist_to,
    )
    .await?;
    let theatre = daily_counts(
        conn,
        "SELECT (COALESCE(scheduled_start, created_at) AT TIME ZONE $3)::date AS day, count(*)::int AS n FROM perioperative_cases
         WHERE tenant_id = $1 AND facility_id = $2 AND status <> 'cancelled'
           AND COALESCE(scheduled_start, created_at) >= $4 AND COALESCE(scheduled_start, created_at) < $5 GROUP BY 1",
        tenant_id, facility_id, &tz_name, hist_from, hist_to,
    )
    .await?;
    let transport = daily_counts(
        conn,
        "SELECT (t.created_at AT TIME ZONE $3)::date AS day, count(*)::int AS n FROM transport_requests t
         JOIN appointments a ON a.id = t.appointment_id
         WHERE t.tenant_id = $1 AND a.facility_id = $2 AND t.created_at >= $4 AND t.created_at < $5 GROUP BY 1",
        tenant_id, facility_id, &tz_name, hist_from, hist_to,
    )
    .await?;
    let mut history = Vec::new();
    let mut d = history_start;
    while d < horizon_start {
        history.push(DailyOpsHistory {
            date: d,
            arrivals: *arrivals.get(&d).unwrap_or(&0),
            admissions: *admissions.get(&d).unwrap_or(&0),
            discharges: *discharges.get(&d).unwrap_or(&0),
            placement_requests: *placements.get(&d).unwrap_or(&0),
            theatre_cases: *theatre.get(&d).unwrap_or(&0),
            transport_requests: *transport.get(&d).unwrap_or(&0),
        });
        d += Duration::days(1);
    }

    let cal = sqlx::query(
        "SELECT name, kind, starts_on, ends_on, demand_multiplier::float8 AS dm, capacity_multiplier::float8 AS cm
         FROM operational_calendar_events
         WHERE tenant_id = $1 AND active AND (facility_id IS NULL OR facility_id = $2)
           AND ends_on >= $3 AND starts_on <= $4
         ORDER BY starts_on, id",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .bind(history_start)
    .bind(horizon_end_date)
    .fetch_all(&mut *conn)
    .await?;
    let mut calendar = Vec::new();
    for r in &cal {
        let starts: NaiveDate = r.get("starts_on");
        let ends: NaiveDate = r.get("ends_on");
        let kind: String = r.get("kind");
        let mut d = starts.max(history_start);
        while d <= ends.min(horizon_end_date) {
            calendar.push(OpsCalendarEffect {
                date: d,
                name: r.get("name"),
                kind: kind.clone(),
                demand_multiplier: r.get("dm"),
                capacity_multiplier: if kind == "closure" { 0.0 } else { r.get("cm") },
            });
            d += Duration::days(1);
        }
    }

    let pos = sqlx::query(
        "SELECT count(*) FILTER (WHERE active AND operational_status = 'open')::int AS total,
                count(*) FILTER (WHERE position_state = 'occupied')::int AS occupied,
                count(*) FILTER (WHERE position_state IN ('blocked','out_of_service') OR operational_status <> 'open' OR NOT active)::int AS blocked
         FROM operational_locations WHERE tenant_id = $1 AND facility_id = $2 AND level = 'position'",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .fetch_one(&mut *conn)
    .await?;
    let discharge_pipeline: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM care_episodes WHERE tenant_id = $1 AND facility_id = $2 AND status IN ('discharge_preparation','discharge_ready')",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .fetch_one(&mut *conn)
    .await?;
    let scheduled_theatre_cases = daily_counts(
        conn,
        "SELECT (scheduled_start AT TIME ZONE $3)::date AS day, count(*)::int AS n FROM perioperative_cases
         WHERE tenant_id = $1 AND facility_id = $2 AND status IN ('scheduled','ready','delayed')
           AND scheduled_start >= $4 AND scheduled_start < $5 GROUP BY 1",
        tenant_id, facility_id, &tz_name, h_start, h_end,
    )
    .await?;
    let theatre_sessions_capacity = daily_counts(
        conn,
        "SELECT (starts_at AT TIME ZONE $3)::date AS day,
                (sum(EXTRACT(EPOCH FROM (ends_at - starts_at)) / 60) / 90)::int AS n
         FROM theatre_sessions WHERE tenant_id = $1 AND facility_id = $2 AND status = 'planned'
           AND starts_at >= $4 AND starts_at < $5 GROUP BY 1",
        tenant_id,
        facility_id,
        &tz_name,
        h_start,
        h_end,
    )
    .await?;
    let gap_rows = sqlx::query(
        "SELECT s.id, s.local_date FROM workforce_shifts s
         WHERE s.tenant_id = $1 AND s.facility_id = $2 AND s.status IN ('planned','active') AND s.starts_at >= $3 AND s.starts_at < $4",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .bind(h_start)
    .bind(h_end)
    .fetch_all(&mut *conn)
    .await?;
    let mut staffing_gaps: BTreeMap<NaiveDate, i32> = BTreeMap::new();
    for r in &gap_rows {
        let sid: Uuid = r.get("id");
        let date: NaiveDate = r.get("local_date");
        let cov = super::workforce::coverage(conn, tenant_id, sid).await?;
        let shortfall: i64 = cov.report.gaps.iter().map(|g| g.shortfall).sum();
        *staffing_gaps.entry(date).or_default() += shortfall as i32;
    }

    let inputs = OpsForecastInputs {
        history,
        calendar,
        horizon_start,
        horizon_days: days,
        total_positions: pos.get("total"),
        occupied_positions: pos.get("occupied"),
        blocked_positions: pos.get("blocked"),
        discharge_pipeline: discharge_pipeline as i32,
        scheduled_theatre_cases,
        staffing_gaps,
        theatre_sessions_capacity,
    };
    Ok((inputs, h_start, h_end))
}

pub async fn create_forecast(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    state: &AppState,
    facility_id: Uuid,
    horizon: &str,
    horizon_start: Option<NaiveDate>,
) -> Result<ForecastRow, ApiError> {
    let days = horizon_days(horizon)
        .ok_or_else(|| ApiError::bad_request("invalid_horizon", "horizon must be 24h or 7d"))?;
    let exists: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM facilities WHERE tenant_id = $1 AND id = $2")
            .bind(ctx.tenant_id)
            .bind(facility_id)
            .fetch_optional(&mut *conn)
            .await?;
    if exists.is_none() {
        return Err(ApiError::not_found());
    }
    let start = horizon_start.unwrap_or_else(|| Utc::now().date_naive());
    let (inputs, h_start, h_end) =
        assemble_inputs(conn, ctx.tenant_id, facility_id, start, days).await?;
    let source_facts = serde_json::to_value(&inputs).map_err(ApiError::internal)?;
    let facts_hash = domain::hash_facts(&source_facts);
    let out = domain::forecast_operations(&inputs);
    let status = match &out {
        OpsForecast::Ready { .. } => "ready",
        OpsForecast::InsufficientHistory { .. } => "insufficient_history",
    };
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO care_operations_forecasts
         (id, tenant_id, facility_id, forecast_version, horizon, horizon_start, horizon_end, status, source_facts, facts_hash, output, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(facility_id)
    .bind(FORECAST_VERSION)
    .bind(horizon)
    .bind(h_start)
    .bind(h_end)
    .bind(status)
    .bind(&source_facts)
    .bind(&facts_hash)
    .bind(serde_json::to_value(&out).map_err(ApiError::internal)?)
    .bind(ctx.user_id)
    .execute(&mut *conn)
    .await?;
    audit::emit(
        &mut *conn,
        ctx,
        "care_ops.forecast.generated",
        &state.cell,
        json!({
            "care_operations_forecast_id": id,
            "facility_id": facility_id,
            "forecast_version": FORECAST_VERSION,
            "horizon": horizon,
            "status": status,
            "history_days": inputs.history.len(),
            "facts_hash": facts_hash,
        }),
        None,
    )
    .await?;
    load_forecast(conn, ctx.tenant_id, id).await
}

pub async fn load_forecast(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<ForecastRow, ApiError> {
    let sql =
        format!("SELECT {COLS} FROM care_operations_forecasts WHERE tenant_id = $1 AND id = $2");
    let row = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(ApiError::not_found)?;
    Ok(from_row(&row))
}

pub async fn list_forecasts(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_ids: Option<&[Uuid]>,
    horizon: Option<&str>,
    limit: i64,
) -> Result<Vec<ForecastRow>, ApiError> {
    let sql = format!(
        "SELECT {COLS} FROM care_operations_forecasts
         WHERE tenant_id = $1 AND ($2::uuid[] IS NULL OR facility_id = ANY($2)) AND ($3::text IS NULL OR horizon = $3)
         ORDER BY generated_at DESC LIMIT $4"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(facility_ids)
        .bind(horizon)
        .bind(limit)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.iter().map(from_row).collect())
}

/// Latest forecast per horizon for a facility (command-center freshness).
pub async fn latest_per_horizon(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_id: Uuid,
) -> Result<Vec<ForecastRow>, ApiError> {
    let sql = format!(
        "SELECT DISTINCT ON (horizon) {COLS} FROM care_operations_forecasts
         WHERE tenant_id = $1 AND facility_id = $2 ORDER BY horizon, generated_at DESC"
    );
    let rows = sqlx::query(&sql)
        .bind(tenant_id)
        .bind(facility_id)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows.iter().map(from_row).collect())
}

/// Public JSON: aggregate output only; the raw source facts are exposed
/// separately to auditors.
pub fn forecast_json(r: &ForecastRow, include_facts: bool) -> Value {
    let mut v = json!({
        "id": r.id,
        "facility_id": r.facility_id,
        "forecast_version": r.forecast_version,
        "horizon": r.horizon,
        "generated_at": r.generated_at,
        "horizon_start": r.horizon_start,
        "horizon_end": r.horizon_end,
        "status": r.status,
        "facts_hash": r.facts_hash,
        "output": r.output,
        "explanation_artifact_id": r.explanation_artifact_id,
    });
    if include_facts {
        v["source_facts"] = r.source_facts.clone();
    }
    v
}
