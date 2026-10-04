//! Transport and location coordination.
//!
//! A transport request is a logistics record linked to one confirmed
//! appointment: requirements (accessibility codes), pickup window, vehicle
//! or team resource booked through the shared `resource_bookings` path,
//! responsible operator and status. Exact pickup addresses and live
//! positions are sealed with the location keyring; when no keyring is
//! configured the operation fails closed (`503 encryption_unavailable`)
//! rather than storing plaintext. Live positions carry a TTL and are
//! purged by the scheduling worker.
//!
//! Emergency transport requires an authorized human decision: an emergency
//! request can only be created by a principal holding
//! `transport.coordinate` and records `authorized_by`. Nothing here is
//! reachable by dMind.

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::notify;
use crate::scheduling;
use crate::state::AppState;
use axum::http::StatusCode;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wellos_domain::access::AppointmentStatus;
use wellos_domain::matcher::BookingPlan;

pub const MAX_REQUIREMENTS: usize = 12;
pub const MAX_ADDRESS: usize = 300;
pub const MAX_NOTE: usize = 500;
pub const MAX_AREA_CODE: usize = 32;
/// Pickup windows must be between 5 minutes and 12 hours long.
const MIN_WINDOW_MINUTES: i64 = 5;
const MAX_WINDOW_HOURS: i64 = 12;

pub const STATUSES: &[&str] = &[
    "requested",
    "scheduled",
    "en_route",
    "picked_up",
    "completed",
    "cancelled",
    "failed",
];

/// `transport-request.v1` state machine.
pub fn can_transition(from: &str, to: &str) -> bool {
    matches!(
        (from, to),
        ("requested", "scheduled" | "cancelled" | "failed")
            | ("scheduled", "en_route" | "cancelled" | "failed")
            | ("en_route", "picked_up" | "cancelled" | "failed")
            | ("picked_up", "completed" | "failed")
    )
}

pub fn is_active(status: &str) -> bool {
    matches!(status, "requested" | "scheduled" | "en_route" | "picked_up")
}

/// Statuses during which live positions may be shared.
pub fn location_sharing_open(status: &str) -> bool {
    matches!(status, "scheduled" | "en_route" | "picked_up")
}

#[derive(Debug, Clone, Serialize)]
pub struct TransportRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub patient_id: Uuid,
    pub appointment_id: Uuid,
    pub facility_id: Uuid,
    pub appointment_starts_at: DateTime<Utc>,
    pub status: String,
    pub requirements: Vec<String>,
    pub emergency: bool,
    pub origin_area_code: Option<String>,
    #[serde(skip)]
    pub pickup_address_enc: Option<Vec<u8>>,
    pub pickup_window_start: Option<DateTime<Utc>>,
    pub pickup_window_end: Option<DateTime<Utc>>,
    pub vehicle_resource_id: Option<Uuid>,
    pub vehicle_name: Option<String>,
    pub booking_id: Option<Uuid>,
    pub operator_user_id: Option<Uuid>,
    pub authorized_by: Option<Uuid>,
    pub failure_reason: Option<String>,
    pub version: i64,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const COLUMNS: &str = "t.id, t.tenant_id, t.patient_id, t.appointment_id, a.facility_id,
    a.starts_at AS appointment_starts_at, t.status, t.requirements, t.emergency, t.origin_area_code,
    t.pickup_address_enc, t.pickup_window_start, t.pickup_window_end, t.vehicle_resource_id,
    r.name AS vehicle_name, t.booking_id, t.operator_user_id, t.authorized_by, t.failure_reason,
    t.version, t.created_by, t.created_at, t.updated_at";
const FROM: &str = "FROM transport_requests t
    JOIN appointments a ON a.id = t.appointment_id
    LEFT JOIN schedulable_resources r ON r.id = t.vehicle_resource_id";

