//! `capacity-forecast.v1` assembly and persistence, plus the governed
//! `capacity-explanation.v1` dMind operation bound to one stored forecast.
//!
//! The server only *gathers facts* (historical appointments, planned
//! bookable slots, tenant-configured operational calendar) and hands them
//! to the deterministic domain forecaster. dMind may explain a persisted
//! forecast; it cannot change schedules, availability or priority, and the
//! console works identically when it is disabled.

use crate::aigov;
use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::scheduling;
use crate::state::AppState;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use dmind_gateway::access::{CapacityExplanationRequest, CAPACITY_EXPLANATION_TEMPLATE};
use dmind_gateway::{hash_json, GatewayError};
use serde::Serialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wellos_domain::access_ai::{CapacityExplanationV1, CAPACITY_EXPLANATION_SCHEMA};
use wellos_domain::ai::{ArtifactStatus, ProviderInfo};
use wellos_domain::capacity::{
    forecast, CalendarEffect, DailyHistory, Forecast, ForecastInputs, PlannedCapacity,
    FORECAST_VERSION,
};
use wellos_domain::matcher::planned_slots_on;

/// Weeks of history read before the horizon start.
pub const HISTORY_WEEKS: i64 = 26;
pub const MIN_HORIZON_DAYS: i64 = 7;
pub const MAX_HORIZON_DAYS: i64 = 90;
pub const DEFAULT_HORIZON_DAYS: i64 = 28;

#[derive(Debug, Clone, Serialize)]
pub struct ForecastRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub facility_id: Uuid,
    pub service_code: String,
    pub forecast_version: String,
    pub horizon_start: NaiveDate,
    pub horizon_end: NaiveDate,
    pub status: String,
    pub inputs_hash: String,
    pub output: Value,
    pub explanation_artifact_id: Option<Uuid>,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, tenant_id, facility_id, service_code, forecast_version, horizon_start,
    horizon_end, status, inputs_hash, output, explanation_artifact_id, created_by, created_at";

fn from_row(r: &sqlx::postgres::PgRow) -> ForecastRow {
    ForecastRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        facility_id: r.get("facility_id"),
        service_code: r.get("service_code"),
        forecast_version: r.get("forecast_version"),
        horizon_start: r.get("horizon_start"),
        horizon_end: r.get("horizon_end"),
        status: r.get("status"),
        inputs_hash: r.get("inputs_hash"),
        output: r.get("output"),
        explanation_artifact_id: r.get("explanation_artifact_id"),
        created_by: r.get("created_by"),
        created_at: r.get("created_at"),
    }
}

pub async fn load_forecast(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<ForecastRow, ApiError> {
    let sql = format!("SELECT {COLUMNS} FROM capacity_forecasts WHERE id = $1 AND tenant_id = $2");
    sqlx::query(&sql)
        .bind(id)
        .bind(tenant_id)
        .fetch_optional(&mut *conn)
        .await?
        .map(|r| from_row(&r))
        .ok_or_else(ApiError::not_found)
}

pub async fn list_forecasts(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_ids: Option<&[Uuid]>,
    service_code: Option<&str>,
    limit: i64,
) -> Result<Vec<ForecastRow>, ApiError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM capacity_forecasts
         WHERE tenant_id = $1
           AND ($2::uuid[] IS NULL OR facility_id = ANY($2))
           AND ($3::text IS NULL OR service_code = $3)
         ORDER BY created_at DESC, id LIMIT $4"
    );
    Ok(sqlx::query(&sql)
        .bind(tenant_id)
        .bind(facility_ids)
        .bind(service_code)
        .bind(limit)
        .fetch_all(&mut *conn)
        .await?
        .iter()
        .map(from_row)
        .collect())
}

pub fn forecast_json(f: &ForecastRow) -> Value {
    json!({
        "id": f.id,
        "facility_id": f.facility_id,
        "service_code": f.service_code,
        "forecast_version": f.forecast_version,
        "horizon_start": f.horizon_start,
        "horizon_end": f.horizon_end,
        "status": f.status,
        "inputs_hash": f.inputs_hash,
        "forecast": f.output,
        "explanation_artifact_id": f.explanation_artifact_id,
        "created_at": f.created_at,
    })
}

