//! Patient access, arrival, triage, care-team assignment, internal alerts and
//! the consultation handoff.
//!
//! A *visit* is the operational episode from appointment/registration to the
//! end of the consultation. Its status follows the explicit state machine in
//! `wellos_domain::triage`; every transition here is performed under the
//! visit row lock, bound to the caller's expected `version`, and audited.
//! dMind may only *propose* an operational priority and destination: the
//! deterministic safety floor is computed server-side, a proposal can never
//! lower it, and a nurse or physician must accept, override or reject it.

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, facility_scope, ResourceCtx};
use crate::ratelimit;
use crate::routes::encounter_docs::{validate_vitals, RecordVitals};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use chrono::{DateTime, Datelike, Utc};
use dmind_gateway::triage::TRIAGE_TEMPLATE;
use dmind_gateway::{GatewayError, TriageRequest};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row, Transaction};
use uuid::Uuid;
use wellos_domain::ai::ArtifactStatus;
use wellos_domain::triage::{
    is_concern, is_red_flag, is_service, safety_floor, ArrivalKind, Priority, TriageVitals,
    VisitStatus, VisitTransition, SAFETY_RULES_VERSION,
};

const MAX_TEXT: usize = 2000;
const MAX_REASON: usize = 500;
const MAX_LIST: usize = 20;

fn char_len(s: &str) -> usize {
    s.chars().count()
}

fn clean_text(value: Option<String>, field: &str, max: usize) -> Result<Option<String>, ApiError> {
    let Some(v) = value else { return Ok(None) };
    let trimmed = v.trim();
    if trimmed.is_empty() {
        return Ok(None);
    }
    if char_len(trimmed) > max {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("{field} exceeds {max} characters"),
        ));
    }
    Ok(Some(trimmed.to_string()))
}

fn clean_list(
    values: Vec<String>,
    field: &str,
    known: fn(&str) -> bool,
) -> Result<Vec<String>, ApiError> {
    if values.len() > MAX_LIST {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("{field} accepts at most {MAX_LIST} entries"),
        ));
    }
    let mut out: Vec<String> = Vec::new();
    for v in values {
        let v = v.trim().to_string();
        if !known(&v) {
            return Err(ApiError::bad_request(
                "validation_failed",
                format!("{field} contains an unknown value"),
            ));
        }
        if !out.contains(&v) {
            out.push(v);
        }
    }
    Ok(out)
}

fn parse_priority(s: &str) -> Result<Priority, ApiError> {
    Priority::parse(s).ok_or_else(|| {
        ApiError::bad_request(
            "validation_failed",
            "priority must be one of non_urgent, standard, urgent, immediate",
        )
    })
}

fn require_service(s: &str) -> Result<String, ApiError> {
    let s = s.trim();
    if !is_service(s) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "service must be one of general_medicine, emergency, nursing, telehealth",
        ));
    }
    Ok(s.to_string())
}

// ---------------------------------------------------------------------------
// Visit row helpers
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct VisitRow {
    id: Uuid,
    tenant_id: Uuid,
    facility_id: Uuid,
    patient_id: Uuid,
    status: VisitStatus,
    arrival_kind: ArrivalKind,
    service: String,
    priority: Option<Priority>,
    encounter_id: Option<Uuid>,
    version: i64,
}

fn visit_from_row(r: &sqlx::postgres::PgRow) -> Result<VisitRow, ApiError> {
    let status = VisitStatus::parse(r.get::<String, _>("status").as_str())
        .ok_or_else(|| ApiError::internal("invalid visit status"))?;
    let arrival_kind = ArrivalKind::parse(r.get::<String, _>("arrival_kind").as_str())
        .ok_or_else(|| ApiError::internal("invalid arrival kind"))?;
    let priority = r
        .get::<Option<String>, _>("priority")
        .as_deref()
        .and_then(Priority::parse);
    Ok(VisitRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        facility_id: r.get("facility_id"),
        patient_id: r.get("patient_id"),
        status,
        arrival_kind,
        service: r.get("service"),
        priority,
        encounter_id: r.get("encounter_id"),
        version: r.get("version"),
    })
}

const VISIT_COLUMNS: &str = "id, tenant_id, facility_id, patient_id, status, arrival_kind, service,
                             priority, encounter_id, version";

async fn load_visit(state: &AppState, id: Uuid) -> Result<VisitRow, ApiError> {
    let row = sqlx::query(&format!("SELECT {VISIT_COLUMNS} FROM visits WHERE id = $1"))
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(ApiError::not_found)?;
    visit_from_row(&row)
}

async fn lock_visit(conn: &mut PgConnection, id: Uuid) -> Result<VisitRow, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {VISIT_COLUMNS} FROM visits WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    visit_from_row(&row)
}

fn resource_ctx(v: &VisitRow) -> ResourceCtx {
    ResourceCtx {
        tenant_id: v.tenant_id,
        patient_id: Some(v.patient_id),
        facility_id: Some(v.facility_id),
    }
}

/// Optimistic concurrency: the caller acts on the version it displayed.
fn require_version(v: &VisitRow, expected: i64) -> Result<(), ApiError> {
    if v.version != expected {
        return Err(ApiError::conflict(
            "stale_version",
            "this visit changed since it was loaded; refresh and try again",
        ));
    }
    Ok(())
}

fn invalid_transition(v: &VisitRow, t: VisitTransition) -> ApiError {
    ApiError::conflict(
        "invalid_visit_transition",
        format!(
            "a {} visit cannot {}",
            v.status.as_str(),
            match t {
                VisitTransition::Arrive => "be marked arrived",
                VisitTransition::StartTriage => "start triage",
                VisitTransition::CompleteTriage => "complete triage",
                VisitTransition::StartConsultation => "start a consultation",
                VisitTransition::ReleaseConsultation => "release the consultation",
                VisitTransition::CompleteConsultation => "be completed",
                VisitTransition::Cancel => "be cancelled",
                VisitTransition::MarkNoShow => "be marked no-show",
            }
        ),
    )
}

/// Apply a state-machine transition to a locked visit row: the domain decides
/// legality, SQL records the timestamp for the new state and bumps `version`.
async fn transition(
    tx: &mut PgConnection,
    v: &VisitRow,
    t: VisitTransition,
) -> Result<VisitStatus, ApiError> {
    let next = v.status.apply(t).map_err(|_| invalid_transition(v, t))?;
    let stamp = match next {
        VisitStatus::Arrived => "arrived_at = now(),",
        VisitStatus::TriageInProgress => "triage_started_at = COALESCE(triage_started_at, now()),",
        VisitStatus::ReadyForConsultation => "ready_at = now(),",
        VisitStatus::InConsultation => "consultation_started_at = now(),",
        VisitStatus::Completed => "completed_at = now(),",
        VisitStatus::Cancelled | VisitStatus::NoShow => "closed_at = now(),",
        VisitStatus::Scheduled => "",
    };
    let updated = sqlx::query(&format!(
        "UPDATE visits SET status = $1, {stamp} version = version + 1, updated_at = now()
         WHERE id = $2 AND version = $3"
    ))
    .bind(next.as_str())
    .bind(v.id)
    .bind(v.version)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(ApiError::conflict(
            "stale_version",
            "this visit changed since it was loaded; refresh and try again",
        ));
    }
    Ok(next)
}

// ---------------------------------------------------------------------------
// Care-team assignment and internal alerts (shared helpers)
// ---------------------------------------------------------------------------

/// The visit's current routing target: an individual or an explicit queue.
struct Target {
    user_id: Option<Uuid>,
    queue_id: Option<Uuid>,
}

async fn current_target(conn: &mut PgConnection, v: &VisitRow) -> Result<Option<Target>, ApiError> {
    let row = sqlx::query(
        "SELECT assignee_user_id, queue_id FROM care_team_assignments
         WHERE tenant_id = $1 AND visit_id = $2 AND active
           AND function IN ('treating_professional', 'destination_queue')
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(v.tenant_id)
    .bind(v.id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| Target {
        user_id: r.get("assignee_user_id"),
        queue_id: r.get("queue_id"),
    }))
}

async fn queue_for_service(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_id: Uuid,
    service: &str,
) -> Result<Uuid, ApiError> {
    // Every facility has a general_medicine queue; a service without its own
    // queue falls back to it so no patient is routed nowhere.
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM service_queues WHERE tenant_id = $1 AND facility_id = $2 AND code = $3
         UNION ALL
         SELECT id FROM service_queues WHERE tenant_id = $1 AND facility_id = $2
           AND code = 'general_medicine'
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(facility_id)
    .bind(service)
    .fetch_optional(&mut *conn)
    .await?;
    id.ok_or_else(|| {
        ApiError::conflict(
            "no_service_queue",
            "this facility has no service queue configured",
        )
    })
}