fn from_row(r: &sqlx::postgres::PgRow) -> TransportRow {
    TransportRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        patient_id: r.get("patient_id"),
        appointment_id: r.get("appointment_id"),
        facility_id: r.get("facility_id"),
        appointment_starts_at: r.get("appointment_starts_at"),
        status: r.get("status"),
        requirements: r.get("requirements"),
        emergency: r.get("emergency"),
        origin_area_code: r.get("origin_area_code"),
        pickup_address_enc: r.get("pickup_address_enc"),
        pickup_window_start: r.get("pickup_window_start"),
        pickup_window_end: r.get("pickup_window_end"),
        vehicle_resource_id: r.get("vehicle_resource_id"),
        vehicle_name: r.get("vehicle_name"),
        booking_id: r.get("booking_id"),
        operator_user_id: r.get("operator_user_id"),
        authorized_by: r.get("authorized_by"),
        failure_reason: r.get("failure_reason"),
        version: r.get("version"),
        created_by: r.get("created_by"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    }
}

pub async fn load(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<TransportRow, ApiError> {
    let sql = format!("SELECT {COLUMNS} {FROM} WHERE t.id = $1 AND t.tenant_id = $2");
    sqlx::query(&sql)
        .bind(id)
        .bind(tenant_id)
        .fetch_optional(&mut *conn)
        .await?
        .map(|r| from_row(&r))
        .ok_or_else(ApiError::not_found)
}

pub async fn lock(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<TransportRow, ApiError> {
    let sql =
        format!("SELECT {COLUMNS} {FROM} WHERE t.id = $1 AND t.tenant_id = $2 FOR UPDATE OF t");
    sqlx::query(&sql)
        .bind(id)
        .bind(tenant_id)
        .fetch_optional(&mut *conn)
        .await?
        .map(|r| from_row(&r))
        .ok_or_else(ApiError::not_found)
}

pub struct ListFilter<'a> {
    pub facility_ids: Option<&'a [Uuid]>,
    pub patient_id: Option<Uuid>,
    pub appointment_id: Option<Uuid>,
    pub statuses: &'a [String],
    pub operator_user_id: Option<Uuid>,
    pub limit: i64,
}

pub async fn list(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    f: ListFilter<'_>,
) -> Result<Vec<TransportRow>, ApiError> {
    let sql = format!(
        "SELECT {COLUMNS} {FROM}
         WHERE t.tenant_id = $1
           AND ($2::uuid[] IS NULL OR a.facility_id = ANY($2))
           AND ($3::uuid IS NULL OR t.patient_id = $3)
           AND ($4::uuid IS NULL OR t.appointment_id = $4)
           AND (cardinality($5::text[]) = 0 OR t.status = ANY($5))
           AND ($6::uuid IS NULL OR t.operator_user_id = $6)
         ORDER BY COALESCE(t.pickup_window_start, a.starts_at), t.id
         LIMIT $7"
    );
    Ok(sqlx::query(&sql)
        .bind(tenant_id)
        .bind(f.facility_ids)
        .bind(f.patient_id)
        .bind(f.appointment_id)
        .bind(f.statuses)
        .bind(f.operator_user_id)
        .bind(f.limit)
        .fetch_all(&mut *conn)
        .await?
        .iter()
        .map(from_row)
        .collect())
}

/// Logistics view; never includes the address or any clinical field.
pub fn transport_json(t: &TransportRow) -> Value {
    json!({
        "id": t.id,
        "patient_id": t.patient_id,
        "appointment_id": t.appointment_id,
        "facility_id": t.facility_id,
        "appointment_starts_at": t.appointment_starts_at,
        "status": t.status,
        "requirements": t.requirements,
        "emergency": t.emergency,
        "origin_area_code": t.origin_area_code,
        "has_pickup_address": t.pickup_address_enc.is_some(),
        "pickup_window_start": t.pickup_window_start,
        "pickup_window_end": t.pickup_window_end,
        "vehicle_resource_id": t.vehicle_resource_id,
        "vehicle_name": t.vehicle_name,
        "operator_user_id": t.operator_user_id,
        "authorized_by": t.authorized_by,
        "failure_reason": t.failure_reason,
        "location_sharing_open": location_sharing_open(&t.status),
        "version": t.version,
        "created_at": t.created_at,
        "updated_at": t.updated_at,
    })
}

pub async fn history_json(conn: &mut PgConnection, id: Uuid) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT from_status, to_status, note, actor, recorded_at
         FROM transport_request_history WHERE transport_request_id = $1
         ORDER BY recorded_at, id",
    )
    .bind(id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "from_status": r.get::<Option<String>, _>("from_status"),
                "to_status": r.get::<String, _>("to_status"),
                "note": r.get::<Option<String>, _>("note"),
                "actor": r.get::<String, _>("actor"),
                "recorded_at": r.get::<DateTime<Utc>, _>("recorded_at"),
            })
        })
        .collect())
}

