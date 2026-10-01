//! Waitlist entries and cancellation-recovery events.
//!
//! Joining is an opt-in: the entry is created together with an active
//! `waitlist_offers` consent decision, and the deterministic eligibility of
//! `crate::recovery` only ever considers consented, active entries. Staff
//! may enrol a patient only after attesting that the patient agreed
//! (`consent_confirmed`), which is audited as such.
//!
//! Recovery events expose the deterministic eligibility, every exclusion
//! reason, the (optional) dMind ranking and the offer cascade. Staff may
//! move one eligible patient to the front with a mandatory reason; urgency
//! floors still apply and the override is audited. Nothing here books an
//! appointment: acceptance is the shared offer → `confirm_offer` path.

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, facility_scope, ResourceCtx};
use crate::ratelimit;
use crate::recovery::{self, EventRow};
use crate::routes::access;
use crate::routes::consent;
use crate::routes::extract::OptionalJson;
use crate::routes::guard;
use crate::scheduling;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wellos_domain::access::{OfferStatus, OfferTransition, Urgency, WeeklyWindow};
use wellos_domain::recovery::{WaitlistStatus, WaitlistTransition};

const MAX_LIST: usize = 30;
const MAX_WINDOWS: usize = 21;
const MAX_REASON: usize = 500;
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/waitlist", get(list_entries).post(create_entry))
        .route("/waitlist/:id", get(get_entry))
        .route("/waitlist/:id/pause", post(pause_entry))
        .route("/waitlist/:id/resume", post(resume_entry))
        .route("/waitlist/:id/leave", post(leave_entry))
        .route("/recovery-events", get(list_events))
        .route("/recovery-events/:id", get(get_event))
        .route("/recovery-events/:id/rank", post(rank_event))
        .route("/recovery-events/:id/override", post(override_event))
        .route("/recovery-events/:id/revoke-offer", post(revoke_offer))
        .route("/recovery-events/:id/close", post(close_event))
}

// ---------------------------------------------------------------------------
// Entries
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct EntryRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub patient_id: Uuid,
    pub service_code: String,
    pub facility_ids: Vec<Uuid>,
    pub modality_codes: Vec<String>,
    pub acceptable_windows: Vec<WeeklyWindow>,
    pub earliest: Option<DateTime<Utc>>,
    pub latest: Option<DateTime<Utc>>,
    pub min_notice_hours: i32,
    pub status: WaitlistStatus,
    pub urgency: Urgency,
    pub urgency_source: String,
    pub access_request_id: Option<Uuid>,
    pub current_appointment_id: Option<Uuid>,
    pub fulfilled_appointment_id: Option<Uuid>,
    pub offers_declined: i32,
    pub joined_at: DateTime<Utc>,
    pub paused_at: Option<DateTime<Utc>>,
    pub left_at: Option<DateTime<Utc>>,
    pub version: i64,
    pub updated_at: DateTime<Utc>,
}

const ENTRY_COLUMNS: &str = "id, tenant_id, patient_id, service_code, facility_ids, modality_codes,
    acceptable_windows, earliest, latest, min_notice_hours, status, urgency, urgency_source,
    access_request_id, current_appointment_id, fulfilled_appointment_id, offers_declined,
    joined_at, paused_at, left_at, version, updated_at";