/// Replace the visit's routing target. Individuals are recorded as the
/// treating professional; queues as the destination. Previous routing rows
/// are closed, never deleted, so the assignment history stays auditable.
async fn route_visit(
    tx: &mut PgConnection,
    ctx: &AuthContext,
    v: &VisitRow,
    target: &Target,
    source: &str,
) -> Result<Uuid, ApiError> {
    sqlx::query(
        "UPDATE care_team_assignments SET active = false, ends_at = now(), updated_at = now()
         WHERE tenant_id = $1 AND visit_id = $2 AND active
           AND function IN ('treating_professional', 'destination_queue')",
    )
    .bind(v.tenant_id)
    .bind(v.id)
    .execute(&mut *tx)
    .await?;
    let id = Uuid::now_v7();
    let function = if target.user_id.is_some() {
        "treating_professional"
    } else {
        "destination_queue"
    };
    sqlx::query(
        "INSERT INTO care_team_assignments
         (id, tenant_id, facility_id, patient_id, visit_id, assignee_user_id, queue_id,
          function, source, assigned_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(id)
    .bind(v.tenant_id)
    .bind(v.facility_id)
    .bind(v.patient_id)
    .bind(v.id)
    .bind(target.user_id)
    .bind(target.queue_id)
    .bind(function)
    .bind(source)
    .bind(ctx.user_id)
    .execute(&mut *tx)
    .await?;
    Ok(id)
}

/// Record that the caller performed a care-team function for this patient
/// (e.g. the triage nurse). Idempotent per visit/function/user.
async fn record_member(
    tx: &mut PgConnection,
    ctx: &AuthContext,
    v: &VisitRow,
    function: &str,
    source: &str,
) -> Result<(), ApiError> {
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM care_team_assignments
         WHERE tenant_id = $1 AND visit_id = $2 AND assignee_user_id = $3 AND function = $4 AND active",
    )
    .bind(v.tenant_id)
    .bind(v.id)
    .bind(ctx.user_id)
    .bind(function)
    .fetch_optional(&mut *tx)
    .await?;
    if existing.is_some() {
        return Ok(());
    }
    sqlx::query(
        "INSERT INTO care_team_assignments
         (id, tenant_id, facility_id, patient_id, visit_id, assignee_user_id, function, source, assigned_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$6)",
    )
    .bind(Uuid::now_v7())
    .bind(v.tenant_id)
    .bind(v.facility_id)
    .bind(v.patient_id)
    .bind(v.id)
    .bind(ctx.user_id)
    .bind(function)
    .bind(source)
    .execute(&mut *tx)
    .await?;
    Ok(())
}

/// Close every open alert for the visit (the work item has moved on).
async fn resolve_alerts(
    tx: &mut PgConnection,
    ctx: &AuthContext,
    state: &AppState,
    v: &VisitRow,
) -> Result<(), ApiError> {
    let resolved = sqlx::query(
        "UPDATE internal_alerts SET status = 'resolved', resolved_at = now()
         WHERE tenant_id = $1 AND visit_id = $2 AND status <> 'resolved'",
    )
    .bind(v.tenant_id)
    .bind(v.id)
    .execute(&mut *tx)
    .await?;
    if resolved.rows_affected() > 0 {
        audit::emit(
            &mut *tx,
            ctx,
            "internal_alert.resolved",
            &state.cell,
            json!({ "visit_id": v.id, "count": resolved.rows_affected() }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
    }
    Ok(())
}

/// Create one directed internal alert for the visit's current target. Alerts
/// never leave the system: no SMS, email or push. Replaces open alerts of the
/// same kind so a re-route does not leave a stale item behind.
async fn raise_alert(
    tx: &mut PgConnection,
    ctx: &AuthContext,
    state: &AppState,
    v: &VisitRow,
    kind: &str,
    priority: Priority,
    target: &Target,
) -> Result<Uuid, ApiError> {
    sqlx::query(
        "UPDATE internal_alerts SET status = 'resolved', resolved_at = now()
         WHERE tenant_id = $1 AND visit_id = $2 AND kind = $3 AND status <> 'resolved'",
    )
    .bind(v.tenant_id)
    .bind(v.id)
    .bind(kind)
    .execute(&mut *tx)
    .await?;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO internal_alerts
         (id, tenant_id, facility_id, patient_id, visit_id, kind, priority, target_user_id,
          target_queue_id, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(id)
    .bind(v.tenant_id)
    .bind(v.facility_id)
    .bind(v.patient_id)
    .bind(v.id)
    .bind(kind)
    .bind(priority.as_str())
    .bind(target.user_id)
    .bind(target.queue_id)
    .bind(ctx.user_id)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        ctx,
        "internal_alert.created",
        &state.cell,
        json!({
            "alert_id": id,
            "visit_id": v.id,
            "kind": kind,
            "target_user_id": target.user_id,
            "target_queue_id": target.queue_id,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(id)
}

/// Whether `user_id` may be the treating professional at this facility: an
/// explicit role assignment granting consultation rights that covers the
/// facility (tenant-wide allowlisted roles included).
async fn user_can_consult_at(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    user_id: Uuid,
    facility_id: Uuid,
) -> Result<bool, ApiError> {
    let rows = sqlx::query(
        "SELECT ra.role, ra.facility_id FROM role_assignments ra
         JOIN users u ON u.id = ra.user_id
         WHERE ra.tenant_id = $1 AND ra.user_id = $2 AND u.tenant_id = $1 AND NOT u.is_service",
    )
    .bind(tenant_id)
    .bind(user_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows.iter().any(|r| {
        let role: String = r.get("role");
        let fac: Option<Uuid> = r.get("facility_id");
        crate::policy::role_allows(&role, actions::ENCOUNTER_START)
            && match fac {
                Some(f) => f == facility_id,
                None => crate::policy::null_facility_is_tenant_wide(&role),
            }
    }))
}

async fn queue_in_facility(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_id: Uuid,
    queue_id: Uuid,
) -> Result<bool, ApiError> {
    let found: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM service_queues WHERE id = $1 AND tenant_id = $2 AND facility_id = $3",
    )
    .bind(queue_id)
    .bind(tenant_id)
    .bind(facility_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(found.is_some())
}

/// Resolve and validate a requested routing target (exactly one of user or
/// queue), or fall back to the service queue when neither is given.
async fn resolve_target(
    tx: &mut PgConnection,
    v: &VisitRow,
    service: &str,
    assignee_user_id: Option<Uuid>,
    queue_id: Option<Uuid>,
) -> Result<Target, ApiError> {
    match (assignee_user_id, queue_id) {
        (Some(_), Some(_)) => Err(ApiError::bad_request(
            "validation_failed",
            "assign either a professional or a queue, not both",
        )),
        (Some(user_id), None) => {
            if !user_can_consult_at(tx, v.tenant_id, user_id, v.facility_id).await? {
                return Err(ApiError::bad_request(
                    "assignee_not_eligible",
                    "the selected professional cannot consult at this facility",
                ));
            }
            Ok(Target {
                user_id: Some(user_id),
                queue_id: None,
            })
        }
        (None, Some(queue_id)) => {
            if !queue_in_facility(tx, v.tenant_id, v.facility_id, queue_id).await? {
                return Err(ApiError::bad_request(
                    "queue_not_eligible",
                    "the selected queue does not belong to this facility",
                ));
            }
            Ok(Target {
                user_id: None,
                queue_id: Some(queue_id),
            })
        }
        (None, None) => Ok(Target {
            user_id: None,
            queue_id: Some(queue_for_service(tx, v.tenant_id, v.facility_id, service).await?),
        }),
    }
}

// ---------------------------------------------------------------------------
// POST /api/v1/visits — scheduled visit or arrival without appointment
// ---------------------------------------------------------------------------

/// Appointment bounds. Together with the per-principal `VisitCreate` rate
/// limit they keep a compromised registration account from filling the
/// fixed-size worklists: an appointment must fall inside a bounded window
/// around now, and one patient can only hold a handful of pending ones.
const SCHEDULE_PAST_GRACE: chrono::Duration = chrono::Duration::hours(1);
const SCHEDULE_HORIZON: chrono::Duration = chrono::Duration::days(365);
const MAX_PENDING_APPOINTMENTS_PER_PATIENT: i64 = 5;

#[derive(Deserialize)]
pub struct CreateVisit {
    pub patient_id: Uuid,
    /// scheduled | walk_in | urgent | remote
    pub arrival_kind: String,
    pub service: String,
    pub reason: Option<String>,
    /// Required for scheduled visits; ignored otherwise.
    pub scheduled_at: Option<DateTime<Utc>>,
}

pub async fn create(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<CreateVisit>,
) -> Result<Json<Value>, ApiError> {
    let arrival_kind = ArrivalKind::parse(body.arrival_kind.trim()).ok_or_else(|| {
        ApiError::bad_request(
            "validation_failed",
            "arrival_kind must be one of scheduled, walk_in, urgent, remote",
        )
    })?;
    let service = require_service(&body.service)?;
    let reason = clean_text(body.reason, "reason", MAX_REASON)?;
    let scheduled_at = match arrival_kind {
        ArrivalKind::Scheduled => {
            let at = body.scheduled_at.ok_or_else(|| {
                ApiError::bad_request(
                    "validation_failed",
                    "scheduled_at is required for a scheduled visit",
                )
            })?;
            let now = Utc::now();
            if at < now - SCHEDULE_PAST_GRACE || at > now + SCHEDULE_HORIZON {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    "scheduled_at must be within the next 365 days (register an elapsed appointment as an arrival)",
                ));
            }
            Some(at)
        }
        _ => None,
    };
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::VisitCreate).await?;

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
        actions::VISIT_MANAGE,
        "visit",
        Some(ResourceCtx {
            tenant_id,
            patient_id: Some(body.patient_id),
            facility_id: Some(facility_id),
        }),
    )
    .await?;

    let id = Uuid::now_v7();
    let status = match arrival_kind {
        ArrivalKind::Scheduled => VisitStatus::Scheduled,
        _ => VisitStatus::Arrived,
    };
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    // The patient lock serializes concurrent arrivals; the partial unique
    // index is the invariant, the pre-check gives a clear message.
    sqlx::query("SELECT id FROM patients WHERE id = $1 FOR UPDATE")
        .bind(body.patient_id)
        .execute(&mut *tx)
        .await?;
    if status == VisitStatus::Arrived {
        let open: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM visits WHERE tenant_id = $1 AND patient_id = $2
               AND status IN ('arrived','triage_in_progress','ready_for_consultation','in_consultation')",
        )
        .bind(tenant_id)
        .bind(body.patient_id)
        .fetch_optional(&mut *tx)
        .await?;
        if open.is_some() {
            return Err(ApiError::conflict(
                "patient_already_present",
                "this patient already has an open visit today",
            ));
        }
    } else {
        let pending: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM visits WHERE tenant_id = $1 AND patient_id = $2
               AND status = 'scheduled' AND scheduled_at >= now() - interval '1 day'",
        )
        .bind(tenant_id)
        .bind(body.patient_id)
        .fetch_one(&mut *tx)
        .await?;
        if pending >= MAX_PENDING_APPOINTMENTS_PER_PATIENT {
            return Err(ApiError::conflict(
                "too_many_pending_appointments",
                format!(
                    "this patient already has {MAX_PENDING_APPOINTMENTS_PER_PATIENT} pending appointments; cancel one before scheduling another"
                ),
            ));
        }
    }
    sqlx::query(
        "INSERT INTO visits (id, tenant_id, facility_id, patient_id, status, arrival_kind, service,
                             reason, scheduled_at, arrived_at, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,
                 CASE WHEN $5 = 'arrived' THEN now() ELSE NULL END, $10)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(facility_id)
    .bind(body.patient_id)
    .bind(status.as_str())
    .bind(arrival_kind.as_str())
    .bind(&service)
    .bind(&reason)
    .bind(scheduled_at)
    .bind(ctx.user_id)
    .execute(&mut *tx)
    .await?;
    let v = lock_visit(&mut tx, id).await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "visit.created",
        &state.cell,
        json!({
            "visit_id": id,
            "patient_id": body.patient_id,
            "arrival_kind": arrival_kind.as_str(),
            "service": service,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    if status == VisitStatus::Arrived {
        on_arrival(&mut tx, &ctx, &state, &v).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({
        "id": id,
        "status": status.as_str(),
        "arrival_kind": arrival_kind.as_str(),
        "version": v.version,
    })))
}

/// Arrival routes the patient to the requested service queue and, for an
/// urgent arrival, raises an internal alert to that queue. Registration alone
/// never notifies individual professionals.
async fn on_arrival(
    tx: &mut PgConnection,
    ctx: &AuthContext,
    state: &AppState,
    v: &VisitRow,
) -> Result<(), ApiError> {
    let queue_service = match v.arrival_kind {
        ArrivalKind::Urgent => "emergency",
        _ => v.service.as_str(),
    };
    let queue = queue_for_service(tx, v.tenant_id, v.facility_id, queue_service).await?;
    let target = Target {
        user_id: None,
        queue_id: Some(queue),
    };
    route_visit(tx, ctx, v, &target, "registration").await?;
    audit::emit(
        &mut *tx,
        ctx,
        "visit.arrived",
        &state.cell,
        json!({ "visit_id": v.id, "arrival_kind": v.arrival_kind.as_str() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    if v.arrival_kind == ArrivalKind::Urgent {
        raise_alert(
            tx,
            ctx,
            state,
            v,
            "urgent_arrival",
            Priority::Urgent,
            &target,
        )
        .await?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Arrive / cancel / no-show
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct VersionedAction {
    pub version: i64,
    pub reason: Option<String>,
}

async fn manage_transition(
    state: &AppState,
    ctx: &AuthContext,
    id: Uuid,
    body: VersionedAction,
    t: VisitTransition,
    event: &str,
) -> Result<Json<Value>, ApiError> {
    let reason = clean_text(body.reason, "reason", MAX_REASON)?;
    let v = load_visit(state, id).await?;
    let allowed = guard(
        state,
        ctx,
        actions::VISIT_MANAGE,
        "visit",
        Some(resource_ctx(&v)),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    if t == VisitTransition::Arrive {
        // Same lock order as `create` (patient, then visit): one open visit
        // per patient is the invariant, the pre-check gives a clear message.
        sqlx::query("SELECT id FROM patients WHERE id = $1 FOR UPDATE")
            .bind(v.patient_id)
            .execute(&mut *tx)
            .await?;
        let open: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM visits WHERE tenant_id = $1 AND patient_id = $2 AND id <> $3
               AND status IN ('arrived','triage_in_progress','ready_for_consultation','in_consultation')",
        )
        .bind(v.tenant_id)
        .bind(v.patient_id)
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?;
        if open.is_some() {
            return Err(ApiError::conflict(
                "patient_already_present",
                "this patient already has an open visit today",
            ));
        }
    }
    let v = lock_visit(&mut tx, id).await?;
    require_version(&v, body.version)?;
    allowed.record(&mut tx, ctx, &state.cell).await?;
    let next = transition(&mut tx, &v, t).await?;
    let v = lock_visit(&mut tx, id).await?;
    match t {
        VisitTransition::Arrive => on_arrival(&mut tx, ctx, state, &v).await?,
        VisitTransition::Cancel | VisitTransition::MarkNoShow => {
            sqlx::query("UPDATE visits SET closed_reason = $2 WHERE id = $1")
                .bind(id)
                .bind(&reason)
                .execute(&mut *tx)
                .await?;
            sqlx::query(
                "UPDATE care_team_assignments SET active = false, ends_at = now(), updated_at = now()
                 WHERE tenant_id = $1 AND visit_id = $2 AND active",
            )
            .bind(v.tenant_id)
            .bind(id)
            .execute(&mut *tx)
            .await?;
            resolve_alerts(&mut tx, ctx, state, &v).await?;
            audit::emit(
                &mut *tx,
                ctx,
                event,
                &state.cell,
                json!({ "visit_id": id }),
                None,
            )
            .await
            .map_err(ApiError::internal)?;
        }
        _ => {}
    }
    tx.commit().await?;
    Ok(Json(
        json!({ "id": id, "status": next.as_str(), "version": v.version }),
    ))
}

pub async fn arrive(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<VersionedAction>,
) -> Result<Json<Value>, ApiError> {
    manage_transition(
        &state,
        &ctx,
        id,
        body,
        VisitTransition::Arrive,
        "visit.arrived",
    )
    .await
}

pub async fn cancel(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<VersionedAction>,
) -> Result<Json<Value>, ApiError> {
    manage_transition(
        &state,
        &ctx,
        id,
        body,
        VisitTransition::Cancel,
        "visit.cancelled",
    )
    .await
}

pub async fn no_show(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<VersionedAction>,
) -> Result<Json<Value>, ApiError> {
    manage_transition(
        &state,
        &ctx,
        id,
        body,
        VisitTransition::MarkNoShow,
        "visit.no_show",
    )
    .await
}

// ---------------------------------------------------------------------------
// GET /api/v1/visits — access / triage / ready worklists
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ListQuery {
    /// access (scheduled + arrived + in triage + ready, today) | triage
    /// (arrived + in triage) | ready (ready + in consultation) | all
    pub view: Option<String>,
    pub facility_id: Option<Uuid>,
}

/// Capability hints are display-only; every action re-authorizes server-side.
struct Caps {
    manage: bool,
    triage: bool,
    consult: bool,
    assign: bool,
}

fn caps_for(ctx: &AuthContext, facility_id: Uuid) -> Caps {
    let covers = |action: &str| match facility_scope(ctx, action) {
        None => true,
        Some(ids) => ids.contains(&facility_id),
    };
    Caps {
        manage: covers(actions::VISIT_MANAGE),
        triage: covers(actions::TRIAGE_WRITE),
        consult: covers(actions::ENCOUNTER_START),
        assign: covers(actions::CARE_TEAM_ASSIGN),
    }
}

/// Whether the caller may pick this visit up: the named professional, or
/// anyone eligible when routed to a queue.
fn can_start(
    ctx: &AuthContext,
    caps: &Caps,
    status: VisitStatus,
    target_user: Option<Uuid>,
) -> bool {
    caps.consult
        && matches!(
            status,
            VisitStatus::ReadyForConsultation | VisitStatus::InConsultation
        )
        && target_user.is_none_or(|u| u == ctx.user_id)
}

const VISIT_LIST_SQL: &str = "
    SELECT v.id, v.facility_id, v.status, v.arrival_kind, v.service, v.reason, v.scheduled_at,
           v.arrived_at, v.ready_at, v.consultation_started_at, v.priority, v.handoff_summary,
           v.encounter_id, v.version, v.updated_at,
           p.id AS patient_id, p.family_name, p.given_name, p.identifier, p.birth_date,
           f.name AS facility_name,
           a.assignee_user_id, au.display_name AS assignee_name,
           a.queue_id, q.name AS queue_name, q.code AS queue_code,
           e.practitioner_id AS encounter_practitioner_id,
           (SELECT COUNT(*) FROM alerts pa
             WHERE pa.tenant_id = v.tenant_id AND pa.patient_id = v.patient_id AND pa.status = 'open') AS alert_count,
           (SELECT COUNT(*) FROM allergies al
             WHERE al.tenant_id = v.tenant_id AND al.patient_id = v.patient_id) AS allergy_count,
           (SELECT COUNT(*) FROM internal_alerts ia
             WHERE ia.tenant_id = v.tenant_id AND ia.visit_id = v.id AND ia.status = 'open') AS open_alerts
    FROM visits v
    JOIN patients p ON p.id = v.patient_id
    JOIN facilities f ON f.id = v.facility_id
    LEFT JOIN LATERAL (
        SELECT assignee_user_id, queue_id FROM care_team_assignments c
        WHERE c.tenant_id = v.tenant_id AND c.visit_id = v.id AND c.active
          AND c.function IN ('treating_professional', 'destination_queue')
        ORDER BY c.created_at DESC LIMIT 1
    ) a ON true
    LEFT JOIN users au ON au.id = a.assignee_user_id
    LEFT JOIN service_queues q ON q.id = a.queue_id
    LEFT JOIN encounters e ON e.id = v.encounter_id
";

fn visit_item(ctx: &AuthContext, r: &sqlx::postgres::PgRow) -> Result<Value, ApiError> {
    let facility_id: Uuid = r.get("facility_id");
    let status = VisitStatus::parse(r.get::<String, _>("status").as_str())
        .ok_or_else(|| ApiError::internal("invalid visit status"))?;
    let caps = caps_for(ctx, facility_id);
    let target_user: Option<Uuid> = r.get("assignee_user_id");
    let encounter_id: Option<Uuid> = r.get("encounter_id");
    let encounter_practitioner: Option<Uuid> = r.get("encounter_practitioner_id");
    let arrived_at: Option<DateTime<Utc>> = r.get("arrived_at");
    let wait_minutes = arrived_at
        .filter(|_| {
            matches!(
                status,
                VisitStatus::Arrived
                    | VisitStatus::TriageInProgress
                    | VisitStatus::ReadyForConsultation
            )
        })
        .map(|a| (Utc::now() - a).num_minutes().max(0));
    let birth_date: chrono::NaiveDate = r.get("birth_date");
    let age_years = age_years(birth_date);
    let can_manage = caps.manage;
    // Resuming is only offered to the practitioner who owns the encounter.
    let can_resume = status == VisitStatus::InConsultation
        && encounter_practitioner.is_some_and(|p| p == ctx.user_id);
    Ok(json!({
        "id": r.get::<Uuid,_>("id"),
        "status": status.as_str(),
        "arrival_kind": r.get::<String,_>("arrival_kind"),
        "service": r.get::<String,_>("service"),
        "reason": r.get::<Option<String>,_>("reason"),
        "scheduled_at": r.get::<Option<DateTime<Utc>>,_>("scheduled_at"),
        "arrived_at": arrived_at,
        "ready_at": r.get::<Option<DateTime<Utc>>,_>("ready_at"),
        "consultation_started_at": r.get::<Option<DateTime<Utc>>,_>("consultation_started_at"),
        "wait_minutes": wait_minutes,
        "priority": r.get::<Option<String>,_>("priority"),
        "handoff_summary": r.get::<Option<String>,_>("handoff_summary"),
        "encounter_id": encounter_id,
        "version": r.get::<i64,_>("version"),
        "updated_at": r.get::<DateTime<Utc>,_>("updated_at"),
        "facility": { "id": facility_id, "name": r.get::<String,_>("facility_name") },
        "patient": {
            "id": r.get::<Uuid,_>("patient_id"),
            "family_name": r.get::<String,_>("family_name"),
            "given_name": r.get::<String,_>("given_name"),
            "identifier": r.get::<String,_>("identifier"),
            "age_years": age_years,
            "alert_count": r.get::<i64,_>("alert_count"),
            "allergy_count": r.get::<i64,_>("allergy_count"),
        },
        "assignment": assignment_json(r),
        "open_alerts": r.get::<i64,_>("open_alerts"),
        "capabilities": {
            "can_arrive": can_manage && status == VisitStatus::Scheduled,
            "can_cancel": can_manage && status.apply(VisitTransition::Cancel).is_ok(),
            "can_no_show": can_manage && status == VisitStatus::Scheduled,
            "can_triage": caps.triage
                && matches!(status, VisitStatus::Arrived | VisitStatus::TriageInProgress | VisitStatus::ReadyForConsultation),
            "can_assign": caps.assign
                && matches!(status, VisitStatus::Arrived | VisitStatus::TriageInProgress | VisitStatus::ReadyForConsultation),
            "can_start_consultation": status == VisitStatus::ReadyForConsultation
                && can_start(ctx, &caps, status, target_user),
            "can_resume_consultation": can_resume,
            "assigned_to_other": target_user.is_some_and(|u| u != ctx.user_id),
        },
    }))
}

fn assignment_json(r: &sqlx::postgres::PgRow) -> Value {
    let user: Option<Uuid> = r.get("assignee_user_id");
    let queue: Option<Uuid> = r.get("queue_id");
    match (user, queue) {
        (Some(u), _) => json!({
            "kind": "professional",
            "user_id": u,
            "display_name": r.get::<Option<String>,_>("assignee_name"),
        }),
        (None, Some(q)) => json!({
            "kind": "queue",
            "queue_id": q,
            "name": r.get::<Option<String>,_>("queue_name"),
            "code": r.get::<Option<String>,_>("queue_code"),
        }),
        (None, None) => Value::Null,
    }
}

fn age_years(birth_date: chrono::NaiveDate) -> i32 {
    let today = Utc::now().date_naive();
    let mut years = today.year_ce().1 as i32 - birth_date.year_ce().1 as i32;
    if (today.month(), today.day()) < (birth_date.month(), birth_date.day()) {
        years -= 1;
    }
    years.max(0)
}

pub async fn list(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::VISIT_READ,
        "visit",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: None,
        }),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;
    // Rows are restricted to facilities where the caller holds VISIT_READ; an
    // empty assignment list means nothing, not everything.
    let scope = facility_scope(&ctx, actions::VISIT_READ);
    let (scope_all, mut scope_ids) = match scope {
        None => (true, Vec::new()),
        Some(ids) => (false, ids),
    };
    if let Some(f) = q.facility_id {
        if scope_all || scope_ids.contains(&f) {
            scope_ids = vec![f];
        } else {
            return Ok(Json(json!({ "items": [], "view": q.view })));
        }
    }
    let scope_all = scope_all && q.facility_id.is_none();
    let statuses: &[&str] = match q.view.as_deref().unwrap_or("access") {
        "access" => &[
            "scheduled",
            "arrived",
            "triage_in_progress",
            "ready_for_consultation",
            "in_consultation",
        ],
        "triage" => &["arrived", "triage_in_progress"],
        "ready" => &["ready_for_consultation", "in_consultation"],
        "closed" => &["completed", "cancelled", "no_show"],
        "all" => &[
            "scheduled",
            "arrived",
            "triage_in_progress",
            "ready_for_consultation",
            "in_consultation",
            "completed",
            "cancelled",
            "no_show",
        ],
        _ => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "view must be one of access, triage, ready, closed, all",
            ))
        }
    };
    let statuses: Vec<String> = statuses.iter().map(|s| s.to_string()).collect();
    // The board is the operational day: appointments within ±24 h and visits
    // closed in the last 24 h. Later appointments stay reachable from the
    // patient chart (`current_for_patient`) until they enter the window.
    let rows = sqlx::query(&format!(
        "{VISIT_LIST_SQL}
         WHERE v.tenant_id = $1 AND ($2 OR v.facility_id = ANY($3))
           AND v.status = ANY($4)
           AND (v.status <> 'scheduled'
                OR v.scheduled_at BETWEEN now() - interval '1 day' AND now() + interval '1 day')
           AND (v.status NOT IN ('completed','cancelled','no_show') OR v.updated_at >= now() - interval '1 day')
         ORDER BY
           CASE v.status
             WHEN 'ready_for_consultation' THEN 0
             WHEN 'triage_in_progress' THEN 1
             WHEN 'arrived' THEN 2
             WHEN 'in_consultation' THEN 3
             WHEN 'scheduled' THEN 4
             ELSE 5 END,
           CASE v.priority
             WHEN 'immediate' THEN 0 WHEN 'urgent' THEN 1 WHEN 'standard' THEN 2
             WHEN 'non_urgent' THEN 3 ELSE 4 END,
           CASE v.arrival_kind WHEN 'urgent' THEN 0 ELSE 1 END,
           v.arrived_at ASC NULLS LAST, v.scheduled_at ASC NULLS LAST, v.id
         LIMIT 200"
    ))
    .bind(ctx.tenant_id)
    .bind(scope_all)
    .bind(&scope_ids)
    .bind(&statuses)
    .fetch_all(&state.pool)
    .await?;
    let items = rows
        .iter()
        .map(|r| visit_item(&ctx, r))
        .collect::<Result<Vec<_>, _>>()?;
    Ok(Json(
        json!({ "items": items, "view": q.view.unwrap_or_else(|| "access".into()) }),
    ))
}

// ---------------------------------------------------------------------------
// GET /api/v1/visits/:id — triage workspace payload
// ---------------------------------------------------------------------------

pub async fn detail(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let v = load_visit(&state, id).await?;
    guard(
        &state,
        &ctx,
        actions::VISIT_READ,
        "visit",
        Some(resource_ctx(&v)),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;
    // One consistent snapshot for the whole payload.
    let mut tx = state.pool.begin().await?;
    sqlx::query("SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY")
        .execute(&mut *tx)
        .await?;
    let row = sqlx::query(&format!("{VISIT_LIST_SQL} WHERE v.id = $1"))
        .bind(id)
        .fetch_optional(&mut *tx)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let mut item = visit_item(&ctx, &row)?;

    let safety = sqlx::query(
        "SELECT p.family_name, p.given_name, p.identifier, p.birth_date, p.sex,
                (SELECT COALESCE(jsonb_agg(jsonb_build_object('substance', a.substance,
                    'criticality', a.criticality) ORDER BY a.recorded_at), '[]'::jsonb)
                   FROM allergies a WHERE a.tenant_id = p.tenant_id AND a.patient_id = p.id) AS allergies,
                (SELECT COALESCE(jsonb_agg(jsonb_build_object('severity', pa.severity,
                    'message', pa.message) ORDER BY pa.created_at DESC), '[]'::jsonb)
                   FROM alerts pa WHERE pa.tenant_id = p.tenant_id AND pa.patient_id = p.id
                     AND pa.status = 'open') AS alerts,
                (SELECT COALESCE(jsonb_agg(jsonb_build_object('display', c.display,
                    'status', c.clinical_status) ORDER BY c.recorded_at DESC), '[]'::jsonb)
                   FROM conditions c WHERE c.tenant_id = p.tenant_id AND c.patient_id = p.id
                     AND c.clinical_status = 'active') AS conditions,
                (SELECT COALESCE(jsonb_agg(jsonb_build_object('name', m.name,
                    'status', m.status) ORDER BY m.name), '[]'::jsonb)
                   FROM medications m WHERE m.tenant_id = p.tenant_id AND m.patient_id = p.id
                     AND m.status = 'active') AS medications
         FROM patients p WHERE p.id = $1",
    )
    .bind(v.patient_id)
    .fetch_one(&mut *tx)
    .await?;

    let assessment = sqlx::query(
        "SELECT t.id, t.version, t.reason, t.concerns, t.onset, t.red_flags, t.note,
                t.vital_signs_id, t.priority, t.safety_floor, t.safety_rules, t.rules_version,
                t.requested_service, t.ai_artifact_id, t.ai_decision, t.completed_at, t.updated_at,
                u.display_name AS author_name
         FROM triage_assessments t JOIN users u ON u.id = t.author_id
         WHERE t.tenant_id = $1 AND t.visit_id = $2",
    )
    .bind(v.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?;
    let assessment_json = assessment.as_ref().map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "version": r.get::<i64,_>("version"),
            "reason": r.get::<Option<String>,_>("reason"),
            "concerns": r.get::<Value,_>("concerns"),
            "onset": r.get::<Option<String>,_>("onset"),
            "red_flags": r.get::<Value,_>("red_flags"),
            "note": r.get::<Option<String>,_>("note"),
            "vital_signs_id": r.get::<Option<Uuid>,_>("vital_signs_id"),
            "priority": r.get::<Option<String>,_>("priority"),
            "safety_floor": r.get::<String,_>("safety_floor"),
            "safety_rules": r.get::<Value,_>("safety_rules"),
            "rules_version": r.get::<String,_>("rules_version"),
            "requested_service": r.get::<Option<String>,_>("requested_service"),
            "ai_artifact_id": r.get::<Option<Uuid>,_>("ai_artifact_id"),
            "ai_decision": r.get::<Option<String>,_>("ai_decision"),
            "completed_at": r.get::<Option<DateTime<Utc>>,_>("completed_at"),
            "updated_at": r.get::<DateTime<Utc>,_>("updated_at"),
            "author_name": r.get::<String,_>("author_name"),
        })
    });

    let vitals = sqlx::query(
        "SELECT id, encounter_id, visit_id, systolic_mmhg, diastolic_mmhg, heart_rate_bpm,
                respiratory_rate_bpm, temperature_c, spo2_percent, weight_kg, height_cm, bmi,
                recorded_at
         FROM vital_signs WHERE tenant_id = $1 AND patient_id = $2
         ORDER BY recorded_at DESC LIMIT 6",
    )
    .bind(v.tenant_id)
    .bind(v.patient_id)
    .fetch_all(&mut *tx)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "encounter_id": r.get::<Option<Uuid>,_>("encounter_id"),
            "visit_id": r.get::<Option<Uuid>,_>("visit_id"),
            "systolic_mmhg": r.get::<Option<Decimal>,_>("systolic_mmhg"),
            "diastolic_mmhg": r.get::<Option<Decimal>,_>("diastolic_mmhg"),
            "heart_rate_bpm": r.get::<Option<Decimal>,_>("heart_rate_bpm"),
            "respiratory_rate_bpm": r.get::<Option<Decimal>,_>("respiratory_rate_bpm"),
            "temperature_c": r.get::<Option<Decimal>,_>("temperature_c"),
            "spo2_percent": r.get::<Option<Decimal>,_>("spo2_percent"),
            "weight_kg": r.get::<Option<Decimal>,_>("weight_kg"),
            "height_cm": r.get::<Option<Decimal>,_>("height_cm"),
            "bmi": r.get::<Option<Decimal>,_>("bmi"),
            "recorded_at": r.get::<DateTime<Utc>,_>("recorded_at"),
        })
    })
    .collect::<Vec<_>>();

    // The latest triage proposal, whatever its review state, so the
    // decision trail stays visible.
    let proposal = sqlx::query(
        "SELECT id, status, model, model_version, route, template, input_hash, output,
                limitations, citations, triage_version, review_decision, review_detail,
                reviewed_at, generated_at
         FROM ai_artifacts WHERE tenant_id = $1 AND visit_id = $2 AND artifact_type = 'triage_proposal'
         ORDER BY generated_at DESC, id DESC LIMIT 1",
    )
    .bind(v.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "status": r.get::<String,_>("status"),
            "model": r.get::<Option<String>,_>("model"),
            "model_version": r.get::<Option<String>,_>("model_version"),
            "route": r.get::<Option<String>,_>("route"),
            "template": r.get::<Option<String>,_>("template"),
            "input_hash": r.get::<Option<String>,_>("input_hash"),
            "output": r.get::<Option<Value>,_>("output"),
            "limitations": r.get::<Value,_>("limitations"),
            "citations": r.get::<Value,_>("citations"),
            "triage_version": r.get::<Option<i64>,_>("triage_version"),
            "review_decision": r.get::<Option<String>,_>("review_decision"),
            "review_detail": r.get::<Value,_>("review_detail"),
            "reviewed_at": r.get::<Option<DateTime<Utc>>,_>("reviewed_at"),
            "generated_at": r.get::<Option<DateTime<Utc>>,_>("generated_at"),
        })
    });

    // Eligible routing targets in this facility (display only; validated
    // again on assignment).
    let professionals = sqlx::query(
        "SELECT DISTINCT u.id, u.display_name FROM role_assignments ra
         JOIN users u ON u.id = ra.user_id
         WHERE ra.tenant_id = $1 AND ra.role = 'physician' AND NOT u.is_service
           AND (ra.facility_id = $2 OR ra.facility_id IS NULL)
         ORDER BY u.display_name",
    )
    .bind(v.tenant_id)
    .bind(v.facility_id)
    .fetch_all(&mut *tx)
    .await?
    .iter()
    .map(|r| json!({ "id": r.get::<Uuid,_>("id"), "display_name": r.get::<String,_>("display_name") }))
    .collect::<Vec<_>>();
    let queues = sqlx::query(
        "SELECT id, code, name FROM service_queues WHERE tenant_id = $1 AND facility_id = $2 ORDER BY name",
    )
    .bind(v.tenant_id)
    .bind(v.facility_id)
    .fetch_all(&mut *tx)
    .await?
    .iter()
    .map(|r| json!({ "id": r.get::<Uuid,_>("id"), "code": r.get::<String,_>("code"), "name": r.get::<String,_>("name") }))
    .collect::<Vec<_>>();
    tx.commit().await?;

    if let Value::Object(ref mut map) = item {
        map.insert(
            "patient_safety".into(),
            json!({
                "family_name": safety.get::<String,_>("family_name"),
                "given_name": safety.get::<String,_>("given_name"),
                "identifier": safety.get::<String,_>("identifier"),
                "birth_date": safety.get::<chrono::NaiveDate,_>("birth_date"),
                "sex": safety.get::<Option<String>,_>("sex"),
                "allergies": safety.get::<Value,_>("allergies"),
                "alerts": safety.get::<Value,_>("alerts"),
                "conditions": safety.get::<Value,_>("conditions"),
                "medications": safety.get::<Value,_>("medications"),
            }),
        );
        map.insert("triage".into(), assessment_json.unwrap_or(Value::Null));
        map.insert("vitals".into(), Value::Array(vitals));
        map.insert("proposal".into(), proposal.unwrap_or(Value::Null));
        map.insert("professionals".into(), Value::Array(professionals));
        map.insert("queues".into(), Value::Array(queues));
        map.insert("rules_version".into(), json!(SAFETY_RULES_VERSION));
    }
    Ok(Json(item))
}

