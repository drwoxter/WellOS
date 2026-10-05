//! dMind Clinical Orders & Diagnostics v1.
//!
//! `service_requests` stays the authoritative clinical order. This module adds
//! the generalized order lifecycle around it (groups, deterministic safety
//! preflight, explicit clinician confirmation, fulfilment state, Access
//! linkage, specimens, typed reports, professional review and release) and
//! the bounded dMind operations beside it. Every state change here is a
//! human decision or a deterministic consequence of one; dMind never creates,
//! signs, reviews or releases anything.

pub mod catalog;
pub mod documents;
pub mod orders;
pub mod reports;
pub mod self_service;
pub mod specimens;

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, facility_scope, ResourceCtx};
use crate::routes::{guard, Allowed};
use crate::scheduling::{self, AppointmentRow};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::postgres::PgRow;
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wellos_domain::access::{AppointmentStatus, AppointmentTransition};
use wellos_domain::diagnostics::{
    FulfilmentMode, OrderPriority, OrderStatus, OrderTransition, ReportStatus,
};

pub const MAX_TEXT: usize = 4_000;
pub const MAX_SHORT: usize = 400;
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

pub fn routes() -> Router<AppState> {
    let router = Router::new();
    #[cfg(feature = "dev-fixtures")]
    let router = router.route(
        &format!("{}/*key", crate::objectstore::FIXTURE_ROUTE_PREFIX),
        axum::routing::put(documents::fixture_put).get(documents::fixture_get),
    );
    router
        .route("/api/v1/diagnostics/catalog", get(catalog::search))
        .route(
            "/api/v1/encounters/:id/diagnostic-orders/preflight",
            post(orders::preflight),
        )
        .route(
            "/api/v1/encounters/:id/diagnostic-orders/suggest",
            post(orders::suggest),
        )
        .route(
            "/api/v1/encounters/:id/diagnostic-orders",
            post(orders::confirm).get(orders::list_for_encounter),
        )
        .route("/api/v1/diagnostics/orders", get(worklist))
        .route("/api/v1/diagnostics/orders/:id", get(detail))
        .route(
            "/api/v1/diagnostics/orders/:id/transition",
            post(orders::transition),
        )
        .route(
            "/api/v1/diagnostics/orders/:id/schedule",
            post(orders::schedule),
        )
        .route(
            "/api/v1/diagnostics/orders/:id/specimens",
            post(specimens::record),
        )
        .route(
            "/api/v1/diagnostics/specimens/:id/events",
            post(specimens::event),
        )
        .route(
            "/api/v1/diagnostics/orders/:id/reports",
            post(reports::issue),
        )
        .route("/api/v1/diagnostics/reports/:id", get(reports::detail))
        .route(
            "/api/v1/diagnostics/reports/:id/synthesis",
            post(reports::synthesize),
        )
        .route(
            "/api/v1/diagnostics/reports/:id/review",
            post(reports::review),
        )
        .route(
            "/api/v1/diagnostics/reports/:id/explanation",
            post(reports::explain),
        )
        .route(
            "/api/v1/diagnostics/reports/:id/explanation/:artifact_id/review",
            post(reports::review_explanation),
        )
        .route(
            "/api/v1/diagnostics/reports/:id/release",
            post(reports::release),
        )
        .route("/api/v1/diagnostics/reviews", get(reports::review_worklist))
        .route(
            "/api/v1/diagnostics/orders/:id/documents",
            post(documents::register),
        )
        .route(
            "/api/v1/diagnostics/documents/:id/complete",
            post(documents::complete),
        )
        .route(
            "/api/v1/diagnostics/documents/:id/download",
            get(documents::download),
        )
        .route(
            "/api/v1/diagnostics/documents/:id/scan",
            post(documents::scan_verdict),
        )
        .route(
            "/api/v1/diagnostics/orders/:id/imaging-studies",
            post(documents::register_imaging_study),
        )
        .route("/api/v1/patients/:id/diagnostics", get(patient_diagnostics))
        .route("/api/v1/me/diagnostics", get(self_service::list))
        .route(
            "/api/v1/me/diagnostics/:report_id",
            get(self_service::detail),
        )
        .route(
            "/api/v1/me/diagnostics/:report_id/documents/:document_id/download",
            get(self_service::download),
        )
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct OrderRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub patient_id: Uuid,
    pub patient_facility_id: Uuid,
    pub encounter_id: Uuid,
    pub requester_id: Uuid,
    pub code_loinc: Option<String>,
    pub display: String,
    pub loop_state: String,
    pub version: i64,
    pub orderable_id: Option<Uuid>,
    pub orderable_code: Option<String>,
    pub orderable_version: Option<i64>,
    pub order_group_id: Option<Uuid>,
    pub category_code: Option<String>,
    pub modality_code: Option<String>,
    pub expected_result_type: String,
    pub order_status: OrderStatus,
    pub fulfilment_mode: FulfilmentMode,
    pub priority: OrderPriority,
    pub clinical_indication: Option<String>,
    pub clinical_question: Option<String>,
    pub requested_window_start: Option<DateTime<Utc>>,
    pub requested_window_end: Option<DateTime<Utc>>,
    pub preparation_en: Option<String>,
    pub preparation_es: Option<String>,
    pub performing_facility_id: Option<Uuid>,
    pub performing_service_code: Option<String>,
    pub performing_professional_id: Option<Uuid>,
    pub performing_resource_id: Option<Uuid>,
    pub access_request_id: Option<Uuid>,
    pub appointment_id: Option<Uuid>,
    pub schedule_conflict: Option<String>,
    pub hold_reason: Option<String>,
    pub cancellation_reason: Option<String>,
    pub rejection_reason: Option<String>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
    pub cancelled_at: Option<DateTime<Utc>>,
    pub source_system: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub const ORDER_COLUMNS: &str = "sr.id, sr.tenant_id, sr.patient_id, p.facility_id AS patient_facility_id, sr.encounter_id,
    sr.requester_id, sr.code_loinc, sr.display, sr.loop_state, sr.version, sr.orderable_id, sr.orderable_code,
    sr.orderable_version, sr.order_group_id, sr.category_code, sr.modality_code, sr.expected_result_type,
    sr.order_status, sr.fulfilment_mode, sr.priority, sr.clinical_indication, sr.clinical_question,
    sr.requested_window_start, sr.requested_window_end, sr.preparation_en, sr.preparation_es,
    sr.performing_facility_id, sr.performing_service_code, sr.performing_professional_id,
    sr.performing_resource_id, sr.access_request_id, sr.appointment_id, sr.schedule_conflict, sr.hold_reason,
    sr.cancellation_reason, sr.rejection_reason, sr.accepted_at, sr.started_at, sr.completed_at,
    sr.cancelled_at, sr.source_system, sr.created_at, sr.updated_at";

pub fn order_from_row(r: &PgRow) -> Result<OrderRow, ApiError> {
    let status: String = r.get("order_status");
    let mode: String = r.get("fulfilment_mode");
    let priority: String = r.get("priority");
    Ok(OrderRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        patient_id: r.get("patient_id"),
        patient_facility_id: r.get("patient_facility_id"),
        encounter_id: r.get("encounter_id"),
        requester_id: r.get("requester_id"),
        code_loinc: r.get("code_loinc"),
        display: r.get("display"),
        loop_state: r.get("loop_state"),
        version: r.get("version"),
        orderable_id: r.get("orderable_id"),
        orderable_code: r.get("orderable_code"),
        orderable_version: r.get("orderable_version"),
        order_group_id: r.get("order_group_id"),
        category_code: r.get("category_code"),
        modality_code: r.get("modality_code"),
        expected_result_type: r.get("expected_result_type"),
        order_status: OrderStatus::parse(&status)
            .ok_or_else(|| ApiError::internal("invalid order status"))?,
        fulfilment_mode: FulfilmentMode::parse(&mode)
            .ok_or_else(|| ApiError::internal("invalid fulfilment mode"))?,
        priority: OrderPriority::parse(&priority)
            .ok_or_else(|| ApiError::internal("invalid order priority"))?,
        clinical_indication: r.get("clinical_indication"),
        clinical_question: r.get("clinical_question"),
        requested_window_start: r.get("requested_window_start"),
        requested_window_end: r.get("requested_window_end"),
        preparation_en: r.get("preparation_en"),
        preparation_es: r.get("preparation_es"),
        performing_facility_id: r.get("performing_facility_id"),
        performing_service_code: r.get("performing_service_code"),
        performing_professional_id: r.get("performing_professional_id"),
        performing_resource_id: r.get("performing_resource_id"),
        access_request_id: r.get("access_request_id"),
        appointment_id: r.get("appointment_id"),
        schedule_conflict: r.get("schedule_conflict"),
        hold_reason: r.get("hold_reason"),
        cancellation_reason: r.get("cancellation_reason"),
        rejection_reason: r.get("rejection_reason"),
        accepted_at: r.get("accepted_at"),
        started_at: r.get("started_at"),
        completed_at: r.get("completed_at"),
        cancelled_at: r.get("cancelled_at"),
        source_system: r.get("source_system"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    })
}

pub async fn load_order(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<OrderRow, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {ORDER_COLUMNS} FROM service_requests sr JOIN patients p ON p.id = sr.patient_id
         WHERE sr.id = $1 AND sr.tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    order_from_row(&row)
}

/// Lock the order row for the rest of the transaction (patient facility is
/// read through the trusted relationship, never from client input).
pub async fn lock_order(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<OrderRow, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {ORDER_COLUMNS} FROM service_requests sr JOIN patients p ON p.id = sr.patient_id
         WHERE sr.id = $1 AND sr.tenant_id = $2 FOR UPDATE OF sr"
    ))
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    order_from_row(&row)
}