fn entry_from_row(r: &sqlx::postgres::PgRow) -> Result<EntryRow, ApiError> {
    let status: String = r.get("status");
    Ok(EntryRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        patient_id: r.get("patient_id"),
        service_code: r.get("service_code"),
        facility_ids: r.get("facility_ids"),
        modality_codes: r.get("modality_codes"),
        acceptable_windows: serde_json::from_value(r.get::<Value, _>("acceptable_windows"))
            .map_err(ApiError::internal)?,
        earliest: r.get("earliest"),
        latest: r.get("latest"),
        min_notice_hours: r.get("min_notice_hours"),
        status: WaitlistStatus::parse(&status)
            .ok_or_else(|| ApiError::internal(format!("unknown waitlist status {status}")))?,
        urgency: scheduling::urgency_of(&r.get::<String, _>("urgency")),
        urgency_source: r.get("urgency_source"),
        access_request_id: r.get("access_request_id"),
        current_appointment_id: r.get("current_appointment_id"),
        fulfilled_appointment_id: r.get("fulfilled_appointment_id"),
        offers_declined: r.get("offers_declined"),
        joined_at: r.get("joined_at"),
        paused_at: r.get("paused_at"),
        left_at: r.get("left_at"),
        version: r.get("version"),
        updated_at: r.get("updated_at"),
    })
}

/// Tenant-scoped load; foreign entries are indistinguishable from unknown.
pub async fn load_entry(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<EntryRow, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {ENTRY_COLUMNS} FROM waitlist_entries WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    entry_from_row(&row)
}

async fn lock_entry(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<EntryRow, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {ENTRY_COLUMNS} FROM waitlist_entries WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    ))
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    entry_from_row(&row)
}

pub fn entry_json(e: &EntryRow) -> Value {
    json!({
        "id": e.id,
        "patient_id": e.patient_id,
        "service_code": e.service_code,
        "facility_ids": e.facility_ids,
        "modality_codes": e.modality_codes,
        "acceptable_windows": e.acceptable_windows,
        "earliest": e.earliest,
        "latest": e.latest,
        "min_notice_hours": e.min_notice_hours,
        "status": e.status.as_str(),
        "urgency": e.urgency.as_str(),
        "urgency_source": e.urgency_source,
        "access_request_id": e.access_request_id,
        "current_appointment_id": e.current_appointment_id,
        "fulfilled_appointment_id": e.fulfilled_appointment_id,
        "offers_declined": e.offers_declined,
        "joined_at": e.joined_at,
        "paused_at": e.paused_at,
        "left_at": e.left_at,
        "version": e.version,
        "updated_at": e.updated_at,
    })
}

/// The live recovery offer (if any) addressed to this entry.
pub async fn current_offer_json(
    conn: &mut PgConnection,
    e: &EntryRow,
) -> Result<Option<Value>, ApiError> {
    let id: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM appointment_offers
         WHERE tenant_id = $1 AND waitlist_entry_id = $2 AND status IN ('offered','held')
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(e.tenant_id)
    .bind(e.id)
    .fetch_optional(&mut *conn)
    .await?;
    match id {
        Some(id) => Ok(Some(scheduling::offer_json(
            &scheduling::load_offer(conn, id).await?,
        ))),
        None => Ok(None),
    }
}

fn entry_ctx(e: &EntryRow) -> ResourceCtx {
    ResourceCtx {
        tenant_id: e.tenant_id,
        patient_id: Some(e.patient_id),
        facility_id: None,
    }
}

/// Preferences of a waitlist entry as supplied by the patient or staff.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JoinInput {
    pub service_code: String,
    #[serde(default)]
    pub facility_ids: Vec<Uuid>,
    #[serde(default)]
    pub modality_codes: Vec<String>,
    #[serde(default)]
    pub acceptable_windows: Vec<WeeklyWindow>,
    pub earliest: Option<DateTime<Utc>>,
    pub latest: Option<DateTime<Utc>>,
    pub min_notice_hours: Option<i32>,
    pub access_request_id: Option<Uuid>,
    pub current_appointment_id: Option<Uuid>,
}

