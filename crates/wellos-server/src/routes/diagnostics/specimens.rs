//! Specimens and chain of custody. Every custody step is an explicit human
//! event on a locked specimen row; the history is append-only.

use super::{
    actor_label, check_len, guard_order, load_order, lock_order, OrderRow, MAX_SHORT, MAX_TEXT,
};
use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::actions;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::Json;
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wellos_domain::diagnostics::{OrderStatus, SpecimenEvent, SpecimenStatus};

const MIN_REASON_CHARS: usize = 3;

#[derive(Debug, Clone)]
pub struct SpecimenRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub patient_id: Uuid,
    pub service_request_id: Uuid,
    pub identifier: String,
    pub specimen_type_code: String,
    pub container_code: Option<String>,
    pub body_site: Option<String>,
    pub status: SpecimenStatus,
    pub collected_at: Option<DateTime<Utc>>,
    pub collected_by: Option<Uuid>,
    pub collection_facility_id: Option<Uuid>,
    pub rejection_reason: Option<String>,
    pub recollection_of: Option<Uuid>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLUMNS: &str = "id, tenant_id, patient_id, service_request_id, identifier, specimen_type_code, container_code,
    body_site, status, collected_at, collected_by, collection_facility_id, rejection_reason, recollection_of,
    version, created_at, updated_at";

fn from_row(r: &sqlx::postgres::PgRow) -> Result<SpecimenRow, ApiError> {
    let status: String = r.get("status");
    Ok(SpecimenRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        patient_id: r.get("patient_id"),
        service_request_id: r.get("service_request_id"),
        identifier: r.get("identifier"),
        specimen_type_code: r.get("specimen_type_code"),
        container_code: r.get("container_code"),
        body_site: r.get("body_site"),
        status: SpecimenStatus::parse(&status)
            .ok_or_else(|| ApiError::internal(format!("unknown specimen status {status}")))?,
        collected_at: r.get("collected_at"),
        collected_by: r.get("collected_by"),
        collection_facility_id: r.get("collection_facility_id"),
        rejection_reason: r.get("rejection_reason"),
        recollection_of: r.get("recollection_of"),
        version: r.get("version"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    })
}

pub fn specimen_json(s: &SpecimenRow) -> Value {
    json!({
        "id": s.id,
        "service_request_id": s.service_request_id,
        "patient_id": s.patient_id,
        "identifier": s.identifier,
        "specimen_type_code": s.specimen_type_code,
        "container_code": s.container_code,
        "body_site": s.body_site,
        "status": s.status.as_str(),
        "collected_at": s.collected_at,
        "collected_by": s.collected_by,
        "collection_facility_id": s.collection_facility_id,
        "rejection_reason": s.rejection_reason,
        "recollection_of": s.recollection_of,
        "version": s.version,
        "created_at": s.created_at,
        "updated_at": s.updated_at,
    })
}

pub async fn load_specimen(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
    for_update: bool,
) -> Result<SpecimenRow, ApiError> {
    let lock = if for_update { "FOR UPDATE" } else { "" };
    let r = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM specimens WHERE id = $1 AND tenant_id = $2 {lock}"
    ))
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    from_row(&r)
}

async fn events_json(conn: &mut PgConnection, specimen_id: Uuid) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT id, event, from_status, to_status, facility_id, location_note, reason, actor, actor_user_id, recorded_at
         FROM specimen_events WHERE specimen_id = $1 ORDER BY recorded_at, id",
    )
    .bind(specimen_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid, _>("id"),
                "event": r.get::<String, _>("event"),
                "from_status": r.get::<Option<String>, _>("from_status"),
                "to_status": r.get::<String, _>("to_status"),
                "facility_id": r.get::<Option<Uuid>, _>("facility_id"),
                "location_note": r.get::<Option<String>, _>("location_note"),
                "reason": r.get::<Option<String>, _>("reason"),
                "actor": r.get::<String, _>("actor"),
                "actor_user_id": r.get::<Option<Uuid>, _>("actor_user_id"),
                "recorded_at": r.get::<DateTime<Utc>, _>("recorded_at"),
            })
        })
        .collect())
}