fn encryption_unavailable() -> ApiError {
    ApiError::new(
        StatusCode::SERVICE_UNAVAILABLE,
        "encryption_unavailable",
        "exact locations cannot be stored: location encryption is not configured",
    )
}

fn keyring(state: &AppState) -> Result<&crate::crypto::Keyring, ApiError> {
    state
        .runtime
        .location
        .keyring
        .as_ref()
        .ok_or_else(encryption_unavailable)
}

fn validate_window(
    start: Option<DateTime<Utc>>,
    end: Option<DateTime<Utc>>,
    appointment_starts_at: DateTime<Utc>,
) -> Result<(), ApiError> {
    match (start, end) {
        (None, None) => Ok(()),
        (Some(s), Some(e)) => {
            let len = e - s;
            if len < Duration::minutes(MIN_WINDOW_MINUTES)
                || len > Duration::hours(MAX_WINDOW_HOURS)
            {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    "pickup window must be between 5 minutes and 12 hours long",
                ));
            }
            if s > appointment_starts_at {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    "pickup window must start before the appointment",
                ));
            }
            Ok(())
        }
        _ => Err(ApiError::bad_request(
            "validation_failed",
            "pickup_window_start and pickup_window_end must be given together",
        )),
    }
}

fn valid_area_code(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | ' ' | '.'))
}

async fn append_history(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
    from: Option<&str>,
    to: &str,
    note: Option<&str>,
    actor: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO transport_request_history
         (id, tenant_id, transport_request_id, from_status, to_status, note, actor)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant_id)
    .bind(id)
    .bind(from)
    .bind(to)
    .bind(note)
    .bind(actor)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct CreateInput {
    pub appointment_id: Uuid,
    #[serde(default)]
    pub requirements: Vec<String>,
    #[serde(default)]
    pub emergency: bool,
    pub origin_area_code: Option<String>,
    /// Exact pickup address; sealed at rest, never returned in lists.
    pub pickup_address: Option<String>,
    pub pickup_window_start: Option<DateTime<Utc>>,
    pub pickup_window_end: Option<DateTime<Utc>>,
    pub note: Option<String>,
}