/// Create the entry for `patient_id` and record the opt-in consent in the
/// same transaction. `urgency` is never taken from the caller: it is the
/// deterministic/human urgency of the linked access request, or routine.
pub async fn join_for(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    patient_id: Uuid,
    input: JoinInput,
    by_patient: bool,
) -> Result<EntryRow, ApiError> {
    let tenant_id = ctx.tenant_id;
    let service = scheduling::load_service(tx, tenant_id, input.service_code.trim()).await?;
    let modality_codes = scheduling::clean_codes(input.modality_codes, "modality_codes", MAX_LIST)?;
    scheduling::require_codes(tx, tenant_id, "modality", &modality_codes, "modality_codes").await?;
    let mut facility_ids = input.facility_ids;
    facility_ids.sort();
    facility_ids.dedup();
    if facility_ids.len() > MAX_LIST {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("facility_ids exceeds {MAX_LIST} entries"),
        ));
    }
    if !facility_ids.is_empty() {
        let known: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM facilities WHERE tenant_id = $1 AND id = ANY($2)",
        )
        .bind(tenant_id)
        .bind(&facility_ids)
        .fetch_one(&mut **tx)
        .await?;
        if known as usize != facility_ids.len() {
            return Err(ApiError::bad_request(
                "validation_failed",
                "facility_ids contains an unknown facility",
            ));
        }
    }
    if input.acceptable_windows.len() > MAX_WINDOWS {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("acceptable_windows exceeds {MAX_WINDOWS} entries"),
        ));
    }
    scheduling::validate_windows(&input.acceptable_windows, "acceptable_windows")?;
    if let (Some(e), Some(l)) = (input.earliest, input.latest) {
        if l <= e {
            return Err(ApiError::bad_request(
                "validation_failed",
                "latest must be after earliest",
            ));
        }
    }
    let min_notice = input.min_notice_hours.unwrap_or(2);
    if !(0..=720).contains(&min_notice) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "min_notice_hours must be between 0 and 720",
        ));
    }
    // Urgency comes from the linked access request (deterministic or human
    // triage) and is never client-supplied.
    let (urgency, urgency_source) = match input.access_request_id {
        Some(req_id) => {
            let r = access::load_request(tx, tenant_id, req_id).await?;
            if r.patient_id != patient_id {
                return Err(ApiError::not_found());
            }
            let source = match r.urgency_source.as_str() {
                "human" => "human",
                "deterministic" => "deterministic",
                _ => "default",
            };
            (r.urgency, source)
        }
        None => (Urgency::Routine, "default"),
    };
    if let Some(appt_id) = input.current_appointment_id {
        let a = scheduling::load_appointment(tx, appt_id).await?;
        if a.tenant_id != tenant_id || a.patient_id != patient_id {
            return Err(ApiError::not_found());
        }
    }
    let duplicate: Option<Uuid> = sqlx::query_scalar(
        "SELECT id FROM waitlist_entries
         WHERE tenant_id = $1 AND patient_id = $2 AND service_code = $3
           AND status IN ('active','paused','offered')
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .bind(&service.code)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some(existing) = duplicate {
        return Err(ApiError::conflict(
            "already_on_waitlist",
            format!("the patient already has a live waitlist entry {existing} for this service"),
        ));
    }
    consent::append_consent(
        tx,
        ctx,
        &state.cell,
        tenant_id,
        patient_id,
        scheduling::CONSENT_WAITLIST,
        "active",
    )
    .await?;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO waitlist_entries (id, tenant_id, patient_id, service_code, facility_ids,
             modality_codes, acceptable_windows, earliest, latest, min_notice_hours, urgency,
             urgency_source, access_request_id, current_appointment_id, created_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(patient_id)
    .bind(&service.code)
    .bind(&facility_ids)
    .bind(&modality_codes)
    .bind(serde_json::to_value(&input.acceptable_windows).map_err(ApiError::internal)?)
    .bind(input.earliest)
    .bind(input.latest)
    .bind(min_notice)
    .bind(urgency.as_str())
    .bind(urgency_source)
    .bind(input.access_request_id)
    .bind(input.current_appointment_id)
    .bind(ctx.user_id)
    .execute(&mut **tx)
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "waitlist.joined",
        &state.cell,
        json!({ "waitlist_entry_id": id, "patient_id": patient_id, "service_code": service.code,
                "by_patient": by_patient, "urgency": urgency.as_str() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    load_entry(tx, tenant_id, id).await
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryActionBody {
    pub version: Option<i64>,
    pub reason: Option<String>,
}

/// Pause, resume or leave. Leaving while an offer is live declines that
/// offer first so the cascade continues with the next eligible patient;
/// pausing while offered is refused (decline or accept the offer first).
pub async fn transition_entry_for(
    state: &AppState,
    ctx: &AuthContext,
    entry: &EntryRow,
    t: WaitlistTransition,
    body: EntryActionBody,
    by_patient: bool,
) -> Result<EntryRow, ApiError> {
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Scheduling).await?;
    let reason = scheduling::clean_text(body.reason, "reason", MAX_REASON)?;
    let mut tx = state.pool.begin().await?;
    let e = transition_entry_in(
        &mut tx,
        ctx,
        state,
        entry.tenant_id,
        entry.id,
        body.version,
        t,
        reason,
        by_patient,
    )
    .await?;
    tx.commit().await?;
    Ok(e)
}

/// Transaction-scoped core of [`transition_entry_for`]; the caller owns the
/// transaction (route wrappers, fixtures).
#[allow(clippy::too_many_arguments)]
pub async fn transition_entry_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    tenant_id: Uuid,
    entry_id: Uuid,
    version: Option<i64>,
    t: WaitlistTransition,
    reason: Option<String>,
    by_patient: bool,
) -> Result<EntryRow, ApiError> {
    let e = lock_entry(tx, tenant_id, entry_id).await?;
    if let Some(v) = version {
        if v != e.version {
            return Err(ApiError::conflict(
                "stale_version",
                "the waitlist entry changed; reload and retry",
            ));
        }
    }
    let mut from = e.status;
    if t == WaitlistTransition::Leave && from == WaitlistStatus::Offered {
        let live: Option<Uuid> = sqlx::query_scalar(
            "SELECT id FROM appointment_offers
             WHERE tenant_id = $1 AND waitlist_entry_id = $2 AND status IN ('offered','held')
             ORDER BY created_at DESC LIMIT 1 FOR UPDATE",
        )
        .bind(e.tenant_id)
        .bind(e.id)
        .fetch_optional(&mut **tx)
        .await?;
        if let Some(offer_id) = live {
            let o = scheduling::lock_offer(tx, offer_id).await?;
            if matches!(o.status, OfferStatus::Offered | OfferStatus::Held) {
                scheduling::release_offer_bookings(tx, o.id).await?;
                scheduling::transition_offer(
                    tx,
                    &o,
                    OfferTransition::Decline,
                    None,
                    Some("left_waitlist"),
                    &scheduling::actor_label(ctx),
                )
                .await?;
                audit::emit(
                    &mut **tx,
                    ctx,
                    "appointment.offer.declined",
                    &state.cell,
                    json!({ "offer_id": o.id, "patient_id": o.patient_id, "by_patient": by_patient,
                            "cancellation_event_id": o.cancellation_event_id,
                            "reason": "left_waitlist" }),
                    None,
                )
                .await
                .map_err(ApiError::internal)?;
                recovery::on_offer_closed(tx, ctx, state, o.id, "declined").await?;
            }
        }
        // `on_offer_closed` returned the entry to `active`.
        from = lock_entry(tx, e.tenant_id, e.id).await?.status;
    }
    let next = from
        .apply(t)
        .map_err(|m| ApiError::conflict("invalid_transition", m))?;
    sqlx::query(
        "UPDATE waitlist_entries SET status = $2,
             paused_at = CASE WHEN $2 = 'paused' THEN now() ELSE paused_at END,
             left_at = CASE WHEN $2 = 'left' THEN now() ELSE left_at END,
             version = version + 1, updated_at = now()
         WHERE id = $1",
    )
    .bind(e.id)
    .bind(next.as_str())
    .execute(&mut **tx)
    .await?;
    let event = match t {
        WaitlistTransition::Pause => "waitlist.paused",
        WaitlistTransition::Resume => "waitlist.resumed",
        _ => "waitlist.left",
    };
    audit::emit(
        &mut **tx,
        ctx,
        event,
        &state.cell,
        json!({ "waitlist_entry_id": e.id, "patient_id": e.patient_id, "from": from.as_str(),
                "to": next.as_str(), "reason": reason, "by_patient": by_patient }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    load_entry(tx, e.tenant_id, e.id).await
}

// ---------------------------------------------------------------------------
// Staff entry routes
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ListEntriesQuery {
    pub status: Option<String>,
    pub service_code: Option<String>,
    pub patient_id: Option<Uuid>,
    pub facility_id: Option<Uuid>,
    pub after: Option<Uuid>,
    pub limit: Option<i64>,
}

fn bounded_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// Facility scoping for staff whose waitlist role is facility-bound: an
/// entry is visible when it names one of the caller's facilities or names
/// no facility at all (tenant-wide preference).
fn scope_filter(ctx: &AuthContext, action: &str) -> (bool, Vec<Uuid>) {
    match facility_scope(ctx, action) {
        None => (true, Vec::new()),
        Some(ids) => (false, ids),
    }
}

async fn list_entries(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListEntriesQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::WAITLIST_MANAGE,
        "waitlist_entry",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: q.patient_id,
            facility_id: q.facility_id,
        }),
    )
    .await?;
    allowed.record_on_pool(&state, &ctx).await?;
    let statuses: Vec<String> = match q.status.as_deref() {
        None | Some("live") => vec!["active".into(), "paused".into(), "offered".into()],
        Some("all") => ["active", "paused", "offered", "fulfilled", "left"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        Some(s) => {
            let st = WaitlistStatus::parse(s).ok_or_else(|| {
                ApiError::bad_request("validation_failed", "unknown status filter")
            })?;
            vec![st.as_str().to_string()]
        }
    };
    let (scope_all, mut scope_ids) = scope_filter(&ctx, actions::WAITLIST_MANAGE);
    if let Some(f) = q.facility_id {
        if scope_all || scope_ids.contains(&f) {
            scope_ids = vec![f];
        } else {
            return Ok(Json(json!({ "items": [], "next_after": null })));
        }
    }
    let scope_all = scope_all && q.facility_id.is_none();
    let limit = bounded_limit(q.limit);
    let rows = sqlx::query(&format!(
        "SELECT {ENTRY_COLUMNS} FROM waitlist_entries
         WHERE tenant_id = $1 AND status = ANY($2)
           AND ($3 OR facility_ids = '{{}}' OR facility_ids && $4)
           AND ($5::uuid IS NULL OR patient_id = $5)
           AND ($6::text IS NULL OR service_code = $6)
           AND ($7::uuid IS NULL OR id > $7)
         ORDER BY id
         LIMIT $8"
    ))
    .bind(ctx.tenant_id)
    .bind(&statuses)
    .bind(scope_all)
    .bind(&scope_ids)
    .bind(q.patient_id)
    .bind(q.service_code.as_deref().map(str::trim))
    .bind(q.after)
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await?;
    let mut items = Vec::with_capacity(rows.len());
    for r in rows.iter().take(limit as usize) {
        items.push(entry_json(&entry_from_row(r)?));
    }
    let next_after = if rows.len() as i64 > limit {
        items.last().and_then(|v| v.get("id").cloned())
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "next_after": next_after })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StaffJoinBody {
    pub patient_id: Uuid,
    /// Staff attestation that the patient agreed to receive waitlist
    /// offers; recorded as the patient's consent decision and audited.
    pub consent_confirmed: bool,
    #[serde(flatten)]
    pub input: JoinInput,
}

async fn create_entry(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<StaffJoinBody>,
) -> Result<(axum::http::StatusCode, Json<Value>), ApiError> {
    if !body.consent_confirmed {
        return Err(ApiError::bad_request(
            "consent_required",
            "confirm that the patient agreed to receive waitlist offers (consent_confirmed)",
        ));
    }
    let mut conn = state.pool.acquire().await?;
    let facility_id: Option<Uuid> =
        sqlx::query_scalar("SELECT facility_id FROM patients WHERE id = $1 AND tenant_id = $2")
            .bind(body.patient_id)
            .bind(ctx.tenant_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(facility_id) = facility_id else {
        return Err(ApiError::not_found());
    };
    let allowed = guard(
        &state,
        &ctx,
        actions::WAITLIST_MANAGE,
        "waitlist_entry",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: Some(body.patient_id),
            facility_id: Some(facility_id),
        }),
    )
    .await?;
    drop(conn);
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let e = join_for(&mut tx, &ctx, &state, body.patient_id, body.input, false).await?;
    tx.commit().await?;
    Ok((axum::http::StatusCode::CREATED, Json(entry_json(&e))))
}

async fn get_entry(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let e = load_entry(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::WAITLIST_MANAGE,
        "waitlist_entry",
        Some(entry_ctx(&e)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let offer = current_offer_json(&mut conn, &e).await?;
    Ok(Json(
        json!({ "entry": entry_json(&e), "current_offer": offer }),
    ))
}

async fn staff_transition(
    state: AppState,
    ctx: AuthContext,
    id: Uuid,
    t: WaitlistTransition,
    body: EntryActionBody,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let e = load_entry(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::WAITLIST_MANAGE,
        "waitlist_entry",
        Some(entry_ctx(&e)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    if t == WaitlistTransition::Leave && body.reason.as_deref().unwrap_or("").trim().is_empty() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "reason is required when staff remove a patient from a waitlist",
        ));
    }
    let e = transition_entry_for(&state, &ctx, &e, t, body, false).await?;
    Ok(Json(entry_json(&e)))
}

async fn pause_entry(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<EntryActionBody>,
) -> Result<Json<Value>, ApiError> {
    staff_transition(
        state,
        ctx,
        id,
        WaitlistTransition::Pause,
        body.0.unwrap_or_default(),
    )
    .await
}

async fn resume_entry(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<EntryActionBody>,
) -> Result<Json<Value>, ApiError> {
    staff_transition(
        state,
        ctx,
        id,
        WaitlistTransition::Resume,
        body.0.unwrap_or_default(),
    )
    .await
}

async fn leave_entry(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<EntryActionBody>,
) -> Result<Json<Value>, ApiError> {
    staff_transition(
        state,
        ctx,
        id,
        WaitlistTransition::Leave,
        body.0.unwrap_or_default(),
    )
    .await
}

// ---------------------------------------------------------------------------
// Recovery events
// ---------------------------------------------------------------------------

fn event_ctx(e: &EventRow) -> ResourceCtx {
    ResourceCtx {
        tenant_id: e.tenant_id,
        patient_id: None,
        facility_id: Some(e.facility_id),
    }
}

async fn load_event_scoped(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<EventRow, ApiError> {
    let e = recovery::load_event(conn, id).await?;
    if e.tenant_id != tenant_id {
        return Err(ApiError::not_found());
    }
    Ok(e)
}

#[derive(Debug, Default, Deserialize)]
pub struct ListEventsQuery {
    pub status: Option<String>,
    pub facility_id: Option<Uuid>,
    pub after: Option<Uuid>,
    pub limit: Option<i64>,
}

async fn list_events(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListEventsQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::WAITLIST_MANAGE,
        "cancellation_event",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: q.facility_id,
        }),
    )
    .await?;
    allowed.record_on_pool(&state, &ctx).await?;
    let statuses: Vec<String> = match q.status.as_deref() {
        None | Some("live") => vec!["open".into(), "offered".into()],
        Some("all") => ["open", "offered", "filled", "exhausted", "closed"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        Some(s @ ("open" | "offered" | "filled" | "exhausted" | "closed")) => vec![s.to_string()],
        Some(_) => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "unknown status filter",
            ))
        }
    };
    let (scope_all, mut scope_ids) = scope_filter(&ctx, actions::WAITLIST_MANAGE);
    if let Some(f) = q.facility_id {
        if scope_all || scope_ids.contains(&f) {
            scope_ids = vec![f];
        } else {
            return Ok(Json(json!({ "items": [], "next_after": null })));
        }
    }
    let scope_all = scope_all && q.facility_id.is_none();
    let limit = bounded_limit(q.limit);
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM cancellation_events
         WHERE tenant_id = $1 AND status = ANY($2)
           AND ($3 OR facility_id = ANY($4))
           AND ($5::uuid IS NULL OR id > $5)
         ORDER BY id
         LIMIT $6",
    )
    .bind(ctx.tenant_id)
    .bind(&statuses)
    .bind(scope_all)
    .bind(&scope_ids)
    .bind(q.after)
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await?;
    let mut conn = state.pool.acquire().await?;
    let mut items = Vec::with_capacity(ids.len());
    for id in ids.iter().take(limit as usize) {
        let e = recovery::load_event(&mut conn, *id).await?;
        items.push(event_summary_json(&e));
    }
    let next_after = if ids.len() as i64 > limit {
        items.last().and_then(|v| v.get("id").cloned())
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "next_after": next_after })))
}