/// Facility context for authorization: staff of the performing facility act
/// on orders performed there; everyone else is evaluated against the
/// patient's home facility. Both facilities come from stored relationships.
pub fn order_ctx(ctx: &AuthContext, o: &OrderRow) -> ResourceCtx {
    let facility = match o.performing_facility_id {
        Some(f)
            if f != o.patient_facility_id
                && ctx.assignments.iter().any(|a| a.facility_id == Some(f)) =>
        {
            f
        }
        _ => o.patient_facility_id,
    };
    ResourceCtx {
        tenant_id: o.tenant_id,
        patient_id: Some(o.patient_id),
        facility_id: Some(facility),
    }
}

pub async fn guard_order(
    state: &AppState,
    ctx: &AuthContext,
    action: &str,
    o: &OrderRow,
) -> Result<Allowed, ApiError> {
    guard(
        state,
        ctx,
        action,
        "diagnostic_order",
        Some(order_ctx(ctx, o)),
    )
    .await
}

pub fn actor_label(ctx: &AuthContext) -> String {
    scheduling::actor_label(ctx)
}

#[allow(clippy::too_many_arguments)]
pub async fn record_history(
    conn: &mut PgConnection,
    o: &OrderRow,
    from: Option<OrderStatus>,
    to: OrderStatus,
    version: i64,
    reason: Option<&str>,
    actor: &str,
    actor_user_id: Option<Uuid>,
    details: Value,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO service_request_history
         (id, tenant_id, service_request_id, from_status, to_status, version, reason, actor, actor_user_id, details)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",
    )
    .bind(Uuid::now_v7())
    .bind(o.tenant_id)
    .bind(o.id)
    .bind(from.map(|s| s.as_str()))
    .bind(to.as_str())
    .bind(version)
    .bind(reason)
    .bind(actor)
    .bind(actor_user_id)
    .bind(details)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Apply one fulfilment transition to a locked order: state machine, version
