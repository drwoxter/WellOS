//! Command-center snapshot: current operational state assembled from the
//! existing authorities (visits, alerts, transport) and the care-operations
//! tables. Every number is a live aggregate query; nothing is cached or
//! invented, and the response carries its generation time for freshness.

use super::{forecast, theatre, workforce};
use crate::error::ApiError;
use chrono::{DateTime, Duration, Utc};
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;

/// Snapshot for one facility.
pub async fn snapshot(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_id: Uuid,
    now: DateTime<Utc>,
) -> Result<Value, ApiError> {
    let ed = sqlx::query(
        "SELECT COALESCE(priority, 'unassigned') AS priority, status, count(*)::int AS n,
                COALESCE(max(EXTRACT(EPOCH FROM ($3 - arrived_at)) / 60), 0)::float8 AS max_wait_min,
                COALESCE(avg(EXTRACT(EPOCH FROM ($3 - arrived_at)) / 60), 0)::float8 AS avg_wait_min
         FROM visits WHERE tenant_id = $1 AND facility_id = $2
           AND status IN ('arrived','triage_in_progress','ready_for_consultation','in_consultation')
         GROUP BY 1, 2 ORDER BY 1, 2",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .bind(now)
    .fetch_all(&mut *conn)
    .await?;
    let mut awaiting_triage = 0;
    let mut awaiting_clinician = 0;
    let mut in_consultation = 0;
    let mut by_priority: Vec<Value> = Vec::new();
    for r in &ed {
        let status: String = r.get("status");
        let n: i32 = r.get("n");
        match status.as_str() {
            "arrived" | "triage_in_progress" => awaiting_triage += n,
            "ready_for_consultation" => awaiting_clinician += n,
            _ => in_consultation += n,
        }
        by_priority.push(json!({
            "priority": r.get::<String, _>("priority"),
            "status": status,
            "count": n,
            "max_wait_minutes": (r.get::<f64, _>("max_wait_min")).round(),
            "avg_wait_minutes": (r.get::<f64, _>("avg_wait_min")).round(),
        }));
    }

    let boarding = sqlx::query(
        "SELECT count(*)::int AS n,
                COALESCE(max(EXTRACT(EPOCH FROM ($3 - r.created_at)) / 60), 0)::float8 AS max_wait_min
         FROM placement_requests r
         WHERE r.tenant_id = $1 AND r.facility_id = $2 AND r.status IN ('requested','candidates_ready','held')",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;

    let capacity = sqlx::query(
        "SELECT u.id AS unit_id, u.code, u.name_en, u.name_es, u.type_code,
                count(p.id) FILTER (WHERE p.active AND p.operational_status = 'open')::int AS total,
                count(p.id) FILTER (WHERE p.position_state = 'available' AND p.active AND p.operational_status = 'open')::int AS available,
                count(p.id) FILTER (WHERE p.position_state = 'occupied')::int AS occupied,
                count(p.id) FILTER (WHERE p.position_state = 'held')::int AS held,
                count(p.id) FILTER (WHERE p.position_state = 'cleaning')::int AS cleaning,
                count(p.id) FILTER (WHERE p.position_state IN ('blocked','out_of_service') OR p.operational_status <> 'open' OR NOT p.active)::int AS constrained
         FROM operational_locations u
         LEFT JOIN operational_locations r ON r.parent_id = u.id AND r.level = 'room'
         LEFT JOIN operational_locations p ON p.level = 'position' AND (p.parent_id = r.id OR p.parent_id = u.id)
         WHERE u.tenant_id = $1 AND u.facility_id = $2 AND u.level = 'unit' AND u.active
         GROUP BY u.id ORDER BY u.code",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .fetch_all(&mut *conn)
    .await?;
    let units: Vec<Value> = capacity
        .iter()
        .map(|r| {
            json!({
                "unit_id": r.get::<Uuid, _>("unit_id"),
                "code": r.get::<String, _>("code"),
                "name_en": r.get::<String, _>("name_en"),
                "name_es": r.get::<String, _>("name_es"),
                "type_code": r.get::<String, _>("type_code"),
                "total": r.get::<i32, _>("total"),
                "available": r.get::<i32, _>("available"),
                "occupied": r.get::<i32, _>("occupied"),
                "held": r.get::<i32, _>("held"),
                "cleaning": r.get::<i32, _>("cleaning"),
                "constrained": r.get::<i32, _>("constrained"),
            })
        })
        .collect();
    let total: i32 = capacity.iter().map(|r| r.get::<i32, _>("total")).sum();
    let occupied: i32 = capacity.iter().map(|r| r.get::<i32, _>("occupied")).sum();
    let available: i32 = capacity.iter().map(|r| r.get::<i32, _>("available")).sum();

    let episodes = sqlx::query(
        "SELECT status, count(*)::int AS n FROM care_episodes WHERE tenant_id = $1 AND facility_id = $2
           AND status NOT IN ('completed','cancelled','error') GROUP BY 1",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .fetch_all(&mut *conn)
    .await?;
    let episode_counts: Value = episodes
        .iter()
        .map(|r| (r.get::<String, _>("status"), json!(r.get::<i32, _>("n"))))
        .collect::<serde_json::Map<_, _>>()
        .into();

    let barriers = sqlx::query(
        "SELECT barrier_category_code AS barrier_category, count(*)::int AS n FROM care_coordination_tasks
         WHERE tenant_id = $1 AND facility_id = $2 AND barrier_category_code IS NOT NULL
           AND status NOT IN ('completed','cancelled') GROUP BY 1 ORDER BY 2 DESC",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .fetch_all(&mut *conn)
    .await?;
    let discharge_barriers: Vec<Value> = barriers
        .iter()
        .map(|r| json!({"category": r.get::<String, _>("barrier_category"), "count": r.get::<i32, _>("n")}))
        .collect();
    let overdue_tasks: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM care_coordination_tasks WHERE tenant_id = $1 AND facility_id = $2
           AND status NOT IN ('completed','cancelled') AND due_at IS NOT NULL AND due_at < $3",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;

    let transport = sqlx::query(
        "SELECT t.status, count(*)::int AS n,
                count(*) FILTER (WHERE t.status IN ('requested','scheduled','en_route') AND t.pickup_window_end < $3)::int AS delayed
         FROM transport_requests t JOIN appointments a ON a.id = t.appointment_id
         WHERE t.tenant_id = $1 AND a.facility_id = $2 AND t.status NOT IN ('completed','cancelled','failed') GROUP BY 1",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .bind(now)
    .fetch_all(&mut *conn)
    .await?;
    let transport_open: i32 = transport.iter().map(|r| r.get::<i32, _>("n")).sum();
    let transport_delayed: i32 = transport.iter().map(|r| r.get::<i32, _>("delayed")).sum();

    let alerts = sqlx::query(
        "SELECT priority, count(*)::int AS n FROM internal_alerts WHERE tenant_id = $1 AND facility_id = $2
           AND status NOT IN ('resolved') GROUP BY 1",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .fetch_all(&mut *conn)
    .await?;
    let unresolved_alerts: Vec<Value> = alerts
        .iter()
        .map(|r| json!({"priority": r.get::<String, _>("priority"), "count": r.get::<i32, _>("n")}))
        .collect();

    let esc = sqlx::query(
        "SELECT kind, status, count(*)::int AS n, min(due_at) AS next_due FROM care_escalations
         WHERE tenant_id = $1 AND facility_id = $2 AND status IN ('armed','escalated','acknowledged') GROUP BY 1, 2",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .fetch_all(&mut *conn)
    .await?;
    let escalations: Vec<Value> = esc
        .iter()
        .map(|r| {
            json!({
                "kind": r.get::<String, _>("kind"),
                "status": r.get::<String, _>("status"),
                "count": r.get::<i32, _>("n"),
                "next_due_at": r.get::<Option<DateTime<Utc>>, _>("next_due"),
            })
        })
        .collect();

    let day_end = now + Duration::hours(24);
    let cases = theatre::list_cases(conn, tenant_id, Some(&[facility_id]), None, true, 200).await?;
    let mut theatre_today = 0;
    let mut theatre_ready = 0;
    let mut theatre_conflicts = 0;
    let mut theatre_missing_prereqs = 0;
    for c in cases.iter().filter(|c| {
        c.scheduled_start
            .map(|s| s >= now - Duration::hours(12) && s < day_end)
            .unwrap_or(false)
    }) {
        theatre_today += 1;
        if c.status == "ready" || c.status == "in_theatre" {
            theatre_ready += 1;
        }
        if c.prerequisites_typed()
            .iter()
            .any(|p| p.status != "met" && p.status != "waived")
        {
            theatre_missing_prereqs += 1;
        }
        if let Some(check) = theatre::readiness(conn, c, None, None).await? {
            if check.conflicts.iter().any(|x| x.blocking) {
                theatre_conflicts += 1;
            }
        }
    }

    let shifts = workforce::list_shifts(
        conn,
        tenant_id,
        Some(&[facility_id]),
        now - Duration::hours(1),
        day_end,
    )
    .await?;
    let mut staffing_gaps: Vec<Value> = Vec::new();
    for s in &shifts {
        let cov = workforce::coverage(conn, tenant_id, s.id).await?;
        if !cov.report.covered {
            staffing_gaps.push(json!({
                "shift_id": s.id,
                "label": s.label,
                "unit_location_id": s.unit_location_id,
                "starts_at": s.starts_at,
                "ends_at": s.ends_at,
                "gaps": cov.report.gaps,
                "reallocation_candidates": cov.reallocations,
            }));
        }
    }

    let forecasts: Vec<Value> = forecast::latest_per_horizon(conn, tenant_id, facility_id)
        .await?
        .iter()
        .map(|f| forecast::forecast_json(f, false))
        .collect();

    Ok(json!({
        "facility_id": facility_id,
        "generated_at": now,
        "emergency": {
            "awaiting_triage": awaiting_triage,
            "awaiting_clinician": awaiting_clinician,
            "in_consultation": in_consultation,
            "by_priority": by_priority,
        },
        "placement_queue": {
            "pending": boarding.get::<i32, _>("n"),
            "max_wait_minutes": boarding.get::<f64, _>("max_wait_min").round(),
        },
        "capacity": {
            "total": total,
            "occupied": occupied,
            "available": available,
            "occupancy_ratio": if total > 0 { occupied as f64 / total as f64 } else { 0.0 },
            "units": units,
        },
        "episodes": episode_counts,
        "discharge": { "barriers": discharge_barriers, "overdue_tasks": overdue_tasks },
        "transport": { "open": transport_open, "delayed": transport_delayed },
        "alerts": unresolved_alerts,
        "escalations": escalations,
        "theatre": {
            "cases_next_24h": theatre_today,
            "ready": theatre_ready,
            "blocking_conflicts": theatre_conflicts,
            "missing_prerequisites": theatre_missing_prereqs,
        },
        "workforce": { "shifts_next_24h": shifts.len(), "gaps": staffing_gaps },
        "forecasts": forecasts,
    }))
}