// ---------------------------------------------------------------------------
// POST /api/v1/visits/:id/triage — save (start or update) the triage assessment
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct SaveTriage {
    pub version: i64,
    pub reason: Option<String>,
    #[serde(default)]
    pub concerns: Vec<String>,
    pub onset: Option<String>,
    #[serde(default)]
    pub red_flags: Vec<String>,
    pub note: Option<String>,
    #[serde(default)]
    pub vitals: RecordVitals,
    /// Human-decided operational priority; must not be below the safety floor.
    pub priority: Option<String>,
    pub requested_service: Option<String>,
}

fn require_triageable(v: &VisitRow) -> Result<(), ApiError> {
    if !matches!(
        v.status,
        VisitStatus::Arrived | VisitStatus::TriageInProgress | VisitStatus::ReadyForConsultation
    ) {
        return Err(ApiError::conflict(
            "visit_not_triageable",
            format!("a {} visit cannot be triaged", v.status.as_str()),
        ));
    }
    Ok(())
}

/// Facts changed: any awaiting proposal no longer matches its inputs.
async fn supersede_awaiting_proposals(
    tx: &mut PgConnection,
    tenant_id: Uuid,
    visit_id: Uuid,
) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE ai_artifacts SET status = $1
         WHERE tenant_id = $2 AND visit_id = $3 AND artifact_type = 'triage_proposal' AND status = $4",
    )
    .bind(ArtifactStatus::Superseded.as_str())
    .bind(tenant_id)
    .bind(visit_id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .execute(&mut *tx)
    .await?;
    Ok(())
}