/// bump, timestamps, history and audit in the caller's transaction.
pub struct TransitionInput<'a> {
    pub transition: OrderTransition,
    pub reason: Option<&'a str>,
    pub actor: &'a str,
    pub actor_user_id: Option<Uuid>,
    pub details: Value,
    pub event: &'static str,
}

pub async fn apply_transition_in(
    tx: &mut PgConnection,
    ctx: &AuthContext,
    state: &AppState,
    o: &OrderRow,
    input: TransitionInput<'_>,
) -> Result<OrderRow, ApiError> {
    let next = o
        .order_status
        .apply(input.transition, o.fulfilment_mode)
        .map_err(|e| ApiError::conflict("invalid_order_transition", e.to_string()))?;
    let extra = match input.transition {
        OrderTransition::Accept => "accepted_at = now(),",
        OrderTransition::Start => "started_at = now(),",
        OrderTransition::Complete => "completed_at = now(),",
        OrderTransition::Cancel => "cancelled_at = now(), cancellation_reason = $4,",
        OrderTransition::Reject => "rejection_reason = $4,",
        OrderTransition::Hold => "hold_reason = $4,",
        OrderTransition::Resume => "hold_reason = NULL,",
        OrderTransition::EnterInError => "cancelled_at = now(),",
        OrderTransition::Schedule | OrderTransition::Unschedule => "",
    };
    let sql = format!(
        "UPDATE service_requests SET order_status = $1, {extra} version = version + 1, updated_at = now()
         WHERE id = $2 AND version = $3 AND order_status = $5"
    );
    let updated = sqlx::query(&sql)
        .bind(next.as_str())
        .bind(o.id)
        .bind(o.version)
        .bind(input.reason)
        .bind(o.order_status.as_str())
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "version_conflict",
            "the order was modified concurrently",
        ));
    }
    record_history(
        tx,
        o,
        Some(o.order_status),
        next,
        o.version + 1,
        input.reason,
        input.actor,
        input.actor_user_id,
        input.details.clone(),
    )
    .await?;
    audit::emit(
        &mut *tx,
        ctx,
        input.event,
        &state.cell,
        json!({
            "service_request_id": o.id,
            "patient_id": o.patient_id,
            "from": o.order_status.as_str(),
            "to": next.as_str(),
            "transition": format!("{:?}", input.transition).to_lowercase(),
            "details": input.details,
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    load_order(tx, o.tenant_id, o.id).await
}

// ---------------------------------------------------------------------------
// Access linkage hooks (called inside scheduling transactions)
// ---------------------------------------------------------------------------

async fn linked_orders_for_update(
    tx: &mut PgConnection,
    tenant_id: Uuid,
    appointment_id: Option<Uuid>,
    access_request_id: Option<Uuid>,
) -> Result<Vec<OrderRow>, ApiError> {
    let rows = sqlx::query(&format!(
        "SELECT {ORDER_COLUMNS} FROM service_requests sr JOIN patients p ON p.id = sr.patient_id
         WHERE sr.tenant_id = $1
           AND (($2::uuid IS NOT NULL AND sr.appointment_id = $2)
             OR ($3::uuid IS NOT NULL AND sr.access_request_id = $3))
           AND sr.order_status IN ('placed','accepted','scheduled','on_hold')
         ORDER BY sr.created_at FOR UPDATE OF sr"
    ))
    .bind(tenant_id)
    .bind(appointment_id)
    .bind(access_request_id)
    .fetch_all(&mut *tx)
    .await?;
    rows.iter().map(order_from_row).collect()
}

/// A confirmed appointment schedules every linked order (same Access request
/// or the appointment it replaces). Rescheduling keeps the order and its
/// history; a prior conflict is cleared because a human rebooked it.
pub async fn on_appointment_confirmed(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    appt: &AppointmentRow,
) -> Result<(), ApiError> {
    let orders = linked_orders_for_update(
        tx,
        appt.tenant_id,
        appt.rescheduled_from,
        appt.access_request_id,
    )
    .await?;
    let actor = actor_label(ctx);
    for mut o in orders {
        if o.order_status == OrderStatus::OnHold {
            continue;
        }
        sqlx::query(
            "UPDATE service_requests
             SET appointment_id = $2, performing_facility_id = $3, schedule_conflict = NULL,
                 updated_at = now()
             WHERE id = $1",
        )
        .bind(o.id)
        .bind(appt.id)
        .bind(appt.facility_id)
        .execute(&mut **tx)
        .await?;
        o.appointment_id = Some(appt.id);
        o.performing_facility_id = Some(appt.facility_id);
        o.schedule_conflict = None;
        if o.order_status == OrderStatus::Placed {
            o = apply_transition_in(
                tx,
                ctx,
                state,
                &o,
                TransitionInput {
                    transition: OrderTransition::Accept,
                    reason: Some("appointment_confirmed"),
                    actor: &actor,
                    actor_user_id: Some(ctx.user_id),
                    details: json!({ "appointment_id": appt.id }),
                    event: "diagnostic.order.transitioned",
                },
            )
            .await?;
        }
        if o.order_status == OrderStatus::Accepted {
            apply_transition_in(
                tx,
                ctx,
                state,
                &o,
                TransitionInput {
                    transition: OrderTransition::Schedule,
                    reason: Some("appointment_confirmed"),
                    actor: &actor,
                    actor_user_id: Some(ctx.user_id),
                    details: json!({
                        "appointment_id": appt.id,
                        "starts_at": appt.starts_at,
                        "facility_id": appt.facility_id,
                    }),
                    event: "diagnostic.order.scheduled",
                },
            )
            .await?;
        } else if o.order_status == OrderStatus::Scheduled {
            record_history(
                tx,
                &o,
                Some(OrderStatus::Scheduled),
                OrderStatus::Scheduled,
                o.version,
                Some("appointment_rescheduled"),
                &actor,
                Some(ctx.user_id),
                json!({ "appointment_id": appt.id, "starts_at": appt.starts_at }),
            )
            .await?;
            audit::emit(
                &mut **tx,
                ctx,
                "diagnostic.order.scheduled",
                &state.cell,
                json!({ "service_request_id": o.id, "appointment_id": appt.id, "rescheduled": true }),
                None,
            )
            .await
            .map_err(ApiError::internal)?;
        }
    }
    Ok(())
}

/// A closed appointment never closes the order. Cancellation and no-show
/// unschedule it and flag a conflict for the diagnostic worklist; fulfilment
/// starts acquisition; a reschedule is relinked when the new appointment is
/// confirmed in the same transaction.
pub async fn on_appointment_closed(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    appt: &AppointmentRow,
    t: AppointmentTransition,
    next: AppointmentStatus,
) -> Result<(), ApiError> {
    if t == AppointmentTransition::Reschedule {
        return Ok(());
    }
    let orders = linked_orders_for_update(tx, appt.tenant_id, Some(appt.id), None).await?;
    let actor = actor_label(ctx);
    for o in orders {
        match next {
            AppointmentStatus::Fulfilled => {
                if o.order_status == OrderStatus::Scheduled {
                    apply_transition_in(
                        tx,
                        ctx,
                        state,
                        &o,
                        TransitionInput {
                            transition: OrderTransition::Start,
                            reason: Some("appointment_fulfilled"),
                            actor: &actor,
                            actor_user_id: Some(ctx.user_id),
                            details: json!({ "appointment_id": appt.id }),
                            event: "diagnostic.order.transitioned",
                        },
                    )
                    .await?;
                }
            }
            AppointmentStatus::Cancelled | AppointmentStatus::NoShow => {
                let conflict = if next == AppointmentStatus::Cancelled {
                    "appointment_cancelled"
                } else {
                    "appointment_no_show"
                };
                sqlx::query(
                    "UPDATE service_requests SET schedule_conflict = $2, updated_at = now() WHERE id = $1",
                )
                .bind(o.id)
                .bind(conflict)
                .execute(&mut **tx)
                .await?;
                let mut o = o;
                o.schedule_conflict = Some(conflict.to_string());
                if o.order_status == OrderStatus::Scheduled {
                    apply_transition_in(
                        tx,
                        ctx,
                        state,
                        &o,
                        TransitionInput {
                            transition: OrderTransition::Unschedule,
                            reason: Some(conflict),
                            actor: &actor,
                            actor_user_id: Some(ctx.user_id),
                            details: json!({ "appointment_id": appt.id, "conflict": conflict }),
                            event: "diagnostic.order.schedule_conflict",
                        },
                    )
                    .await?;
                } else {
                    audit::emit(
                        &mut **tx,
                        ctx,
                        "diagnostic.order.schedule_conflict",
                        &state.cell,
                        json!({ "service_request_id": o.id, "appointment_id": appt.id, "conflict": conflict }),
                        None,
                    )
                    .await
                    .map_err(ApiError::internal)?;
                }
            }
            _ => {}
        }
    }
    Ok(())
}

/// Confirmed appointments caught by a new facility closure keep their orders
/// scheduled but flag them for a human decision.
pub async fn flag_facility_closed(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    state: &AppState,
    tenant_id: Uuid,
    appointment_ids: &[Uuid],
) -> Result<(), ApiError> {
    if appointment_ids.is_empty() {
        return Ok(());
    }
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "UPDATE service_requests SET schedule_conflict = 'facility_closed', updated_at = now()
         WHERE tenant_id = $1 AND appointment_id = ANY($2)
           AND order_status IN ('placed','accepted','scheduled')
         RETURNING id",
    )
    .bind(tenant_id)
    .bind(appointment_ids)
    .fetch_all(&mut *conn)
    .await?;
    for id in ids {
        audit::emit(
            &mut *conn,
            ctx,
            "diagnostic.order.schedule_conflict",
            &state.cell,
            json!({ "service_request_id": id, "conflict": "facility_closed" }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// Views
// ---------------------------------------------------------------------------

pub fn order_json(o: &OrderRow) -> Value {
    json!({
        "id": o.id,
        "patient_id": o.patient_id,
        "encounter_id": o.encounter_id,
        "requester_id": o.requester_id,
        "order_group_id": o.order_group_id,
        "orderable_id": o.orderable_id,
        "orderable_code": o.orderable_code,
        "orderable_version": o.orderable_version,
        "code_loinc": o.code_loinc,
        "display": o.display,
        "category_code": o.category_code,
        "modality_code": o.modality_code,
        "expected_result_type": o.expected_result_type,
        "order_status": o.order_status.as_str(),
        "loop_state": o.loop_state,
        "fulfilment_mode": o.fulfilment_mode.as_str(),
        "priority": o.priority.as_str(),
        "clinical_indication": o.clinical_indication,
        "clinical_question": o.clinical_question,
        "requested_window_start": o.requested_window_start,
        "requested_window_end": o.requested_window_end,
        "preparation_en": o.preparation_en,
        "preparation_es": o.preparation_es,
        "performing_facility_id": o.performing_facility_id,
        "performing_service_code": o.performing_service_code,
        "performing_professional_id": o.performing_professional_id,
        "performing_resource_id": o.performing_resource_id,
        "access_request_id": o.access_request_id,
        "appointment_id": o.appointment_id,
        "schedule_conflict": o.schedule_conflict,
        "hold_reason": o.hold_reason,
        "cancellation_reason": o.cancellation_reason,
        "rejection_reason": o.rejection_reason,
        "accepted_at": o.accepted_at,
        "started_at": o.started_at,
        "completed_at": o.completed_at,
        "cancelled_at": o.cancelled_at,
        "source_system": o.source_system,
        "version": o.version,
        "created_at": o.created_at,
        "updated_at": o.updated_at,
    })
}

pub async fn history_json(conn: &mut PgConnection, order_id: Uuid) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT from_status, to_status, version, reason, actor, actor_user_id, details, recorded_at
         FROM service_request_history WHERE service_request_id = $1 ORDER BY recorded_at, version",
    )
    .bind(order_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "from_status": r.get::<Option<String>, _>("from_status"),
                "to_status": r.get::<String, _>("to_status"),
                "version": r.get::<i64, _>("version"),
                "reason": r.get::<Option<String>, _>("reason"),
                "actor": r.get::<String, _>("actor"),
                "actor_user_id": r.get::<Option<Uuid>, _>("actor_user_id"),
                "details": r.get::<Value, _>("details"),
                "recorded_at": r.get::<DateTime<Utc>, _>("recorded_at"),
            })
        })
        .collect())
}