/// Gather the deterministic inputs for one facility/service and horizon.
pub async fn assemble_inputs(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_id: Uuid,
    service_code: &str,
    horizon_start: NaiveDate,
    horizon_days: i64,
) -> Result<ForecastInputs, ApiError> {
    let horizon_end = horizon_start + Duration::days(horizon_days - 1);
    let history_start = horizon_start - Duration::weeks(HISTORY_WEEKS);
    let service = scheduling::load_service(conn, tenant_id, service_code).await?;
    let facilities = scheduling::load_facility_facts(conn, tenant_id, Some(&[facility_id])).await?;
    let facility = facilities.first().ok_or_else(|| {
        ApiError::bad_request("unknown_facility", "facility is not in this tenant")
    })?;
    let tz: chrono_tz::Tz = facility.time_zone.parse().unwrap_or(chrono_tz::UTC);

    // Historical demand: appointments *requested for* each day (all
    // outcomes), cancellations with their lead time, and no-shows. Dates
    // are the facility's local dates.
    let rows = sqlx::query(
        "SELECT (starts_at AT TIME ZONE $4)::date AS day,
                count(*)::int AS demand,
                count(*) FILTER (WHERE status = 'cancelled')::int AS cancellations,
                count(*) FILTER (WHERE status = 'no_show')::int AS no_shows,
                avg(extract(epoch FROM (starts_at - cancelled_at)) / 3600.0)
                    FILTER (WHERE status = 'cancelled' AND cancelled_at IS NOT NULL) AS lead_hours
         FROM appointments
         WHERE tenant_id = $1 AND facility_id = $2 AND service_code = $3
           AND starts_at >= ($5::date::timestamp AT TIME ZONE $4)
           AND starts_at <  ($6::date::timestamp AT TIME ZONE $4)
         GROUP BY 1 ORDER BY 1",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .bind(service_code)
    .bind(&facility.time_zone)
    .bind(history_start)
    .bind(horizon_start)
    .fetch_all(&mut *conn)
    .await?;
    let mut history: Vec<DailyHistory> = rows
        .iter()
        .map(|r| DailyHistory {
            date: r.get("day"),
            demand: r.get("demand"),
            cancellations: r.get("cancellations"),
            no_shows: r.get("no_shows"),
            cancellation_lead_hours: r.get::<Option<f64>, _>("lead_hours"),
        })
        .collect();
    // Zero-fill so weekday means are not biased by missing days.
    let mut d = history_start;
    while d < horizon_start {
        if !history.iter().any(|h| h.date == d) {
            history.push(DailyHistory {
                date: d,
                demand: 0,
                cancellations: 0,
                no_shows: 0,
                cancellation_lead_hours: None,
            });
        }
        d += Duration::days(1);
    }
    history.sort_by_key(|h| h.date);

    // Planned bookable slots over the horizon from the same availability
    // facts the matcher uses (rules, breaks, opening hours, exceptions).
    let window_start = wellos_domain::matcher::local_to_utc(
        tz,
        horizon_start,
        chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap_or_default(),
    );
    let window_end = wellos_domain::matcher::local_to_utc(
        tz,
        horizon_end + Duration::days(1),
        chrono::NaiveTime::from_hms_opt(0, 0, 0).unwrap_or_default(),
    );
    let resources = scheduling::load_resource_facts(
        conn,
        tenant_id,
        Some(&[facility_id]),
        service_code,
        &[],
        window_start,
        window_end,
    )
    .await?;
    let confirmed_rows = sqlx::query(
        "SELECT (starts_at AT TIME ZONE $4)::date AS day, count(*)::int AS confirmed
         FROM appointments
         WHERE tenant_id = $1 AND facility_id = $2 AND service_code = $3
           AND status IN ('confirmed', 'rescheduled')
           AND starts_at >= $5 AND starts_at < $6
         GROUP BY 1",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .bind(service_code)
    .bind(&facility.time_zone)
    .bind(window_start)
    .bind(window_end)
    .fetch_all(&mut *conn)
    .await?;
    let mut planned = Vec::with_capacity(horizon_days as usize);
    let mut d = horizon_start;
    while d <= horizon_end {
        let mut slots = 0;
        let mut lost = 0;
        for r in &resources {
            let p = planned_slots_on(
                r,
                Some(facility),
                d,
                service_code,
                service.config.duration_minutes,
            );
            slots += p.slots;
            lost += p.exception_slots_lost;
        }
        let confirmed = confirmed_rows
            .iter()
            .find(|r| r.get::<NaiveDate, _>("day") == d)
            .map(|r| r.get::<i32, _>("confirmed"))
            .unwrap_or(0);
        planned.push(PlannedCapacity {
            date: d,
            slots,
            confirmed,
            exception_slots_lost: lost,
        });
        d += Duration::days(1);
    }

    // Tenant-configured operational calendar (holidays, school breaks,
    // local events, seasonal periods, closures) for the facility or tenant.
    let cal = sqlx::query(
        "SELECT name, kind, starts_on, ends_on, demand_multiplier::float8 AS dm,
                capacity_multiplier::float8 AS cm
         FROM operational_calendar_events
         WHERE tenant_id = $1 AND active
           AND (facility_id IS NULL OR facility_id = $2)
           AND ends_on >= $3 AND starts_on <= $4
         ORDER BY starts_on, id",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .bind(history_start)
    .bind(horizon_end)
    .fetch_all(&mut *conn)
    .await?;
    let mut calendar = Vec::new();
    for r in &cal {
        let starts: NaiveDate = r.get("starts_on");
        let ends: NaiveDate = r.get("ends_on");
        let mut d = starts.max(history_start);
        while d <= ends.min(horizon_end) {
            calendar.push(CalendarEffect {
                date: d,
                name: r.get("name"),
                kind: r.get("kind"),
                demand_multiplier: r.get("dm"),
                capacity_multiplier: r.get("cm"),
            });
            d += Duration::days(1);
        }
    }
    Ok(ForecastInputs {
        history,
        planned,
        calendar,
        horizon_start,
        horizon_end,
    })
}

/// Compute and persist a forecast; returns the stored row.
pub async fn create_forecast(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    facility_id: Uuid,
    service_code: &str,
    horizon_start: NaiveDate,
    horizon_days: i64,
) -> Result<ForecastRow, ApiError> {
    if !(MIN_HORIZON_DAYS..=MAX_HORIZON_DAYS).contains(&horizon_days) {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("horizon_days must be between {MIN_HORIZON_DAYS} and {MAX_HORIZON_DAYS}"),
        ));
    }
    let inputs = assemble_inputs(
        tx,
        ctx.tenant_id,
        facility_id,
        service_code,
        horizon_start,
        horizon_days,
    )
    .await?;
    let inputs_hash = hash_json(&inputs);
    let out = forecast(&inputs);
    let status = match &out {
        Forecast::Ready { .. } => "ready",
        Forecast::InsufficientHistory { .. } => "insufficient_history",
    };
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO capacity_forecasts
         (id, tenant_id, facility_id, service_code, forecast_version, horizon_start, horizon_end,
          status, inputs_hash, output, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(facility_id)
    .bind(service_code)
    .bind(FORECAST_VERSION)
    .bind(inputs.horizon_start)
    .bind(inputs.horizon_end)
    .bind(status)
    .bind(&inputs_hash)
    .bind(serde_json::to_value(&out).map_err(ApiError::internal)?)
    .bind(ctx.user_id)
    .execute(&mut **tx)
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "capacity.forecast.calculated",
        &state.cell,
        json!({
            "capacity_forecast_id": id,
            "facility_id": facility_id,
            "service_code": service_code,
            "forecast_version": FORECAST_VERSION,
            "status": status,
            "horizon_start": inputs.horizon_start,
            "horizon_end": inputs.horizon_end,
            "history_days": inputs.history.len(),
            "calendar_effects": inputs.calendar.len(),
            "inputs_hash": inputs_hash,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    load_forecast(tx, ctx.tenant_id, id).await
}

#[derive(Debug, Clone, Serialize)]
pub struct ExplanationOutcome {
    pub mode: &'static str,
    pub artifact_id: Option<Uuid>,
    pub explanation: Option<CapacityExplanationV1>,
    pub synthetic: Option<bool>,
    pub reused: bool,
    pub reason: Option<String>,
}

fn skipped(reason: &str) -> ExplanationOutcome {
    ExplanationOutcome {
        mode: "deterministic",
        artifact_id: None,
        explanation: None,
        synthetic: None,
        reused: false,
        reason: Some(reason.to_string()),
    }
}

/// Governed `capacity-explanation.v1` for one persisted forecast. Returns
/// the stored explanation when one already exists for the same forecast
/// and language (reuse), otherwise one bounded model call.
pub async fn explain(
    state: &AppState,
    ctx: &AuthContext,
    forecast_id: Uuid,
    language: &str,
) -> Result<ExplanationOutcome, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let f = load_forecast(&mut conn, ctx.tenant_id, forecast_id).await?;
    let facility_name: Option<String> =
        sqlx::query_scalar("SELECT name FROM facilities WHERE id = $1 AND tenant_id = $2")
            .bind(f.facility_id)
            .bind(f.tenant_id)
            .fetch_optional(&mut *conn)
            .await?;
    let service = scheduling::load_service(&mut conn, f.tenant_id, &f.service_code)
        .await
        .ok();
    drop(conn);
    let status = state.gateway.status();
    if !status.state.is_callable() {
        return Ok(skipped(match status.state {
            dmind_gateway::CapabilityState::Disabled => "disabled",
            _ => "unavailable",
        }));
    }
    let es = language.starts_with("es");
    let service_label = service
        .as_ref()
        .map(|s| {
            if es {
                s.name_es.clone()
            } else {
                s.name_en.clone()
            }
        })
        .unwrap_or_else(|| f.service_code.clone());
    let parsed: Forecast = serde_json::from_value(f.output.clone()).map_err(ApiError::internal)?;
    let req = CapacityExplanationRequest {
        template: CAPACITY_EXPLANATION_TEMPLATE.to_string(),
        language: language.to_string(),
        scope_label: format!(
            "{service_label} · {}",
            facility_name.unwrap_or_else(|| "facility".into())
        ),
        forecast: parsed,
    };
    let hash = hash_json(&req);
    let scope = aigov::ReuseScope::CapacityExplanation {
        capacity_forecast_id: f.id,
    };
    audit::record(
        &state.pool,
        ctx,
        "ai.artifact.requested",
        Some("capacity_forecast"),
        Some(f.id.to_string()),
        "allow",
        Some(CAPACITY_EXPLANATION_TEMPLATE),
    )
    .await
    .map_err(ApiError::internal)?;
    let plan = match aigov::plan_scoped(
        state,
        f.tenant_id,
        None,
        scope,
        &hash,
        CAPACITY_EXPLANATION_SCHEMA,
    )
    .await
    {
        Ok(p) => p,
        Err(err) => {
            let reason = match err.code {
                "ai_quota_exceeded" => "quota_exceeded",
                "ai_disabled" => "disabled",
                _ => "unavailable",
            };
            audit::record(
                &state.pool,
                ctx,
                "ai.generation.skipped",
                Some("capacity_forecast"),
                Some(f.id.to_string()),
                "deny",
                Some(reason),
            )
            .await
            .map_err(ApiError::internal)?;
            return Ok(skipped(reason));
        }
    };
    let synthetic = plan.model_synthetic();
    let (output, provider, prompt_version, usage, reused_from, execution_id): (
        CapacityExplanationV1,
        ProviderInfo,
        String,
        Option<dmind_gateway::Usage>,
        Option<Uuid>,
        Option<Uuid>,
    ) = match plan {
        aigov::ExecutionPlan::Reuse(prior) => (
            prior.output_as()?,
            ProviderInfo {
                provider: prior.route.clone(),
                model: prior.model.clone(),
                model_version: prior.model_version.clone(),
            },
            prior.prompt_version.clone(),
            prior.usage_as(),
            Some(prior.id),
            None,
        ),
        aigov::ExecutionPlan::Execute { execution_id, .. } => {
            match state.gateway.explain_capacity(&req).await {
                Ok(resp) => (
                    resp.output,
                    resp.provider,
                    resp.prompt_version,
                    resp.usage,
                    None,
                    Some(execution_id),
                ),
                Err(err) => {
                    let code = match err {
                        GatewayError::Disabled(_) => "disabled",
                        GatewayError::InvalidOutput(_) => "invalid_output",
                        GatewayError::PolicyDenied(_) => "policy_denied",
                        GatewayError::Unavailable(_) => "unavailable",
                    };
                    audit::record(
                        &state.pool,
                        ctx,
                        "ai.generation.failed",
                        Some("capacity_forecast"),
                        Some(f.id.to_string()),
                        "deny",
                        Some(code),
                    )
                    .await
                    .map_err(ApiError::internal)?;
                    return Ok(skipped(code));
                }
            }
        }
    };
    // Server-side re-validation against the forecast the model was given.
    if let Err(err) = output.validate(&req.dates(), &req.fact_refs()) {
        audit::record(
            &state.pool,
            ctx,
            "ai.generation.failed",
            Some("capacity_forecast"),
            Some(f.id.to_string()),
            "deny",
            Some(&format!("invalid_output: {err}")),
        )
        .await
        .map_err(ApiError::internal)?;
        return Ok(skipped("invalid_output"));
    }
    let mut tx = state.pool.begin().await?;
    let artifact_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, capacity_forecast_id, artifact_type, autonomy_level, status,
          model, model_version, route, template, input_hash, output, output_schema,
          citations, limitations, generated_at)
         VALUES ($1,$2,NULL,$3,'capacity_explanation','A1',$4,$5,$6,$7,$8,$9,$10,$11,$12,$13, now())",
    )
    .bind(artifact_id)
    .bind(f.tenant_id)
    .bind(f.id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .bind(&provider.model)
    .bind(&provider.model_version)
    .bind(&provider.provider)
    .bind(CAPACITY_EXPLANATION_TEMPLATE)
    .bind(&hash)
    .bind(serde_json::to_value(&output).map_err(ApiError::internal)?)
    .bind(CAPACITY_EXPLANATION_SCHEMA)
    .bind(serde_json::to_value(&output.cited_sources).map_err(ApiError::internal)?)
    .bind(serde_json::to_value(&output.limitations).map_err(ApiError::internal)?)
    .execute(&mut *tx)
    .await?;
    let input_refs = req.fact_refs();
    aigov::annotate(
        &mut tx,
        artifact_id,
        &aigov::Provenance {
            scope,
            provider: &provider,
            prompt_version: &prompt_version,
            input_refs: &input_refs,
            usage: usage.as_ref(),
            synthetic,
            reused_from,
        },
    )
    .await?;
    if let Some(execution_id) = execution_id {
        aigov::bind_execution(&mut tx, execution_id, artifact_id).await?;
    }
    sqlx::query(
        "UPDATE capacity_forecasts SET explanation_artifact_id = $2 WHERE id = $1 AND tenant_id = $3",
    )
    .bind(f.id)
    .bind(artifact_id)
    .bind(f.tenant_id)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        ctx,
        "ai.artifact.generated",
        &state.cell,
        json!({
            "artifact_id": artifact_id,
            "capacity_forecast_id": f.id,
            "template": CAPACITY_EXPLANATION_TEMPLATE,
            "prompt_version": prompt_version,
            "model": provider.model,
            "input_hash": hash,
            "reused_from": reused_from,
            "synthetic": synthetic,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(ExplanationOutcome {
        mode: "dmind",
        artifact_id: Some(artifact_id),
        explanation: Some(output),
        synthetic: Some(synthetic),
        reused: reused_from.is_some(),
        reason: None,
    })
}