pub async fn save_triage(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<SaveTriage>,
) -> Result<Json<Value>, ApiError> {
    let reason = clean_text(body.reason, "reason", MAX_REASON)?;
    let onset = clean_text(body.onset, "onset", 200)?;
    let note = clean_text(body.note, "note", MAX_TEXT)?;
    let concerns = clean_list(body.concerns, "concerns", is_concern)?;
    let red_flags = clean_list(body.red_flags, "red_flags", is_red_flag)?;
    let requested_service = match body.requested_service.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(s) => Some(require_service(s)?),
    };
    let priority = body.priority.as_deref().map(parse_priority).transpose()?;
    if !body.vitals.is_empty() {
        validate_vitals(&body.vitals)?;
    }

    let v = load_visit(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::TRIAGE_WRITE,
        "triage",
        Some(resource_ctx(&v)),
    )
    .await?;
    require_triageable(&v)?;

    let mut tx = state.pool.begin().await?;
    let v = lock_visit(&mut tx, id).await?;
    require_version(&v, body.version)?;
    require_triageable(&v)?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;

    let existing_vitals: Option<Uuid> = sqlx::query_scalar(
        "SELECT vital_signs_id FROM triage_assessments WHERE tenant_id = $1 AND visit_id = $2",
    )
    .bind(v.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .flatten();
    let vitals_id = if body.vitals.is_empty() {
        existing_vitals
    } else {
        let vid = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO vital_signs
             (id, tenant_id, visit_id, patient_id, recorded_by, systolic_mmhg, diastolic_mmhg,
              heart_rate_bpm, respiratory_rate_bpm, temperature_c, spo2_percent, weight_kg,
              height_cm, bmi)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
        )
        .bind(vid)
        .bind(v.tenant_id)
        .bind(id)
        .bind(v.patient_id)
        .bind(ctx.user_id)
        .bind(body.vitals.systolic_mmhg)
        .bind(body.vitals.diastolic_mmhg)
        .bind(body.vitals.heart_rate_bpm)
        .bind(body.vitals.respiratory_rate_bpm)
        .bind(body.vitals.temperature_c)
        .bind(body.vitals.spo2_percent)
        .bind(body.vitals.weight_kg)
        .bind(body.vitals.height_cm)
        .bind(body.vitals.bmi())
        .execute(&mut *tx)
        .await?;
        Some(vid)
    };
    // The safety floor is computed from every measurement on record for this
    // visit, so a partial re-measurement can never drop an earlier abnormal
    // value out of the rules.
    let floor_vitals = load_visit_vitals(&mut tx, v.tenant_id, id).await?.vitals;
    let (floor, hits) = safety_floor(v.arrival_kind, &red_flags, &floor_vitals);
    if let Some(p) = priority {
        if p < floor {
            return Err(ApiError::new(
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "priority_below_safety_floor",
                format!(
                    "deterministic safety rules require at least '{}' for this patient",
                    floor.as_str()
                ),
            ));
        }
    }
    let hits_json = serde_json::to_value(&hits).map_err(ApiError::internal)?;
    let concerns_json = serde_json::to_value(&concerns).map_err(ApiError::internal)?;
    let flags_json = serde_json::to_value(&red_flags).map_err(ApiError::internal)?;
    let assessment_id = Uuid::now_v7();
    let row = sqlx::query(
        "INSERT INTO triage_assessments
         (id, tenant_id, visit_id, patient_id, author_id, reason, concerns, onset, red_flags, note,
          vital_signs_id, priority, safety_floor, safety_rules, rules_version, requested_service)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16)
         ON CONFLICT (tenant_id, visit_id) DO UPDATE SET
            author_id = EXCLUDED.author_id, reason = EXCLUDED.reason, concerns = EXCLUDED.concerns,
            onset = EXCLUDED.onset, red_flags = EXCLUDED.red_flags, note = EXCLUDED.note,
            vital_signs_id = EXCLUDED.vital_signs_id, priority = EXCLUDED.priority,
            safety_floor = EXCLUDED.safety_floor, safety_rules = EXCLUDED.safety_rules,
            rules_version = EXCLUDED.rules_version, requested_service = EXCLUDED.requested_service,
            version = triage_assessments.version + 1, updated_at = now()
         RETURNING id, version",
    )
    .bind(assessment_id)
    .bind(v.tenant_id)
    .bind(id)
    .bind(v.patient_id)
    .bind(ctx.user_id)
    .bind(&reason)
    .bind(&concerns_json)
    .bind(&onset)
    .bind(&flags_json)
    .bind(&note)
    .bind(vitals_id)
    .bind(priority.map(|p| p.as_str()))
    .bind(floor.as_str())
    .bind(&hits_json)
    .bind(SAFETY_RULES_VERSION)
    .bind(&requested_service)
    .fetch_one(&mut *tx)
    .await?;
    let triage_version: i64 = row.get("version");
    supersede_awaiting_proposals(&mut tx, v.tenant_id, id).await?;
    record_member(&mut tx, &ctx, &v, "triage_nurse", "triage").await?;
    let status = if v.status == VisitStatus::Arrived {
        transition(&mut tx, &v, VisitTransition::StartTriage).await?
    } else {
        // Saving again bumps the visit version so stale tabs are detected.
        sqlx::query("UPDATE visits SET version = version + 1, updated_at = now() WHERE id = $1")
            .bind(id)
            .execute(&mut *tx)
            .await?;
        v.status
    };
    audit::emit(
        &mut *tx,
        &ctx,
        "visit.triage.saved",
        &state.cell,
        json!({
            "visit_id": id,
            "triage_version": triage_version,
            "safety_floor": floor.as_str(),
            "rules_version": SAFETY_RULES_VERSION,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({
        "id": id,
        "status": status.as_str(),
        "version": v.version + 1,
        "triage_version": triage_version,
        "safety_floor": floor.as_str(),
        "safety_rules": hits,
        "rules_version": SAFETY_RULES_VERSION,
        "vital_signs_id": vitals_id,
    })))
}