async fn appointment_summary(
    conn: &mut PgConnection,
    appointment_id: Option<Uuid>,
) -> Result<Value, ApiError> {
    let Some(id) = appointment_id else {
        return Ok(Value::Null);
    };
    let row = sqlx::query(
        "SELECT a.id, a.status, a.starts_at, a.ends_at, a.time_zone, a.facility_id, f.name AS facility_name,
                a.service_code
         FROM appointments a JOIN facilities f ON f.id = a.facility_id WHERE a.id = $1",
    )
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row
        .map(|r| {
            json!({
                "id": r.get::<Uuid, _>("id"),
                "status": r.get::<String, _>("status"),
                "starts_at": r.get::<DateTime<Utc>, _>("starts_at"),
                "ends_at": r.get::<DateTime<Utc>, _>("ends_at"),
                "time_zone": r.get::<String, _>("time_zone"),
                "facility_id": r.get::<Uuid, _>("facility_id"),
                "facility_name": r.get::<String, _>("facility_name"),
                "service_code": r.get::<String, _>("service_code"),
            })
        })
        .unwrap_or(Value::Null))
}

/// Full order view: order, history, specimens, reports (with components),
/// documents, imaging studies, reviews, release decisions, appointment.
pub async fn order_detail_json(conn: &mut PgConnection, o: &OrderRow) -> Result<Value, ApiError> {
    let history = history_json(conn, o.id).await?;
    let specimens = specimens::list_json(conn, o.id).await?;
    let reports = reports::list_for_order_json(conn, o).await?;
    let documents = documents::list_for_order_json(conn, o.id, false).await?;
    let imaging = documents::imaging_for_order_json(conn, o.id).await?;
    let appointment = appointment_summary(conn, o.appointment_id).await?;
    let safety = match o.order_group_id {
        Some(g) => orders::group_safety_json(conn, g).await?,
        None => Value::Null,
    };
    let mut v = order_json(o);
    v["history"] = Value::Array(history);
    v["specimens"] = Value::Array(specimens);
    v["reports"] = Value::Array(reports);
    v["documents"] = Value::Array(documents);
    v["imaging_studies"] = Value::Array(imaging);
    v["appointment"] = appointment;
    v["safety"] = safety;
    Ok(v)
}