fn event_summary_json(e: &EventRow) -> Value {
    json!({
        "id": e.id,
        "appointment_id": e.appointment_id,
        "facility_id": e.facility_id,
        "service_code": e.service_code,
        "modality_code": e.modality_code,
        "starts_at": e.starts_at,
        "ends_at": e.ends_at,
        "status": e.status,
        "eligible_count": e.eligible.len(),
        "pending_count": e.eligible.iter().filter(|r| r.outcome.is_none()).count(),
        "excluded_count": e.excluded.len(),
        "ranking_mode": e.ranking_mode,
        "current_offer_id": e.current_offer_id,
        "offers_made": e.offers_made,
        "override_reason": e.override_reason,
        "closed_reason": e.closed_reason,
        "version": e.version,
        "created_at": e.created_at,
        "updated_at": e.updated_at,
    })
}

async fn event_detail(conn: &mut PgConnection, e: &EventRow) -> Result<Value, ApiError> {
    let mut detail = recovery::event_json(e);
    let current_offer = match e.current_offer_id {
        Some(id) => Some(scheduling::offer_json(
            &scheduling::load_offer(conn, id).await?,
        )),
        None => None,
    };
    let artifact = match e.ranking_artifact_id {
        Some(id) => {
            let row = sqlx::query(
                "SELECT id, status, synthetic, provider, model, prompt_version, reused_from, created_at
                 FROM ai_artifacts WHERE id = $1 AND tenant_id = $2",
            )
            .bind(id)
            .bind(e.tenant_id)
            .fetch_optional(&mut *conn)
            .await?;
            row.map(|r| {
                json!({
                    "id": r.get::<Uuid, _>("id"),
                    "status": r.get::<String, _>("status"),
                    "synthetic": r.get::<bool, _>("synthetic"),
                    "provider": r.get::<String, _>("provider"),
                    "model": r.get::<String, _>("model"),
                    "prompt_version": r.get::<String, _>("prompt_version"),
                    "reused": r.get::<Option<Uuid>, _>("reused_from").is_some(),
                    "created_at": r.get::<DateTime<Utc>, _>("created_at"),
                })
            })
        }
        None => None,
    };
    if let Value::Object(map) = &mut detail {
        map.insert("current_offer".into(), current_offer.unwrap_or(Value::Null));
        map.insert("ranking_artifact".into(), artifact.unwrap_or(Value::Null));
    }
    Ok(detail)
}