/// The measurements the safety rules consider, in citation order.
const SAFETY_VITALS: [&str; 5] = [
    "systolic_mmhg",
    "heart_rate_bpm",
    "respiratory_rate_bpm",
    "temperature_c",
    "spo2_percent",
];

/// Effective vital-sign snapshot of a visit together with the row each
/// value was taken from, so provenance can name the measurement's source
/// rather than whichever row happened to be recorded last.
struct EffectiveVitals {
    vitals: TriageVitals,
    /// `(measurement, value, source row)` for every non-null effective value.
    sources: Vec<(&'static str, Decimal, Uuid)>,
}

/// `(reference, statement)` fact for one effective measurement, citing the
/// vital-sign row it was taken from: `vital_signs:<row id>:<measurement>`.
pub(crate) fn vital_fact(measurement: &str, value: Decimal, source: Uuid) -> (String, String) {
    (
        format!("vital_signs:{source}:{measurement}"),
        format!("{measurement} {}", value.normalize()),
    )
}

/// Facts for a snapshot recorded entirely in one vital-sign row.
pub(crate) fn single_row_vital_facts(source: Uuid, v: &TriageVitals) -> Vec<(String, String)> {
    [
        ("systolic_mmhg", v.systolic_mmhg),
        ("heart_rate_bpm", v.heart_rate_bpm),
        ("respiratory_rate_bpm", v.respiratory_rate_bpm),
        ("temperature_c", v.temperature_c),
        ("spo2_percent", v.spo2_percent),
    ]
    .into_iter()
    .filter_map(|(m, value)| value.map(|value| vital_fact(m, value, source)))
    .collect()
}

impl EffectiveVitals {
    /// Facts naming the source row of every effective value.
    fn facts(&self) -> Vec<(String, String)> {
        self.sources
            .iter()
            .map(|(m, value, source)| vital_fact(m, *value, *source))
            .collect()
    }
}

/// The vitals the safety rules see: for each measurement, the most recently
/// recorded non-null value among all vital-sign rows of the visit. Rows stay
/// append-only records of what was measured when; the effective view carries
/// earlier readings forward until a newer reading of the same measurement
/// replaces them.
async fn load_visit_vitals(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    visit_id: Uuid,
) -> Result<EffectiveVitals, ApiError> {
    // One row per measurement: the value and the id of the row it came from.
    let latest = SAFETY_VITALS
        .iter()
        .map(|m| {
            format!(
                "(SELECT '{m}'::text AS measurement, {m} AS value, id AS source FROM vs
                  WHERE {m} IS NOT NULL ORDER BY recorded_at DESC, id DESC LIMIT 1)"
            )
        })
        .collect::<Vec<_>>()
        .join(" UNION ALL ");
    let rows = sqlx::query(&format!(
        "WITH vs AS (SELECT * FROM vital_signs WHERE tenant_id = $1 AND visit_id = $2)
         SELECT measurement, value, source FROM ({latest}) effective"
    ))
    .bind(tenant_id)
    .bind(visit_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut vitals = TriageVitals::default();
    let mut sources = Vec::with_capacity(rows.len());
    for m in SAFETY_VITALS {
        let Some(r) = rows.iter().find(|r| r.get::<String, _>("measurement") == m) else {
            continue;
        };
        let value: Decimal = r.get("value");
        let slot = match m {
            "systolic_mmhg" => &mut vitals.systolic_mmhg,
            "heart_rate_bpm" => &mut vitals.heart_rate_bpm,
            "respiratory_rate_bpm" => &mut vitals.respiratory_rate_bpm,
            "temperature_c" => &mut vitals.temperature_c,
            _ => &mut vitals.spo2_percent,
        };
        *slot = Some(value);
        sources.push((m, value, r.get("source")));
    }
    Ok(EffectiveVitals { vitals, sources })
}

// ---------------------------------------------------------------------------
// POST /api/v1/visits/:id/triage/proposal — dMind triage proposal (A2)
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct ProposalRequest {
    pub language: Option<String>,
}

pub async fn propose(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: Option<Json<ProposalRequest>>,
) -> Result<Json<Value>, ApiError> {
    let language = body
        .and_then(|Json(b)| b.language)
        .filter(|l| l == "es")
        .unwrap_or_else(|| "en".to_string());
    let v = load_visit(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::TRIAGE_WRITE,
        "triage",
        Some(resource_ctx(&v)),
    )
    .await?;
    require_triageable(&v)?;

    let mut tx = state.pool.begin().await?;
    // The visit lock freezes the facts the proposal cites for the whole
    // generation, so the artifact is bound to exactly one triage version.
    let v = lock_visit(&mut tx, id).await?;
    require_triageable(&v)?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let a = sqlx::query(
        "SELECT t.version, t.reason, t.concerns, t.onset, t.red_flags, t.vital_signs_id,
                t.requested_service, p.birth_date
         FROM triage_assessments t JOIN patients p ON p.id = t.patient_id
         WHERE t.tenant_id = $1 AND t.visit_id = $2",
    )
    .bind(v.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        ApiError::conflict(
            "triage_not_started",
            "save the triage assessment before requesting a proposal",
        )
    })?;
    let triage_version: i64 = a.get("version");
    let concerns: Vec<String> =
        serde_json::from_value(a.get::<Value, _>("concerns")).unwrap_or_default();
    let red_flags: Vec<String> =
        serde_json::from_value(a.get::<Value, _>("red_flags")).unwrap_or_default();
    let effective = load_visit_vitals(&mut tx, v.tenant_id, id).await?;
    let allergies: Vec<String> = sqlx::query_scalar(
        "SELECT substance FROM allergies WHERE tenant_id = $1 AND patient_id = $2 ORDER BY substance",
    )
    .bind(v.tenant_id)
    .bind(v.patient_id)
    .fetch_all(&mut *tx)
    .await?;
    let mut facts: Vec<(String, String)> = vec![
        (
            "visit.arrival_kind".into(),
            v.arrival_kind.as_str().to_string(),
        ),
        ("triage.version".into(), triage_version.to_string()),
    ];
    // Every effective measurement cites the row it was carried forward from,
    // not just the latest (possibly sparse) recording.
    facts.extend(effective.facts());
    let req = TriageRequest {
        template: TRIAGE_TEMPLATE.to_string(),
        language,
        arrival_kind: v.arrival_kind,
        age_years: Some(age_years(a.get::<chrono::NaiveDate, _>("birth_date"))),
        reason: a.get("reason"),
        concerns,
        onset: a.get("onset"),
        red_flags,
        vitals: effective.vitals,
        allergies,
        requested_service: a.get("requested_service"),
        facts,
    };
    audit::emit(
        &mut *tx,
        &ctx,
        "ai.artifact.requested",
        &state.cell,
        json!({ "visit_id": id, "triage_version": triage_version, "template": TRIAGE_TEMPLATE }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let resp = match state.gateway.propose_triage(&req).await {
        Ok(r) => r,
        Err(GatewayError::Unavailable(_)) => {
            audit::emit(
                &mut *tx,
                &ctx,
                "ai.provider.unavailable",
                &state.cell,
                json!({ "visit_id": id, "template": TRIAGE_TEMPLATE }),
                None,
            )
            .await
            .map_err(ApiError::internal)?;
            tx.commit().await?;
            return Err(ApiError::new(
                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                "ai_unavailable",
                "the triage assistant is unavailable; triage continues without it",
            ));
        }
        Err(other) => return Err(ApiError::internal(other)),
    };
    // Defense in depth: the provider already clamps, the server clamps again
    // so no proposal can ever sit below the deterministic floor.
    let (floor, _) = safety_floor(v.arrival_kind, &req.red_flags, &req.vitals);
    let output = resp.output.clamp_to_floor(floor);
    output.validate().map_err(ApiError::internal)?;

    let artifact_id = Uuid::now_v7();
    supersede_awaiting_proposals(&mut tx, v.tenant_id, id).await?;
    sqlx::query(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, visit_id, artifact_type, autonomy_level, status,
          model, model_version, route, template, input_hash, output, output_schema,
          citations, limitations, triage_version, generated_at)
         VALUES ($1,$2,$3,$4,'triage_proposal','A2',$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15, now())",
    )
    .bind(artifact_id)
    .bind(v.tenant_id)
    .bind(v.patient_id)
    .bind(id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .bind(&resp.model)
    .bind(&resp.model_version)
    .bind(&resp.route)
    .bind(TRIAGE_TEMPLATE)
    .bind(&resp.input_hash)
    .bind(serde_json::to_value(&output).map_err(ApiError::internal)?)
    .bind(wellos_domain::triage::TRIAGE_PROPOSAL_SCHEMA)
    .bind(serde_json::to_value(&output.cited_sources).map_err(ApiError::internal)?)
    .bind(serde_json::to_value(&output.limitations).map_err(ApiError::internal)?)
    .bind(triage_version)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE triage_assessments SET ai_artifact_id = $3, ai_decision = NULL, updated_at = now()
         WHERE tenant_id = $1 AND visit_id = $2",
    )
    .bind(v.tenant_id)
    .bind(id)
    .bind(artifact_id)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "ai.artifact.generated",
        &state.cell,
        json!({
            "artifact_id": artifact_id,
            "visit_id": id,
            "triage_version": triage_version,
            "input_hash": resp.input_hash,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({
        "id": artifact_id,
        "status": ArtifactStatus::AwaitingReview.as_str(),
        "output": output,
        "limitations": output.limitations,
        "citations": output.cited_sources,
        "triage_version": triage_version,
        "model": resp.model,
        "model_version": resp.model_version,
        "route": resp.route,
        "template": TRIAGE_TEMPLATE,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/visits/:id/triage/proposal/:artifact_id/review — accept /
// override / reject (human decision; never automatic)
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct ReviewProposal {
    pub version: i64,
    /// accept | override | reject
    pub decision: String,
    /// Required for override: the clinician's own priority and service.
    pub priority: Option<String>,
    pub requested_service: Option<String>,
    pub note: Option<String>,
}

pub async fn review_proposal(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path((id, artifact_id)): Path<(Uuid, Uuid)>,
    Json(body): Json<ReviewProposal>,
) -> Result<Json<Value>, ApiError> {
    let decision = match body.decision.as_str() {
        "accept" | "override" | "reject" => body.decision.clone(),
        _ => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "decision must be accept, override or reject",
            ))
        }
    };
    let note = clean_text(body.note, "note", MAX_TEXT)?;
    let override_priority = body.priority.as_deref().map(parse_priority).transpose()?;
    let override_service = match body.requested_service.as_deref().map(str::trim) {
        None | Some("") => None,
        Some(s) => Some(require_service(s)?),
    };
    if decision == "override" && override_priority.is_none() && override_service.is_none() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "override requires a priority or a service",
        ));
    }