pub async fn detail(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let o = load_order(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard_order(&state, &ctx, actions::DIAGNOSTIC_READ, &o).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let mut v = order_detail_json(&mut conn, &o).await?;
    let mut items = vec![v.clone()];
    scheduling::attach_patient_summaries(&mut *conn, ctx.tenant_id, &mut items, true).await?;
    v["patient"] = items[0]["patient"].clone();
    Ok(Json(v))
}

#[derive(Debug, Default, Deserialize)]
pub struct WorklistQuery {
    pub status: Option<String>,
    pub category: Option<String>,
    pub modality: Option<String>,
    pub facility_id: Option<Uuid>,
    pub patient_id: Option<Uuid>,
    pub conflicts_only: Option<bool>,
    pub q: Option<String>,
    pub cursor: Option<String>,
    pub limit: Option<i64>,
}

fn decode_cursor(raw: &str) -> Option<(i32, DateTime<Utc>, Uuid)> {
    let mut parts = raw.splitn(3, '|');
    let rank: i32 = parts.next()?.parse().ok()?;
    let at: DateTime<Utc> = parts.next()?.parse().ok()?;
    let id: Uuid = parts.next()?.parse().ok()?;
    Some((rank, at, id))
}

/// Diagnostic worklist for performing staff: priority first, then oldest.
/// Scheduling conflicts are visible here so a human decides.
pub async fn worklist(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<WorklistQuery>,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::DIAGNOSTIC_READ,
        "diagnostic_worklist",
        None,
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;
    let scope = facility_scope(&ctx, actions::DIAGNOSTIC_READ);
    if matches!(&scope, Some(ids) if ids.is_empty()) {
        return Ok(Json(
            json!({ "items": [], "has_more": false, "next_cursor": null }),
        ));
    }
    let statuses: Vec<String> = match q.status.as_deref() {
        None | Some("") | Some("open") => OrderStatus::ALL
            .iter()
            .filter(|s| s.is_open())
            .map(|s| s.as_str().to_string())
            .collect(),
        Some("all") => OrderStatus::ALL
            .iter()
            .map(|s| s.as_str().to_string())
            .collect(),
        Some(list) => {
            let mut out = Vec::new();
            for s in list.split(',') {
                let st = OrderStatus::parse(s.trim()).ok_or_else(|| {
                    ApiError::bad_request("validation_failed", "unknown order status filter")
                })?;
                out.push(st.as_str().to_string());
            }
            out
        }
    };
    let cursor = match q.cursor.as_deref() {
        None => None,
        Some(raw) => Some(
            decode_cursor(raw)
                .ok_or_else(|| ApiError::bad_request("invalid_cursor", "malformed cursor"))?,
        ),
    };
    let limit = q.limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT);
    let pattern =
        q.q.as_deref()
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(|s| {
                let escaped = s
                    .replace('\\', "\\\\")
                    .replace('%', "\\%")
                    .replace('_', "\\_");
                format!("%{escaped}%")
            });
    let rows = sqlx::query(&format!(
        "SELECT {ORDER_COLUMNS},
                CASE sr.priority WHEN 'stat' THEN 0 WHEN 'urgent' THEN 1 WHEN 'timed' THEN 2 ELSE 3 END AS rank
         FROM service_requests sr JOIN patients p ON p.id = sr.patient_id
         WHERE sr.tenant_id = $1
           AND sr.order_status = ANY($2)
           AND ($3::uuid[] IS NULL OR COALESCE(sr.performing_facility_id, p.facility_id) = ANY($3)
                OR p.facility_id = ANY($3))
           AND ($4::text IS NULL OR sr.category_code = $4)
           AND ($5::text IS NULL OR sr.modality_code = $5)
           AND ($6::uuid IS NULL OR COALESCE(sr.performing_facility_id, p.facility_id) = $6)
           AND ($7::uuid IS NULL OR sr.patient_id = $7)
           AND (NOT $8::boolean OR sr.schedule_conflict IS NOT NULL)
           AND ($9::text IS NULL OR sr.display ILIKE $9 OR sr.orderable_code ILIKE $9
                OR p.family_name ILIKE $9 OR p.given_name ILIKE $9 OR p.identifier ILIKE $9)
           AND ($10::int IS NULL OR
                (CASE sr.priority WHEN 'stat' THEN 0 WHEN 'urgent' THEN 1 WHEN 'timed' THEN 2 ELSE 3 END,
                 sr.created_at, sr.id) > ($10, $11, $12))
         ORDER BY rank, sr.created_at, sr.id
         LIMIT $13"
    ))
    .bind(ctx.tenant_id)
    .bind(&statuses)
    .bind(scope.as_deref())
    .bind(q.category.as_deref().filter(|s| !s.is_empty()))
    .bind(q.modality.as_deref().filter(|s| !s.is_empty()))
    .bind(q.facility_id)
    .bind(q.patient_id)
    .bind(q.conflicts_only.unwrap_or(false))
    .bind(pattern)
    .bind(cursor.map(|c| c.0))
    .bind(cursor.map(|c| c.1))
    .bind(cursor.map(|c| c.2))
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await?;
    let has_more = rows.len() as i64 > limit;
    let mut items = Vec::new();
    let mut next_cursor = None;
    for r in rows.iter().take(limit as usize) {
        let o = order_from_row(r)?;
        let rank: i32 = r.get("rank");
        next_cursor = Some(format!("{rank}|{}|{}", o.created_at.to_rfc3339(), o.id));
        let mut v = order_json(&o);
        v["latest_report_status"] = Value::Null;
        items.push(v);
    }
    let mut conn = state.pool.acquire().await?;
    reports::attach_latest_report_status(&mut conn, &mut items).await?;
    scheduling::attach_patient_summaries(&mut *conn, ctx.tenant_id, &mut items, true).await?;
    Ok(Json(json!({
        "items": items,
        "has_more": has_more,
        "next_cursor": if has_more { next_cursor } else { None },
    })))
}