/// Create a request for a confirmed appointment. `staff` controls whether
/// an emergency flag is accepted (human coordinator only). Transport
/// consent must be active for the patient.
pub async fn create(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    input: CreateInput,
    staff: bool,
) -> Result<TransportRow, ApiError> {
    let a = scheduling::lock_appointment(tx, input.appointment_id).await?;
    if a.tenant_id != ctx.tenant_id {
        return Err(ApiError::not_found());
    }
    if !matches!(a.status, AppointmentStatus::Confirmed) {
        return Err(ApiError::conflict(
            "appointment_not_confirmed",
            "transport can only be arranged for a confirmed appointment",
        ));
    }
    if !scheduling::consent_active(tx, a.tenant_id, a.patient_id, scheduling::CONSENT_TRANSPORT)
        .await?
    {
        return Err(scheduling::consent_required(scheduling::CONSENT_TRANSPORT));
    }
    if input.emergency && !staff {
        return Err(ApiError::forbidden(
            "emergency transport requires an authorized coordinator decision",
        ));
    }
    let existing: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM transport_requests WHERE appointment_id = $1
           AND status IN ('requested','scheduled','en_route','picked_up') LIMIT 1",
    )
    .bind(a.id)
    .fetch_optional(&mut **tx)
    .await?;
    if existing.is_some() {
        return Err(ApiError::conflict(
            "transport_exists",
            "this appointment already has an active transport request",
        ));
    }
    if input.requirements.len() > MAX_REQUIREMENTS {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("requirements exceeds {MAX_REQUIREMENTS} entries"),
        ));
    }
    let known = scheduling::active_codes(tx, a.tenant_id, "accessibility_capability").await?;
    let mut requirements: Vec<String> = Vec::new();
    for code in input.requirements {
        if !known.contains(&code) {
            return Err(ApiError::bad_request(
                "unknown_accessibility_code",
                "requirements contains an unknown accessibility capability",
            ));
        }
        if !requirements.contains(&code) {
            requirements.push(code);
        }
    }
    let area = scheduling::clean_text(input.origin_area_code, "origin_area_code", MAX_AREA_CODE)?;
    if let Some(a) = &area {
        if !valid_area_code(a) {
            return Err(ApiError::bad_request(
                "validation_failed",
                "origin_area_code is not valid",
            ));
        }
    }
    validate_window(
        input.pickup_window_start,
        input.pickup_window_end,
        a.starts_at,
    )?;
    let address = scheduling::clean_text(input.pickup_address, "pickup_address", MAX_ADDRESS)?;
    let address_enc = match &address {
        Some(addr) => Some(keyring(state)?.seal(addr.as_bytes())),
        None => None,
    };
    let note = scheduling::clean_text(input.note, "note", MAX_NOTE)?;
    let id = Uuid::now_v7();
    let authorized_by = input.emergency.then_some(ctx.user_id);
    sqlx::query(
        "INSERT INTO transport_requests
         (id, tenant_id, patient_id, appointment_id, status, requirements, emergency,
          origin_area_code, pickup_address_enc, pickup_window_start, pickup_window_end,
          authorized_by, created_by)
         VALUES ($1,$2,$3,$4,'requested',$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(id)
    .bind(a.tenant_id)
    .bind(a.patient_id)
    .bind(a.id)
    .bind(&requirements)
    .bind(input.emergency)
    .bind(&area)
    .bind(&address_enc)
    .bind(input.pickup_window_start)
    .bind(input.pickup_window_end)
    .bind(authorized_by)
    .bind(ctx.user_id)
    .execute(&mut **tx)
    .await?;
    let actor = scheduling::actor_label(ctx);
    append_history(
        tx,
        a.tenant_id,
        id,
        None,
        "requested",
        note.as_deref(),
        &actor,
    )
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "transport.requested",
        &state.cell,
        json!({
            "transport_request_id": id,
            "appointment_id": a.id,
            "patient_id": a.patient_id,
            "facility_id": a.facility_id,
            "emergency": input.emergency,
            "authorized_by": authorized_by,
            "requirements": requirements,
            "has_pickup_address": address_enc.is_some(),
            "channel": if staff { "staff" } else { "self_service" },
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let policy = scheduling::load_policy(tx, a.tenant_id).await?;
    notify::schedule_transport_status(
        tx,
        ctx,
        state,
        a.tenant_id,
        a.patient_id,
        a.id,
        id,
        "requested",
        input.pickup_window_start,
        &policy,
    )
    .await?;
    load(tx, a.tenant_id, id).await
}

#[derive(Debug, Clone, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct TransitionInput {
    pub status: String,
    pub version: Option<i64>,
    pub vehicle_resource_id: Option<Uuid>,
    pub operator_user_id: Option<Uuid>,
    pub pickup_window_start: Option<DateTime<Utc>>,
    pub pickup_window_end: Option<DateTime<Utc>>,
    /// Mandatory for `cancelled` and `failed`.
    pub reason: Option<String>,
    pub note: Option<String>,
}

/// Whether `code` is a transport-capable resource type in this tenant:
/// either flagged in its catalog config (`{"transport": true}`) or one of
/// the conventional built-in codes.
async fn transport_type(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    code: &str,
) -> Result<bool, ApiError> {
    if matches!(
        code,
        "vehicle" | "accessible_vehicle" | "ambulance" | "home_visit_team"
    ) {
        return Ok(true);
    }
    let flagged: Option<bool> = sqlx::query_scalar(
        "SELECT (config->>'transport')::boolean FROM catalog_entries
         WHERE tenant_id = $1 AND kind = 'resource_type' AND code = $2 AND active",
    )
    .bind(tenant_id)
    .bind(code)
    .fetch_optional(&mut *conn)
    .await?
    .flatten();
    Ok(flagged.unwrap_or(false))
}

/// Move a request through the state machine. Scheduling books the vehicle
/// over the pickup window through the shared booking path (exclusion
/// constraint, capacity slots); cancellation/failure releases it.
pub async fn transition(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    id: Uuid,
    input: TransitionInput,
    staff: bool,
) -> Result<TransportRow, ApiError> {
    let t = lock(tx, ctx.tenant_id, id).await?;
    if let Some(v) = input.version {
        if v != t.version {
            return Err(ApiError::conflict(
                "version_conflict",
                "the transport request changed; reload and retry",
            ));
        }
    }
    let to = input.status.as_str();
    if !STATUSES.contains(&to) {
        return Err(ApiError::bad_request("validation_failed", "unknown status"));
    }
    if !can_transition(&t.status, to) {
        return Err(ApiError::conflict(
            "invalid_transition",
            format!(
                "a transport request in status {} cannot become {to}",
                t.status
            ),
        ));
    }
    if !staff && to != "cancelled" {
        return Err(ApiError::forbidden(
            "patients may only cancel their transport request",
        ));
    }
    if !staff && !matches!(t.status.as_str(), "requested" | "scheduled") {
        return Err(ApiError::conflict(
            "invalid_transition",
            "a transport already under way can only be cancelled by the coordinator",
        ));
    }
    let reason = scheduling::clean_text(input.reason, "reason", MAX_NOTE)?;
    if matches!(to, "cancelled" | "failed") && reason.is_none() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "reason is required to cancel or fail a transport request",
        ));
    }
    let note = scheduling::clean_text(input.note, "note", MAX_NOTE)?;
    let mut window_start = t.pickup_window_start;
    let mut window_end = t.pickup_window_end;
    let mut vehicle = t.vehicle_resource_id;
    let mut booking_id = t.booking_id;
    let mut operator = t.operator_user_id;
    if to == "scheduled" {
        if input.pickup_window_start.is_some() || input.pickup_window_end.is_some() {
            validate_window(
                input.pickup_window_start,
                input.pickup_window_end,
                t.appointment_starts_at,
            )?;
            window_start = input.pickup_window_start;
            window_end = input.pickup_window_end;
        }
        let (Some(ws), Some(we)) = (window_start, window_end) else {
            return Err(ApiError::bad_request(
                "validation_failed",
                "a pickup window is required to schedule transport",
            ));
        };
        let vehicle_id = input.vehicle_resource_id.ok_or_else(|| {
            ApiError::bad_request(
                "validation_failed",
                "vehicle_resource_id is required to schedule transport",
            )
        })?;
        let res = sqlx::query(
            "SELECT resource_type_code, active FROM schedulable_resources
             WHERE id = $1 AND tenant_id = $2",
        )
        .bind(vehicle_id)
        .bind(ctx.tenant_id)
        .fetch_optional(&mut **tx)
        .await?
        .ok_or_else(|| {
            ApiError::bad_request("unknown_resource", "vehicle_resource_id is unknown")
        })?;
        let type_code: String = res.get("resource_type_code");
        let active: bool = res.get("active");
        if !active || !transport_type(tx, ctx.tenant_id, &type_code).await? {
            return Err(ApiError::bad_request(
                "not_transport_resource",
                "the resource is not an active transport resource",
            ));
        }
        if let Some(op) = input.operator_user_id {
            let ok: Option<Uuid> = sqlx::query_scalar(
                "SELECT id FROM users WHERE id = $1 AND tenant_id = $2 AND NOT is_service",
            )
            .bind(op)
            .bind(ctx.tenant_id)
            .fetch_optional(&mut **tx)
            .await?;
            if ok.is_none() {
                return Err(ApiError::bad_request(
                    "unknown_operator",
                    "operator_user_id is not a user of this tenant",
                ));
            }
            operator = Some(op);
        }
        let held = scheduling::book_plans(
            tx,
            ctx.tenant_id,
            &[BookingPlan {
                resource_id: vehicle_id,
                role: "transport".into(),
                slot_index: 0,
                start: ws,
                end: we,
            }],
            "appointment",
            None,
            Some(t.appointment_id),
            None,
            ctx.user_id,
        )
        .await
        .map_err(|e| {
            if e.code == "slot_taken" {
                ApiError::conflict(
                    "vehicle_unavailable",
                    "the vehicle is already booked over this pickup window",
                )
            } else {
                e
            }
        })?;
        vehicle = Some(vehicle_id);
        booking_id = held.first().map(|h| h.booking_id);
    }
    if matches!(to, "cancelled" | "failed" | "completed") {
        if let Some(b) = booking_id {
            sqlx::query(
                "UPDATE resource_bookings SET status = 'released', released_at = now()
                 WHERE id = $1 AND status = 'active'",
            )
            .bind(b)
            .execute(&mut **tx)
            .await?;
        }
        // Live positions end with the episode.
        sqlx::query("DELETE FROM transport_live_locations WHERE transport_request_id = $1")
            .bind(t.id)
            .execute(&mut **tx)
            .await?;
    }
    if staff && to != "scheduled" {
        if let Some(op) = input.operator_user_id {
            operator = Some(op);
        }
    }
    sqlx::query(
        "UPDATE transport_requests
         SET status = $2, pickup_window_start = $3, pickup_window_end = $4,
             vehicle_resource_id = $5, booking_id = $6, operator_user_id = $7,
             failure_reason = CASE WHEN $2 IN ('cancelled','failed') THEN $8 ELSE failure_reason END,
             version = version + 1, updated_at = now()
         WHERE id = $1",
    )
    .bind(t.id)
    .bind(to)
    .bind(window_start)
    .bind(window_end)
    .bind(vehicle)
    .bind(booking_id)
    .bind(operator)
    .bind(&reason)
    .execute(&mut **tx)
    .await?;
    let actor = scheduling::actor_label(ctx);
    let history_note = reason.clone().or(note);
    append_history(
        tx,
        t.tenant_id,
        t.id,
        Some(&t.status),
        to,
        history_note.as_deref(),
        &actor,
    )
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "transport.status_changed",
        &state.cell,
        json!({
            "transport_request_id": t.id,
            "appointment_id": t.appointment_id,
            "from": t.status,
            "to": to,
            "vehicle_resource_id": vehicle,
            "operator_user_id": operator,
            "reason": reason,
            "channel": if staff { "staff" } else { "self_service" },
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let policy = scheduling::load_policy(tx, t.tenant_id).await?;
    notify::schedule_transport_status(
        tx,
        ctx,
        state,
        t.tenant_id,
        t.patient_id,
        t.appointment_id,
        t.id,
        to,
        window_start,
        &policy,
    )
    .await?;
    load(tx, t.tenant_id, t.id).await
}

/// Cancel active transport when its appointment closes (same transaction
/// as the appointment transition). Returns the number of requests closed.
pub async fn cancel_for_appointment(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    appointment_id: Uuid,
    reason: &str,
) -> Result<usize, ApiError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM transport_requests WHERE appointment_id = $1
           AND status IN ('requested','scheduled','en_route','picked_up')",
    )
    .bind(appointment_id)
    .fetch_all(&mut **tx)
    .await?;
    for id in &ids {
        transition(
            tx,
            ctx,
            state,
            *id,
            TransitionInput {
                status: "cancelled".into(),
                reason: Some(reason.to_string()),
                ..Default::default()
            },
            true,
        )
        .await?;
    }
    Ok(ids.len())
}