    let v = load_visit(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::TRIAGE_WRITE,
        "triage",
        Some(resource_ctx(&v)),
    )
    .await?;
    require_triageable(&v)?;
    let mut tx = state.pool.begin().await?;
    let v = lock_visit(&mut tx, id).await?;
    require_version(&v, body.version)?;
    require_triageable(&v)?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;

    let art = sqlx::query(
        "SELECT status, output, triage_version FROM ai_artifacts
         WHERE id = $1 AND tenant_id = $2 AND visit_id = $3 AND artifact_type = 'triage_proposal'",
    )
    .bind(artifact_id)
    .bind(v.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let status = ArtifactStatus::parse(art.get::<String, _>("status").as_str())
        .ok_or_else(|| ApiError::internal("invalid artifact status"))?;
    if status != ArtifactStatus::AwaitingReview {
        return Err(ApiError::conflict(
            "invalid_artifact_state",
            "this proposal has already been reviewed or superseded",
        ));
    }
    let a = sqlx::query(
        "SELECT version, safety_floor FROM triage_assessments WHERE tenant_id = $1 AND visit_id = $2",
    )
    .bind(v.tenant_id)
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    if art.get::<Option<i64>, _>("triage_version") != Some(a.get::<i64, _>("version")) {
        return Err(ApiError::conflict(
            "artifact_stale",
            "the proposal was generated from an older triage version; request a new one",
        ));
    }
    let floor = Priority::parse(a.get::<String, _>("safety_floor").as_str())
        .ok_or_else(|| ApiError::internal("invalid safety floor"))?;
    let output: wellos_domain::triage::TriageProposalV1 =
        serde_json::from_value(art.get::<Value, _>("output")).map_err(ApiError::internal)?;

    let (applied_priority, applied_service) = match decision.as_str() {
        "accept" => (
            Some(output.proposed_priority),
            Some(output.proposed_service.clone()),
        ),
        "override" => (override_priority, override_service.clone()),
        _ => (None, None),
    };
    if let Some(p) = applied_priority {
        if p < floor {
            return Err(ApiError::new(
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                "priority_below_safety_floor",
                format!(
                    "deterministic safety rules require at least '{}' for this patient",
                    floor.as_str()
                ),
            ));
        }
    }
    let review_decision = if decision == "reject" {
        "rejected"
    } else {
        "approved"
    };
    let next = status
        .review(if decision == "reject" {
            wellos_domain::ai::ReviewDecision::Rejected
        } else {
            wellos_domain::ai::ReviewDecision::Approved
        })
        .map_err(|e| ApiError::conflict("invalid_artifact_state", e.to_string()))?;
    let detail = json!({
        "decision": decision,
        "applied_priority": applied_priority.map(|p| p.as_str()),
        "applied_service": applied_service,
        "proposed_priority": output.proposed_priority.as_str(),
        "proposed_service": output.proposed_service,
        "safety_floor": floor.as_str(),
    });
    let updated = sqlx::query(
        "UPDATE ai_artifacts SET status = $1, reviewer_id = $2, review_decision = $3, review_note = $4,
                review_detail = $5, reviewed_at = now()
         WHERE id = $6 AND status = $7",
    )
    .bind(next.as_str())
    .bind(ctx.user_id)
    .bind(review_decision)
    .bind(&note)
    .bind(&detail)
    .bind(artifact_id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(ApiError::conflict(
            "invalid_artifact_state",
            "this proposal was reviewed concurrently",
        ));
    }
    // Accepting/overriding writes the clinician's decision into the
    // assessment; the assessment version is unchanged because the facts the
    // proposal cited did not change.
    sqlx::query(
        "UPDATE triage_assessments SET ai_artifact_id = $3, ai_decision = $4,
                priority = COALESCE($5, priority),
                requested_service = COALESCE($6, requested_service),
                updated_at = now()
         WHERE tenant_id = $1 AND visit_id = $2",
    )
    .bind(v.tenant_id)
    .bind(id)
    .bind(artifact_id)
    .bind(&decision)
    .bind(applied_priority.map(|p| p.as_str()))
    .bind(&applied_service)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE visits SET version = version + 1, updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "ai.artifact.reviewed",
        &state.cell,
        json!({ "artifact_id": artifact_id, "visit_id": id, "decision": decision }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({
        "id": artifact_id,
        "status": next.as_str(),
        "decision": decision,
        "applied_priority": applied_priority.map(|p| p.as_str()),
        "applied_service": applied_service,
        "version": v.version + 1,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/visits/:id/assign — route to a professional or a queue
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct Assign {
    pub version: i64,
    pub assignee_user_id: Option<Uuid>,
    pub queue_id: Option<Uuid>,
}

pub async fn assign(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<Assign>,
) -> Result<Json<Value>, ApiError> {
    if body.assignee_user_id.is_none() && body.queue_id.is_none() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "assign a professional or a queue",
        ));
    }
    let v = load_visit(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::CARE_TEAM_ASSIGN,
        "care_team",
        Some(resource_ctx(&v)),
    )
    .await?;
    require_triageable(&v)?;
    let mut tx = state.pool.begin().await?;
    let v = lock_visit(&mut tx, id).await?;
    require_version(&v, body.version)?;
    require_triageable(&v)?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let target = resolve_target(
        &mut tx,
        &v,
        &v.service,
        body.assignee_user_id,
        body.queue_id,
    )
    .await?;
    let assignment_id = route_visit(&mut tx, &ctx, &v, &target, "handoff").await?;
    sqlx::query("UPDATE visits SET version = version + 1, updated_at = now() WHERE id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "visit.assigned",
        &state.cell,
        json!({
            "visit_id": id,
            "assignment_id": assignment_id,
            "target_user_id": target.user_id,
            "target_queue_id": target.queue_id,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    // A patient already waiting for consultation is re-announced to the new
    // target; earlier ready alerts for the visit are resolved.
    if v.status == VisitStatus::ReadyForConsultation {
        raise_alert(
            &mut tx,
            &ctx,
            &state,
            &v,
            "patient_ready",
            v.priority.unwrap_or(Priority::Standard),
            &target,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Json(
        json!({ "id": id, "assignment_id": assignment_id, "version": v.version + 1 }),
    ))
}

// ---------------------------------------------------------------------------
// POST /api/v1/visits/:id/triage/complete — ready for consultation + alert
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
pub struct CompleteTriage {
    pub version: i64,
    pub priority: String,
    pub requested_service: String,
    pub handoff_summary: Option<String>,
    pub assignee_user_id: Option<Uuid>,
    pub queue_id: Option<Uuid>,
}

pub async fn complete_triage(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<CompleteTriage>,
) -> Result<Json<Value>, ApiError> {
    let priority = parse_priority(&body.priority)?;
    let service = require_service(&body.requested_service)?;
    let handoff = clean_text(body.handoff_summary, "handoff_summary", MAX_TEXT)?;
    let v = load_visit(&state, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::TRIAGE_WRITE,
        "triage",
        Some(resource_ctx(&v)),
    )
    .await?;
    if v.status != VisitStatus::TriageInProgress {
        return Err(invalid_transition(&v, VisitTransition::CompleteTriage));
    }
    let mut tx = state.pool.begin().await?;
    let v = lock_visit(&mut tx, id).await?;
    require_version(&v, body.version)?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let a = sqlx::query(
        "SELECT safety_floor, ai_artifact_id, ai_decision FROM triage_assessments
         WHERE tenant_id = $1 AND visit_id = $2",
    )
    .bind(v.tenant_id)
    .bind(id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(|| {
        ApiError::conflict(
            "triage_not_started",
            "save the triage assessment before completing triage",
        )
    })?;
    let floor = Priority::parse(a.get::<String, _>("safety_floor").as_str())
        .ok_or_else(|| ApiError::internal("invalid safety floor"))?;
    if priority < floor {
        return Err(ApiError::new(
            axum::http::StatusCode::UNPROCESSABLE_ENTITY,
            "priority_below_safety_floor",
            format!(
                "deterministic safety rules require at least '{}' for this patient",
                floor.as_str()
            ),
        ));
    }
    // A proposal still awaiting review must be decided explicitly before the
    // handoff, so no dMind output is silently carried forward or dropped.
    let awaiting: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM ai_artifacts
         WHERE tenant_id = $1 AND visit_id = $2 AND artifact_type = 'triage_proposal' AND status = $3
         LIMIT 1",
    )
    .bind(v.tenant_id)
    .bind(id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .fetch_optional(&mut *tx)
    .await?;
    if awaiting.is_some() {
        return Err(ApiError::conflict(
            "proposal_awaiting_review",
            "accept, override or reject the dMind proposal before completing triage",
        ));
    }
    sqlx::query(
        "UPDATE triage_assessments SET priority = $3, requested_service = $4, completed_at = now(),
                updated_at = now()
         WHERE tenant_id = $1 AND visit_id = $2",
    )
    .bind(v.tenant_id)
    .bind(id)
    .bind(priority.as_str())
    .bind(&service)
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "UPDATE visits SET priority = $2, service = $3, handoff_summary = $4 WHERE id = $1",
    )
    .bind(id)
    .bind(priority.as_str())
    .bind(&service)
    .bind(&handoff)
    .execute(&mut *tx)
    .await?;
    let next = transition(&mut tx, &v, VisitTransition::CompleteTriage).await?;
    let v = lock_visit(&mut tx, id).await?;
    let target = match (body.assignee_user_id, body.queue_id) {
        (None, None) => match current_target(&mut tx, &v).await? {
            // Keep an explicit individual assignment; re-route queue
            // destinations to the decided service.
            Some(t) if t.user_id.is_some() => t,
            _ => resolve_target(&mut tx, &v, &service, None, None).await?,
        },
        (u, q) => resolve_target(&mut tx, &v, &service, u, q).await?,
    };
    route_visit(&mut tx, &ctx, &v, &target, "triage").await?;
    record_member(&mut tx, &ctx, &v, "triage_nurse", "triage").await?;
    // Urgent-arrival alerts are superseded by the ready alert.
    sqlx::query(
        "UPDATE internal_alerts SET status = 'resolved', resolved_at = now()
         WHERE tenant_id = $1 AND visit_id = $2 AND kind = 'urgent_arrival' AND status <> 'resolved'",
    )
    .bind(v.tenant_id)
    .bind(id)
    .execute(&mut *tx)
    .await?;
    let alert_id = raise_alert(
        &mut tx,
        &ctx,
        &state,
        &v,
        "patient_ready",
        priority,
        &target,
    )
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "visit.triage.completed",
        &state.cell,
        json!({
            "visit_id": id,
            "priority": priority.as_str(),
            "service": service,
            "safety_floor": floor.as_str(),
            "ai_decision": a.get::<Option<String>,_>("ai_decision"),
            "target_user_id": target.user_id,
            "target_queue_id": target.queue_id,
            "alert_id": alert_id,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({
        "id": id,
        "status": next.as_str(),
        "priority": priority.as_str(),
        "service": service,
        "version": v.version,
        "alert_id": alert_id,
    })))
}

// ---------------------------------------------------------------------------
// POST /api/v1/visits/:id/start-consultation — handoff into the encounter
// ---------------------------------------------------------------------------

#[derive(Deserialize, Default)]
pub struct StartConsultation {
    pub version: Option<i64>,
}

pub async fn start_consultation(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: Option<Json<StartConsultation>>,
) -> Result<Json<Value>, ApiError> {
    let expected_version = body.and_then(|Json(b)| b.version);
    let v = load_visit(&state, id).await?;
    // Starting an encounter establishes the care relationship; facility
    // scope and role are checked centrally.
    let allowed = guard(
        &state,
        &ctx,
        actions::ENCOUNTER_START,
        "encounter",
        Some(ResourceCtx {
            tenant_id: v.tenant_id,
            patient_id: None,
            facility_id: Some(v.facility_id),
        }),
    )
    .await?;

    let mut tx = state.pool.begin().await?;
    // Lock order: patient first (as encounters::start does), then visit, so
    // create-or-resume and the visit handoff serialize the same way.
    sqlx::query("SELECT id FROM patients WHERE id = $1 FOR UPDATE")
        .bind(v.patient_id)
        .execute(&mut *tx)
        .await?;
    let v = lock_visit(&mut tx, id).await?;
    if let Some(expected) = expected_version {
        require_version(&v, expected)?;
    }
    allowed.record(&mut tx, &ctx, &state.cell).await?;

    if v.status == VisitStatus::InConsultation {
        // Resume: only the practitioner who owns the encounter.
        let owner: Option<Uuid> =
            sqlx::query_scalar("SELECT practitioner_id FROM encounters WHERE id = $1")
                .bind(v.encounter_id)
                .fetch_optional(&mut *tx)
                .await?;
        if owner == Some(ctx.user_id) {
            tx.commit().await?;
            return Ok(Json(json!({
                "visit_id": id,
                "encounter_id": v.encounter_id,
                "resumed": true,
                "status": v.status.as_str(),
            })));
        }
        return Err(ApiError::conflict(
            "consultation_in_progress",
            "another professional is already consulting this patient",
        ));
    }
    if v.status != VisitStatus::ReadyForConsultation {
        return Err(invalid_transition(&v, VisitTransition::StartConsultation));
    }
    // Routing is honoured: a visit assigned to a named professional is not
    // picked up by someone else without an explicit reassignment.
    let target = current_target(&mut tx, &v).await?;
    if let Some(Target {
        user_id: Some(u), ..
    }) = &target
    {
        if *u != ctx.user_id {
            return Err(ApiError::conflict(
                "assigned_to_other_professional",
                "this patient is assigned to another professional; reassign before starting",
            ));
        }
    }

    // Create-or-resume the caller's consultation for this patient (same rule
    // as POST /encounters with resume=true). The draft is locked with the
    // same row lock encounter sign and cancel take, and the predicate is
    // re-evaluated once the lock is held: a draft closed by a transaction
    // that committed first is not returned, and one returned stays
    // `in_progress` until this handoff commits. Lock order stays
    // patient → visit → encounter; sign/cancel lock the encounter and then
    // the visit *already linked* to it, which is never the one held here.
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM encounters
         WHERE tenant_id = $1 AND patient_id = $2 AND practitioner_id = $3
           AND status = 'in_progress' AND encounter_type = 'consultation'
         ORDER BY started_at DESC LIMIT 1
         FOR UPDATE",
    )
    .bind(v.tenant_id)
    .bind(v.patient_id)
    .bind(ctx.user_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (encounter_id, resumed) = match existing {
        Some(e) => (e, true),
        None => {
            let e = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO encounters (id, tenant_id, facility_id, patient_id, practitioner_id, encounter_type)
                 VALUES ($1,$2,$3,$4,$5,'consultation')",
            )
            .bind(e)
            .bind(v.tenant_id)
            .bind(v.facility_id)
            .bind(v.patient_id)
            .bind(ctx.user_id)
            .execute(&mut *tx)
            .await?;
            audit::emit(
                &mut *tx,
                &ctx,
                "encounter.started",
                &state.cell,
                json!({
                    "encounter_id": e,
                    "patient_id": v.patient_id,
                    "encounter_type": "consultation",
                    "visit_id": id,
                }),
                None,
            )
            .await
            .map_err(ApiError::internal)?;
            (e, false)
        }
    };
    sqlx::query("UPDATE visits SET encounter_id = $2 WHERE id = $1")
        .bind(id)
        .bind(encounter_id)
        .execute(&mut *tx)
        .await?;
    let next = transition(&mut tx, &v, VisitTransition::StartConsultation).await?;
    let v = lock_visit(&mut tx, id).await?;
    route_visit(
        &mut tx,
        &ctx,
        &v,
        &Target {
            user_id: Some(ctx.user_id),
            queue_id: None,
        },
        "handoff",
    )
    .await?;
    resolve_alerts(&mut tx, &ctx, &state, &v).await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "visit.consultation.started",
        &state.cell,
        json!({ "visit_id": id, "encounter_id": encounter_id, "resumed": resumed }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({
        "visit_id": id,
        "encounter_id": encounter_id,
        "resumed": resumed,
        "status": next.as_str(),
        "version": v.version,
    })))
}

/// Handoff context for the consultation workspace: the visit that led into
/// this encounter with its triage summary, so the clinician sees why the
/// patient is here without opening the triage screen.
pub async fn handoff_for_encounter(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    encounter_id: Uuid,
) -> Result<Value, ApiError> {
    let row = sqlx::query(
        "SELECT v.id, v.status, v.arrival_kind, v.service, v.reason, v.arrived_at, v.ready_at,
                v.priority, v.handoff_summary,
                t.concerns, t.red_flags, t.safety_floor, t.safety_rules, t.rules_version,
                t.onset, t.note AS triage_note, t.completed_at AS triage_completed_at,
                tu.display_name AS triage_author
         FROM visits v
         LEFT JOIN triage_assessments t ON t.tenant_id = v.tenant_id AND t.visit_id = v.id
         LEFT JOIN users tu ON tu.id = t.author_id
         WHERE v.tenant_id = $1 AND v.encounter_id = $2",
    )
    .bind(tenant_id)
    .bind(encounter_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row
        .map(|r| {
            let has_triage = r.get::<Option<String>, _>("safety_floor").is_some();
            json!({
                "id": r.get::<Uuid,_>("id"),
                "status": r.get::<String,_>("status"),
                "arrival_kind": r.get::<String,_>("arrival_kind"),
                "service": r.get::<String,_>("service"),
                "reason": r.get::<Option<String>,_>("reason"),
                "arrived_at": r.get::<Option<DateTime<Utc>>,_>("arrived_at"),
                "ready_at": r.get::<Option<DateTime<Utc>>,_>("ready_at"),
                "priority": r.get::<Option<String>,_>("priority"),
                "handoff_summary": r.get::<Option<String>,_>("handoff_summary"),
                "triage": if has_triage {
                    json!({
                        "concerns": r.get::<Value,_>("concerns"),
                        "red_flags": r.get::<Value,_>("red_flags"),
                        "onset": r.get::<Option<String>,_>("onset"),
                        "note": r.get::<Option<String>,_>("triage_note"),
                        "safety_floor": r.get::<Option<String>,_>("safety_floor"),
                        "safety_rules": r.get::<Value,_>("safety_rules"),
                        "rules_version": r.get::<Option<String>,_>("rules_version"),
                        "completed_at": r.get::<Option<DateTime<Utc>>,_>("triage_completed_at"),
                        "author_name": r.get::<Option<String>,_>("triage_author"),
                    })
                } else {
                    Value::Null
                },
            })
        })
        .unwrap_or(Value::Null))
}

/// The patient's current visit for the chart: the open visit, or else the
/// nearest upcoming appointment. Display-only capabilities as in the list.
pub async fn current_for_patient(
    state: &AppState,
    ctx: &AuthContext,
    tenant_id: Uuid,
    patient_id: Uuid,
) -> Result<Value, ApiError> {
    let row = sqlx::query(&format!(
        "{VISIT_LIST_SQL}
         WHERE v.tenant_id = $1 AND v.patient_id = $2
           AND v.status IN ('scheduled','arrived','triage_in_progress','ready_for_consultation','in_consultation')
           AND (v.status <> 'scheduled' OR v.scheduled_at >= now() - interval '1 day')
         ORDER BY CASE WHEN v.status = 'scheduled' THEN 1 ELSE 0 END, v.scheduled_at ASC NULLS LAST, v.id
         LIMIT 1"
    ))
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_optional(&state.pool)
    .await?;
    match row {
        Some(r) => visit_item(ctx, &r),
        None => Ok(Value::Null),
    }
}

/// Whether the caller may register arrivals/appointments for a patient at
/// this facility (display hint; the create route re-authorizes).
pub fn can_manage_visits_at(ctx: &AuthContext, facility_id: Uuid) -> bool {
    caps_for(ctx, facility_id).manage
}

/// Called inside the encounter sign transaction: the visit that handed off
/// into this encounter is completed with it.
pub async fn complete_for_encounter(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    tenant_id: Uuid,
    encounter_id: Uuid,
) -> Result<(), ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {VISIT_COLUMNS} FROM visits WHERE tenant_id = $1 AND encounter_id = $2 FOR UPDATE"
    ))
    .bind(tenant_id)
    .bind(encounter_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else { return Ok(()) };
    let v = visit_from_row(&row)?;
    if v.status != VisitStatus::InConsultation {
        return Ok(());
    }
    transition(tx, &v, VisitTransition::CompleteConsultation).await?;
    sqlx::query(
        "UPDATE care_team_assignments SET active = false, ends_at = now(), updated_at = now()
         WHERE tenant_id = $1 AND visit_id = $2 AND active",
    )
    .bind(v.tenant_id)
    .bind(v.id)
    .execute(&mut **tx)
    .await?;
    resolve_alerts(tx, ctx, state, &v).await?;
    audit::emit(
        &mut **tx,
        ctx,
        "visit.completed",
        &state.cell,
        json!({ "visit_id": v.id, "encounter_id": encounter_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(())
}

/// Called inside the encounter cancel transaction: the patient is still
/// waiting, so the visit returns to the ready list under its previous routing
/// and the handoff alert is raised again.
pub async fn release_for_encounter(
    tx: &mut Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    tenant_id: Uuid,
    encounter_id: Uuid,
) -> Result<(), ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {VISIT_COLUMNS} FROM visits WHERE tenant_id = $1 AND encounter_id = $2 FOR UPDATE"
    ))
    .bind(tenant_id)
    .bind(encounter_id)
    .fetch_optional(&mut **tx)
    .await?;
    let Some(row) = row else { return Ok(()) };
    let v = visit_from_row(&row)?;
    if v.status != VisitStatus::InConsultation {
        return Ok(());
    }
    transition(tx, &v, VisitTransition::ReleaseConsultation).await?;
    sqlx::query(
        "UPDATE visits SET encounter_id = NULL, consultation_started_at = NULL WHERE id = $1",
    )
    .bind(v.id)
    .execute(&mut **tx)
    .await?;
    let v = lock_visit(tx, v.id).await?;
    let target = resolve_target(tx, &v, &v.service, None, None).await?;
    route_visit(tx, ctx, &v, &target, "handoff").await?;
    raise_alert(
        tx,
        ctx,
        state,
        &v,
        "patient_ready",
        v.priority.unwrap_or(Priority::Standard),
        &target,
    )
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "visit.consultation.released",
        &state.cell,
        json!({ "visit_id": v.id, "encounter_id": encounter_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Internal alerts: GET /api/v1/alerts, POST /api/v1/alerts/:id/acknowledge
// ---------------------------------------------------------------------------

/// Queue alerts are visible to the professionals who work that queue in that
/// facility: the nursing queue to triage staff, every other queue to those
/// who may start consultations. Directed alerts are visible to their target.
///
/// The decision is a SQL predicate ([`ALERT_VISIBLE_SQL`]) rather than a
/// post-filter: the alert list applies it before its result cap, and the
/// acknowledgement lookup applies it before touching any patient or facility
/// data, so an alert the caller may not see is indistinguishable from one
/// that does not exist.
struct AlertScope {
    /// Facilities whose nursing-queue alerts are visible; `None` = tenant-wide.
    nursing: Option<Vec<Uuid>>,
    /// Facilities whose other queue alerts are visible; `None` = tenant-wide.
    consultation: Option<Vec<Uuid>>,
}

impl AlertScope {
    fn for_ctx(ctx: &AuthContext) -> Self {
        Self {
            nursing: facility_scope(ctx, actions::TRIAGE_WRITE),
            consultation: facility_scope(ctx, actions::ENCOUNTER_START),
        }
    }

    /// Binds `$1..$6` as consumed by [`ALERT_VISIBLE_SQL`].
    fn bind<'q>(
        &'q self,
        q: sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments>,
        ctx: &AuthContext,
    ) -> sqlx::query::Query<'q, sqlx::Postgres, sqlx::postgres::PgArguments> {
        q.bind(ctx.tenant_id)
            .bind(ctx.user_id)
            .bind(self.nursing.is_none())
            .bind(self.nursing.as_deref().unwrap_or(&[]))
            .bind(self.consultation.is_none())
            .bind(self.consultation.as_deref().unwrap_or(&[]))
    }
}

/// Visibility predicate over `internal_alerts ia LEFT JOIN service_queues q`.
/// Parameters: `$1` tenant, `$2` caller, `$3`/`$4` nursing scope
/// (tenant-wide flag, facility list), `$5`/`$6` consultation scope.
const ALERT_VISIBLE_SQL: &str = "
    ia.tenant_id = $1
    AND (
      ia.target_user_id = $2
      OR (ia.target_user_id IS NULL AND ia.target_queue_id IS NOT NULL
          AND CASE WHEN q.code = 'nursing'
                   THEN ($3 OR ia.facility_id = ANY($4))
                   ELSE ($5 OR ia.facility_id = ANY($6)) END)
    )";

const ALERT_SQL: &str = "
    SELECT ia.id, ia.facility_id, ia.visit_id, ia.kind, ia.priority, ia.status, ia.target_user_id,
           ia.target_queue_id, ia.created_at, ia.acknowledged_at, ia.acknowledged_by,
           q.code AS queue_code, q.name AS queue_name,
           v.status AS visit_status, v.reason, v.arrived_at, v.handoff_summary, v.encounter_id,
           v.version AS visit_version,
           p.id AS patient_id, p.family_name, p.given_name, p.identifier
    FROM internal_alerts ia
    JOIN visits v ON v.id = ia.visit_id
    JOIN patients p ON p.id = ia.patient_id
    LEFT JOIN service_queues q ON q.id = ia.target_queue_id
";

fn alert_item(ctx: &AuthContext, r: &sqlx::postgres::PgRow) -> Value {
    let acknowledged_by: Option<Uuid> = r.get("acknowledged_by");
    let arrived_at: Option<DateTime<Utc>> = r.get("arrived_at");
    json!({
        "id": r.get::<Uuid,_>("id"),
        "visit_id": r.get::<Uuid,_>("visit_id"),
        "kind": r.get::<String,_>("kind"),
        "priority": r.get::<String,_>("priority"),
        "status": r.get::<String,_>("status"),
        "created_at": r.get::<DateTime<Utc>,_>("created_at"),
        "acknowledged_at": r.get::<Option<DateTime<Utc>>,_>("acknowledged_at"),
        "acknowledged_by_me": acknowledged_by == Some(ctx.user_id),
        "target": if r.get::<Option<Uuid>,_>("target_user_id").is_some() {
            json!({ "kind": "professional" })
        } else {
            json!({ "kind": "queue", "code": r.get::<Option<String>,_>("queue_code"), "name": r.get::<Option<String>,_>("queue_name") })
        },
        "visit": {
            "status": r.get::<String,_>("visit_status"),
            "reason": r.get::<Option<String>,_>("reason"),
            "wait_minutes": arrived_at.map(|a| (Utc::now() - a).num_minutes().max(0)),
            "handoff_summary": r.get::<Option<String>,_>("handoff_summary"),
            "encounter_id": r.get::<Option<Uuid>,_>("encounter_id"),
            "version": r.get::<i64,_>("visit_version"),
        },
        "patient": {
            "id": r.get::<Uuid,_>("patient_id"),
            "family_name": r.get::<String,_>("family_name"),
            "given_name": r.get::<String,_>("given_name"),
            "identifier": r.get::<String,_>("identifier"),
        },
    })
}

pub async fn list_alerts(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::VISIT_READ,
        "internal_alert",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: None,
        }),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;
    let scope = AlertScope::for_ctx(&ctx);
    let sql = format!(
        "{ALERT_SQL}
         WHERE {ALERT_VISIBLE_SQL} AND ia.status <> 'resolved'
         ORDER BY CASE ia.priority WHEN 'immediate' THEN 0 WHEN 'urgent' THEN 1 WHEN 'standard' THEN 2 ELSE 3 END,
                  ia.created_at ASC
         LIMIT 100"
    );
    let rows = scope
        .bind(sqlx::query(&sql), &ctx)
        .fetch_all(&state.pool)
        .await?;
    let items: Vec<Value> = rows.iter().map(|r| alert_item(&ctx, r)).collect();
    let unacknowledged = items.iter().filter(|i| i["status"] == "open").count();
    Ok(Json(
        json!({ "items": items, "unacknowledged": unacknowledged }),
    ))
}

pub async fn acknowledge_alert(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    // Visibility is decided by the lookup itself: an alert outside the
    // caller's scope yields the same `not_found` as an unknown id, and no
    // patient or facility data is read (or authorization attempted) for it.
    let scope = AlertScope::for_ctx(&ctx);
    let sql = format!(
        "SELECT ia.tenant_id, ia.facility_id, ia.patient_id
         FROM internal_alerts ia LEFT JOIN service_queues q ON q.id = ia.target_queue_id
         WHERE ia.id = $7 AND {ALERT_VISIBLE_SQL}"
    );
    let row = scope
        .bind(sqlx::query(&sql), &ctx)
        .bind(id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let tenant_id: Uuid = row.get("tenant_id");
    let facility_id: Uuid = row.get("facility_id");
    let allowed = guard(
        &state,
        &ctx,
        actions::ALERT_ACKNOWLEDGE,
        "internal_alert",
        Some(ResourceCtx {
            tenant_id,
            patient_id: Some(row.get("patient_id")),
            facility_id: Some(facility_id),
        }),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let updated = sqlx::query(
        "UPDATE internal_alerts SET status = 'acknowledged', acknowledged_by = $2, acknowledged_at = now()
         WHERE id = $1 AND status = 'open'",
    )
    .bind(id)
    .bind(ctx.user_id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(ApiError::conflict(
            "alert_not_open",
            "this alert was already acknowledged or resolved",
        ));
    }
    audit::emit(
        &mut *tx,
        &ctx,
        "internal_alert.acknowledged",
        &state.cell,
        json!({ "alert_id": id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({ "id": id, "status": "acknowledged" })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::RoleAssignment;
    use crate::policy::roles;
    use crate::policy::Purpose;

    fn ctx(role: &str, facility: Uuid) -> AuthContext {
        AuthContext {
            user_id: Uuid::now_v7(),
            tenant_id: Uuid::now_v7(),
            username: role.to_string(),
            display_name: role.to_string(),
            is_service: false,
            roles: vec![role.to_string()],
            assignments: vec![RoleAssignment {
                role: role.to_string(),
                facility_id: Some(facility),
            }],
            scopes: vec![],
            purpose_of_use: Purpose::Treatment,
            break_glass_reason: None,
            web_session_id: None,
            correlation_id: Uuid::now_v7(),
        }
    }

    #[test]
    fn queue_alerts_route_by_function_and_facility() {
        let facility = Uuid::now_v7();
        let physician = AlertScope::for_ctx(&ctx(roles::PHYSICIAN, facility));
        assert_eq!(physician.consultation, Some(vec![facility]));
        // Physicians may triage, so they also serve the nursing queue.
        assert_eq!(physician.nursing, Some(vec![facility]));
        let nurse = AlertScope::for_ctx(&ctx(roles::NURSE, facility));
        assert_eq!(nurse.nursing, Some(vec![facility]));
        assert_eq!(nurse.consultation, Some(vec![]));
        let registration = AlertScope::for_ctx(&ctx(roles::REGISTRATION, facility));
        assert_eq!(registration.nursing, Some(vec![]));
        assert_eq!(registration.consultation, Some(vec![]));
        // Ordinary clinical roles never become tenant-wide through a NULL
        // facility assignment.
        let mut unscoped = ctx(roles::PHYSICIAN, facility);
        unscoped.assignments[0].facility_id = None;
        let unscoped = AlertScope::for_ctx(&unscoped);
        assert_eq!(unscoped.consultation, Some(vec![]));
    }
}