/// Pending and recent diagnostics for Patient Brief / Patient 360.
pub async fn patient_diagnostics(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(patient_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let p = sqlx::query("SELECT tenant_id, facility_id FROM patients WHERE id = $1")
        .bind(patient_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let tenant_id: Uuid = p.get("tenant_id");
    guard(
        &state,
        &ctx,
        actions::DIAGNOSTIC_READ,
        "patient_diagnostics",
        Some(ResourceCtx {
            tenant_id,
            patient_id: Some(patient_id),
            facility_id: Some(p.get("facility_id")),
        }),
    )
    .await?
    .record(&mut conn, &ctx, &state.cell)
    .await?;
    Ok(Json(
        patient_diagnostics_json(&mut conn, tenant_id, patient_id).await?,
    ))
}

pub async fn patient_diagnostics_json(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
) -> Result<Value, ApiError> {
    let rows = sqlx::query(&format!(
        "SELECT {ORDER_COLUMNS} FROM service_requests sr JOIN patients p ON p.id = sr.patient_id
         WHERE sr.tenant_id = $1 AND sr.patient_id = $2
         ORDER BY sr.created_at DESC LIMIT 200"
    ))
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut pending = Vec::new();
    let mut completed = Vec::new();
    for r in &rows {
        let o = order_from_row(r)?;
        let mut v = order_json(&o);
        v["latest_report_status"] = Value::Null;
        if o.order_status.is_open() {
            pending.push(v);
        } else if o.order_status == OrderStatus::Completed {
            completed.push(v);
        }
    }
    reports::attach_latest_report_status(conn, &mut pending).await?;
    reports::attach_latest_report_status(conn, &mut completed).await?;
    let reports = reports::recent_for_patient_json(conn, tenant_id, patient_id, 20).await?;
    Ok(json!({
        "pending_orders": pending,
        "completed_orders": completed,
        "recent_reports": reports,
    }))
}

pub fn parse_report_status(s: &str) -> Result<ReportStatus, ApiError> {
    ReportStatus::parse(s)
        .ok_or_else(|| ApiError::bad_request("validation_failed", "unknown report status"))
}

pub fn check_len(
    field: &'static str,
    value: Option<&str>,
    max: usize,
) -> Result<Option<String>, ApiError> {
    match value.map(str::trim).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(v) if v.chars().count() > max => Err(ApiError::bad_request(
            "validation_failed",
            format!("{field} exceeds {max} characters"),
        )),
        Some(v) => Ok(Some(v.to_string())),
    }
}

pub fn require(field: &'static str, value: Option<String>) -> Result<String, ApiError> {
    value.ok_or_else(|| ApiError::bad_request("validation_failed", format!("{field} is required")))
}