async fn get_event(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let e = load_event_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::WAITLIST_MANAGE,
        "cancellation_event",
        Some(event_ctx(&e)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    Ok(Json(event_detail(&mut conn, &e).await?))
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RankBody {
    pub language: Option<String>,
}

/// One bounded `cancellation-recovery.v1` call over the not-yet-offered
/// remainder; the response states whether dMind or the deterministic order
/// applies and why.
async fn rank_event(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<RankBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let language = scheduling::clean_text(body.language, "language", 35)?
        .map(|l| l.to_lowercase())
        .unwrap_or_else(|| "en".into());
    let mut conn = state.pool.acquire().await?;
    let e = load_event_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::WAITLIST_MANAGE,
        "cancellation_event",
        Some(event_ctx(&e)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let outcome = recovery::rank_event(&state, &ctx, e.id, &language).await?;
    let mut conn = state.pool.acquire().await?;
    let e = recovery::load_event(&mut conn, e.id).await?;
    Ok(Json(json!({
        "ranking": outcome,
        "event": event_detail(&mut conn, &e).await?,
    })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OverrideBody {
    pub waitlist_entry_id: Uuid,
    pub reason: String,
    pub version: Option<i64>,
}

async fn override_event(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<OverrideBody>,
) -> Result<Json<Value>, ApiError> {
    let reason = scheduling::clean_text(Some(body.reason), "reason", MAX_REASON)?
        .ok_or_else(|| ApiError::bad_request("validation_failed", "reason is required"))?;
    let mut conn = state.pool.acquire().await?;
    let e = load_event_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::WAITLIST_MANAGE,
        "cancellation_event",
        Some(event_ctx(&e)),
    )
    .await?;
    drop(conn);
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let e = recovery::override_next(
        &mut tx,
        &ctx,
        &state,
        e.id,
        body.waitlist_entry_id,
        &reason,
        body.version,
    )
    .await?;
    let detail = event_detail(&mut tx, &e).await?;
    tx.commit().await?;
    Ok(Json(detail))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReasonBody {
    pub reason: String,
}

async fn revoke_offer(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ReasonBody>,
) -> Result<Json<Value>, ApiError> {
    let reason = scheduling::clean_text(Some(body.reason), "reason", MAX_REASON)?
        .ok_or_else(|| ApiError::bad_request("validation_failed", "reason is required"))?;
    let mut conn = state.pool.acquire().await?;
    let e = load_event_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::WAITLIST_MANAGE,
        "cancellation_event",
        Some(event_ctx(&e)),
    )
    .await?;
    drop(conn);
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let cascaded = recovery::revoke_current(&mut tx, &ctx, &state, e.id, &reason).await?;
    let e = recovery::load_event(&mut tx, e.id).await?;
    let detail = event_detail(&mut tx, &e).await?;
    tx.commit().await?;
    Ok(Json(json!({ "event": detail, "cascaded": cascaded })))
}

/// Close an event by hand (the slot is being handled otherwise). A live
/// offer is revoked first; the reason is mandatory and audited.
async fn close_event(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<ReasonBody>,
) -> Result<Json<Value>, ApiError> {
    let reason = scheduling::clean_text(Some(body.reason), "reason", MAX_REASON)?
        .ok_or_else(|| ApiError::bad_request("validation_failed", "reason is required"))?;
    let mut conn = state.pool.acquire().await?;
    let e = load_event_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::WAITLIST_MANAGE,
        "cancellation_event",
        Some(event_ctx(&e)),
    )
    .await?;
    drop(conn);
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let e = recovery::close_by_staff(&mut tx, &ctx, &state, e.id, &reason).await?;
    let detail = event_detail(&mut tx, &e).await?;
    tx.commit().await?;
    Ok(Json(detail))
}
