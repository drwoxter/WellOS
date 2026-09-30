//! `/api/v1/me/...`: patient and representative self-service.
//!
//! Every handler derives the patient it acts for from an *active*
//! patient-access grant of the authenticated identity (`grants::resolve_grant`)
//! and then authorizes `patient.self_service` against that patient. A
//! `patient_id` in the query or body only selects among the caller's own
//! grants (a representative may manage several dependants); it never widens
//! the accessible set. Sub-resources (requests, offers, appointments,
//! calendars) are re-checked against the grant and reported as `404` when
//! they belong to anybody else, so identifiers cannot be enumerated.
//!
//! The scheduling logic itself is the shared typed helpers of
//! `routes::access` and `scheduling`: nothing here creates appointments,
//! visits or holds by another path.

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, ResourceCtx};
use crate::ratelimit;
use crate::routes::access::{
    self, AcceptBody, AmendBody, CloseBody, ConfirmBody, ConstraintsInput, CreateRequestBody,
    InterpretBody, MatchBody, OfferActionBody, Origin, RescheduleOptionsBody, TransitionBody,
};
use crate::routes::consent::{self, SELF_SERVICE_PURPOSES};
use crate::routes::extract::OptionalJson;
use crate::routes::grants::{self, GrantRow};
use crate::routes::guard;
use crate::routes::waitlist::{self, EntryActionBody, EntryRow, JoinInput};
use crate::scheduling::{self, AppointmentRow, OfferRow};
use crate::state::AppState;
use crate::transport;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, NaiveTime, Utc};
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wellos_domain::access::WeeklyWindow;
use wellos_domain::ics::{self, IcsError, MAX_HORIZON_DAYS, MAX_ICS_BYTES, MAX_TOTAL_INTERVALS};
use wellos_domain::matcher::Interval;
use wellos_domain::recovery::WaitlistTransition;

const MAX_LIST: usize = 30;
const MAX_CODE: usize = 64;
const MAX_EMAIL: usize = 254;
const MAX_ENDPOINT: usize = 2048;
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;
const SOURCE_TYPES: &[&str] = &["ics_import", "device_sync"];

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/me", get(me))
        .route(
            "/me/access-requests",
            get(list_requests).post(create_request),
        )
        .route("/me/access-requests/:id", get(get_request))
        .route("/me/access-requests/:id/amend", post(amend_request))
        .route("/me/access-requests/:id/submit", post(submit_request))
        .route("/me/access-requests/:id/withdraw", post(withdraw_request))
        .route("/me/access-requests/:id/interpret", post(interpret_request))
        .route("/me/access-requests/:id/match", post(match_request))
        .route("/me/access-requests/:id/offers", get(request_offers))
        .route("/me/access-requests/:id/history", get(request_history))
        .route("/me/offers", get(list_offers))
        .route("/me/offers/:id", get(get_offer))
        .route("/me/offers/:id/hold", post(hold_offer))
        .route("/me/offers/:id/release", post(release_offer))
        .route("/me/offers/:id/decline", post(decline_offer))
        .route("/me/offers/:id/accept", post(accept_offer))
        .route("/me/appointments", get(list_appointments))
        .route("/me/appointments/:id", get(get_appointment))
        .route("/me/appointments/:id/history", get(appointment_history))
        .route("/me/appointments/:id/ics", get(appointment_ics))
        .route("/me/appointments/:id/confirm", post(confirm_appointment))
        .route(
            "/me/appointments/:id/reschedule-options",
            post(reschedule_options),
        )
        .route("/me/appointments/:id/cancel", post(cancel_appointment))
        .route(
            "/me/preferences",
            get(get_preferences).put(update_preferences),
        )
        .route("/me/consents", get(list_consents).post(set_consent))
        .route("/me/calendars", get(list_calendars))
        .route("/me/calendars/ics", post(import_ics))
        .route("/me/calendars/device-sync", post(device_sync))
        .route("/me/calendars/:id/disconnect", post(disconnect_calendar))
        .route("/me/waitlist", get(list_waitlist).post(join_waitlist))
        .route("/me/waitlist/:id", get(get_waitlist_entry))
        .route("/me/waitlist/:id/pause", post(pause_waitlist))
        .route("/me/waitlist/:id/resume", post(resume_waitlist))
        .route("/me/waitlist/:id/leave", post(leave_waitlist))
        .route("/me/notifications", get(list_notifications))
        .route("/me/notifications/:id/read", post(read_notification))
        .route("/me/transport", get(list_transport).post(request_transport))
        .route("/me/transport/:id", get(get_transport))
        .route("/me/transport/:id/cancel", post(cancel_transport))
        .route("/me/transport/:id/location", post(share_transport_location))
}

// ---------------------------------------------------------------------------
// Grant scoping
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct PatientQuery {
    pub patient_id: Option<Uuid>,
}

fn patient_ctx(g: &GrantRow) -> Option<ResourceCtx> {
    Some(ResourceCtx {
        tenant_id: g.tenant_id,
        patient_id: Some(g.patient_id),
        facility_id: Some(g.patient_facility_id),
    })
}

/// Role/purpose check first (so a denial is audited with the caller's
/// identity even when no grant exists), then the grant, then the
/// patient-scoped authorization whose allow is recorded by the caller.
async fn scope(
    state: &AppState,
    ctx: &AuthContext,
    conn: &mut PgConnection,
    resource_type: &str,
    requested: Option<Uuid>,
) -> Result<(GrantRow, crate::routes::Allowed), ApiError> {
    guard(
        state,
        ctx,
        actions::PATIENT_SELF_SERVICE,
        resource_type,
        None,
    )
    .await?;
    let g = grants::resolve_grant(conn, ctx, requested).await?;
    let allowed = guard(
        state,
        ctx,
        actions::PATIENT_SELF_SERVICE,
        resource_type,
        patient_ctx(&g),
    )
    .await?;
    Ok((g, allowed))
}

fn booked_via(g: &GrantRow) -> &'static str {
    if g.relationship == "self" {
        "patient"
    } else {
        "representative"
    }
}

fn bounded_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