/// Decrypt the pickup address for an authorized coordinator; audited as a
/// sensitive read by the caller.
pub fn open_address(state: &AppState, t: &TransportRow) -> Result<Option<String>, ApiError> {
    let Some(enc) = &t.pickup_address_enc else {
        return Ok(None);
    };
    let plain = keyring(state)?.open(enc).map_err(|_| {
        ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "address_undecryptable",
            "the stored address cannot be opened with the configured keys",
        )
    })?;
    Ok(Some(String::from_utf8_lossy(&plain).into_owned()))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LivePosition {
    pub latitude: f64,
    pub longitude: f64,
    pub recorded_at: DateTime<Utc>,
}

/// Store one sealed live position with the configured TTL. Allowed only
/// while the episode is active for sharing; fails closed without a keyring.
pub async fn share_location(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    t: &TransportRow,
    latitude: f64,
    longitude: f64,
) -> Result<DateTime<Utc>, ApiError> {
    if !(-90.0..=90.0).contains(&latitude) || !(-180.0..=180.0).contains(&longitude) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "latitude/longitude out of range",
        ));
    }
    if !location_sharing_open(&t.status) {
        return Err(ApiError::conflict(
            "sharing_closed",
            "live location can only be shared during an active transport episode",
        ));
    }
    let ring = keyring(state)?;
    let now = Utc::now();
    let ttl = Duration::from_std(state.runtime.location.live_location_ttl)
        .unwrap_or_else(|_| Duration::minutes(15));
    let expires_at = now + ttl;
    let pos = LivePosition {
        latitude,
        longitude,
        recorded_at: now,
    };
    let sealed = ring.seal(&serde_json::to_vec(&pos).map_err(ApiError::internal)?);
    sqlx::query(
        "INSERT INTO transport_live_locations
         (id, tenant_id, transport_request_id, coordinates_enc, shared_by, recorded_at, expires_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(Uuid::now_v7())
    .bind(t.tenant_id)
    .bind(t.id)
    .bind(&sealed)
    .bind(ctx.user_id)
    .bind(now)
    .bind(expires_at)
    .execute(&mut **tx)
    .await?;
    // Keep only the newest position per sharer inside the episode.
    sqlx::query(
        "DELETE FROM transport_live_locations
         WHERE transport_request_id = $1 AND shared_by = $2 AND recorded_at < $3",
    )
    .bind(t.id)
    .bind(ctx.user_id)
    .bind(now)
    .execute(&mut **tx)
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "transport.location.shared",
        &state.cell,
        json!({
            "transport_request_id": t.id,
            "expires_at": expires_at,
            "key_id": crate::crypto::Keyring::key_id_of(&sealed),
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(expires_at)
}

#[derive(Debug, Clone, Serialize)]
pub struct SharedPosition {
    pub shared_by: Uuid,
    pub role: &'static str,
    pub latitude: f64,
    pub longitude: f64,
    pub recorded_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

/// Newest unexpired position per sharer, decrypted. The caller audits the
/// read (`transport.location.read`).
pub async fn live_positions(
    conn: &mut PgConnection,
    state: &AppState,
    t: &TransportRow,
) -> Result<Vec<SharedPosition>, ApiError> {
    let rows = sqlx::query(
        "SELECT DISTINCT ON (shared_by) shared_by, coordinates_enc, expires_at
         FROM transport_live_locations
         WHERE transport_request_id = $1 AND expires_at > now()
         ORDER BY shared_by, recorded_at DESC",
    )
    .bind(t.id)
    .fetch_all(&mut *conn)
    .await?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let ring = keyring(state)?;
    let mut out = Vec::with_capacity(rows.len());
    for r in &rows {
        let enc: Vec<u8> = r.get("coordinates_enc");
        let Ok(plain) = ring.open(&enc) else {
            continue;
        };
        let Ok(pos) = serde_json::from_slice::<LivePosition>(&plain) else {
            continue;
        };
        let shared_by: Uuid = r.get("shared_by");
        out.push(SharedPosition {
            shared_by,
            role: if t.operator_user_id == Some(shared_by) {
                "operator"
            } else {
                "patient_side"
            },
            latitude: pos.latitude,
            longitude: pos.longitude,
            recorded_at: pos.recorded_at,
            expires_at: r.get("expires_at"),
        });
    }
    Ok(out)
}

/// Delete expired live positions and positions of closed episodes. Runs
/// from the scheduling worker on every tick; audited per tenant.
pub async fn purge_expired_locations(state: &AppState, worker_id: &str) -> Result<u64, ApiError> {
    let rows = sqlx::query(
        "WITH gone AS (
             DELETE FROM transport_live_locations l
             USING transport_requests t
             WHERE t.id = l.transport_request_id
               AND (l.expires_at <= now()
                    OR t.status IN ('completed','cancelled','failed'))
             RETURNING l.tenant_id
         )
         SELECT tenant_id, count(*)::bigint AS n FROM gone GROUP BY tenant_id",
    )
    .fetch_all(&state.pool)
    .await?;
    let mut total = 0u64;
    for r in &rows {
        let tenant_id: Uuid = r.get("tenant_id");
        let n: i64 = r.get("n");
        total += n as u64;
        let ctx = notify::system_context(tenant_id, worker_id);
        audit::record(
            &state.pool,
            &ctx,
            "transport.location.purged",
            Some("transport_live_locations"),
            None,
            "allow",
            Some(&format!("{n} expired positions deleted")),
        )
        .await
        .map_err(ApiError::internal)?;
    }
    Ok(total)
}