/// Specimens of one order with their custody events.
pub async fn list_json(conn: &mut PgConnection, order_id: Uuid) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(&format!(
        "SELECT {COLUMNS} FROM specimens WHERE service_request_id = $1 ORDER BY created_at, id"
    ))
    .bind(order_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let s = from_row(r)?;
        let mut v = specimen_json(&s);
        v["events"] = Value::Array(events_json(conn, s.id).await?);
        out.push(v);
    }
    Ok(out)
}

#[allow(clippy::too_many_arguments)]
async fn record_event(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    s: &SpecimenRow,
    event: &str,
    from: Option<SpecimenStatus>,
    to: SpecimenStatus,
    facility_id: Option<Uuid>,
    location_note: Option<&str>,
    reason: Option<&str>,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO specimen_events
         (id, tenant_id, specimen_id, event, from_status, to_status, facility_id, location_note, reason, actor_user_id, actor)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(Uuid::now_v7())
    .bind(s.tenant_id)
    .bind(s.id)
    .bind(event)
    .bind(from.map(|f| f.as_str()))
    .bind(to.as_str())
    .bind(facility_id)
    .bind(location_note)
    .bind(reason)
    .bind(ctx.user_id)
    .bind(actor_label(ctx))
    .execute(&mut *conn)
    .await?;
    Ok(())
}

async fn facility_of_tenant(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facility_id: Option<Uuid>,
) -> Result<Option<Uuid>, ApiError> {
    let Some(f) = facility_id else {
        return Ok(None);
    };
    let ok: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM facilities WHERE id = $1 AND tenant_id = $2")
            .bind(f)
            .bind(tenant_id)
            .fetch_optional(&mut *conn)
            .await?;
    ok.map(Some).ok_or_else(|| {
        ApiError::bad_request(
            "validation_failed",
            "facility_id is not a facility of this tenant",
        )
    })
}

#[derive(Debug, Deserialize)]
pub struct RecordBody {
    pub identifier: Option<String>,
    pub specimen_type_code: String,
    pub container_code: Option<String>,
    pub body_site: Option<String>,
    /// `true` records the specimen as collected now (the common bedside
    /// path); `false` plans it for later collection.
    #[serde(default = "default_true")]
    pub collected: bool,
    pub collected_at: Option<DateTime<Utc>>,
    pub collection_facility_id: Option<Uuid>,
    pub recollection_of: Option<Uuid>,
    pub location_note: Option<String>,
}

fn default_true() -> bool {
    true
}

fn generated_identifier(order: &OrderRow, now: DateTime<Utc>) -> String {
    // The trailing bytes of a v7 UUID are random; the leading ones are the
    // millisecond clock and repeat for every specimen recorded in the same minute.
    let short: String = Uuid::now_v7().simple().to_string()[24..].to_uppercase();
    format!(
        "SP-{}-{}-{short}",
        now.format("%Y%m%d"),
        order.id.simple().to_string()[..6].to_uppercase()
    )
}