/// Who the caller is on the self-service surface: the patients they may act
/// for, nothing about staff roles or other users.
async fn me(State(state): State<AppState>, ctx: AuthContext) -> Result<Json<Value>, ApiError> {
    let allowed = guard(&state, &ctx, actions::PATIENT_SELF_SERVICE, "me", None).await?;
    let mut conn = state.pool.acquire().await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let grants = grants::active_grants(&mut conn, ctx.tenant_id, ctx.user_id).await?;
    Ok(Json(json!({
        "user_id": ctx.user_id,
        "display_name": ctx.display_name,
        "patients": grants.iter().map(grants::grant_self_json).collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------------------
// Access requests
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListRequestsQuery {
    pub patient_id: Option<Uuid>,
    pub status: Option<String>,
    pub limit: Option<i64>,
}

async fn list_requests(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListRequestsQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(&state, &ctx, &mut conn, "access_request", q.patient_id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let status = scheduling::clean_text(q.status, "status", MAX_CODE)?;
    let rows = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM access_requests
         WHERE tenant_id = $1 AND patient_id = $2 AND ($3::text IS NULL OR status = $3)
         ORDER BY created_at DESC, id DESC LIMIT $4",
    )
    .bind(g.tenant_id)
    .bind(g.patient_id)
    .bind(&status)
    .bind(bounded_limit(q.limit))
    .fetch_all(&mut *conn)
    .await?;
    let mut items = Vec::with_capacity(rows.len());
    for id in rows {
        let r = access::load_request(&mut conn, g.tenant_id, id).await?;
        items.push(access::request_json(&r));
    }
    Ok(Json(json!({ "patient_id": g.patient_id, "items": items })))
}

/// Self-service request body. There is deliberately no `urgency`: clinical
/// urgency is established by staff or deterministic triage, never claimed.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeCreateRequestBody {
    pub patient_id: Option<Uuid>,
    pub facility_id: Option<Uuid>,
    pub free_text: Option<String>,
    #[serde(default)]
    pub constraints: ConstraintsInput,
    #[serde(default = "default_true")]
    pub submit: bool,
    pub idempotency_key: Option<String>,
}

fn default_true() -> bool {
    true
}

async fn create_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<MeCreateRequestBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(&state, &ctx, &mut conn, "access_request", body.patient_id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let channel = booked_via(&g);
    let r = access::create_request_for(
        &state,
        &ctx,
        g.patient_id,
        channel,
        CreateRequestBody {
            patient_id: g.patient_id,
            facility_id: body.facility_id,
            free_text: body.free_text,
            constraints: body.constraints,
            urgency: None,
            submit: body.submit,
            idempotency_key: body.idempotency_key,
        },
    )
    .await?;
    Ok((StatusCode::CREATED, Json(access::request_json(&r))))
}

/// Load a request and prove the caller holds a grant for its patient.
async fn own_request(
    state: &AppState,
    ctx: &AuthContext,
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<(access::RequestRow, GrantRow, crate::routes::Allowed), ApiError> {
    guard(
        state,
        ctx,
        actions::PATIENT_SELF_SERVICE,
        "access_request",
        None,
    )
    .await?;
    let r = access::load_request(conn, ctx.tenant_id, id).await?;
    let g = grants::grant_for_patient(conn, ctx, r.patient_id).await?;
    let allowed = guard(
        state,
        ctx,
        actions::PATIENT_SELF_SERVICE,
        "access_request",
        patient_ctx(&g),
    )
    .await?;
    Ok((r, g, allowed))
}

/// The self-service view of a request: the request, its live offers and a
/// concise account of the last matcher run (mode, counts, why candidates
/// were excluded) so the patient can see how options were produced.
async fn request_view(conn: &mut PgConnection, r: &access::RequestRow) -> Result<Value, ApiError> {
    let offers = access::offers_for_request(conn, r).await?;
    let run = access::latest_run(conn, r.tenant_id, r.id).await?;
    Ok(json!({
        "request": access::request_json(r),
        "offers": offers.iter().map(scheduling::offer_json).collect::<Vec<_>>(),
        "matcher_run": run,
    }))
}

async fn get_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (r, _, allowed) = own_request(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    Ok(Json(request_view(&mut conn, &r).await?))
}

async fn request_offers(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (r, _, allowed) = own_request(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let offers = access::offers_for_request(&mut conn, &r).await?;
    Ok(Json(json!({
        "items": offers.iter().map(scheduling::offer_json).collect::<Vec<_>>(),
    })))
}

async fn request_history(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (r, _, allowed) = own_request(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let items = access::request_history_rows(&mut conn, r.tenant_id, r.id).await?;
    Ok(Json(json!({ "items": items })))
}

/// Answer the open scheduling questions (constraints and free text only).
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeAmendBody {
    pub version: Option<i64>,
    pub facility_id: Option<Uuid>,
    pub free_text: Option<String>,
    #[serde(default)]
    pub constraints: ConstraintsInput,
}

async fn amend_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<MeAmendBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (r, _, allowed) = own_request(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let r = access::amend_request_for(
        &state,
        &ctx,
        r,
        AmendBody {
            version: body.version,
            facility_id: body.facility_id,
            free_text: body.free_text,
            constraints: body.constraints,
            urgency: None,
        },
        false,
    )
    .await?;
    Ok(Json(access::request_json(&r)))
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionBody {
    pub version: Option<i64>,
}

async fn submit_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<VersionBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (r, _, allowed) = own_request(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let version = body.0.and_then(|b| b.version);
    let r = access::submit_request_for(&state, &ctx, r, version).await?;
    Ok(Json(access::request_json(&r)))
}

async fn withdraw_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<TransitionBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (r, _, allowed) = own_request(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let body = body.0.unwrap_or(TransitionBody {
        version: None,
        reason: None,
    });
    let r = access::withdraw_request_for(&state, &ctx, r, body).await?;
    Ok(Json(access::request_json(&r)))
}

async fn interpret_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<InterpretBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (r, _, allowed) = own_request(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let out = access::interpret_request_for(&state, &ctx, r, body.0.unwrap_or_default()).await?;
    Ok(Json(out))
}

/// Patient-side matcher run. The one-time origin is used in memory for
/// travel feasibility only (and only with `scheduling_location` consent,
/// enforced by the shared helper); it is never stored.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeMatchBody {
    pub version: Option<i64>,
    pub origin: Option<Origin>,
    pub ranking: Option<bool>,
    pub language: Option<String>,
}

async fn match_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<MeMatchBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (r, _, allowed) = own_request(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let body = body.0.unwrap_or_default();
    let m = access::run_matcher_for(
        &state,
        &ctx,
        r,
        MatchBody {
            version: body.version,
            origin: body.origin,
            ranking: body.ranking,
            language: body.language,
        },
        "patient",
    )
    .await?;
    Ok(Json(access::match_result_json(&m)))
}

// ---------------------------------------------------------------------------
// Offers
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ListOffersQuery {
    pub patient_id: Option<Uuid>,
    /// `live` (default: offered or held), `all`.
    pub status: Option<String>,
    pub limit: Option<i64>,
}

/// Every offer addressed to the patient, whichever path produced it
/// (matcher run, reschedule or cancellation recovery), newest first.
async fn list_offers(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListOffersQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(&state, &ctx, &mut conn, "appointment_offer", q.patient_id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let statuses: Vec<String> = match q.status.as_deref() {
        None | Some("live") => vec!["offered".into(), "held".into()],
        Some("all") => [
            "offered", "held", "accepted", "declined", "expired", "revoked",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect(),
        Some(_) => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "status must be live or all",
            ))
        }
    };
    let ids = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM appointment_offers
         WHERE tenant_id = $1 AND patient_id = $2 AND status = ANY($3)
         ORDER BY created_at DESC, id
         LIMIT $4",
    )
    .bind(g.tenant_id)
    .bind(g.patient_id)
    .bind(&statuses)
    .bind(bounded_limit(q.limit))
    .fetch_all(&mut *conn)
    .await?;
    let mut items = Vec::with_capacity(ids.len());
    for id in ids {
        let o = access::load_offer_scoped(&mut conn, g.tenant_id, id).await?;
        items.push(scheduling::offer_json(&o));
    }
    Ok(Json(json!({ "patient_id": g.patient_id, "items": items })))
}

async fn own_offer(
    state: &AppState,
    ctx: &AuthContext,
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<(OfferRow, GrantRow, crate::routes::Allowed), ApiError> {
    guard(
        state,
        ctx,
        actions::PATIENT_SELF_SERVICE,
        "appointment_offer",
        None,
    )
    .await?;
    let o = access::load_offer_scoped(conn, ctx.tenant_id, id).await?;
    let g = grants::grant_for_patient(conn, ctx, o.patient_id).await?;
    let allowed = guard(
        state,
        ctx,
        actions::PATIENT_SELF_SERVICE,
        "appointment_offer",
        patient_ctx(&g),
    )
    .await?;
    Ok((o, g, allowed))
}

async fn get_offer(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (o, _, allowed) = own_offer(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let history = access::offer_history_json(&mut conn, &o).await?;
    Ok(Json(json!({
        "offer": scheduling::offer_json(&o),
        "history": history,
    })))
}

async fn hold_offer(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<OfferActionBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (o, _, allowed) = own_offer(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let o = access::hold_offer_for(&state, &ctx, o, body.0.unwrap_or_default()).await?;
    Ok(Json(scheduling::offer_json(&o)))
}

async fn release_offer(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<OfferActionBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (o, _, allowed) = own_offer(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let o = access::release_hold_for(&state, &ctx, o, body.0.unwrap_or_default()).await?;
    Ok(Json(scheduling::offer_json(&o)))
}

async fn decline_offer(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<OfferActionBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (o, _, allowed) = own_offer(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let o = access::decline_offer_for(&state, &ctx, o, body.0.unwrap_or_default(), true).await?;
    Ok(Json(scheduling::offer_json(&o)))
}

/// Patient acceptance: no staff override, no urgency; the shared helper
/// enforces the hold, the policy windows and the atomic booking.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeAcceptBody {
    pub version: Option<i64>,
    pub reason: Option<String>,
    pub idempotency_key: Option<String>,
    pub reschedule_of: Option<Uuid>,
    pub reschedule_reason: Option<String>,
}

async fn accept_offer(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<MeAcceptBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (o, g, allowed) = own_offer(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    if let Some(b) = &body.0 {
        if let Some(prev) = b.reschedule_of {
            // The appointment being replaced must be the same patient's.
            let a = access::load_appointment_scoped(&mut conn, ctx.tenant_id, prev).await?;
            if a.patient_id != g.patient_id {
                return Err(ApiError::not_found());
            }
        }
    }
    drop(conn);
    let body = body.0.unwrap_or_default();
    let a = access::accept_offer_for(
        &state,
        &ctx,
        o,
        AcceptBody {
            version: body.version,
            reason: body.reason,
            override_reason: None,
            idempotency_key: body.idempotency_key,
            reschedule_of: body.reschedule_of,
            reschedule_reason: body.reschedule_reason,
        },
        booked_via(&g),
    )
    .await?;
    let mut conn = state.pool.acquire().await?;
    let detail = access::appointment_detail_json(&mut conn, &a).await?;
    Ok((StatusCode::CREATED, Json(detail)))
}

// ---------------------------------------------------------------------------
// Appointments
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct ListAppointmentsQuery {
    pub patient_id: Option<Uuid>,
    /// `upcoming` (default), `past` or `all`.
    pub range: Option<String>,
    pub status: Option<String>,
    pub limit: Option<i64>,
}

async fn list_appointments(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListAppointmentsQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(&state, &ctx, &mut conn, "appointment", q.patient_id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let range = q.range.as_deref().unwrap_or("upcoming");
    if !matches!(range, "upcoming" | "past" | "all") {
        return Err(ApiError::bad_request(
            "validation_failed",
            "range must be upcoming, past or all",
        ));
    }
    let status = scheduling::clean_text(q.status, "status", MAX_CODE)?;
    let ids = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM appointments
         WHERE tenant_id = $1 AND patient_id = $2
           AND ($3::text IS NULL OR status = $3)
           AND ($4 = 'all' OR ($4 = 'upcoming' AND ends_at >= now()) OR ($4 = 'past' AND ends_at < now()))
         ORDER BY CASE WHEN $4 = 'past' THEN -extract(epoch from starts_at) ELSE extract(epoch from starts_at) END, id
         LIMIT $5",
    )
    .bind(g.tenant_id)
    .bind(g.patient_id)
    .bind(&status)
    .bind(range)
    .bind(bounded_limit(q.limit))
    .fetch_all(&mut *conn)
    .await?;
    let mut items = Vec::with_capacity(ids.len());
    for id in ids {
        let a = access::load_appointment_scoped(&mut conn, g.tenant_id, id).await?;
        items.push(scheduling::appointment_json(&a));
    }
    Ok(Json(json!({ "patient_id": g.patient_id, "items": items })))
}

async fn own_appointment(
    state: &AppState,
    ctx: &AuthContext,
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<(AppointmentRow, GrantRow, crate::routes::Allowed), ApiError> {
    guard(
        state,
        ctx,
        actions::PATIENT_SELF_SERVICE,
        "appointment",
        None,
    )
    .await?;
    let a = access::load_appointment_scoped(conn, ctx.tenant_id, id).await?;
    let g = grants::grant_for_patient(conn, ctx, a.patient_id).await?;
    let allowed = guard(
        state,
        ctx,
        actions::PATIENT_SELF_SERVICE,
        "appointment",
        patient_ctx(&g),
    )
    .await?;
    Ok((a, g, allowed))
}

async fn get_appointment(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (a, _, allowed) = own_appointment(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    Ok(Json(access::appointment_detail_json(&mut conn, &a).await?))
}

async fn appointment_history(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (a, _, allowed) = own_appointment(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let items = access::appointment_history_json(&mut conn, &a).await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
pub struct IcsQuery {
    pub lang: Option<String>,
}

async fn appointment_ics(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Query(q): Query<IcsQuery>,
) -> Result<Response, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (a, _, allowed) = own_appointment(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let language = match q.lang.as_deref() {
        Some("es") => "es",
        _ => "en",
    };
    access::appointment_ics_for(&state, &ctx, &a, language).await
}

async fn confirm_appointment(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<ConfirmBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (a, _, allowed) = own_appointment(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let a =
        access::confirm_attendance_for(&state, &ctx, a, body.0.unwrap_or_default(), true).await?;
    Ok(Json(scheduling::appointment_json(&a)))
}

async fn reschedule_options(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<RescheduleOptionsBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (a, g, allowed) = own_appointment(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let m =
        access::reschedule_options_for(&state, &ctx, a, body.0.unwrap_or_default(), booked_via(&g))
            .await?;
    Ok(Json(access::match_result_json(&m)))
}

/// Cancel within policy. Patients cannot override the cancellation
/// window: inside it the request is refused with the staff contact path.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeCancelBody {
    pub version: Option<i64>,
    pub reason_code: Option<String>,
    pub note: Option<String>,
}

async fn cancel_appointment(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<MeCancelBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (a, _, allowed) = own_appointment(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let body = body.0.unwrap_or_default();
    let a = access::close_appointment_for(
        &state,
        &ctx,
        a,
        wellos_domain::access::AppointmentTransition::Cancel,
        CloseBody {
            version: body.version,
            reason_code: body.reason_code,
            note: body.note,
            override_reason: None,
        },
        true,
    )
    .await?;
    Ok(Json(scheduling::appointment_json(&a)))
}

// ---------------------------------------------------------------------------
// Preferences
// ---------------------------------------------------------------------------

fn preferences_json(patient_id: Uuid, p: &scheduling::Preferences, contact: &Contact) -> Value {
    json!({
        "patient_id": patient_id,
        "available_windows": p.available_windows,
        "unavailable_windows": p.unavailable_windows,
        "preferred_modalities": p.preferred_modalities,
        "preferred_facility_ids": p.preferred_facility_ids,
        "language": p.language,
        "accessibility_needs": p.accessibility_needs,
        "time_zone": p.time_zone,
        "channels": p.channels,
        "quiet_hours_start": p.quiet_hours_start,
        "quiet_hours_end": p.quiet_hours_end,
        "has_contact_email": contact.has_email,
        "has_push_endpoint": contact.has_push,
        "version": p.version,
    })
}

struct Contact {
    has_email: bool,
    has_push: bool,
}

/// Contact details are stored sealed; the API reports only whether one is
/// on file, never the value.
async fn contact_flags(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
) -> Result<Contact, ApiError> {
    let row = sqlx::query(
        "SELECT contact_email_enc IS NOT NULL AS has_email, push_endpoint_enc IS NOT NULL AS has_push
         FROM patient_scheduling_preferences WHERE tenant_id = $1 AND patient_id = $2",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(match row {
        Some(r) => Contact {
            has_email: r.get("has_email"),
            has_push: r.get("has_push"),
        },
        None => Contact {
            has_email: false,
            has_push: false,
        },
    })
}

async fn get_preferences(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<PatientQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(
        &state,
        &ctx,
        &mut conn,
        "scheduling_preferences",
        q.patient_id,
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let p = scheduling::load_preferences(&mut conn, g.tenant_id, g.patient_id).await?;
    let c = contact_flags(&mut conn, g.tenant_id, g.patient_id).await?;
    Ok(Json(preferences_json(g.patient_id, &p, &c)))
}

/// Full replacement of the stored preferences (omitted fields reset to
/// their defaults). `contact_email` / `push_endpoint`: absent keeps the
/// stored value, `null` clears it, a string replaces it (sealed at rest).
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PreferencesBody {
    pub patient_id: Option<Uuid>,
    pub version: Option<i64>,
    #[serde(default)]
    pub available_windows: Vec<WeeklyWindow>,
    #[serde(default)]
    pub unavailable_windows: Vec<WeeklyWindow>,
    #[serde(default)]
    pub preferred_modalities: Vec<String>,
    #[serde(default)]
    pub preferred_facility_ids: Vec<Uuid>,
    pub language: Option<String>,
    #[serde(default)]
    pub accessibility_needs: Vec<String>,
    pub time_zone: Option<String>,
    pub channels: Option<Vec<String>>,
    pub quiet_hours_start: Option<NaiveTime>,
    pub quiet_hours_end: Option<NaiveTime>,
    #[serde(default, deserialize_with = "deserialize_tristate")]
    pub contact_email: Tristate<String>,
    #[serde(default, deserialize_with = "deserialize_tristate")]
    pub push_endpoint: Tristate<String>,
}

/// Distinguishes "not sent" from an explicit `null` in a JSON body.
#[derive(Debug, Default)]
pub enum Tristate<T> {
    #[default]
    Absent,
    Clear,
    Set(T),
}

fn deserialize_tristate<'de, D, T>(d: D) -> Result<Tristate<T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(d).map(|o| match o {
        None => Tristate::Clear,
        Some(v) => Tristate::Set(v),
    })
}

fn clean_codes(values: Vec<String>, field: &str) -> Result<Vec<String>, ApiError> {
    if values.len() > MAX_LIST {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("{field} accepts at most {MAX_LIST} entries"),
        ));
    }
    let mut out = Vec::with_capacity(values.len());
    for v in values {
        if let Some(c) = scheduling::clean_text(Some(v), field, MAX_CODE)? {
            if !out.contains(&c) {
                out.push(c);
            }
        }
    }
    Ok(out)
}

fn seal_contact(
    state: &AppState,
    value: Tristate<String>,
    field: &str,
    max: usize,
    check: fn(&str) -> bool,
) -> Result<Tristate<Vec<u8>>, ApiError> {
    match value {
        Tristate::Absent => Ok(Tristate::Absent),
        Tristate::Clear => Ok(Tristate::Clear),
        Tristate::Set(v) => {
            let v = scheduling::clean_text(Some(v), field, max)?
                .filter(|s| check(s))
                .ok_or_else(|| {
                    ApiError::bad_request("validation_failed", format!("{field} is not valid"))
                })?;
            let keyring = state.runtime.location.keyring.as_ref().ok_or_else(|| {
                ApiError::new(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "encryption_unavailable",
                    "contact details cannot be stored: application encryption is not configured",
                )
            })?;
            Ok(Tristate::Set(keyring.seal(v.as_bytes())))
        }
    }
}

fn plausible_email(s: &str) -> bool {
    let Some((local, domain)) = s.rsplit_once('@') else {
        return false;
    };
    !local.is_empty()
        && domain.contains('.')
        && !domain.starts_with('.')
        && !domain.ends_with('.')
        && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

fn plausible_endpoint(s: &str) -> bool {
    s.starts_with("https://") && !s.chars().any(|c| c.is_whitespace() || c.is_control())
}

async fn update_preferences(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<PreferencesBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(
        &state,
        &ctx,
        &mut conn,
        "scheduling_preferences",
        body.patient_id,
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;

    scheduling::validate_windows(&body.available_windows, "available_windows")?;
    scheduling::validate_windows(&body.unavailable_windows, "unavailable_windows")?;
    let modalities = clean_codes(body.preferred_modalities, "preferred_modalities")?;
    scheduling::require_codes(
        &mut conn,
        g.tenant_id,
        "modality",
        &modalities,
        "preferred_modalities",
    )
    .await?;
    let accessibility = clean_codes(body.accessibility_needs, "accessibility_needs")?;
    scheduling::require_codes(
        &mut conn,
        g.tenant_id,
        "accessibility_capability",
        &accessibility,
        "accessibility_needs",
    )
    .await?;
    if body.preferred_facility_ids.len() > MAX_LIST {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("preferred_facility_ids accepts at most {MAX_LIST} entries"),
        ));
    }
    let mut facilities: Vec<Uuid> = Vec::new();
    for f in body.preferred_facility_ids {
        if !facilities.contains(&f) {
            facilities.push(f);
        }
    }
    if !facilities.is_empty() {
        let known: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM facilities WHERE tenant_id = $1 AND id = ANY($2)",
        )
        .bind(g.tenant_id)
        .bind(&facilities)
        .fetch_one(&mut *conn)
        .await?;
        if known as usize != facilities.len() {
            return Err(ApiError::bad_request(
                "validation_failed",
                "preferred_facility_ids must be facilities of this tenant",
            ));
        }
    }
    let language = scheduling::clean_text(body.language, "language", 35)?.map(|l| l.to_lowercase());
    let time_zone = scheduling::clean_text(body.time_zone, "time_zone", MAX_CODE)?;
    if let Some(tz) = &time_zone {
        if tz.parse::<chrono_tz::Tz>().is_err() {
            return Err(ApiError::bad_request(
                "validation_failed",
                "time_zone must be an IANA time zone",
            ));
        }
    }
    let channels = match body.channels {
        None => vec!["in_app".to_string()],
        Some(c) => {
            let mut c = clean_codes(c, "channels")?;
            if let Some(bad) = c
                .iter()
                .find(|c| !crate::notify::CHANNELS.contains(&c.as_str()))
            {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    format!("unknown notification channel {bad}"),
                ));
            }
            if !c.iter().any(|c| c == "in_app") {
                c.insert(0, "in_app".into());
            }
            c
        }
    };
    if body.quiet_hours_start.is_some() != body.quiet_hours_end.is_some() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "quiet_hours_start and quiet_hours_end must be set together",
        ));
    }
    let email = seal_contact(
        &state,
        body.contact_email,
        "contact_email",
        MAX_EMAIL,
        plausible_email,
    )?;
    let push = seal_contact(
        &state,
        body.push_endpoint,
        "push_endpoint",
        MAX_ENDPOINT,
        plausible_endpoint,
    )?;
    drop(conn);

    let mut tx = state.pool.begin().await?;
    let current: Option<i64> = sqlx::query_scalar(
        "SELECT version FROM patient_scheduling_preferences WHERE tenant_id = $1 AND patient_id = $2 FOR UPDATE",
    )
    .bind(g.tenant_id)
    .bind(g.patient_id)
    .fetch_optional(&mut *tx)
    .await?;
    if let (Some(expected), Some(actual)) = (body.version, current) {
        if expected != actual {
            return Err(scheduling::stale());
        }
    }
    let available = serde_json::to_value(&body.available_windows).map_err(ApiError::internal)?;
    let unavailable =
        serde_json::to_value(&body.unavailable_windows).map_err(ApiError::internal)?;
    let (email_mode, email_value): (i16, Option<Vec<u8>>) = match email {
        Tristate::Absent => (0, None),
        Tristate::Clear => (1, None),
        Tristate::Set(v) => (2, Some(v)),
    };
    let (push_mode, push_value): (i16, Option<Vec<u8>>) = match push {
        Tristate::Absent => (0, None),
        Tristate::Clear => (1, None),
        Tristate::Set(v) => (2, Some(v)),
    };
    sqlx::query(
        "INSERT INTO patient_scheduling_preferences
            (patient_id, tenant_id, available_windows, unavailable_windows, preferred_modalities,
             preferred_facility_ids, language, accessibility_needs, time_zone, channels,
             quiet_hours_start, quiet_hours_end, contact_email_enc, push_endpoint_enc, updated_by)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,
                 CASE WHEN $13 = 2 THEN $14 ELSE NULL END,
                 CASE WHEN $15 = 2 THEN $16 ELSE NULL END, $17)
         ON CONFLICT (patient_id) DO UPDATE SET
            available_windows = EXCLUDED.available_windows,
            unavailable_windows = EXCLUDED.unavailable_windows,
            preferred_modalities = EXCLUDED.preferred_modalities,
            preferred_facility_ids = EXCLUDED.preferred_facility_ids,
            language = EXCLUDED.language,
            accessibility_needs = EXCLUDED.accessibility_needs,
            time_zone = EXCLUDED.time_zone,
            channels = EXCLUDED.channels,
            quiet_hours_start = EXCLUDED.quiet_hours_start,
            quiet_hours_end = EXCLUDED.quiet_hours_end,
            contact_email_enc = CASE $13 WHEN 0 THEN patient_scheduling_preferences.contact_email_enc
                                         WHEN 1 THEN NULL ELSE $14 END,
            push_endpoint_enc = CASE $15 WHEN 0 THEN patient_scheduling_preferences.push_endpoint_enc
                                         WHEN 1 THEN NULL ELSE $16 END,
            version = patient_scheduling_preferences.version + 1,
            updated_by = EXCLUDED.updated_by,
            updated_at = now()
         WHERE patient_scheduling_preferences.tenant_id = EXCLUDED.tenant_id",
    )
    .bind(g.patient_id)
    .bind(g.tenant_id)
    .bind(available)
    .bind(unavailable)
    .bind(&modalities)
    .bind(&facilities)
    .bind(&language)
    .bind(&accessibility)
    .bind(&time_zone)
    .bind(&channels)
    .bind(body.quiet_hours_start)
    .bind(body.quiet_hours_end)
    .bind(email_mode)
    .bind(email_value)
    .bind(push_mode)
    .bind(push_value)
    .bind(ctx.user_id)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "patient_preferences.updated",
        &state.cell,
        json!({ "patient_id": g.patient_id, "relationship": g.relationship,
                "channels": channels, "has_time_zone": time_zone.is_some(),
                "available_windows": body.available_windows.len(),
                "unavailable_windows": body.unavailable_windows.len(),
                "contact_email": email_mode, "push_endpoint": push_mode }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let p = scheduling::load_preferences(&mut tx, g.tenant_id, g.patient_id).await?;
    let c = contact_flags(&mut tx, g.tenant_id, g.patient_id).await?;
    tx.commit().await?;
    Ok(Json(preferences_json(g.patient_id, &p, &c)))
}

// ---------------------------------------------------------------------------
// Scheduling consents
// ---------------------------------------------------------------------------

async fn list_consents(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<PatientQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(&state, &ctx, &mut conn, "consent", q.patient_id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let mut items = Vec::with_capacity(SELF_SERVICE_PURPOSES.len());
    for purpose in SELF_SERVICE_PURPOSES {
        let active =
            scheduling::consent_active(&mut conn, g.tenant_id, g.patient_id, purpose).await?;
        items.push(json!({
            "purpose": purpose,
            "status": if active { "active" } else { "revoked" },
        }));
    }
    Ok(Json(json!({ "patient_id": g.patient_id, "items": items })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeConsentBody {
    pub patient_id: Option<Uuid>,
    pub purpose: String,
    pub status: String,
}

/// Grant or withdraw one of the scheduling purposes. Withdrawing
/// `scheduling_calendar` also disconnects every connected calendar and
/// deletes its derived busy intervals in the same transaction.
async fn set_consent(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<MeConsentBody>,
) -> Result<Json<Value>, ApiError> {
    if !SELF_SERVICE_PURPOSES.contains(&body.purpose.as_str()) {
        return Err(ApiError::bad_request(
            "unknown_purpose",
            "purpose must be one of the scheduling consents",
        ));
    }
    if !matches!(body.status.as_str(), "active" | "revoked") {
        return Err(ApiError::bad_request(
            "validation_failed",
            "status must be 'active' or 'revoked'",
        ));
    }
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(&state, &ctx, &mut conn, "consent", body.patient_id).await?;
    drop(conn);
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    consent::append_consent(
        &mut tx,
        &ctx,
        &state.cell,
        g.tenant_id,
        g.patient_id,
        &body.purpose,
        &body.status,
    )
    .await?;
    let mut disconnected = 0u64;
    if body.purpose == scheduling::CONSENT_CALENDAR && body.status == "revoked" {
        let ids: Vec<Uuid> = sqlx::query_scalar(
            "SELECT id FROM patient_calendar_sources
             WHERE tenant_id = $1 AND patient_id = $2 AND status = 'connected' FOR UPDATE",
        )
        .bind(g.tenant_id)
        .bind(g.patient_id)
        .fetch_all(&mut *tx)
        .await?;
        for id in ids {
            disconnect_source(&mut tx, &state, &ctx, &g, id, "consent_withdrawn").await?;
            disconnected += 1;
        }
    }
    tx.commit().await?;
    Ok(Json(json!({
        "patient_id": g.patient_id,
        "purpose": body.purpose,
        "status": body.status,
        "calendars_disconnected": disconnected,
    })))
}

// ---------------------------------------------------------------------------
// Personal calendars
// ---------------------------------------------------------------------------

fn source_json(r: &sqlx::postgres::PgRow) -> Value {
    json!({
        "id": r.get::<Uuid, _>("id"),
        "source_type": r.get::<String, _>("source_type"),
        "time_zone": r.get::<String, _>("time_zone"),
        "integrity_hash": r.get::<String, _>("integrity_hash"),
        "horizon_start": r.get::<DateTime<Utc>, _>("horizon_start"),
        "horizon_end": r.get::<DateTime<Utc>, _>("horizon_end"),
        "interval_count": r.get::<i32, _>("interval_count"),
        "status": r.get::<String, _>("status"),
        "connected_at": r.get::<DateTime<Utc>, _>("connected_at"),
        "last_synced_at": r.get::<Option<DateTime<Utc>>, _>("last_synced_at"),
        "disconnected_at": r.get::<Option<DateTime<Utc>>, _>("disconnected_at"),
    })
}

const SOURCE_COLUMNS: &str =
    "id, source_type, time_zone, integrity_hash, horizon_start, horizon_end, interval_count,
     status, connected_at, last_synced_at, disconnected_at";

async fn list_calendars(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<PatientQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(&state, &ctx, &mut conn, "patient_calendar", q.patient_id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let consent = scheduling::consent_active(
        &mut conn,
        g.tenant_id,
        g.patient_id,
        scheduling::CONSENT_CALENDAR,
    )
    .await?;
    let rows = sqlx::query(&format!(
        "SELECT {SOURCE_COLUMNS} FROM patient_calendar_sources
         WHERE tenant_id = $1 AND patient_id = $2
         ORDER BY (status <> 'connected'), connected_at DESC LIMIT 50"
    ))
    .bind(g.tenant_id)
    .bind(g.patient_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Json(json!({
        "patient_id": g.patient_id,
        "calendar_consent": consent,
        "limits": {
            "max_bytes": MAX_ICS_BYTES,
            "max_intervals": MAX_TOTAL_INTERVALS,
            "max_horizon_days": MAX_HORIZON_DAYS,
        },
        "items": rows.iter().map(source_json).collect::<Vec<_>>(),
    })))
}

async fn require_calendar_consent(conn: &mut PgConnection, g: &GrantRow) -> Result<(), ApiError> {
    if scheduling::consent_active(
        conn,
        g.tenant_id,
        g.patient_id,
        scheduling::CONSENT_CALENDAR,
    )
    .await?
    {
        Ok(())
    } else {
        Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "consent_required",
            "scheduling_calendar consent is required before calendar data can be used",
        ))
    }
}

fn horizon(days: Option<i64>) -> Result<Interval, ApiError> {
    let days = days.unwrap_or(MAX_HORIZON_DAYS);
    if !(1..=MAX_HORIZON_DAYS).contains(&days) {
        return Err(ApiError::bad_request(
            "validation_failed",
            format!("horizon_days must be between 1 and {MAX_HORIZON_DAYS}"),
        ));
    }
    // The span (one day of grace for events in progress plus the forward
    // horizon) must stay within the parser's bound.
    let now = Utc::now();
    let start = now - Duration::days(1);
    let end = start + Duration::days(days);
    Ok(Interval::new(start, end))
}

fn ics_error(e: IcsError) -> ApiError {
    match e {
        IcsError::TooLarge(_) => ApiError::new(
            StatusCode::PAYLOAD_TOO_LARGE,
            "calendar_too_large",
            format!("calendar files are limited to {MAX_ICS_BYTES} bytes"),
        ),
        other => ApiError::bad_request("calendar_invalid", other.to_string()),
    }
}

struct Normalized {
    intervals: Vec<Interval>,
    time_zone: String,
    integrity_hash: String,
    horizon: Interval,
    meta: Value,
}

/// Persist normalized busy intervals for a source (new or re-synced) and
/// audit it. Replaces the previous intervals of the source wholesale so a
/// re-import never accumulates stale busy time. Stores nothing beyond the
/// intervals, time zone, source type, hash and counts.
async fn store_source(
    tx: &mut PgConnection,
    state: &AppState,
    ctx: &AuthContext,
    g: &GrantRow,
    source_type: &str,
    existing: Option<Uuid>,
    n: Normalized,
) -> Result<Value, ApiError> {
    if !SOURCE_TYPES.contains(&source_type) {
        return Err(ApiError::internal(anyhow::anyhow!(
            "unknown calendar source type {source_type}"
        )));
    }
    let (id, event) = match existing {
        Some(id) => {
            let row = sqlx::query(
                "SELECT source_type, status FROM patient_calendar_sources
                 WHERE id = $1 AND tenant_id = $2 AND patient_id = $3 FOR UPDATE",
            )
            .bind(id)
            .bind(g.tenant_id)
            .bind(g.patient_id)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(ApiError::not_found)?;
            if row.get::<String, _>("source_type") != source_type {
                return Err(ApiError::conflict(
                    "source_type_mismatch",
                    "the calendar source has a different type",
                ));
            }
            if row.get::<String, _>("status") != "connected" {
                return Err(ApiError::conflict(
                    "invalid_transition",
                    "the calendar source is disconnected; connect a new one",
                ));
            }
            sqlx::query(
                "UPDATE patient_calendar_sources
                 SET time_zone = $2, integrity_hash = $3, horizon_start = $4, horizon_end = $5,
                     interval_count = $6, last_synced_at = now()
                 WHERE id = $1",
            )
            .bind(id)
            .bind(&n.time_zone)
            .bind(&n.integrity_hash)
            .bind(n.horizon.start)
            .bind(n.horizon.end)
            .bind(n.intervals.len() as i32)
            .execute(&mut *tx)
            .await?;
            sqlx::query("DELETE FROM patient_busy_intervals WHERE source_id = $1")
                .bind(id)
                .execute(&mut *tx)
                .await?;
            (id, "patient_calendar.synchronized")
        }
        None => {
            let connected: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM patient_calendar_sources
                 WHERE tenant_id = $1 AND patient_id = $2 AND status = 'connected'",
            )
            .bind(g.tenant_id)
            .bind(g.patient_id)
            .fetch_one(&mut *tx)
            .await?;
            if connected >= 5 {
                return Err(ApiError::conflict(
                    "too_many_calendars",
                    "at most 5 connected calendars per patient; disconnect one first",
                ));
            }
            let id = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO patient_calendar_sources
                    (id, tenant_id, patient_id, source_type, time_zone, integrity_hash, horizon_start,
                     horizon_end, interval_count, connected_by, last_synced_at)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,now())",
            )
            .bind(id)
            .bind(g.tenant_id)
            .bind(g.patient_id)
            .bind(source_type)
            .bind(&n.time_zone)
            .bind(&n.integrity_hash)
            .bind(n.horizon.start)
            .bind(n.horizon.end)
            .bind(n.intervals.len() as i32)
            .bind(ctx.user_id)
            .execute(&mut *tx)
            .await?;
            (id, "patient_calendar.connected")
        }
    };
    if !n.intervals.is_empty() {
        let ids: Vec<Uuid> = n.intervals.iter().map(|_| Uuid::now_v7()).collect();
        let starts: Vec<DateTime<Utc>> = n.intervals.iter().map(|i| i.start).collect();
        let ends: Vec<DateTime<Utc>> = n.intervals.iter().map(|i| i.end).collect();
        sqlx::query(
            "INSERT INTO patient_busy_intervals (id, tenant_id, patient_id, source_id, starts_at, ends_at)
             SELECT x.id, $2, $3, $4, x.s, x.e
             FROM UNNEST($1::uuid[], $5::timestamptz[], $6::timestamptz[]) AS x(id, s, e)",
        )
        .bind(&ids)
        .bind(g.tenant_id)
        .bind(g.patient_id)
        .bind(id)
        .bind(&starts)
        .bind(&ends)
        .execute(&mut *tx)
        .await?;
    }
    audit::emit(
        &mut *tx,
        ctx,
        event,
        &state.cell,
        json!({ "patient_id": g.patient_id, "source_id": id, "source_type": source_type,
                "relationship": g.relationship, "interval_count": n.intervals.len(),
                "integrity_hash": n.integrity_hash, "meta": n.meta }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let row = sqlx::query(&format!(
        "SELECT {SOURCE_COLUMNS} FROM patient_calendar_sources WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    let mut out = source_json(&row);
    out["import"] = n.meta;
    Ok(out)
}

#[derive(Debug, Deserialize)]
pub struct ImportIcsQuery {
    pub patient_id: Option<Uuid>,
    /// Re-sync an existing `ics_import` source instead of connecting a new one.
    pub source_id: Option<Uuid>,
    /// IANA zone for floating times in the file (default: preference or UTC).
    pub time_zone: Option<String>,
    pub horizon_days: Option<i64>,
}

/// RFC 5545 busy-time import. The body is the raw `.ics`; it is parsed in
/// memory by the bounded domain parser and discarded. Only the normalized
/// busy intervals, time zone, hash and counts are stored.
async fn import_ics(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ImportIcsQuery>,
    body: Bytes,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if body.len() > MAX_ICS_BYTES {
        return Err(ics_error(IcsError::TooLarge(MAX_ICS_BYTES)));
    }
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(&state, &ctx, &mut conn, "patient_calendar", q.patient_id).await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    require_calendar_consent(&mut conn, &g).await?;
    let default_tz = match scheduling::clean_text(q.time_zone, "time_zone", MAX_CODE)? {
        Some(tz) => tz,
        None => scheduling::load_preferences(&mut conn, g.tenant_id, g.patient_id)
            .await?
            .time_zone
            .unwrap_or_else(|| "UTC".to_string()),
    };
    if default_tz.parse::<chrono_tz::Tz>().is_err() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "time_zone must be an IANA time zone",
        ));
    }
    let horizon = horizon(q.horizon_days)?;
    let import =
        ics::busy_intervals(&body, &default_tz, horizon.start, horizon.end).map_err(ics_error)?;
    drop(body);
    drop(conn);
    let n = Normalized {
        meta: json!({
            "events_seen": import.events_seen,
            "events_skipped": import.events_skipped,
            "interval_count": import.intervals.len(),
        }),
        intervals: import.intervals,
        time_zone: import.time_zone,
        integrity_hash: import.integrity_hash,
        horizon,
    };
    let created = q.source_id.is_none();
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let out = store_source(&mut tx, &state, &ctx, &g, "ics_import", q.source_id, n).await?;
    tx.commit().await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(out)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusyInput {
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

/// Device / mobile free-busy sync: the client sends only busy intervals it
/// already computed locally; the server never sees event content.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSyncBody {
    pub patient_id: Option<Uuid>,
    pub source_id: Option<Uuid>,
    pub time_zone: String,
    pub horizon_days: Option<i64>,
    #[serde(default)]
    pub intervals: Vec<BusyInput>,
}

async fn device_sync(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<DeviceSyncBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    if body.intervals.len() > MAX_TOTAL_INTERVALS {
        return Err(ApiError::bad_request(
            "calendar_too_large",
            format!("at most {MAX_TOTAL_INTERVALS} busy intervals per sync"),
        ));
    }
    let time_zone = scheduling::clean_text(Some(body.time_zone), "time_zone", MAX_CODE)?
        .filter(|tz| tz.parse::<chrono_tz::Tz>().is_ok())
        .ok_or_else(|| {
            ApiError::bad_request("validation_failed", "time_zone must be an IANA time zone")
        })?;
    let horizon = horizon(body.horizon_days)?;
    let mut raw = Vec::with_capacity(body.intervals.len());
    let mut skipped = 0usize;
    for i in &body.intervals {
        if i.ends_at <= i.starts_at {
            return Err(ApiError::bad_request(
                "validation_failed",
                "each interval needs ends_at after starts_at",
            ));
        }
        let clipped = Interval::new(i.starts_at.max(horizon.start), i.ends_at.min(horizon.end));
        if clipped.end <= clipped.start {
            skipped += 1;
            continue;
        }
        raw.push(clipped);
    }
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(&state, &ctx, &mut conn, "patient_calendar", body.patient_id).await?;
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    require_calendar_consent(&mut conn, &g).await?;
    drop(conn);
    let merged = ics::merge_intervals(raw);
    let mut hasher = <sha2::Sha256 as sha2::Digest>::new();
    for i in &merged {
        sha2::Digest::update(&mut hasher, i.start.timestamp().to_be_bytes());
        sha2::Digest::update(&mut hasher, i.end.timestamp().to_be_bytes());
    }
    let n = Normalized {
        meta: json!({
            "intervals_received": body.intervals.len(),
            "intervals_skipped": skipped,
            "interval_count": merged.len(),
        }),
        intervals: merged,
        time_zone,
        integrity_hash: hex::encode(sha2::Digest::finalize(hasher)),
        horizon,
    };
    let created = body.source_id.is_none();
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let out = store_source(&mut tx, &state, &ctx, &g, "device_sync", body.source_id, n).await?;
    tx.commit().await?;
    let status = if created {
        StatusCode::CREATED
    } else {
        StatusCode::OK
    };
    Ok((status, Json(out)))
}

/// Mark a source disconnected and delete every busy interval derived from
/// it. Idempotent for an already disconnected source.
async fn disconnect_source(
    tx: &mut PgConnection,
    state: &AppState,
    ctx: &AuthContext,
    g: &GrantRow,
    id: Uuid,
    reason: &str,
) -> Result<Value, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {SOURCE_COLUMNS} FROM patient_calendar_sources
         WHERE id = $1 AND tenant_id = $2 AND patient_id = $3 FOR UPDATE"
    ))
    .bind(id)
    .bind(g.tenant_id)
    .bind(g.patient_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    if row.get::<String, _>("status") == "disconnected" {
        return Ok(source_json(&row));
    }
    let deleted = sqlx::query("DELETE FROM patient_busy_intervals WHERE source_id = $1")
        .bind(id)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    sqlx::query(
        "UPDATE patient_calendar_sources
         SET status = 'disconnected', disconnected_at = now(), interval_count = 0
         WHERE id = $1",
    )
    .bind(id)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        ctx,
        "patient_calendar.disconnected",
        &state.cell,
        json!({ "patient_id": g.patient_id, "source_id": id,
                "source_type": row.get::<String, _>("source_type"),
                "intervals_deleted": deleted, "reason": reason }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let row = sqlx::query(&format!(
        "SELECT {SOURCE_COLUMNS} FROM patient_calendar_sources WHERE id = $1"
    ))
    .bind(id)
    .fetch_one(&mut *tx)
    .await?;
    Ok(source_json(&row))
}

async fn disconnect_calendar(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::PATIENT_SELF_SERVICE,
        "patient_calendar",
        None,
    )
    .await?;
    let mut conn = state.pool.acquire().await?;
    // Locate the owner through the caller's grants only: a source of any
    // other patient is indistinguishable from a missing one.
    let owner: Option<Uuid> = sqlx::query_scalar(
        "SELECT patient_id FROM patient_calendar_sources WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .fetch_optional(&mut *conn)
    .await?;
    let g =
        grants::grant_for_patient(&mut conn, &ctx, owner.ok_or_else(ApiError::not_found)?).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::PATIENT_SELF_SERVICE,
        "patient_calendar",
        patient_ctx(&g),
    )
    .await?;
    drop(conn);
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let out = disconnect_source(&mut tx, &state, &ctx, &g, id, "patient_request").await?;
    tx.commit().await?;
    Ok(Json(out))
}

// ---------------------------------------------------------------------------
// Waitlist (cancellation recovery opt-in)
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ListWaitlistQuery {
    pub patient_id: Option<Uuid>,
    /// `live` (default: active, paused, offered) or `all`.
    pub status: Option<String>,
    pub limit: Option<i64>,
}

async fn list_waitlist(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListWaitlistQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(&state, &ctx, &mut conn, "waitlist_entry", q.patient_id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let statuses: Vec<String> = match q.status.as_deref() {
        None | Some("live") => vec!["active".into(), "paused".into(), "offered".into()],
        Some("all") => ["active", "paused", "offered", "fulfilled", "left"]
            .iter()
            .map(|s| s.to_string())
            .collect(),
        Some(_) => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "status must be live or all",
            ))
        }
    };
    let ids = sqlx::query_scalar::<_, Uuid>(
        "SELECT id FROM waitlist_entries
         WHERE tenant_id = $1 AND patient_id = $2 AND status = ANY($3)
         ORDER BY joined_at DESC, id
         LIMIT $4",
    )
    .bind(g.tenant_id)
    .bind(g.patient_id)
    .bind(&statuses)
    .bind(bounded_limit(q.limit))
    .fetch_all(&mut *conn)
    .await?;
    let mut items = Vec::with_capacity(ids.len());
    for id in ids {
        let e = waitlist::load_entry(&mut conn, g.tenant_id, id).await?;
        let offer = waitlist::current_offer_json(&mut conn, &e).await?;
        let mut v = waitlist::entry_json(&e);
        if let Value::Object(map) = &mut v {
            map.insert("current_offer".into(), offer.unwrap_or(Value::Null));
        }
        items.push(v);
    }
    Ok(Json(json!({ "patient_id": g.patient_id, "items": items })))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeJoinBody {
    pub patient_id: Option<Uuid>,
    #[serde(flatten)]
    pub input: JoinInput,
}

/// Joining *is* the opt-in: the entry and the active `waitlist_offers`
/// consent are recorded together, by the patient or their representative.
async fn join_waitlist(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<MeJoinBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(&state, &ctx, &mut conn, "waitlist_entry", body.patient_id).await?;
    drop(conn);
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let e = waitlist::join_for(&mut tx, &ctx, &state, g.patient_id, body.input, true).await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(waitlist::entry_json(&e))))
}

async fn own_waitlist_entry(
    state: &AppState,
    ctx: &AuthContext,
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<(EntryRow, crate::routes::Allowed), ApiError> {
    guard(
        state,
        ctx,
        actions::PATIENT_SELF_SERVICE,
        "waitlist_entry",
        None,
    )
    .await?;
    let e = waitlist::load_entry(conn, ctx.tenant_id, id).await?;
    let g = grants::grant_for_patient(conn, ctx, e.patient_id).await?;
    let allowed = guard(
        state,
        ctx,
        actions::PATIENT_SELF_SERVICE,
        "waitlist_entry",
        patient_ctx(&g),
    )
    .await?;
    Ok((e, allowed))
}

async fn get_waitlist_entry(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (e, allowed) = own_waitlist_entry(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let offer = waitlist::current_offer_json(&mut conn, &e).await?;
    Ok(Json(
        json!({ "entry": waitlist::entry_json(&e), "current_offer": offer }),
    ))
}

async fn me_waitlist_transition(
    state: AppState,
    ctx: AuthContext,
    id: Uuid,
    t: WaitlistTransition,
    body: EntryActionBody,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (e, allowed) = own_waitlist_entry(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let e = waitlist::transition_entry_for(&state, &ctx, &e, t, body, true).await?;
    Ok(Json(waitlist::entry_json(&e)))
}

async fn pause_waitlist(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<EntryActionBody>,
) -> Result<Json<Value>, ApiError> {
    me_waitlist_transition(
        state,
        ctx,
        id,
        WaitlistTransition::Pause,
        body.0.unwrap_or_default(),
    )
    .await
}

async fn resume_waitlist(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<EntryActionBody>,
) -> Result<Json<Value>, ApiError> {
    me_waitlist_transition(
        state,
        ctx,
        id,
        WaitlistTransition::Resume,
        body.0.unwrap_or_default(),
    )
    .await
}

async fn leave_waitlist(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<EntryActionBody>,
) -> Result<Json<Value>, ApiError> {
    me_waitlist_transition(
        state,
        ctx,
        id,
        WaitlistTransition::Leave,
        body.0.unwrap_or_default(),
    )
    .await
}

// ---------------------------------------------------------------------------
// In-app notifications
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct NotificationsQuery {
    pub patient_id: Option<Uuid>,
    pub unread_only: Option<bool>,
    pub limit: Option<i64>,
}

/// In-app inbox: delivered notifications addressed to the caller's
/// patient(s) or to the caller directly. Payloads are the PHI-minimized
/// templates the worker stored; nothing is re-derived here.
async fn list_notifications(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<NotificationsQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::PATIENT_SELF_SERVICE,
        "notification",
        None,
    )
    .await?;
    let mut conn = state.pool.acquire().await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let grants = grants::active_grants(&mut conn, ctx.tenant_id, ctx.user_id).await?;
    let patient_ids: Vec<Uuid> = match q.patient_id {
        Some(p) => {
            if !grants.iter().any(|g| g.patient_id == p) {
                return Err(ApiError::not_found());
            }
            vec![p]
        }
        None => grants.iter().map(|g| g.patient_id).collect(),
    };
    let rows = sqlx::query(
        "SELECT id, patient_id, kind, appointment_id, offer_id, payload, language, status, scheduled_for,
                delivered_at, read_at
         FROM notifications
         WHERE tenant_id = $1
           AND (patient_id = ANY($2) OR ($3::uuid IS NULL AND user_id = $4))
           AND status IN ('delivered', 'partially_delivered')
           AND 'in_app' = ANY(channels)
           AND ($5::bool IS NOT TRUE OR read_at IS NULL)
         ORDER BY COALESCE(delivered_at, scheduled_for) DESC, id DESC
         LIMIT $6",
    )
    .bind(ctx.tenant_id)
    .bind(&patient_ids)
    .bind(q.patient_id)
    .bind(ctx.user_id)
    .bind(q.unread_only)
    .bind(bounded_limit(q.limit))
    .fetch_all(&mut *conn)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid, _>("id"),
                "patient_id": r.get::<Option<Uuid>, _>("patient_id"),
                "kind": r.get::<String, _>("kind"),
                "appointment_id": r.get::<Option<Uuid>, _>("appointment_id"),
                "offer_id": r.get::<Option<Uuid>, _>("offer_id"),
                "payload": r.get::<Value, _>("payload"),
                "language": r.get::<String, _>("language"),
                "status": r.get::<String, _>("status"),
                "delivered_at": r.get::<Option<DateTime<Utc>>, _>("delivered_at"),
                "read_at": r.get::<Option<DateTime<Utc>>, _>("read_at"),
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
}

async fn read_notification(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::PATIENT_SELF_SERVICE,
        "notification",
        None,
    )
    .await?;
    let mut conn = state.pool.acquire().await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let grants = grants::active_grants(&mut conn, ctx.tenant_id, ctx.user_id).await?;
    let patient_ids: Vec<Uuid> = grants.iter().map(|g| g.patient_id).collect();
    let updated = sqlx::query(
        "UPDATE notifications SET read_at = COALESCE(read_at, now()), updated_at = now()
         WHERE id = $1 AND tenant_id = $2
           AND (patient_id = ANY($3) OR user_id = $4)
           AND status IN ('delivered', 'partially_delivered')
         RETURNING read_at",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(&patient_ids)
    .bind(ctx.user_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok(Json(json!({
        "id": id,
        "read_at": updated.get::<Option<DateTime<Utc>>, _>("read_at"),
    })))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn email_and_endpoint_plausibility() {
        assert!(plausible_email("ana@example.org"));
        assert!(!plausible_email("ana@localhost"));
        assert!(!plausible_email("@example.org"));
        assert!(!plausible_email("ana @example.org"));
        assert!(plausible_endpoint("https://push.example.org/v1/abc"));
        assert!(!plausible_endpoint("http://push.example.org/v1/abc"));
    }

    #[test]
    fn source_types_match_schema() {
        assert_eq!(SOURCE_TYPES, &["ics_import", "device_sync"]);
    }
}

// ---------------------------------------------------------------------------
// Transport support (logistics only, grant-scoped)
// ---------------------------------------------------------------------------

/// Patient-side request body: no emergency flag, no operator, no vehicle.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeTransportBody {
    pub appointment_id: Uuid,
    #[serde(default)]
    pub requirements: Vec<String>,
    pub origin_area_code: Option<String>,
    pub pickup_address: Option<String>,
    pub pickup_window_start: Option<DateTime<Utc>>,
    pub pickup_window_end: Option<DateTime<Utc>>,
    pub note: Option<String>,
}

async fn own_transport(
    state: &AppState,
    ctx: &AuthContext,
    conn: &mut PgConnection,
    id: Uuid,
) -> Result<(transport::TransportRow, GrantRow, crate::routes::Allowed), ApiError> {
    guard(
        state,
        ctx,
        actions::PATIENT_SELF_SERVICE,
        "transport_request",
        None,
    )
    .await?;
    let t = transport::load(conn, ctx.tenant_id, id).await?;
    let g = grants::grant_for_patient(conn, ctx, t.patient_id).await?;
    let allowed = guard(
        state,
        ctx,
        actions::PATIENT_SELF_SERVICE,
        "transport_request",
        patient_ctx(&g),
    )
    .await?;
    Ok((t, g, allowed))
}

async fn list_transport(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<PatientQuery>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (g, allowed) = scope(&state, &ctx, &mut conn, "transport_request", q.patient_id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let rows = transport::list(
        &mut conn,
        ctx.tenant_id,
        transport::ListFilter {
            facility_ids: None,
            patient_id: Some(g.patient_id),
            appointment_id: None,
            statuses: &[],
            operator_user_id: None,
            limit: 50,
        },
    )
    .await?;
    Ok(Json(json!({
        "patient_id": g.patient_id,
        "items": rows.iter().map(transport::transport_json).collect::<Vec<_>>(),
        "consent_active": scheduling::consent_active(
            &mut conn, ctx.tenant_id, g.patient_id, scheduling::CONSENT_TRANSPORT).await?,
        "location_encryption_configured": state.runtime.location.keyring.is_some(),
    })))
}

async fn request_transport(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<MeTransportBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (a, g, allowed) = own_appointment(&state, &ctx, &mut conn, body.appointment_id).await?;
    drop(conn);
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    debug_assert_eq!(a.patient_id, g.patient_id);
    let t = transport::create(
        &mut tx,
        &ctx,
        &state,
        transport::CreateInput {
            appointment_id: a.id,
            requirements: body.requirements,
            emergency: false,
            origin_area_code: body.origin_area_code,
            pickup_address: body.pickup_address,
            pickup_window_start: body.pickup_window_start,
            pickup_window_end: body.pickup_window_end,
            note: body.note,
        },
        false,
    )
    .await?;
    tx.commit().await?;
    Ok((StatusCode::CREATED, Json(transport::transport_json(&t))))
}

async fn get_transport(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (t, _, allowed) = own_transport(&state, &ctx, &mut conn, id).await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let mut body = transport::transport_json(&t);
    body["history"] = json!(transport::history_json(&mut conn, t.id).await?);
    Ok(Json(body))
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeTransportCancelBody {
    pub version: Option<i64>,
    pub reason: Option<String>,
}

async fn cancel_transport(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<MeTransportCancelBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (_, _, allowed) = own_transport(&state, &ctx, &mut conn, id).await?;
    drop(conn);
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let body = body.0.unwrap_or_default();
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let t = transport::transition(
        &mut tx,
        &ctx,
        &state,
        id,
        transport::TransitionInput {
            status: "cancelled".into(),
            version: body.version,
            reason: Some(body.reason.unwrap_or_else(|| "patient_cancelled".into())),
            ..Default::default()
        },
        false,
    )
    .await?;
    tx.commit().await?;
    Ok(Json(transport::transport_json(&t)))
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeLocationBody {
    pub latitude: f64,
    pub longitude: f64,
}

/// Patient-side one-time position during pickup; requires location consent
/// and is sealed with the same TTL as operator positions.
async fn share_transport_location(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<MeLocationBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let (t, g, allowed) = own_transport(&state, &ctx, &mut conn, id).await?;
    if !scheduling::consent_active(
        &mut conn,
        ctx.tenant_id,
        g.patient_id,
        scheduling::CONSENT_LOCATION,
    )
    .await?
    {
        return Err(scheduling::consent_required(scheduling::CONSENT_LOCATION));
    }
    drop(conn);
    ratelimit::enforce_for_principal(&state, &ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let expires_at =
        transport::share_location(&mut tx, &ctx, &state, &t, body.latitude, body.longitude).await?;
    tx.commit().await?;
    Ok(Json(json!({
        "transport_request_id": t.id,
        "expires_at": expires_at,
    })))
}