/// `POST /api/v1/diagnostics/orders/:id/specimens`
pub async fn record(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(order_id): Path<Uuid>,
    Json(body): Json<RecordBody>,
) -> Result<Json<Value>, ApiError> {
    let type_code = check_len(
        "specimen_type_code",
        Some(&body.specimen_type_code),
        MAX_SHORT,
    )?
    .ok_or_else(|| ApiError::bad_request("validation_failed", "specimen_type_code is required"))?;
    let identifier = check_len("identifier", body.identifier.as_deref(), MAX_SHORT)?;
    let container = check_len("container_code", body.container_code.as_deref(), MAX_SHORT)?;
    let body_site = check_len("body_site", body.body_site.as_deref(), MAX_SHORT)?;
    let location_note = check_len("location_note", body.location_note.as_deref(), MAX_TEXT)?;
    let mut conn = state.pool.acquire().await?;
    let o = load_order(&mut conn, ctx.tenant_id, order_id).await?;
    drop(conn);
    let allowed = guard_order(&state, &ctx, actions::SPECIMEN_HANDLE, &o).await?;

    let mut tx = state.pool.begin().await?;
    let o = lock_order(&mut tx, ctx.tenant_id, order_id).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    if !matches!(
        o.order_status,
        OrderStatus::Accepted | OrderStatus::Scheduled | OrderStatus::InProgress
    ) {
        return Err(ApiError::conflict(
            "order_not_in_acquisition",
            "specimens can be recorded for accepted, scheduled or in-progress orders only",
        ));
    }
    let facility_id = facility_of_tenant(&mut tx, o.tenant_id, body.collection_facility_id)
        .await?
        .or(o.performing_facility_id)
        .or(Some(o.patient_facility_id));
    if let Some(prev) = body.recollection_of {
        let p = load_specimen(&mut tx, o.tenant_id, prev, true).await?;
        if p.service_request_id != o.id {
            return Err(ApiError::bad_request(
                "validation_failed",
                "recollection_of must reference a specimen of the same order",
            ));
        }
        if p.status != SpecimenStatus::Rejected {
            return Err(ApiError::conflict(
                "specimen_not_rejected",
                "only a rejected specimen can be recollected",
            ));
        }
    }
    let now = Utc::now();
    let collected_at = if body.collected {
        Some(body.collected_at.unwrap_or(now))
    } else {
        None
    };
    if collected_at.is_some_and(|t| t > now + chrono::Duration::minutes(5)) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "collected_at cannot be in the future",
        ));
    }
    let identifier = identifier.unwrap_or_else(|| generated_identifier(&o, now));
    let status = if body.collected {
        SpecimenStatus::Collected
    } else {
        SpecimenStatus::Planned
    };
    let id = Uuid::now_v7();
    let inserted = sqlx::query(
        "INSERT INTO specimens
         (id, tenant_id, patient_id, service_request_id, identifier, specimen_type_code, container_code, body_site,
          status, collected_at, collected_by, collection_facility_id, recollection_of, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",
    )
    .bind(id)
    .bind(o.tenant_id)
    .bind(o.patient_id)
    .bind(o.id)
    .bind(&identifier)
    .bind(&type_code)
    .bind(&container)
    .bind(&body_site)
    .bind(status.as_str())
    .bind(collected_at)
    .bind(collected_at.map(|_| ctx.user_id))
    .bind(facility_id)
    .bind(body.recollection_of)
    .bind(ctx.user_id)
    .execute(&mut *tx)
    .await;
    if let Err(e) = inserted {
        if matches!(&e, sqlx::Error::Database(db) if db.is_unique_violation()) {
            return Err(ApiError::conflict(
                "specimen_identifier_in_use",
                "a specimen with this identifier already exists in this tenant",
            ));
        }
        return Err(e.into());
    }
    let s = load_specimen(&mut tx, o.tenant_id, id, false).await?;
    record_event(
        &mut tx,
        &ctx,
        &s,
        "planned",
        None,
        SpecimenStatus::Planned,
        facility_id,
        location_note.as_deref(),
        None,
    )
    .await?;
    if body.collected {
        record_event(
            &mut tx,
            &ctx,
            &s,
            SpecimenEvent::Collected.as_str(),
            Some(SpecimenStatus::Planned),
            SpecimenStatus::Collected,
            facility_id,
            location_note.as_deref(),
            None,
        )
        .await?;
    }
    // Collecting a specimen is the start of acquisition for orders that are
    // not already in progress (the fulfilment mode is already recorded).
    let mut order = o.clone();
    if body.collected
        && matches!(
            o.order_status,
            OrderStatus::Accepted | OrderStatus::Scheduled
        )
    {
        if o.order_status == OrderStatus::Accepted && o.fulfilment_mode.requires_appointment() {
            return Err(ApiError::conflict(
                "fulfilment_mode_required",
                "this order awaits an appointment; start it with an explicit fulfilment mode before collecting",
            ));
        }
        order = super::apply_transition_in(
            &mut tx,
            &ctx,
            &state,
            &o,
            super::TransitionInput {
                transition: wellos_domain::diagnostics::OrderTransition::Start,
                reason: None,
                actor: &actor_label(&ctx),
                actor_user_id: Some(ctx.user_id),
                details: json!({ "specimen_id": id, "trigger": "specimen_collected" }),
                event: "diagnostic_order.started",
            },
        )
        .await?;
    }
    audit::emit(
        &mut *tx,
        &ctx,
        "specimen.recorded",
        &state.cell,
        json!({ "specimen_id": id, "service_request_id": o.id, "patient_id": o.patient_id,
                "identifier": identifier, "status": status.as_str(), "recollection_of": body.recollection_of }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    let mut v = specimen_json(&s);
    v["events"] = Value::Array(events_json(&mut conn, s.id).await?);
    v["order"] = super::order_json(&order);
    Ok(Json(v))
}

#[derive(Debug, Deserialize)]
pub struct EventBody {
    pub event: String,
    pub version: i64,
    pub facility_id: Option<Uuid>,
    pub location_note: Option<String>,
    pub reason: Option<String>,
}

fn parse_event(s: &str) -> Result<SpecimenEvent, ApiError> {
    SpecimenEvent::parse(s.trim()).ok_or_else(|| {
        ApiError::bad_request(
            "validation_failed",
            "event must be collected, dispatched, received, processing_started, processed, rejected or consumed",
        )
    })
}

/// `POST /api/v1/diagnostics/specimens/:id/events`
pub async fn event(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<EventBody>,
) -> Result<Json<Value>, ApiError> {
    let ev = parse_event(&body.event)?;
    let reason = check_len("reason", body.reason.as_deref(), MAX_TEXT)?;
    let location_note = check_len("location_note", body.location_note.as_deref(), MAX_TEXT)?;
    if ev == SpecimenEvent::Rejected
        && reason.as_deref().map_or(0, |r| r.chars().count()) < MIN_REASON_CHARS
    {
        return Err(ApiError::bad_request(
            "validation_failed",
            "a rejection reason is required",
        ));
    }
    let mut conn = state.pool.acquire().await?;
    let s = load_specimen(&mut conn, ctx.tenant_id, id, false).await?;
    let o = load_order(&mut conn, ctx.tenant_id, s.service_request_id).await?;
    drop(conn);
    let allowed = guard_order(&state, &ctx, actions::SPECIMEN_HANDLE, &o).await?;

    let mut tx = state.pool.begin().await?;
    let s = load_specimen(&mut tx, ctx.tenant_id, id, true).await?;
    if s.version != body.version {
        return Err(ApiError::conflict(
            "version_conflict",
            "the specimen changed since it was displayed; reload it",
        ));
    }
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let next = s
        .status
        .apply(ev)
        .map_err(|e| ApiError::conflict("invalid_specimen_event", e.to_string()))?;
    let facility_id = facility_of_tenant(&mut tx, s.tenant_id, body.facility_id).await?;
    let extra = match ev {
        SpecimenEvent::Collected => "collected_at = now(), collected_by = $4, collection_facility_id = COALESCE($5, collection_facility_id),",
        SpecimenEvent::Rejected => "rejection_reason = $6,",
        _ => "",
    };
    let sql = format!(
        "UPDATE specimens SET status = $1, {extra} version = version + 1, updated_at = now()
         WHERE id = $2 AND version = $3"
    );
    let updated = sqlx::query(&sql)
        .bind(next.as_str())
        .bind(s.id)
        .bind(s.version)
        .bind(ctx.user_id)
        .bind(facility_id)
        .bind(&reason)
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "version_conflict",
            "the specimen was modified concurrently",
        ));
    }
    record_event(
        &mut tx,
        &ctx,
        &s,
        ev.as_str(),
        Some(s.status),
        next,
        facility_id,
        location_note.as_deref(),
        reason.as_deref(),
    )
    .await?;
    if ev == SpecimenEvent::Rejected {
        record_event(
            &mut tx,
            &ctx,
            &s,
            "recollection_requested",
            Some(next),
            next,
            facility_id,
            None,
            reason.as_deref(),
        )
        .await?;
    }
    audit::emit(
        &mut *tx,
        &ctx,
        "specimen.event",
        &state.cell,
        json!({ "specimen_id": s.id, "service_request_id": s.service_request_id, "patient_id": s.patient_id,
                "event": ev.as_str(), "from": s.status.as_str(), "to": next.as_str(), "reason": reason }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let fresh = load_specimen(&mut tx, s.tenant_id, s.id, false).await?;
    tx.commit().await?;
    let mut conn = state.pool.acquire().await?;
    let mut v = specimen_json(&fresh);
    v["events"] = Value::Array(events_json(&mut conn, fresh.id).await?);
    v["recollection_required"] = json!(ev == SpecimenEvent::Rejected);
    Ok(Json(v))
}
