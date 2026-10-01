//! Staff-facing access and scheduling routes: access requests, deterministic
//! matcher runs, offers/holds, appointments and calendar export.
//!
//! The access request, offer and appointment are explicit versioned state
//! machines (`wellos_domain::access`); every transition here goes through
//! them under a row lock bound to the caller's expected `version`. The
//! matcher (`access-matcher.v1`) is the only source of candidates; dMind may
//! reorder and explain them through one bounded, governed
//! `appointment-ranking.v1` call and is never consulted once per slot. When
//! the model is disabled, unavailable, unconsented or out of quota the run
//! completes deterministically and says so.
//!
//! Urgency is deterministic or human: red-flag text and urgent requests are
//! routed to clinical triage by [`requires_clinical_triage`]; `access-intent`
//! can only *suggest* triage, never set or lower urgency.
//!
//! The patient/representative surface (`/api/v1/me/...`) reuses the `pub`
//! request/offer/appointment helpers of this module with the accessible
//! patient derived from the authenticated grant.

use crate::aigov;
use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, facility_scope, ResourceCtx};
use crate::ratelimit;
use crate::routes::extract::OptionalJson;
use crate::routes::guard;
use crate::scheduling::{self, AppointmentRow, OfferRow};
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Duration, Utc};
use dmind_gateway::access::{
    AccessIntentRequest, RankingCandidate, RankingRequest, VocabularyTerm, ACCESS_INTENT_TEMPLATE,
    APPOINTMENT_RANKING_TEMPLATE, MAX_INTENT_TEXT_CHARS, MAX_RANKING_CANDIDATES,
};
use dmind_gateway::{hash_json, GatewayError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use uuid::Uuid;
use wellos_domain::access::{
    requires_clinical_triage, AccessRequestStatus, AccessRequestTransition, AppointmentStatus,
    AppointmentTransition, OfferStatus, OfferTransition, Urgency, WeeklyWindow,
};
use wellos_domain::access_ai::{
    AccessIntentV1, AppointmentRankingV1, ACCESS_INTENT_SCHEMA, APPOINTMENT_RANKING_SCHEMA,
};
use wellos_domain::ai::{ArtifactStatus, ProviderInfo};
use wellos_domain::matcher::{
    find_candidates, Candidate, MatchFacts, MatchOutput, RequestConstraints,
};

const MAX_FREE_TEXT: usize = MAX_INTENT_TEXT_CHARS;
const MAX_REASON: usize = 500;
const MAX_LIST: usize = 30;
const MAX_WINDOWS: usize = 21;
const DEFAULT_LIMIT: i64 = 50;
const MAX_LIMIT: i64 = 200;
const RESCHEDULE_REASON: &str = "reschedule";
/// Candidates handed to one ranking call; the deterministic order already
/// puts the best first so the model only ever sees the head of the list.
const RANKING_HEAD: usize = 12;

pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/access-requests", post(create_request).get(list_requests))
        .route(
            "/access-requests/:id",
            get(get_request).patch(amend_request),
        )
        .route("/access-requests/:id/history", get(request_history_route))
        .route("/access-requests/:id/submit", post(submit_request))
        .route("/access-requests/:id/interpret", post(interpret_request))
        .route(
            "/access-requests/:id/route-to-triage",
            post(route_to_triage),
        )
        .route("/access-requests/:id/clear-triage", post(clear_triage))
        .route("/access-requests/:id/match", post(run_matcher))
        .route("/access-requests/:id/offers", get(request_offers))
        .route("/access-requests/:id/close", post(close_request))
        .route("/access-requests/:id/withdraw", post(withdraw_request))
        .route("/matcher-runs/:id", get(get_matcher_run))
        .route("/offers/:id", get(get_offer))
        .route("/offers/:id/history", get(offer_history_route))
        .route("/offers/:id/hold", post(hold_offer_route))
        .route("/offers/:id/release-hold", post(release_hold_route))
        .route("/offers/:id/decline", post(decline_offer_route))
        .route("/offers/:id/accept", post(accept_offer_route))
        .route("/appointments", get(list_appointments))
        .route("/appointments/:id", get(get_appointment))
        .route("/appointments/:id/history", get(appointment_history_route))
        .route("/appointments/:id/ics", get(appointment_ics))
        .route("/appointments/:id/confirm", post(confirm_attendance))
        .route(
            "/appointments/:id/reschedule-options",
            post(reschedule_options),
        )
        .route("/appointments/:id/cancel", post(cancel_appointment))
        .route("/appointments/:id/no-show", post(no_show_appointment))
        .route("/appointments/:id/fulfil", post(fulfil_appointment))
}

// ---------------------------------------------------------------------------
// Access request model
// ---------------------------------------------------------------------------

/// Structured scheduling constraints of a request. Stored as JSON on the
/// request; every code is validated against the tenant catalogs when
/// written. Travel origin is deliberately absent — coordinates are supplied
/// per matcher call and never persisted with the request.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct Constraints {
    #[serde(default)]
    pub service_code: Option<String>,
    #[serde(default)]
    pub specialty_code: Option<String>,
    #[serde(default)]
    pub modality_codes: Vec<String>,
    #[serde(default)]
    pub facility_ids: Vec<Uuid>,
    #[serde(default)]
    pub earliest: Option<DateTime<Utc>>,
    #[serde(default)]
    pub latest: Option<DateTime<Utc>>,
    #[serde(default)]
    pub preferred_windows: Vec<WeeklyWindow>,
    #[serde(default)]
    pub max_travel_minutes: Option<i32>,
    #[serde(default)]
    pub continuity_required: bool,
    #[serde(default)]
    pub accessibility_codes: Vec<String>,
    #[serde(default)]
    pub language: Option<String>,
    #[serde(default)]
    pub transport_requested: bool,
    #[serde(default)]
    pub has_referral: bool,
    /// Set when the request exists to move an existing appointment.
    #[serde(default)]
    pub reschedule_of: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct RequestRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub patient_id: Uuid,
    pub facility_id: Option<Uuid>,
    pub status: AccessRequestStatus,
    pub channel: String,
    pub free_text: Option<String>,
    pub constraints: Constraints,
    pub missing_info: Vec<String>,
    pub urgency: Urgency,
    pub urgency_source: String,
    pub triage_reason: Option<String>,
    pub intent_artifact_id: Option<Uuid>,
    pub appointment_id: Option<Uuid>,
    pub closed_reason: Option<String>,
    pub version: i64,
    pub created_by: Uuid,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const REQUEST_COLUMNS: &str =
    "id, tenant_id, patient_id, facility_id, status, channel, free_text, constraints,
    missing_info, urgency, urgency_source, triage_reason, intent_artifact_id, appointment_id,
    closed_reason, version, created_by, created_at, updated_at";

fn request_from_row(r: &sqlx::postgres::PgRow) -> Result<RequestRow, ApiError> {
    let status: String = r.get("status");
    let urgency: String = r.get("urgency");
    let constraints: Value = r.get("constraints");
    Ok(RequestRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        patient_id: r.get("patient_id"),
        facility_id: r.get("facility_id"),
        status: AccessRequestStatus::parse(&status)
            .ok_or_else(|| ApiError::internal(format!("unknown access request status {status}")))?,
        channel: r.get("channel"),
        free_text: r.get("free_text"),
        constraints: serde_json::from_value(constraints).map_err(ApiError::internal)?,
        missing_info: r.get("missing_info"),
        urgency: scheduling::urgency_of(&urgency),
        urgency_source: r.get("urgency_source"),
        triage_reason: r.get("triage_reason"),
        intent_artifact_id: r.get("intent_artifact_id"),
        appointment_id: r.get("appointment_id"),
        closed_reason: r.get("closed_reason"),
        version: r.get("version"),
        created_by: r.get("created_by"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    })
}

/// Tenant-scoped load; a foreign or unknown id is indistinguishable (404).
pub async fn load_request(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<RequestRow, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {REQUEST_COLUMNS} FROM access_requests WHERE id = $1 AND tenant_id = $2"
    ))
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    request_from_row(&row)
}

pub async fn lock_request(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<RequestRow, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {REQUEST_COLUMNS} FROM access_requests WHERE id = $1 AND tenant_id = $2 FOR UPDATE"
    ))
    .bind(id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    request_from_row(&row)
}

pub fn request_json(r: &RequestRow) -> Value {
    json!({
        "id": r.id,
        "patient_id": r.patient_id,
        "facility_id": r.facility_id,
        "status": r.status.as_str(),
        "channel": r.channel,
        "free_text": r.free_text,
        "constraints": r.constraints,
        "missing_info": r.missing_info,
        "urgency": r.urgency.as_str(),
        "urgency_source": r.urgency_source,
        "triage_reason": r.triage_reason,
        "intent_artifact_id": r.intent_artifact_id,
        "appointment_id": r.appointment_id,
        "closed_reason": r.closed_reason,
        "version": r.version,
        "created_by": r.created_by,
        "created_at": r.created_at,
        "updated_at": r.updated_at,
    })
}

pub async fn request_history_rows(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    request_id: Uuid,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT from_status, to_status, reason, actor, recorded_at
         FROM access_request_history WHERE tenant_id = $1 AND access_request_id = $2
         ORDER BY recorded_at, id",
    )
    .bind(tenant_id)
    .bind(request_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "from_status": r.get::<Option<String>, _>("from_status"),
                "to_status": r.get::<String, _>("to_status"),
                "reason": r.get::<Option<String>, _>("reason"),
                "actor": r.get::<String, _>("actor"),
                "recorded_at": r.get::<DateTime<Utc>, _>("recorded_at"),
            })
        })
        .collect())
}

fn invalid_request_transition(r: &RequestRow, t: AccessRequestTransition) -> ApiError {
    ApiError::conflict(
        "invalid_transition",
        format!(
            "an access request in status {} cannot {:?}",
            r.status.as_str(),
            t
        ),
    )
}

/// Move a locked request through the typed state machine, bumping its
/// version and writing the append-only history row. Callers that also
/// change columns pass them via `extra_set` (positional `$N` beyond `$3`
/// are not available; keep to literal SQL fragments).
pub async fn transition_request(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    r: &RequestRow,
    t: AccessRequestTransition,
    reason: Option<&str>,
) -> Result<AccessRequestStatus, ApiError> {
    let next = r
        .status
        .apply(t)
        .map_err(|_| invalid_request_transition(r, t))?;
    let updated = sqlx::query(
        "UPDATE access_requests SET status = $1, version = version + 1, updated_at = now()
         WHERE id = $2 AND version = $3",
    )
    .bind(next.as_str())
    .bind(r.id)
    .bind(r.version)
    .execute(&mut *conn)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(scheduling::stale());
    }
    request_history(
        conn,
        r.tenant_id,
        r.id,
        Some(r.status.as_str()),
        next.as_str(),
        reason,
        &scheduling::actor_label(ctx),
    )
    .await?;
    Ok(next)
}

pub async fn request_history(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    request_id: Uuid,
    from: Option<&str>,
    to: &str,
    reason: Option<&str>,
    actor: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "INSERT INTO access_request_history (id, tenant_id, access_request_id, from_status, to_status, reason, actor)
         VALUES ($1,$2,$3,$4,$5,$6,$7)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant_id)
    .bind(request_id)
    .bind(from)
    .bind(to)
    .bind(reason)
    .bind(actor)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

fn require_version(current: i64, expected: Option<i64>) -> Result<(), ApiError> {
    match expected {
        Some(v) if v != current => Err(scheduling::stale()),
        _ => Ok(()),
    }
}

fn require_reason(value: Option<String>, field: &str) -> Result<String, ApiError> {
    scheduling::clean_text(value, field, MAX_REASON)?
        .ok_or_else(|| ApiError::bad_request("validation_failed", format!("{field} is required")))
}

fn bounded_limit(limit: Option<i64>) -> i64 {
    limit.unwrap_or(DEFAULT_LIMIT).clamp(1, MAX_LIMIT)
}

// ---------------------------------------------------------------------------
// Constraint validation
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct ConstraintsInput {
    pub service_code: Option<String>,
    pub specialty_code: Option<String>,
    #[serde(default)]
    pub modality_codes: Option<Vec<String>>,
    #[serde(default)]
    pub facility_ids: Option<Vec<Uuid>>,
    pub earliest: Option<DateTime<Utc>>,
    pub latest: Option<DateTime<Utc>>,
    #[serde(default)]
    pub preferred_windows: Option<Vec<WeeklyWindow>>,
    pub max_travel_minutes: Option<i32>,
    pub continuity_required: Option<bool>,
    #[serde(default)]
    pub accessibility_codes: Option<Vec<String>>,
    pub language: Option<String>,
    pub transport_requested: Option<bool>,
    pub has_referral: Option<bool>,
}

/// Merge validated input over `base`: `None` keeps the current value, an
/// explicit value (including an empty list) replaces it.
pub async fn apply_constraints(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    base: &Constraints,
    input: ConstraintsInput,
) -> Result<Constraints, ApiError> {
    let mut c = base.clone();
    if let Some(code) = input.service_code {
        let code = code.trim().to_string();
        c.service_code = if code.is_empty() {
            None
        } else {
            Some(scheduling::load_service(conn, tenant_id, &code).await?.code)
        };
    }
    if let Some(code) = input.specialty_code {
        let code = code.trim().to_string();
        if code.is_empty() {
            c.specialty_code = None;
        } else {
            scheduling::require_codes(
                conn,
                tenant_id,
                "specialty",
                std::slice::from_ref(&code),
                "specialty_code",
            )
            .await?;
            c.specialty_code = Some(code);
        }
    }
    if let Some(codes) = input.modality_codes {
        let codes = scheduling::clean_codes(codes, "modality_codes", MAX_LIST)?;
        scheduling::require_codes(conn, tenant_id, "modality", &codes, "modality_codes").await?;
        c.modality_codes = codes;
    }
    if let Some(codes) = input.accessibility_codes {
        let codes = scheduling::clean_codes(codes, "accessibility_codes", MAX_LIST)?;
        scheduling::require_codes(
            conn,
            tenant_id,
            "accessibility_capability",
            &codes,
            "accessibility_codes",
        )
        .await?;
        c.accessibility_codes = codes;
    }
    if let Some(ids) = input.facility_ids {
        if ids.len() > MAX_LIST {
            return Err(ApiError::bad_request(
                "validation_failed",
                format!("facility_ids exceeds {MAX_LIST} entries"),
            ));
        }
        let mut ids = ids;
        ids.sort();
        ids.dedup();
        if !ids.is_empty() {
            let known: i64 = sqlx::query_scalar(
                "SELECT COUNT(*) FROM facilities WHERE tenant_id = $1 AND id = ANY($2)",
            )
            .bind(tenant_id)
            .bind(&ids)
            .fetch_one(&mut *conn)
            .await?;
            if known as usize != ids.len() {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    "facility_ids contains an unknown facility",
                ));
            }
        }
        c.facility_ids = ids;
    }
    if let Some(w) = input.preferred_windows {
        if w.len() > MAX_WINDOWS {
            return Err(ApiError::bad_request(
                "validation_failed",
                format!("preferred_windows exceeds {MAX_WINDOWS} entries"),
            ));
        }
        scheduling::validate_windows(&w, "preferred_windows")?;
        c.preferred_windows = w;
    }
    if input.earliest.is_some() {
        c.earliest = input.earliest;
    }
    if input.latest.is_some() {
        c.latest = input.latest;
    }
    if let (Some(e), Some(l)) = (c.earliest, c.latest) {
        if l <= e {
            return Err(ApiError::bad_request(
                "validation_failed",
                "latest must be after earliest",
            ));
        }
    }
    if let Some(m) = input.max_travel_minutes {
        if !(5..=600).contains(&m) {
            return Err(ApiError::bad_request(
                "validation_failed",
                "max_travel_minutes must be between 5 and 600",
            ));
        }
        c.max_travel_minutes = Some(m);
    }
    if let Some(v) = input.continuity_required {
        c.continuity_required = v;
    }
    if let Some(v) = input.transport_requested {
        c.transport_requested = v;
    }
    if let Some(v) = input.has_referral {
        c.has_referral = v;
    }
    if let Some(lang) = input.language {
        c.language = scheduling::clean_text(Some(lang), "language", 35)?.map(|l| l.to_lowercase());
    }
    Ok(c)
}

/// Questions still open before a matcher run can be attempted.
/// Structured fields the request already carries; the intent model is told
/// not to ask for them again.
pub fn known_constraint_fields(c: &Constraints) -> Vec<String> {
    let mut out = Vec::new();
    if c.service_code.is_some() || c.specialty_code.is_some() {
        out.push("service".to_string());
    }
    if !c.modality_codes.is_empty() {
        out.push("modality".to_string());
    }
    if !c.preferred_windows.is_empty() {
        out.push("preferred_windows".to_string());
    }
    if !c.facility_ids.is_empty() {
        out.push("facility".to_string());
    }
    if c.language.is_some() {
        out.push("language".to_string());
    }
    if !c.accessibility_codes.is_empty() {
        out.push("accessibility".to_string());
    }
    out
}

pub fn missing_information(c: &Constraints) -> Vec<String> {
    let mut m = Vec::new();
    if c.service_code.is_none() {
        m.push("service".to_string());
    }
    m
}

fn urgency_from(value: Option<String>) -> Result<Option<Urgency>, ApiError> {
    match value {
        None => Ok(None),
        Some(s) => Urgency::parse(s.trim()).map(Some).ok_or_else(|| {
            ApiError::bad_request(
                "validation_failed",
                "urgency must be routine, priority or urgent",
            )
        }),
    }
}

async fn write_constraints(
    conn: &mut PgConnection,
    r: &RequestRow,
    c: &Constraints,
    facility_id: Option<Uuid>,
    free_text: Option<&str>,
    urgency: Option<(Urgency, &str)>,
) -> Result<(), ApiError> {
    let missing = missing_information(c);
    let updated = sqlx::query(
        "UPDATE access_requests
         SET constraints = $1, missing_info = $2, facility_id = $3, free_text = $4,
             urgency = COALESCE($5, urgency), urgency_source = COALESCE($6, urgency_source),
             version = version + 1, updated_at = now()
         WHERE id = $7 AND version = $8",
    )
    .bind(serde_json::to_value(c).map_err(ApiError::internal)?)
    .bind(&missing)
    .bind(facility_id)
    .bind(free_text)
    .bind(urgency.map(|(u, _)| u.as_str()))
    .bind(urgency.map(|(_, s)| s))
    .bind(r.id)
    .bind(r.version)
    .execute(&mut *conn)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(scheduling::stale());
    }
    Ok(())
}

/// Deterministic triage floor: urgent requests and red-flag text go to
/// clinical triage before any option is offered. Returns the new status.
pub async fn apply_triage_floor(
    conn: &mut PgConnection,
    ctx: &AuthContext,
    state: &AppState,
    r: &RequestRow,
) -> Result<Option<&'static str>, ApiError> {
    let Some(reason) = requires_clinical_triage(r.free_text.as_deref(), r.urgency) else {
        return Ok(None);
    };
    if r.status == AccessRequestStatus::NeedsClinicalTriage {
        return Ok(Some(reason));
    }
    if r.status
        .apply(AccessRequestTransition::RouteToTriage)
        .is_err()
    {
        return Ok(None);
    }
    transition_request(
        conn,
        ctx,
        r,
        AccessRequestTransition::RouteToTriage,
        Some(reason),
    )
    .await?;
    sqlx::query("UPDATE access_requests SET triage_reason = $2 WHERE id = $1")
        .bind(r.id)
        .bind(reason)
        .execute(&mut *conn)
        .await?;
    audit::emit(
        &mut *conn,
        ctx,
        "access_request.triage_routed",
        &state.cell,
        json!({ "access_request_id": r.id, "patient_id": r.patient_id, "reason": reason, "source": "deterministic" }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(Some(reason))
}

fn request_ctx(r: &RequestRow) -> ResourceCtx {
    ResourceCtx {
        tenant_id: r.tenant_id,
        patient_id: Some(r.patient_id),
        facility_id: r.facility_id,
    }
}

// ---------------------------------------------------------------------------
// Access request routes
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct CreateRequestBody {
    pub patient_id: Uuid,
    pub facility_id: Option<Uuid>,
    pub free_text: Option<String>,
    #[serde(default)]
    pub constraints: ConstraintsInput,
    /// Staff-established urgency; recorded as `human`.
    pub urgency: Option<String>,
    /// `false` leaves the request in `draft` for later completion.
    #[serde(default = "default_true")]
    pub submit: bool,
    pub idempotency_key: Option<String>,
}

fn default_true() -> bool {
    true
}

/// Create a request for a patient of the caller's tenant. The patient must
/// exist in the tenant; the request is bound to the facility the staff
/// member chose (or none) and authorized against it.
pub async fn create_request_for(
    state: &AppState,
    ctx: &AuthContext,
    patient_id: Uuid,
    channel: &str,
    body: CreateRequestBody,
) -> Result<RequestRow, ApiError> {
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    let r = create_request_in(&mut tx, ctx, state, patient_id, channel, body).await?;
    tx.commit().await?;
    Ok(r)
}

/// Transaction-level body of [`create_request_for`]: validation, idempotent
/// replay, insert, history, audit and (optionally) submission with the
/// deterministic triage floor.
pub async fn create_request_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    patient_id: Uuid,
    channel: &str,
    body: CreateRequestBody,
) -> Result<RequestRow, ApiError> {
    let idempotency_key = scheduling::clean_text(body.idempotency_key, "idempotency_key", 128)?;
    let free_text = scheduling::clean_text(body.free_text, "free_text", MAX_FREE_TEXT)?;
    let urgency = urgency_from(body.urgency)?;
    if urgency.is_some() && channel != "staff" {
        return Err(ApiError::bad_request(
            "validation_failed",
            "urgency is established by staff or deterministic rules",
        ));
    }
    if let Some(key) = &idempotency_key {
        if let Some(existing) = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM access_requests WHERE tenant_id = $1 AND created_by = $2 AND idempotency_key = $3",
        )
        .bind(ctx.tenant_id)
        .bind(ctx.user_id)
        .bind(key)
        .fetch_optional(&mut **tx)
        .await?
        {
            let r = load_request(tx, ctx.tenant_id, existing).await?;
            if r.patient_id != patient_id {
                return Err(ApiError::conflict(
                    "idempotency_conflict",
                    "this idempotency key was used for a different request",
                ));
            }
            return Ok(r);
        }
    }
    let known: Option<Uuid> =
        sqlx::query_scalar("SELECT id FROM patients WHERE id = $1 AND tenant_id = $2")
            .bind(patient_id)
            .bind(ctx.tenant_id)
            .fetch_optional(&mut **tx)
            .await?;
    if known.is_none() {
        return Err(ApiError::not_found());
    }
    if let Some(f) = body.facility_id {
        let ok: Option<Uuid> =
            sqlx::query_scalar("SELECT id FROM facilities WHERE id = $1 AND tenant_id = $2")
                .bind(f)
                .bind(ctx.tenant_id)
                .fetch_optional(&mut **tx)
                .await?;
        if ok.is_none() {
            return Err(ApiError::bad_request(
                "validation_failed",
                "facility_id is not a facility of this tenant",
            ));
        }
    }
    let constraints =
        apply_constraints(tx, ctx.tenant_id, &Constraints::default(), body.constraints).await?;
    let missing = missing_information(&constraints);
    let id = Uuid::now_v7();
    let (urgency_value, urgency_source) = match urgency {
        Some(u) => (u, "human"),
        None => (Urgency::Routine, "default"),
    };
    sqlx::query(
        "INSERT INTO access_requests (id, tenant_id, patient_id, facility_id, status, channel, free_text,
             constraints, missing_info, urgency, urgency_source, idempotency_key, created_by)
         VALUES ($1,$2,$3,$4,'draft',$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(patient_id)
    .bind(body.facility_id)
    .bind(channel)
    .bind(&free_text)
    .bind(serde_json::to_value(&constraints).map_err(ApiError::internal)?)
    .bind(&missing)
    .bind(urgency_value.as_str())
    .bind(urgency_source)
    .bind(&idempotency_key)
    .bind(ctx.user_id)
    .execute(&mut **tx)
    .await?;
    request_history(
        tx,
        ctx.tenant_id,
        id,
        None,
        "draft",
        None,
        &scheduling::actor_label(ctx),
    )
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "access_request.created",
        &state.cell,
        json!({ "access_request_id": id, "patient_id": patient_id, "channel": channel,
                "has_free_text": free_text.is_some(), "urgency": urgency_value.as_str() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let mut r = load_request(tx, ctx.tenant_id, id).await?;
    if body.submit {
        transition_request(tx, ctx, &r, AccessRequestTransition::Submit, None).await?;
        r = load_request(tx, ctx.tenant_id, id).await?;
        apply_triage_floor(tx, ctx, state, &r).await?;
        r = load_request(tx, ctx.tenant_id, id).await?;
    }
    Ok(r)
}

async fn create_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<CreateRequestBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "access_request",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: Some(body.patient_id),
            facility_id: body.facility_id,
        }),
    )
    .await?;
    allowed.record_on_pool(&state, &ctx).await?;
    let patient_id = body.patient_id;
    let r = create_request_for(&state, &ctx, patient_id, "staff", body).await?;
    Ok((StatusCode::CREATED, Json(request_json(&r))))
}

#[derive(Debug, Deserialize)]
pub struct ListRequestsQuery {
    pub status: Option<String>,
    pub facility_id: Option<Uuid>,
    pub patient_id: Option<Uuid>,
    pub limit: Option<i64>,
    /// Keyset cursor: the `id` of the last row of the previous page.
    pub after: Option<Uuid>,
}

async fn list_requests(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListRequestsQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "access_request",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: q.patient_id,
            facility_id: q.facility_id,
        }),
    )
    .await?;
    allowed.record_on_pool(&state, &ctx).await?;
    let statuses: Vec<String> = match q.status.as_deref() {
        None | Some("open") => vec![
            "submitted".into(),
            "needs_clinical_triage".into(),
            "options_ready".into(),
        ],
        Some("all") => [
            AccessRequestStatus::Draft,
            AccessRequestStatus::Submitted,
            AccessRequestStatus::NeedsClinicalTriage,
            AccessRequestStatus::OptionsReady,
            AccessRequestStatus::Booked,
            AccessRequestStatus::Closed,
            AccessRequestStatus::Withdrawn,
        ]
        .iter()
        .map(|s| s.as_str().to_string())
        .collect(),
        Some(s) => {
            let st = AccessRequestStatus::parse(s).ok_or_else(|| {
                ApiError::bad_request("validation_failed", "unknown status filter")
            })?;
            vec![st.as_str().to_string()]
        }
    };
    let (scope_all, mut scope_ids) = match facility_scope(&ctx, actions::SCHEDULING_READ) {
        None => (true, Vec::new()),
        Some(ids) => (false, ids),
    };
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
        "SELECT {REQUEST_COLUMNS} FROM access_requests
         WHERE tenant_id = $1 AND status = ANY($2)
           AND ($3 OR facility_id IS NULL OR facility_id = ANY($4))
           AND ($5::uuid IS NULL OR patient_id = $5)
           AND ($6::uuid IS NULL OR id > $6)
         ORDER BY id
         LIMIT $7"
    ))
    .bind(ctx.tenant_id)
    .bind(&statuses)
    .bind(scope_all)
    .bind(&scope_ids)
    .bind(q.patient_id)
    .bind(q.after)
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await?;
    let mut items = Vec::with_capacity(rows.len());
    for r in rows.iter().take(limit as usize) {
        items.push(request_json(&request_from_row(r)?));
    }
    let next_after = if rows.len() as i64 > limit {
        items.last().and_then(|v| v.get("id").cloned())
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "next_after": next_after })))
}

async fn get_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let r = load_request(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "access_request",
        Some(request_ctx(&r)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let offers = offers_for_request(&mut conn, &r).await?;
    let run = latest_run(&mut conn, r.tenant_id, r.id).await?;
    Ok(Json(json!({
        "request": request_json(&r),
        "offers": offers.iter().map(scheduling::offer_json).collect::<Vec<_>>(),
        "matcher_run": run,
    })))
}

async fn request_history_route(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let r = load_request(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "access_request",
        Some(request_ctx(&r)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let items = request_history_rows(&mut conn, r.tenant_id, r.id).await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Deserialize)]
pub struct AmendBody {
    pub version: Option<i64>,
    pub facility_id: Option<Uuid>,
    pub free_text: Option<String>,
    #[serde(default)]
    pub constraints: ConstraintsInput,
    pub urgency: Option<String>,
}

/// Amend constraints (and optionally free text / staff urgency). A request
/// with options moves back to `submitted`; live offers are revoked because
/// they answered the previous constraints. Draft requests stay drafts.
pub async fn amend_request_for(
    state: &AppState,
    ctx: &AuthContext,
    r: RequestRow,
    body: AmendBody,
    allow_urgency: bool,
) -> Result<RequestRow, ApiError> {
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Scheduling).await?;
    let urgency = urgency_from(body.urgency)?;
    if urgency.is_some() && !allow_urgency {
        return Err(ApiError::bad_request(
            "validation_failed",
            "urgency is established by staff or deterministic rules",
        ));
    }
    let mut tx = state.pool.begin().await?;
    let r = lock_request(&mut tx, r.tenant_id, r.id).await?;
    require_version(r.version, body.version)?;
    let can_amend = r.status == AccessRequestStatus::Draft
        || r.status.apply(AccessRequestTransition::Amend).is_ok();
    if !can_amend {
        return Err(invalid_request_transition(
            &r,
            AccessRequestTransition::Amend,
        ));
    }
    let facility_id = match body.facility_id {
        Some(f) => {
            let ok: Option<Uuid> =
                sqlx::query_scalar("SELECT id FROM facilities WHERE id = $1 AND tenant_id = $2")
                    .bind(f)
                    .bind(ctx.tenant_id)
                    .fetch_optional(&mut *tx)
                    .await?;
            if ok.is_none() {
                return Err(ApiError::bad_request(
                    "validation_failed",
                    "facility_id is not a facility of this tenant",
                ));
            }
            Some(f)
        }
        None => r.facility_id,
    };
    let free_text = match body.free_text {
        Some(t) => scheduling::clean_text(Some(t), "free_text", MAX_FREE_TEXT)?,
        None => r.free_text.clone(),
    };
    let constraints =
        apply_constraints(&mut tx, ctx.tenant_id, &r.constraints, body.constraints).await?;
    write_constraints(
        &mut tx,
        &r,
        &constraints,
        facility_id,
        free_text.as_deref(),
        urgency.map(|u| (u, "human")),
    )
    .await?;
    let mut r = load_request(&mut tx, ctx.tenant_id, r.id).await?;
    if r.status != AccessRequestStatus::Draft {
        revoke_live_offers(&mut tx, ctx, state, &r, "request_amended").await?;
        transition_request(
            &mut tx,
            ctx,
            &r,
            AccessRequestTransition::Amend,
            Some("amended"),
        )
        .await?;
        r = load_request(&mut tx, ctx.tenant_id, r.id).await?;
        apply_triage_floor(&mut tx, ctx, state, &r).await?;
        r = load_request(&mut tx, ctx.tenant_id, r.id).await?;
    }
    audit::emit(
        &mut *tx,
        ctx,
        "access_request.amended",
        &state.cell,
        json!({ "access_request_id": r.id, "patient_id": r.patient_id, "status": r.status.as_str(),
                "urgency_changed": urgency.is_some() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(r)
}

async fn amend_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<AmendBody>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let r = load_request(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "access_request",
        Some(request_ctx(&r)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let r = amend_request_for(&state, &ctx, r, body, true).await?;
    Ok(Json(request_json(&r)))
}

#[derive(Debug, Default, Deserialize)]
pub struct TransitionBody {
    pub version: Option<i64>,
    pub reason: Option<String>,
}

/// Submit a draft; the deterministic triage floor applies immediately.
pub async fn submit_request_for(
    state: &AppState,
    ctx: &AuthContext,
    r: RequestRow,
    version: Option<i64>,
) -> Result<RequestRow, ApiError> {
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    let r = lock_request(&mut tx, r.tenant_id, r.id).await?;
    require_version(r.version, version)?;
    transition_request(&mut tx, ctx, &r, AccessRequestTransition::Submit, None).await?;
    let r = load_request(&mut tx, ctx.tenant_id, r.id).await?;
    apply_triage_floor(&mut tx, ctx, state, &r).await?;
    let r = load_request(&mut tx, ctx.tenant_id, r.id).await?;
    audit::emit(
        &mut *tx,
        ctx,
        "access_request.submitted",
        &state.cell,
        json!({ "access_request_id": r.id, "patient_id": r.patient_id, "status": r.status.as_str() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(r)
}

async fn submit_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<TransitionBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let mut conn = state.pool.acquire().await?;
    let r = load_request(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "access_request",
        Some(request_ctx(&r)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let r = submit_request_for(&state, &ctx, r, body.version).await?;
    Ok(Json(request_json(&r)))
}

async fn simple_transition(
    state: &AppState,
    ctx: &AuthContext,
    id: Uuid,
    t: AccessRequestTransition,
    body: TransitionBody,
    event: &str,
    reason_required: bool,
) -> Result<RequestRow, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let r = load_request(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        state,
        ctx,
        actions::SCHEDULING_MANAGE,
        "access_request",
        Some(request_ctx(&r)),
    )
    .await?;
    drop(conn);
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Scheduling).await?;
    let reason = if reason_required {
        Some(require_reason(body.reason, "reason")?)
    } else {
        scheduling::clean_text(body.reason, "reason", MAX_REASON)?
    };
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, ctx, &state.cell).await?;
    let r = lock_request(&mut tx, r.tenant_id, r.id).await?;
    require_version(r.version, body.version)?;
    let next = transition_request(&mut tx, ctx, &r, t, reason.as_deref()).await?;
    match t {
        AccessRequestTransition::RouteToTriage => {
            sqlx::query("UPDATE access_requests SET triage_reason = $2 WHERE id = $1")
                .bind(r.id)
                .bind(reason.as_deref().unwrap_or("staff_decision"))
                .execute(&mut *tx)
                .await?;
        }
        AccessRequestTransition::Close | AccessRequestTransition::Withdraw => {
            sqlx::query("UPDATE access_requests SET closed_reason = $2 WHERE id = $1")
                .bind(r.id)
                .bind(reason.as_deref())
                .execute(&mut *tx)
                .await?;
            revoke_live_offers(&mut tx, ctx, state, &r, next.as_str()).await?;
        }
        _ => {}
    }
    audit::emit(
        &mut *tx,
        ctx,
        event,
        &state.cell,
        json!({ "access_request_id": r.id, "patient_id": r.patient_id, "from": r.status.as_str(),
                "to": next.as_str(), "reason": reason }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let r = load_request(&mut tx, ctx.tenant_id, r.id).await?;
    tx.commit().await?;
    Ok(r)
}

async fn route_to_triage(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<TransitionBody>,
) -> Result<Json<Value>, ApiError> {
    let r = simple_transition(
        &state,
        &ctx,
        id,
        AccessRequestTransition::RouteToTriage,
        body.0.unwrap_or_default(),
        "access_request.triage_routed",
        true,
    )
    .await?;
    Ok(Json(request_json(&r)))
}

async fn clear_triage(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<TransitionBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let mut conn = state.pool.acquire().await?;
    let r = load_request(&mut conn, ctx.tenant_id, id).await?;
    // Clearing triage is a clinical decision on the request: the caller
    // must hold the triage capability, not just scheduling.
    let allowed = guard(
        &state,
        &ctx,
        actions::TRIAGE_WRITE,
        "access_request",
        Some(request_ctx(&r)),
    )
    .await?;
    drop(conn);
    let reason = require_reason(body.reason, "reason")?;
    let urgency = urgency_from(body_urgency(&reason))?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let r = lock_request(&mut tx, r.tenant_id, r.id).await?;
    require_version(r.version, body.version)?;
    // A cleared request whose text still trips the red-flag floor would be
    // re-routed at the next submit; the human decision lowers it to
    // `priority` at most so the floor no longer fires on urgency alone.
    let human_urgency = urgency.unwrap_or(match r.urgency {
        Urgency::Urgent => Urgency::Priority,
        u => u,
    });
    transition_request(
        &mut tx,
        &ctx,
        &r,
        AccessRequestTransition::TriageCleared,
        Some(&reason),
    )
    .await?;
    sqlx::query(
        "UPDATE access_requests SET urgency = $2, urgency_source = 'human', triage_reason = NULL WHERE id = $1",
    )
    .bind(r.id)
    .bind(human_urgency.as_str())
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "access_request.triage_cleared",
        &state.cell,
        json!({ "access_request_id": r.id, "patient_id": r.patient_id, "urgency": human_urgency.as_str() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let r = load_request(&mut tx, ctx.tenant_id, r.id).await?;
    tx.commit().await?;
    Ok(Json(request_json(&r)))
}

/// `clear-triage` carries the decided urgency as `reason` prefix
/// `urgency:<level>;` when the clinician sets one; anything else is a note.
fn body_urgency(reason: &str) -> Option<String> {
    reason
        .strip_prefix("urgency:")
        .and_then(|rest| rest.split(';').next())
        .map(|s| s.trim().to_string())
}

async fn close_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<TransitionBody>,
) -> Result<Json<Value>, ApiError> {
    let r = simple_transition(
        &state,
        &ctx,
        id,
        AccessRequestTransition::Close,
        body.0.unwrap_or_default(),
        "access_request.closed",
        true,
    )
    .await?;
    Ok(Json(request_json(&r)))
}

async fn withdraw_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<TransitionBody>,
) -> Result<Json<Value>, ApiError> {
    let r = simple_transition(
        &state,
        &ctx,
        id,
        AccessRequestTransition::Withdraw,
        body.0.unwrap_or_default(),
        "access_request.withdrawn",
        false,
    )
    .await?;
    Ok(Json(request_json(&r)))
}

/// Withdraw on behalf of the patient (self-service path): no staff action.
pub async fn withdraw_request_for(
    state: &AppState,
    ctx: &AuthContext,
    r: RequestRow,
    body: TransitionBody,
) -> Result<RequestRow, ApiError> {
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Scheduling).await?;
    let reason = scheduling::clean_text(body.reason, "reason", MAX_REASON)?;
    let mut tx = state.pool.begin().await?;
    let r = lock_request(&mut tx, r.tenant_id, r.id).await?;
    require_version(r.version, body.version)?;
    transition_request(
        &mut tx,
        ctx,
        &r,
        AccessRequestTransition::Withdraw,
        reason.as_deref(),
    )
    .await?;
    sqlx::query("UPDATE access_requests SET closed_reason = $2 WHERE id = $1")
        .bind(r.id)
        .bind(reason.as_deref())
        .execute(&mut *tx)
        .await?;
    revoke_live_offers(&mut tx, ctx, state, &r, "withdrawn").await?;
    audit::emit(
        &mut *tx,
        ctx,
        "access_request.withdrawn",
        &state.cell,
        json!({ "access_request_id": r.id, "patient_id": r.patient_id, "by_patient": true }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let r = load_request(&mut tx, ctx.tenant_id, r.id).await?;
    tx.commit().await?;
    Ok(r)
}

/// Offers still live for a request are revoked and their holds released.
async fn revoke_live_offers(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    r: &RequestRow,
    reason: &str,
) -> Result<(), ApiError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT id FROM appointment_offers WHERE access_request_id = $1 AND status IN ('offered','held') ORDER BY id",
    )
    .bind(r.id)
    .fetch_all(&mut **tx)
    .await?;
    for id in ids {
        scheduling::revoke_offer_for_resource(tx, ctx, state, id, reason).await?;
    }
    Ok(())
}

pub async fn offers_for_request(
    conn: &mut PgConnection,
    r: &RequestRow,
) -> Result<Vec<OfferRow>, ApiError> {
    let ids: Vec<Uuid> = sqlx::query_scalar(
        "SELECT o.id FROM appointment_offers o
         WHERE o.access_request_id = $1
           AND o.matcher_run_id IS NOT DISTINCT FROM (
                SELECT id FROM matcher_runs WHERE access_request_id = $1 ORDER BY created_at DESC LIMIT 1)
         ORDER BY o.rank NULLS LAST, o.starts_at, o.id",
    )
    .bind(r.id)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(ids.len());
    for id in ids {
        out.push(scheduling::load_offer(conn, id).await?);
    }
    Ok(out)
}

pub async fn latest_run(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    request_id: Uuid,
) -> Result<Option<Value>, ApiError> {
    let row = sqlx::query(
        "SELECT id, matcher_version, rejected_summary, ranking_mode, ranking_artifact_id, created_at, expires_at,
                jsonb_array_length(candidates) AS candidate_count
         FROM matcher_runs WHERE tenant_id = $1 AND access_request_id = $2
         ORDER BY created_at DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(request_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| run_summary_json(&r)))
}

fn run_summary_json(r: &sqlx::postgres::PgRow) -> Value {
    json!({
        "id": r.get::<Uuid, _>("id"),
        "matcher_version": r.get::<String, _>("matcher_version"),
        "candidate_count": r.get::<i32, _>("candidate_count"),
        "rejected_summary": r.get::<Value, _>("rejected_summary"),
        "ranking_mode": r.get::<String, _>("ranking_mode"),
        "ranking_artifact_id": r.get::<Option<Uuid>, _>("ranking_artifact_id"),
        "created_at": r.get::<DateTime<Utc>, _>("created_at"),
        "expires_at": r.get::<DateTime<Utc>, _>("expires_at"),
    })
}

async fn request_offers(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let r = load_request(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "access_request",
        Some(request_ctx(&r)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    scheduling::sweep_expired(&mut conn, &ctx, &state.cell, ctx.tenant_id).await?;
    let offers = offers_for_request(&mut conn, &r).await?;
    Ok(Json(json!({
        "items": offers.iter().map(scheduling::offer_json).collect::<Vec<_>>(),
    })))
}

// ---------------------------------------------------------------------------
// access-intent.v1
// ---------------------------------------------------------------------------

async fn vocabulary(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    kind: &str,
) -> Result<Vec<VocabularyTerm>, ApiError> {
    let rows = sqlx::query(
        "SELECT code, name_en, name_es, synonyms FROM catalog_entries
         WHERE tenant_id = $1 AND kind = $2 AND active ORDER BY code LIMIT 500",
    )
    .bind(tenant_id)
    .bind(kind)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| VocabularyTerm {
            code: r.get("code"),
            name_en: r.get("name_en"),
            name_es: r.get("name_es"),
            synonyms: r.get("synonyms"),
        })
        .collect())
}

async fn facility_vocabulary(
    conn: &mut PgConnection,
    tenant_id: Uuid,
) -> Result<Vec<VocabularyTerm>, ApiError> {
    let rows = sqlx::query("SELECT id, name FROM facilities WHERE tenant_id = $1 ORDER BY name")
        .bind(tenant_id)
        .fetch_all(&mut *conn)
        .await?;
    Ok(rows
        .iter()
        .map(|r| {
            let name: String = r.get("name");
            VocabularyTerm {
                code: r.get::<Uuid, _>("id").to_string(),
                name_en: name.clone(),
                name_es: name,
                synonyms: Vec::new(),
            }
        })
        .collect())
}

#[derive(Debug, Default, Deserialize)]
pub struct InterpretBody {
    pub version: Option<i64>,
    pub language: Option<String>,
}

/// Governed `access-intent.v1`: structures the request's free text into
/// catalog codes and open questions. Fields the request already has are
/// kept; the intent fills only what is missing. The deterministic triage
/// floor is applied by the gateway and again here; a model-only triage
/// suggestion is surfaced for a human, never acted on automatically.
pub async fn interpret_request_for(
    state: &AppState,
    ctx: &AuthContext,
    r: RequestRow,
    body: InterpretBody,
) -> Result<Value, ApiError> {
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Scheduling).await?;
    let language = body
        .language
        .as_deref()
        .map(str::trim)
        .filter(|l| *l == "es" || *l == "en")
        .unwrap_or("en")
        .to_string();
    let Some(free_text) = r.free_text.clone() else {
        return Err(ApiError::conflict(
            "no_free_text",
            "the request has no text to interpret",
        ));
    };
    if !matches!(
        r.status,
        AccessRequestStatus::Draft
            | AccessRequestStatus::Submitted
            | AccessRequestStatus::OptionsReady
            | AccessRequestStatus::NeedsClinicalTriage
    ) {
        return Err(ApiError::conflict(
            "invalid_transition",
            "closed requests cannot be interpreted",
        ));
    }
    let mut tx = state.pool.begin().await?;
    let r = lock_request(&mut tx, r.tenant_id, r.id).await?;
    require_version(r.version, body.version)?;
    let req = AccessIntentRequest {
        template: ACCESS_INTENT_TEMPLATE.to_string(),
        language: language.clone(),
        free_text,
        urgency: r.urgency,
        already_known: known_constraint_fields(&r.constraints),
        services: vocabulary(&mut tx, r.tenant_id, "clinical_service").await?,
        specialties: vocabulary(&mut tx, r.tenant_id, "specialty").await?,
        modalities: vocabulary(&mut tx, r.tenant_id, "modality").await?,
        accessibility: vocabulary(&mut tx, r.tenant_id, "accessibility_capability").await?,
        facilities: facility_vocabulary(&mut tx, r.tenant_id).await?,
    };
    audit::emit(
        &mut *tx,
        ctx,
        "ai.artifact.requested",
        &state.cell,
        json!({ "access_request_id": r.id, "patient_id": r.patient_id, "template": ACCESS_INTENT_TEMPLATE }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;

    let hash = hash_json(&req);
    let scope = aigov::ReuseScope::AccessIntent {
        access_request_id: r.id,
    };
    let plan = aigov::plan(
        state,
        r.tenant_id,
        r.patient_id,
        scope,
        &hash,
        ACCESS_INTENT_SCHEMA,
    )
    .await?;
    let synthetic = plan.model_synthetic();
    let vocab = req.vocabulary();
    let floor = req.floor_reason();
    let (output, provider, prompt_version, usage, reused_from, execution_id) = match plan {
        aigov::ExecutionPlan::Reuse(prior) => {
            let output: AccessIntentV1 = prior.output_as()?;
            (
                output,
                ProviderInfo {
                    provider: prior.route.clone(),
                    model: prior.model.clone(),
                    model_version: prior.model_version.clone(),
                },
                prior.prompt_version.clone(),
                prior.usage_as(),
                Some(prior.id),
                None,
            )
        }
        aigov::ExecutionPlan::Execute { execution_id, .. } => {
            match state.gateway.interpret_access_intent(&req).await {
                Ok(resp) => (
                    resp.output,
                    resp.provider,
                    resp.prompt_version,
                    resp.usage,
                    None,
                    Some(execution_id),
                ),
                Err(err) => {
                    record_generation_failure(state, ctx, r.patient_id, &err).await?;
                    return Err(aigov::gateway_error(err));
                }
            }
        }
    };
    // Defence in depth: the floor and vocabulary are re-applied to exactly
    // what is stored.
    let output = output
        .finalize(&vocab, floor)
        .map_err(|e| ApiError::internal(format!("intent validation: {e}")))?;

    let mut tx = state.pool.begin().await?;
    let r = lock_request(&mut tx, r.tenant_id, r.id).await?;
    let artifact_id = Uuid::now_v7();
    sqlx::query(
        "UPDATE ai_artifacts SET status = $1
         WHERE tenant_id = $2 AND access_request_id = $3 AND artifact_type = 'access_intent' AND status = $4",
    )
    .bind(ArtifactStatus::Superseded.as_str())
    .bind(r.tenant_id)
    .bind(r.id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .execute(&mut *tx)
    .await?;
    sqlx::query(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, access_request_id, artifact_type, autonomy_level, status,
          model, model_version, route, template, input_hash, output, output_schema,
          citations, limitations, generated_at)
         VALUES ($1,$2,$3,$4,'access_intent','A1',$5,$6,$7,$8,$9,$10,$11,$12,$13,$14, now())",
    )
    .bind(artifact_id)
    .bind(r.tenant_id)
    .bind(r.patient_id)
    .bind(r.id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .bind(&provider.model)
    .bind(&provider.model_version)
    .bind(&provider.provider)
    .bind(ACCESS_INTENT_TEMPLATE)
    .bind(&hash)
    .bind(serde_json::to_value(&output).map_err(ApiError::internal)?)
    .bind(ACCESS_INTENT_SCHEMA)
    .bind(serde_json::to_value(&output.cited_sources).map_err(ApiError::internal)?)
    .bind(serde_json::to_value(&output.limitations).map_err(ApiError::internal)?)
    .execute(&mut *tx)
    .await?;
    let input_refs = AccessIntentV1::citable(&vocab);
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

    // Fill only what the request does not already say; every code the
    // intent proposes is re-validated against the catalogs like any input.
    let c = &r.constraints;
    let facility_ids: Vec<Uuid> = output
        .facility_codes
        .iter()
        .filter_map(|f| f.parse().ok())
        .collect();
    let input = ConstraintsInput {
        service_code: c
            .service_code
            .is_none()
            .then_some(output.service_code.clone())
            .flatten(),
        specialty_code: c
            .specialty_code
            .is_none()
            .then_some(output.specialty_code.clone())
            .flatten(),
        modality_codes: (c.modality_codes.is_empty() && !output.modality_codes.is_empty())
            .then_some(output.modality_codes.clone()),
        facility_ids: (c.facility_ids.is_empty() && !facility_ids.is_empty())
            .then_some(facility_ids),
        earliest: c
            .earliest
            .is_none()
            .then_some(
                output
                    .earliest_date
                    .and_then(|d| d.and_hms_opt(0, 0, 0).map(|t| t.and_utc())),
            )
            .flatten(),
        latest: c
            .latest
            .is_none()
            .then_some(
                output
                    .latest_date
                    .and_then(|d| d.and_hms_opt(23, 59, 59).map(|t| t.and_utc())),
            )
            .flatten(),
        preferred_windows: (c.preferred_windows.is_empty() && !output.preferred_windows.is_empty())
            .then_some(output.preferred_windows.clone()),
        max_travel_minutes: None,
        continuity_required: (!c.continuity_required && output.continuity_requested)
            .then_some(true),
        accessibility_codes: (c.accessibility_codes.is_empty()
            && !output.accessibility_codes.is_empty())
        .then_some(output.accessibility_codes.clone()),
        language: c
            .language
            .is_none()
            .then_some(output.language.clone())
            .flatten(),
        transport_requested: (!c.transport_requested && output.transport_requested).then_some(true),
        has_referral: None,
    };
    let merged = apply_constraints(&mut tx, r.tenant_id, c, input).await?;
    let mut missing = missing_information(&merged);
    for q in &output.missing_information {
        if !missing.contains(q) {
            missing.push(q.clone());
        }
    }
    missing.truncate(MAX_LIST);
    let updated = sqlx::query(
        "UPDATE access_requests SET constraints = $1, missing_info = $2, intent_artifact_id = $3,
             version = version + 1, updated_at = now()
         WHERE id = $4 AND version = $5",
    )
    .bind(serde_json::to_value(&merged).map_err(ApiError::internal)?)
    .bind(&missing)
    .bind(artifact_id)
    .bind(r.id)
    .bind(r.version)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(scheduling::stale());
    }
    let mut r = load_request(&mut tx, r.tenant_id, r.id).await?;
    // Only the deterministic floor moves the state machine.
    if floor.is_some() {
        apply_triage_floor(&mut tx, ctx, state, &r).await?;
        r = load_request(&mut tx, r.tenant_id, r.id).await?;
    }
    audit::emit(
        &mut *tx,
        ctx,
        "ai.artifact.generated",
        &state.cell,
        json!({
            "artifact_id": artifact_id,
            "access_request_id": r.id,
            "patient_id": r.patient_id,
            "template": ACCESS_INTENT_TEMPLATE,
            "prompt_version": prompt_version,
            "model": provider.model,
            "input_hash": hash,
            "reused_from": reused_from,
            "synthetic": synthetic,
            "triage_floor": floor,
            "triage_suggested_by_model": output.clinical_triage_suggested && floor.is_none(),
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(json!({
        "request": request_json(&r),
        "intent": {
            "artifact_id": artifact_id,
            "output": output,
            "provider": provider,
            "prompt_version": prompt_version,
            "synthetic": synthetic,
            "reused": reused_from.is_some(),
            "triage_floor": floor,
            "triage_suggested": output.clinical_triage_suggested,
        },
    }))
}

async fn record_generation_failure(
    state: &AppState,
    ctx: &AuthContext,
    patient_id: Uuid,
    err: &GatewayError,
) -> Result<(), ApiError> {
    audit::record(
        &state.pool,
        ctx,
        "ai.generation.failed",
        Some("patient"),
        Some(patient_id.to_string()),
        "deny",
        Some(failure_code(err)),
    )
    .await
    .map_err(ApiError::internal)
}

fn failure_code(err: &GatewayError) -> &'static str {
    match err {
        GatewayError::Unavailable(_) => "provider_unavailable",
        GatewayError::Disabled(_) => "provider_disabled",
        GatewayError::InvalidOutput(_) => "invalid_output",
        GatewayError::PolicyDenied(_) => "policy_denied",
    }
}

async fn interpret_request(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<InterpretBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let mut conn = state.pool.acquire().await?;
    let r = load_request(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "access_request",
        Some(request_ctx(&r)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    Ok(Json(interpret_request_for(&state, &ctx, r, body).await?))
}

// ---------------------------------------------------------------------------
// access-matcher.v1 runs
// ---------------------------------------------------------------------------

#[derive(Debug, Default, Deserialize)]
pub struct MatchBody {
    pub version: Option<i64>,
    /// One-time travel origin; used in memory for this run only and never
    /// stored. Requires the patient's `scheduling_location` consent.
    pub origin: Option<Origin>,
    /// `false` skips dMind ranking even when available.
    pub ranking: Option<bool>,
    pub language: Option<String>,
}

#[derive(Debug, Clone, Copy, Deserialize)]
pub struct Origin {
    pub latitude: f64,
    pub longitude: f64,
}

#[derive(Debug, Serialize)]
pub struct RankingOutcome {
    pub mode: &'static str,
    pub artifact_id: Option<Uuid>,
    pub synthetic: Option<bool>,
    pub reused: bool,
    /// Why the run stayed deterministic (`disabled`, `unavailable`,
    /// `consent_required`, `quota_exceeded`, `invalid_output`,
    /// `single_candidate`, `not_requested`).
    pub reason: Option<String>,
}

pub struct MatchResult {
    pub request: RequestRow,
    pub run_id: Uuid,
    pub offers: Vec<OfferRow>,
    pub rejected: BTreeMap<String, u64>,
    pub ranking: RankingOutcome,
}

pub fn match_result_json(m: &MatchResult) -> Value {
    json!({
        "request": request_json(&m.request),
        "matcher_run_id": m.run_id,
        "matcher_version": wellos_domain::matcher::MATCHER_VERSION,
        "offers": m.offers.iter().map(scheduling::offer_json).collect::<Vec<_>>(),
        "rejected_summary": m.rejected,
        "ranking": m.ranking,
    })
}

/// The persisted deterministic half of a matcher run.
pub struct MatcherRun {
    pub run_id: Uuid,
    pub output: MatchOutput,
    pub facts: MatchFacts,
    pub offers: Vec<OfferRow>,
}

/// Run `access-matcher.v1` for a request inside the caller's transaction:
/// lock and validate the request, assemble the facts, persist the run
/// verbatim, revoke live offers of earlier runs and materialize the new
/// candidates as offers. Deterministic only; dMind ranking happens after
/// commit in [`run_matcher_for`].
#[allow(clippy::too_many_arguments)]
pub async fn run_matcher_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    tenant_id: Uuid,
    request_id: Uuid,
    version: Option<i64>,
    origin: Option<Origin>,
    offered_to: &str,
) -> Result<MatcherRun, ApiError> {
    let r = lock_request(tx, tenant_id, request_id).await?;
    require_version(r.version, version)?;
    if r.status == AccessRequestStatus::NeedsClinicalTriage {
        return Err(ApiError::conflict(
            "clinical_triage_required",
            "this request must be seen by clinical triage before options are offered",
        ));
    }
    if r.status
        .apply(AccessRequestTransition::OptionsGenerated)
        .is_err()
    {
        return Err(invalid_request_transition(
            &r,
            AccessRequestTransition::OptionsGenerated,
        ));
    }
    let Some(service_code) = r.constraints.service_code.clone() else {
        return Err(ApiError::conflict(
            "missing_information",
            "the requested service must be known before options can be generated",
        ));
    };
    let service = scheduling::load_service(tx, r.tenant_id, &service_code).await?;
    if service.config.requires_referral && !r.constraints.has_referral {
        return Err(ApiError::conflict(
            "referral_required",
            "this service requires a referral; record it on the request first",
        ));
    }
    let policy = scheduling::load_policy(tx, r.tenant_id).await?;
    let prefs = scheduling::load_preferences(tx, r.tenant_id, r.patient_id).await?;
    let now = Utc::now();
    let window_start = r.constraints.earliest.map_or(now, |e| e.max(now)).max(now);
    let horizon_end = now + Duration::days(policy.horizon_days as i64);
    let window_end = r
        .constraints
        .latest
        .map_or(horizon_end, |l| l.min(horizon_end));
    if window_end <= window_start {
        return Err(ApiError::conflict(
            "window_empty",
            "the requested time window is in the past or beyond the scheduling horizon",
        ));
    }
    let facility_filter: Vec<Uuid> = if !r.constraints.facility_ids.is_empty() {
        r.constraints.facility_ids.clone()
    } else if let Some(f) = r.facility_id {
        vec![f]
    } else {
        prefs.preferred_facility_ids.clone()
    };
    let facility_only = (!facility_filter.is_empty()).then_some(facility_filter.as_slice());
    let facilities = scheduling::load_facility_facts(tx, r.tenant_id, facility_only).await?;
    let resources = scheduling::load_resource_facts(
        tx,
        r.tenant_id,
        facility_only,
        &service.code,
        &service.config.required_resource_types,
        window_start,
        window_end,
    )
    .await?;
    let origin = match origin {
        Some(o) => {
            if !scheduling::consent_active(
                tx,
                r.tenant_id,
                r.patient_id,
                scheduling::CONSENT_LOCATION,
            )
            .await?
            {
                return Err(scheduling::consent_required(scheduling::CONSENT_LOCATION));
            }
            Some((o.latitude, o.longitude))
        }
        None => None,
    };
    let patient = scheduling::load_patient_facts(
        tx,
        r.tenant_id,
        r.patient_id,
        &prefs,
        &policy.time_zone,
        origin,
        r.constraints.has_referral,
        window_start,
        window_end,
    )
    .await?;
    let mut patient = patient;
    if let Some(l) = &r.constraints.language {
        patient.language = Some(l.clone());
    }
    for code in &r.constraints.accessibility_codes {
        if !patient.accessibility_needs.contains(code) {
            patient.accessibility_needs.push(code.clone());
        }
    }
    let policy_facts = scheduling::load_policy_facts(
        tx,
        r.tenant_id,
        &policy,
        facility_only,
        window_start,
        window_end,
    )
    .await?;
    let waitlist_position = waitlist_position(tx, r.tenant_id, r.patient_id, &service.code).await?;
    let modality_codes = if r.constraints.modality_codes.is_empty() {
        prefs.preferred_modalities.clone()
    } else {
        r.constraints.modality_codes.clone()
    };
    let facts = MatchFacts {
        now,
        request: RequestConstraints {
            service_code: service.code.clone(),
            service: service.config.clone(),
            modality_codes,
            facility_ids: facility_filter.clone(),
            earliest: r.constraints.earliest,
            latest: r.constraints.latest,
            preferred_windows: r.constraints.preferred_windows.clone(),
            max_travel_minutes: r.constraints.max_travel_minutes,
            continuity_required: r.constraints.continuity_required,
            urgency: r.urgency,
            requested_at: r.created_at,
            waitlist_position,
        },
        patient,
        facilities,
        resources,
        policy: policy_facts,
    };
    let output = find_candidates(&facts);

    // Persist the exact source facts minus the one-time travel origin.
    let mut stored_facts = facts.clone();
    stored_facts.patient.origin = None;
    let run_id = Uuid::now_v7();
    let expires_at = now + Duration::minutes(policy.offer_ttl_minutes as i64);
    sqlx::query(
        "INSERT INTO matcher_runs (id, tenant_id, access_request_id, patient_id, matcher_version, source_facts,
             candidates, rejected_summary, ranking_mode, created_by, expires_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'deterministic',$9,$10)",
    )
    .bind(run_id)
    .bind(r.tenant_id)
    .bind(r.id)
    .bind(r.patient_id)
    .bind(&output.matcher_version)
    .bind(json!({ "facts": stored_facts, "facts_hash": output.facts_hash, "origin_supplied": origin.is_some(),
                  "window_start": output.window_start, "window_end": output.window_end }))
    .bind(serde_json::to_value(&output.candidates).map_err(ApiError::internal)?)
    .bind(serde_json::to_value(&output.rejected).map_err(ApiError::internal)?)
    .bind(ctx.user_id)
    .bind(expires_at)
    .execute(&mut **tx)
    .await?;
    revoke_live_offers(tx, ctx, state, &r, "superseded_by_new_run").await?;
    let offers = scheduling::materialize_offers(
        tx,
        ctx,
        r.tenant_id,
        r.patient_id,
        r.id,
        run_id,
        &service.code,
        &output.candidates,
        &BTreeMap::new(),
        offered_to,
        policy.offer_ttl_minutes,
    )
    .await?;
    transition_request(
        tx,
        ctx,
        &r,
        AccessRequestTransition::OptionsGenerated,
        Some(&format!("{} options", output.candidates.len())),
    )
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "access_request.matched",
        &state.cell,
        json!({
            "access_request_id": r.id,
            "patient_id": r.patient_id,
            "matcher_run_id": run_id,
            "matcher_version": output.matcher_version,
            "candidates": output.candidates.len(),
            "rejected": output.rejected,
            "origin_supplied": origin.is_some(),
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(MatcherRun {
        run_id,
        output,
        facts,
        offers,
    })
}

/// Run `access-matcher.v1` for a request, persist the run verbatim and
/// materialize its candidates as offers. Then, at most once, ask dMind to
/// reorder and explain the head of the list; any AI failure leaves the
/// deterministic result intact and reported as such.
pub async fn run_matcher_for(
    state: &AppState,
    ctx: &AuthContext,
    r: RequestRow,
    body: MatchBody,
    offered_to: &str,
) -> Result<MatchResult, ApiError> {
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Scheduling).await?;
    let language = body
        .language
        .as_deref()
        .map(str::trim)
        .filter(|l| *l == "es" || *l == "en")
        .unwrap_or("en")
        .to_string();
    if let Some(o) = body.origin {
        if !(-90.0..=90.0).contains(&o.latitude) || !(-180.0..=180.0).contains(&o.longitude) {
            return Err(ApiError::bad_request(
                "validation_failed",
                "origin coordinates are out of range",
            ));
        }
    }
    let mut tx = state.pool.begin().await?;
    let run = run_matcher_in(
        &mut tx,
        ctx,
        state,
        r.tenant_id,
        r.id,
        body.version,
        body.origin,
        offered_to,
    )
    .await?;
    let r = load_request(&mut tx, r.tenant_id, r.id).await?;
    tx.commit().await?;
    let MatcherRun {
        run_id,
        output,
        facts,
        offers,
    } = run;

    let ranking = if body.ranking == Some(false) {
        RankingOutcome {
            mode: "deterministic",
            artifact_id: None,
            synthetic: None,
            reused: false,
            reason: Some("not_requested".into()),
        }
    } else if output.candidates.len() < 2 {
        RankingOutcome {
            mode: "deterministic",
            artifact_id: None,
            synthetic: None,
            reused: false,
            reason: Some("single_candidate".into()),
        }
    } else {
        rank_run(
            state,
            ctx,
            &r,
            run_id,
            &output.candidates,
            &facts,
            &language,
        )
        .await?
    };
    let mut conn = state.pool.acquire().await?;
    let request = load_request(&mut conn, r.tenant_id, r.id).await?;
    let offers = if ranking.artifact_id.is_some() {
        offers_for_request(&mut conn, &request).await?
    } else {
        offers
    };
    Ok(MatchResult {
        request,
        run_id,
        offers,
        rejected: output.rejected,
        ranking,
    })
}

async fn waitlist_position(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
    service_code: &str,
) -> Result<Option<(i32, i32)>, ApiError> {
    let row = sqlx::query(
        "WITH q AS (
            SELECT patient_id, row_number() OVER (ORDER BY joined_at, id) AS pos, count(*) OVER () AS total
            FROM waitlist_entries WHERE tenant_id = $1 AND service_code = $2 AND status = 'active')
         SELECT pos::int AS pos, total::int AS total FROM q WHERE patient_id = $3",
    )
    .bind(tenant_id)
    .bind(service_code)
    .bind(patient_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| (r.get::<i32, _>("pos"), r.get::<i32, _>("total"))))
}

fn ranking_facts(facts: &MatchFacts) -> Vec<(String, String)> {
    let mut out = vec![
        (
            "request:service".to_string(),
            format!("requested service {}", facts.request.service_code),
        ),
        (
            "request:urgency".to_string(),
            format!("urgency {}", facts.request.urgency.as_str()),
        ),
    ];
    if !facts.request.preferred_windows.is_empty() {
        out.push((
            "request:preferred_windows".to_string(),
            format!(
                "{} preferred weekly windows",
                facts.request.preferred_windows.len()
            ),
        ));
    }
    if let Some(m) = facts.request.max_travel_minutes {
        out.push((
            "request:max_travel".to_string(),
            format!("maximum travel {m} minutes"),
        ));
    }
    if facts.request.continuity_required {
        out.push((
            "request:continuity".to_string(),
            "continuity with the current care team requested".to_string(),
        ));
    }
    if !facts.patient.accessibility_needs.is_empty() {
        out.push((
            "patient:accessibility".to_string(),
            format!(
                "accessibility needs: {}",
                facts.patient.accessibility_needs.join(", ")
            ),
        ));
    }
    if let Some(l) = &facts.patient.language {
        out.push(("patient:language".to_string(), format!("language {l}")));
    }
    out
}

/// One bounded `appointment-ranking.v1` call over the deterministic head.
/// Never fails the run: every problem is returned as the reason the result
/// stayed deterministic.
async fn rank_run(
    state: &AppState,
    ctx: &AuthContext,
    r: &RequestRow,
    run_id: Uuid,
    candidates: &[Candidate],
    facts: &MatchFacts,
    language: &str,
) -> Result<RankingOutcome, ApiError> {
    let deterministic = |reason: &str| RankingOutcome {
        mode: "deterministic",
        artifact_id: None,
        synthetic: None,
        reused: false,
        reason: Some(reason.to_string()),
    };
    let status = state.gateway.status();
    if !status.state.is_callable() {
        return Ok(deterministic(match status.state {
            dmind_gateway::CapabilityState::Disabled => "disabled",
            _ => "unavailable",
        }));
    }
    let mut conn = state.pool.acquire().await?;
    let req = ranking_request(&mut conn, r.tenant_id, facts, candidates, language).await?;
    drop(conn);
    let hash = hash_json(&req);
    let scope = aigov::ReuseScope::AppointmentRanking {
        matcher_run_id: run_id,
    };
    audit::record(
        &state.pool,
        ctx,
        "ai.artifact.requested",
        Some("matcher_run"),
        Some(run_id.to_string()),
        "allow",
        Some(APPOINTMENT_RANKING_TEMPLATE),
    )
    .await
    .map_err(ApiError::internal)?;
    let plan = match aigov::plan(
        state,
        r.tenant_id,
        r.patient_id,
        scope,
        &hash,
        APPOINTMENT_RANKING_SCHEMA,
    )
    .await
    {
        Ok(p) => p,
        Err(e) => {
            let reason = match e.code {
                "ai_external_consent_required" => "consent_required",
                "ai_quota_exceeded" => "quota_exceeded",
                "ai_disabled" => "disabled",
                _ => "unavailable",
            };
            audit::record(
                &state.pool,
                ctx,
                "ai.generation.skipped",
                Some("matcher_run"),
                Some(run_id.to_string()),
                "deny",
                Some(reason),
            )
            .await
            .map_err(ApiError::internal)?;
            return Ok(deterministic(reason));
        }
    };
    let synthetic = plan.model_synthetic();
    let (output, provider, prompt_version, usage, reused_from, execution_id) = match plan {
        aigov::ExecutionPlan::Reuse(prior) => {
            let output: AppointmentRankingV1 = prior.output_as()?;
            (
                output,
                ProviderInfo {
                    provider: prior.route.clone(),
                    model: prior.model.clone(),
                    model_version: prior.model_version.clone(),
                },
                prior.prompt_version.clone(),
                prior.usage_as(),
                Some(prior.id),
                None,
            )
        }
        aigov::ExecutionPlan::Execute { execution_id, .. } => {
            match state.gateway.rank_appointments(&req).await {
                Ok(resp) => (
                    resp.output,
                    resp.provider,
                    resp.prompt_version,
                    resp.usage,
                    None,
                    Some(execution_id),
                ),
                Err(err) => {
                    record_generation_failure(state, ctx, r.patient_id, &err).await?;
                    return Ok(deterministic(match err {
                        GatewayError::Disabled(_) => "disabled",
                        GatewayError::InvalidOutput(_) => "invalid_output",
                        GatewayError::PolicyDenied(_) => "policy_denied",
                        GatewayError::Unavailable(_) => "unavailable",
                    }));
                }
            }
        }
    };
    // Server-side re-validation against the exact candidate set: a ranking
    // that names, drops or duplicates a candidate is discarded.
    if let Err(e) = output.validate(&req.candidate_ids(), &req.fact_refs()) {
        audit::record(
            &state.pool,
            ctx,
            "ai.generation.failed",
            Some("matcher_run"),
            Some(run_id.to_string()),
            "deny",
            Some(&format!("invalid_output: {e}")),
        )
        .await
        .map_err(ApiError::internal)?;
        return Ok(deterministic("invalid_output"));
    }

    let mut tx = state.pool.begin().await?;
    let artifact_id = persist_ranking(
        &mut tx,
        ctx,
        state,
        r,
        run_id,
        RankingResult {
            request: &req,
            output: &output,
            provider: &provider,
            prompt_version: &prompt_version,
            usage: usage.as_ref(),
            synthetic,
            reused_from,
            execution_id,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(RankingOutcome {
        mode: "dmind",
        artifact_id: Some(artifact_id),
        synthetic: Some(synthetic),
        reused: reused_from.is_some(),
        reason: None,
    })
}

/// The bounded `appointment-ranking.v1` request for a deterministic run:
/// the head of the candidate list (never one call per slot) with the
/// tenant's facility and resource labels attached.
pub async fn ranking_request(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    facts: &MatchFacts,
    candidates: &[Candidate],
    language: &str,
) -> Result<RankingRequest, ApiError> {
    let head = &candidates[..candidates
        .len()
        .min(RANKING_HEAD)
        .min(MAX_RANKING_CANDIDATES)];
    let facility_names: BTreeMap<Uuid, String> =
        sqlx::query("SELECT id, name FROM facilities WHERE tenant_id = $1")
            .bind(tenant_id)
            .fetch_all(&mut *conn)
            .await?
            .iter()
            .map(|row| (row.get::<Uuid, _>("id"), row.get::<String, _>("name")))
            .collect();
    let resource_names: BTreeMap<Uuid, String> = facts
        .resources
        .iter()
        .map(|res| (res.resource_id, res.name.clone()))
        .collect();
    Ok(RankingRequest {
        template: APPOINTMENT_RANKING_TEMPLATE.to_string(),
        language: language.to_string(),
        facts: ranking_facts(facts),
        candidates: head
            .iter()
            .map(|c| RankingCandidate {
                candidate_id: c.candidate_id.clone(),
                starts_at: c.starts_at,
                ends_at: c.ends_at,
                facility_label: facility_names
                    .get(&c.facility_id)
                    .cloned()
                    .unwrap_or_else(|| "facility".to_string()),
                modality_code: c.modality_code.clone(),
                resource_labels: c
                    .resources
                    .iter()
                    .map(|res| {
                        resource_names
                            .get(&res.resource_id)
                            .cloned()
                            .unwrap_or_else(|| res.role.clone())
                    })
                    .collect(),
                score: c.score,
                factors: c
                    .factors
                    .iter()
                    .map(|f| (f.code.clone(), f.points, f.detail.clone()))
                    .collect(),
                travel_minutes: c.travel.as_ref().map(|t| t.minutes),
                reasons: c.reasons.clone(),
            })
            .collect(),
    })
}

/// A validated ranking about to be persisted against its matcher run.
pub struct RankingResult<'a> {
    pub request: &'a RankingRequest,
    pub output: &'a AppointmentRankingV1,
    pub provider: &'a ProviderInfo,
    pub prompt_version: &'a str,
    pub usage: Option<&'a dmind_gateway::Usage>,
    pub synthetic: bool,
    pub reused_from: Option<Uuid>,
    pub execution_id: Option<Uuid>,
}

/// Persist an already validated ranking as an `appointment_ranking`
/// artifact bound to its matcher run, reorder the ranked head of the live
/// offers and attach each explanation to its own candidate. Candidates
/// beyond the head keep deterministic order.
pub async fn persist_ranking(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    r: &RequestRow,
    run_id: Uuid,
    result: RankingResult<'_>,
) -> Result<Uuid, ApiError> {
    let RankingResult {
        request: req,
        output,
        provider,
        prompt_version,
        usage,
        synthetic,
        reused_from,
        execution_id,
    } = result;
    let hash = hash_json(req);
    let scope = aigov::ReuseScope::AppointmentRanking {
        matcher_run_id: run_id,
    };
    let artifact_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, matcher_run_id, artifact_type, autonomy_level, status,
          model, model_version, route, template, input_hash, output, output_schema,
          citations, limitations, generated_at)
         VALUES ($1,$2,$3,$4,'appointment_ranking','A1',$5,$6,$7,$8,$9,$10,$11,$12,$13,$14, now())",
    )
    .bind(artifact_id)
    .bind(r.tenant_id)
    .bind(r.patient_id)
    .bind(run_id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .bind(&provider.model)
    .bind(&provider.model_version)
    .bind(&provider.provider)
    .bind(APPOINTMENT_RANKING_TEMPLATE)
    .bind(&hash)
    .bind(serde_json::to_value(output).map_err(ApiError::internal)?)
    .bind(APPOINTMENT_RANKING_SCHEMA)
    .bind(serde_json::to_value(&output.cited_sources).map_err(ApiError::internal)?)
    .bind(serde_json::to_value(&output.limitations).map_err(ApiError::internal)?)
    .execute(&mut **tx)
    .await?;
    let mut input_refs = req.fact_refs();
    input_refs.extend(req.candidate_ids());
    aigov::annotate(
        tx,
        artifact_id,
        &aigov::Provenance {
            scope,
            provider,
            prompt_version,
            input_refs: &input_refs,
            usage,
            synthetic,
            reused_from,
        },
    )
    .await?;
    if let Some(execution_id) = execution_id {
        aigov::bind_execution(tx, execution_id, artifact_id).await?;
    }
    for ranked in &output.ranked {
        sqlx::query(
            "UPDATE appointment_offers SET rank = $3, explanation = $4, updated_at = now()
             WHERE matcher_run_id = $1 AND candidate_id = $2 AND status IN ('offered','held')",
        )
        .bind(run_id)
        .bind(&ranked.candidate_id)
        .bind(ranked.rank as i32)
        .bind(json!({
            "artifact_id": artifact_id,
            "text": ranked.explanation,
            "cited_sources": ranked.cited_sources,
            "synthetic": synthetic,
        }))
        .execute(&mut **tx)
        .await?;
    }
    sqlx::query(
        "UPDATE matcher_runs SET ranking_mode = 'dmind', ranking_artifact_id = $2 WHERE id = $1",
    )
    .bind(run_id)
    .bind(artifact_id)
    .execute(&mut **tx)
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "ai.artifact.generated",
        &state.cell,
        json!({
            "artifact_id": artifact_id,
            "matcher_run_id": run_id,
            "access_request_id": r.id,
            "patient_id": r.patient_id,
            "template": APPOINTMENT_RANKING_TEMPLATE,
            "prompt_version": prompt_version,
            "model": provider.model,
            "input_hash": hash,
            "reused_from": reused_from,
            "synthetic": synthetic,
            "ranked": output.ranked.len(),
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(artifact_id)
}

async fn run_matcher(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<MatchBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let mut conn = state.pool.acquire().await?;
    let r = load_request(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "access_request",
        Some(request_ctx(&r)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let m = run_matcher_for(&state, &ctx, r, body, "staff").await?;
    Ok(Json(match_result_json(&m)))
}

async fn get_matcher_run(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let row = sqlx::query(
        "SELECT id, access_request_id, patient_id, matcher_version, source_facts, candidates, rejected_summary,
                ranking_mode, ranking_artifact_id, created_at, expires_at,
                jsonb_array_length(candidates) AS candidate_count
         FROM matcher_runs WHERE id = $1 AND tenant_id = $2",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let request_id: Uuid = row.get("access_request_id");
    let r = load_request(&mut conn, ctx.tenant_id, request_id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "matcher_run",
        Some(request_ctx(&r)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let mut v = run_summary_json(&row);
    v["access_request_id"] = json!(request_id);
    v["patient_id"] = json!(row.get::<Uuid, _>("patient_id"));
    v["candidates"] = row.get::<Value, _>("candidates");
    v["source_facts"] = row.get::<Value, _>("source_facts");
    if let Some(artifact_id) = row.get::<Option<Uuid>, _>("ranking_artifact_id") {
        v["ranking_artifact"] = artifact_json(&mut conn, ctx.tenant_id, artifact_id).await?;
    }
    audit::emit(
        &mut *conn,
        &ctx,
        "access_request.matcher_run.read",
        &state.cell,
        json!({ "matcher_run_id": id, "access_request_id": request_id, "patient_id": r.patient_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(Json(v))
}

async fn artifact_json(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    artifact_id: Uuid,
) -> Result<Value, ApiError> {
    let row = sqlx::query(
        "SELECT id, artifact_type, status, model, model_version, route, provider, prompt_version, output,
                citations, limitations, synthetic, reused_from, usage, generated_at
         FROM ai_artifacts WHERE id = $1 AND tenant_id = $2",
    )
    .bind(artifact_id)
    .bind(tenant_id)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(match row {
        None => Value::Null,
        Some(r) => json!({
            "id": r.get::<Uuid, _>("id"),
            "artifact_type": r.get::<String, _>("artifact_type"),
            "status": r.get::<String, _>("status"),
            "model": r.get::<Option<String>, _>("model"),
            "model_version": r.get::<Option<String>, _>("model_version"),
            "provider": r.get::<Option<String>, _>("provider").or(r.get::<Option<String>, _>("route")),
            "prompt_version": r.get::<Option<String>, _>("prompt_version"),
            "output": r.get::<Option<Value>, _>("output"),
            "citations": r.get::<Value, _>("citations"),
            "limitations": r.get::<Value, _>("limitations"),
            "synthetic": r.get::<bool, _>("synthetic"),
            "reused_from": r.get::<Option<Uuid>, _>("reused_from"),
            "usage": r.get::<Option<Value>, _>("usage"),
            "generated_at": r.get::<Option<DateTime<Utc>>, _>("generated_at"),
        }),
    })
}

// ---------------------------------------------------------------------------
// Offers and holds
// ---------------------------------------------------------------------------

fn offer_ctx(o: &OfferRow) -> ResourceCtx {
    ResourceCtx {
        tenant_id: o.tenant_id,
        patient_id: Some(o.patient_id),
        facility_id: Some(o.facility_id),
    }
}

/// Tenant-scoped load; foreign offers are indistinguishable from unknown.
pub async fn load_offer_scoped(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<OfferRow, ApiError> {
    let o = scheduling::load_offer(conn, id).await?;
    if o.tenant_id != tenant_id {
        return Err(ApiError::not_found());
    }
    Ok(o)
}

async fn get_offer(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let o = load_offer_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "appointment_offer",
        Some(offer_ctx(&o)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    Ok(Json(scheduling::offer_json(&o)))
}

pub async fn offer_history_json(
    conn: &mut PgConnection,
    o: &OfferRow,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT from_status, to_status, reason, actor, recorded_at
         FROM appointment_offer_history WHERE tenant_id = $1 AND offer_id = $2 ORDER BY recorded_at, id",
    )
    .bind(o.tenant_id)
    .bind(o.id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "from_status": r.get::<Option<String>, _>("from_status"),
                "to_status": r.get::<String, _>("to_status"),
                "reason": r.get::<Option<String>, _>("reason"),
                "actor": r.get::<String, _>("actor"),
                "recorded_at": r.get::<DateTime<Utc>, _>("recorded_at"),
            })
        })
        .collect())
}

async fn offer_history_route(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let o = load_offer_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "appointment_offer",
        Some(offer_ctx(&o)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let items = offer_history_json(&mut conn, &o).await?;
    Ok(Json(json!({ "items": items })))
}

#[derive(Debug, Default, Deserialize)]
pub struct OfferActionBody {
    pub version: Option<i64>,
    pub reason: Option<String>,
}

/// Place the temporary hold behind an offer. Concurrent holds on the same
/// capacity are serialized by `book_plans`; the loser gets 409 `slot_taken`
/// and an audited race-lost record.
pub async fn hold_offer_for(
    state: &AppState,
    ctx: &AuthContext,
    o: OfferRow,
    body: OfferActionBody,
) -> Result<OfferRow, ApiError> {
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    let held = match hold_offer_in(&mut tx, ctx, state, o.id, body.version).await {
        Ok(h) => h,
        Err(e) if e.code == "slot_taken" => {
            drop(tx);
            scheduling::record_race_lost(state, ctx, o.id, "hold").await;
            return Err(e);
        }
        Err(e) => return Err(e),
    };
    tx.commit().await?;
    Ok(held)
}

/// Transaction-scoped core of [`hold_offer_for`]: lock, re-hold a lapsed
/// hold, and place the atomic hold. The caller owns the transaction (route
/// wrappers, fixtures).
pub async fn hold_offer_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    offer_id: Uuid,
    version: Option<i64>,
) -> Result<OfferRow, ApiError> {
    let o = scheduling::lock_offer(tx, offer_id).await?;
    require_version(o.version, version)?;
    if o.status == OfferStatus::Held {
        if o.hold_expires_at.is_some_and(|h| h > Utc::now()) {
            return Ok(o);
        }
        // Lapsed hold: release then re-hold below.
        scheduling::release_offer_bookings(tx, o.id).await?;
        scheduling::transition_offer(
            tx,
            &o,
            OfferTransition::ReleaseHold,
            None,
            Some("hold_lapsed"),
            &scheduling::actor_label(ctx),
        )
        .await?;
    }
    let o = scheduling::lock_offer(tx, o.id).await?;
    if o.status.apply(OfferTransition::Hold).is_err() {
        return Err(ApiError::conflict(
            "invalid_transition",
            format!("an offer in status {} cannot be held", o.status.as_str()),
        ));
    }
    let policy = scheduling::load_policy(tx, o.tenant_id).await?;
    scheduling::hold_offer(tx, ctx, state, &o, &policy).await
}

async fn hold_offer_route(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<OfferActionBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let mut conn = state.pool.acquire().await?;
    let o = load_offer_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "appointment_offer",
        Some(offer_ctx(&o)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let o = hold_offer_for(&state, &ctx, o, body).await?;
    Ok(Json(scheduling::offer_json(&o)))
}

pub async fn release_hold_for(
    state: &AppState,
    ctx: &AuthContext,
    o: OfferRow,
    body: OfferActionBody,
) -> Result<OfferRow, ApiError> {
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Scheduling).await?;
    let reason = scheduling::clean_text(body.reason, "reason", MAX_REASON)?;
    let mut tx = state.pool.begin().await?;
    let o = scheduling::lock_offer(&mut tx, o.id).await?;
    require_version(o.version, body.version)?;
    scheduling::release_offer_bookings(&mut tx, o.id).await?;
    scheduling::transition_offer(
        &mut tx,
        &o,
        OfferTransition::ReleaseHold,
        None,
        reason.as_deref().or(Some("released")),
        &scheduling::actor_label(ctx),
    )
    .await?;
    audit::emit(
        &mut *tx,
        ctx,
        "appointment.offer.released",
        &state.cell,
        json!({ "offer_id": o.id, "patient_id": o.patient_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let o = scheduling::load_offer(&mut tx, o.id).await?;
    tx.commit().await?;
    Ok(o)
}

async fn release_hold_route(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<OfferActionBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let mut conn = state.pool.acquire().await?;
    let o = load_offer_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "appointment_offer",
        Some(offer_ctx(&o)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let o = release_hold_for(&state, &ctx, o, body).await?;
    Ok(Json(scheduling::offer_json(&o)))
}

pub async fn decline_offer_for(
    state: &AppState,
    ctx: &AuthContext,
    o: OfferRow,
    body: OfferActionBody,
    by_patient: bool,
) -> Result<OfferRow, ApiError> {
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Scheduling).await?;
    let reason = scheduling::clean_text(body.reason, "reason", MAX_REASON)?;
    let mut tx = state.pool.begin().await?;
    let o = scheduling::lock_offer(&mut tx, o.id).await?;
    require_version(o.version, body.version)?;
    scheduling::release_offer_bookings(&mut tx, o.id).await?;
    scheduling::transition_offer(
        &mut tx,
        &o,
        OfferTransition::Decline,
        None,
        reason.as_deref().or(Some("declined")),
        &scheduling::actor_label(ctx),
    )
    .await?;
    audit::emit(
        &mut *tx,
        ctx,
        "appointment.offer.declined",
        &state.cell,
        json!({ "offer_id": o.id, "patient_id": o.patient_id, "by_patient": by_patient,
                "cancellation_event_id": o.cancellation_event_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    if o.cancellation_event_id.is_some() {
        crate::recovery::on_offer_closed(&mut tx, ctx, state, o.id, "declined").await?;
    }
    let o = scheduling::load_offer(&mut tx, o.id).await?;
    tx.commit().await?;
    Ok(o)
}

async fn decline_offer_route(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<OfferActionBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let mut conn = state.pool.acquire().await?;
    let o = load_offer_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "appointment_offer",
        Some(offer_ctx(&o)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let o = decline_offer_for(&state, &ctx, o, body, false).await?;
    Ok(Json(scheduling::offer_json(&o)))
}

#[derive(Debug, Default, Deserialize)]
pub struct AcceptBody {
    pub version: Option<i64>,
    pub reason: Option<String>,
    /// Mandatory when staff book outside policy (notice, window, etc.).
    pub override_reason: Option<String>,
    pub idempotency_key: Option<String>,
    /// The appointment this acceptance replaces (reschedule).
    pub reschedule_of: Option<Uuid>,
    pub reschedule_reason: Option<String>,
}

/// Accept an offer into a confirmed appointment (or a reschedule of an
/// existing one). Atomic: bookings convert or are placed, the appointment
/// and linked visit are written, siblings revoked and notifications
/// scheduled in one transaction; a lost race is 409 `slot_taken`.
pub async fn accept_offer_for(
    state: &AppState,
    ctx: &AuthContext,
    o: OfferRow,
    body: AcceptBody,
    booked_via: &str,
) -> Result<AppointmentRow, ApiError> {
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Scheduling).await?;
    let by_patient = matches!(booked_via, "patient" | "representative");
    let reason = scheduling::clean_text(body.reason, "reason", MAX_REASON)?;
    let override_reason =
        scheduling::clean_text(body.override_reason, "override_reason", MAX_REASON)?;
    if by_patient && override_reason.is_some() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "only staff can override scheduling policy",
        ));
    }
    let idempotency_key = scheduling::clean_text(body.idempotency_key, "idempotency_key", 128)?;
    let reschedule_reason =
        scheduling::clean_text(body.reschedule_reason, "reschedule_reason", MAX_REASON)?;
    let mut tx = state.pool.begin().await?;
    let input = AcceptInput {
        offer_id: o.id,
        version: body.version,
        reason,
        override_reason,
        idempotency_key,
        reschedule_of: body.reschedule_of,
        reschedule_reason,
        booked_via,
    };
    let a = match accept_offer_in(&mut tx, ctx, state, input).await {
        Ok(a) => a,
        Err(e) if e.code == "slot_taken" => {
            drop(tx);
            scheduling::record_race_lost(state, ctx, o.id, "accept").await;
            return Err(e);
        }
        Err(e) => return Err(e),
    };
    tx.commit().await?;
    Ok(a)
}

/// Validated acceptance parameters for [`accept_offer_in`].
pub struct AcceptInput<'a> {
    pub offer_id: Uuid,
    pub version: Option<i64>,
    pub reason: Option<String>,
    pub override_reason: Option<String>,
    pub idempotency_key: Option<String>,
    pub reschedule_of: Option<Uuid>,
    pub reschedule_reason: Option<String>,
    /// `staff`, `patient` or `representative`; recovery offers are recorded
    /// as `waitlist` whichever channel accepted them.
    pub booked_via: &'a str,
}

/// Transaction-scoped core of [`accept_offer_for`]. The caller owns the
/// transaction (route wrappers, fixtures).
pub async fn accept_offer_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    input: AcceptInput<'_>,
) -> Result<AppointmentRow, ApiError> {
    let AcceptInput {
        offer_id,
        version,
        reason,
        override_reason,
        idempotency_key,
        reschedule_of,
        reschedule_reason,
        booked_via,
    } = input;
    let by_patient = matches!(booked_via, "patient" | "representative");
    let o = scheduling::load_offer(tx, offer_id).await?;
    if let Some(key) = &idempotency_key {
        if let Some(existing) = sqlx::query_scalar::<_, Uuid>(
            "SELECT id FROM appointments WHERE tenant_id = $1 AND booked_by = $2 AND idempotency_key = $3",
        )
        .bind(o.tenant_id)
        .bind(ctx.user_id)
        .bind(key)
        .fetch_optional(&mut **tx)
        .await?
        {
            let a = scheduling::load_appointment(tx, existing).await?;
            if a.offer_id != Some(o.id) {
                return Err(ApiError::conflict(
                    "idempotency_conflict",
                    "this idempotency key was used for a different offer",
                ));
            }
            return Ok(a);
        }
    }
    let o = scheduling::lock_offer(tx, o.id).await?;
    require_version(o.version, version)?;
    let policy = scheduling::load_policy(tx, o.tenant_id).await?;
    // Acceptance always goes through a hold: an un-held offer takes its
    // atomic hold here, in the same transaction, so a concurrent acceptance of
    // the same capacity loses on the booking exclusion rather than racing.
    let o = if o.status == OfferStatus::Offered {
        scheduling::hold_offer(tx, ctx, state, &o, &policy).await?
    } else {
        o
    };
    if o.status.apply(OfferTransition::Accept).is_err() {
        return Err(ApiError::conflict(
            "invalid_transition",
            format!(
                "an offer in status {} cannot be accepted",
                o.status.as_str()
            ),
        ));
    }
    let service = scheduling::load_service(tx, o.tenant_id, &o.service_code).await?;
    let now = Utc::now();
    let notice_ok = o.starts_at - now >= Duration::hours(policy.min_notice_hours as i64);
    if !notice_ok && override_reason.is_none() {
        return Err(ApiError::conflict(
            "min_notice",
            format!(
                "options must start at least {} hours ahead; staff can override with a reason",
                policy.min_notice_hours
            ),
        ));
    }
    let prior = match reschedule_of {
        Some(id) => {
            let a = scheduling::lock_appointment(tx, id).await?;
            if a.tenant_id != o.tenant_id || a.patient_id != o.patient_id {
                return Err(ApiError::not_found());
            }
            if a.status.apply(AppointmentTransition::Reschedule).is_err() {
                return Err(ApiError::conflict(
                    "invalid_transition",
                    format!(
                        "an appointment in status {} cannot be rescheduled",
                        a.status.as_str()
                    ),
                ));
            }
            if by_patient
                && !wellos_domain::access::within_patient_window(
                    now,
                    a.starts_at,
                    policy.reschedule_window_hours as i64,
                )
            {
                return Err(ApiError::conflict(
                    "reschedule_window_closed",
                    format!(
                        "appointments can be rescheduled online up to {} hours before they start; contact the facility",
                        policy.reschedule_window_hours
                    ),
                ));
            }
            Some(a)
        }
        None => None,
    };
    // Recovery offers record their waitlist origin whichever channel
    // accepted them; `booked_by` keeps the accepting principal.
    let booked_via = if o.waitlist_entry_id.is_some() {
        "waitlist"
    } else {
        booked_via
    };
    let input = scheduling::ConfirmInput {
        offer: &o,
        booked_via,
        reason,
        override_reason,
        idempotency_key,
        reschedule_of: prior.as_ref(),
        reschedule_reason: reschedule_reason
            .or_else(|| prior.as_ref().map(|_| RESCHEDULE_REASON.to_string())),
    };
    scheduling::confirm_offer(tx, ctx, state, input, &policy, &service).await
}

async fn accept_offer_route(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<AcceptBody>,
) -> Result<(StatusCode, Json<Value>), ApiError> {
    let body = body.0.unwrap_or_default();
    let mut conn = state.pool.acquire().await?;
    let o = load_offer_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "appointment_offer",
        Some(offer_ctx(&o)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let a = accept_offer_for(&state, &ctx, o, body, "staff").await?;
    Ok((StatusCode::CREATED, Json(scheduling::appointment_json(&a))))
}

// ---------------------------------------------------------------------------
// Appointments
// ---------------------------------------------------------------------------

fn appointment_ctx(a: &AppointmentRow) -> ResourceCtx {
    ResourceCtx {
        tenant_id: a.tenant_id,
        patient_id: Some(a.patient_id),
        facility_id: Some(a.facility_id),
    }
}

pub async fn load_appointment_scoped(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> Result<AppointmentRow, ApiError> {
    let a = scheduling::load_appointment(conn, id).await?;
    if a.tenant_id != tenant_id {
        return Err(ApiError::not_found());
    }
    Ok(a)
}

#[derive(Debug, Deserialize)]
pub struct ListAppointmentsQuery {
    pub facility_id: Option<Uuid>,
    pub patient_id: Option<Uuid>,
    pub resource_id: Option<Uuid>,
    pub service_code: Option<String>,
    pub status: Option<String>,
    pub from: Option<DateTime<Utc>>,
    pub to: Option<DateTime<Utc>>,
    pub limit: Option<i64>,
    pub after: Option<Uuid>,
}

async fn list_appointments(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListAppointmentsQuery>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "appointment",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: q.patient_id,
            facility_id: q.facility_id,
        }),
    )
    .await?;
    allowed.record_on_pool(&state, &ctx).await?;
    let (scope_all, mut scope_ids) = match facility_scope(&ctx, actions::SCHEDULING_READ) {
        None => (true, Vec::new()),
        Some(ids) => (false, ids),
    };
    if let Some(f) = q.facility_id {
        if scope_all || scope_ids.contains(&f) {
            scope_ids = vec![f];
        } else {
            return Ok(Json(json!({ "items": [], "next_after": null })));
        }
    }
    let scope_all = scope_all && q.facility_id.is_none();
    let statuses: Vec<String> = match q.status.as_deref() {
        None | Some("live") => vec!["confirmed".into()],
        Some("all") => [
            AppointmentStatus::Confirmed,
            AppointmentStatus::Rescheduled,
            AppointmentStatus::Cancelled,
            AppointmentStatus::Fulfilled,
            AppointmentStatus::NoShow,
        ]
        .iter()
        .map(|s| s.as_str().to_string())
        .collect(),
        Some(s) => {
            let st = AppointmentStatus::parse(s).ok_or_else(|| {
                ApiError::bad_request("validation_failed", "unknown status filter")
            })?;
            vec![st.as_str().to_string()]
        }
    };
    let from = q.from.unwrap_or_else(|| Utc::now() - Duration::days(1));
    let to = q.to.unwrap_or(from + Duration::days(7));
    if to <= from || to - from > Duration::days(62) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "the window must be between 1 minute and 62 days",
        ));
    }
    let limit = bounded_limit(q.limit);
    let rows = sqlx::query(&format!(
        "SELECT {} FROM appointments a
         WHERE a.tenant_id = $1 AND ($2 OR a.facility_id = ANY($3))
           AND a.status = ANY($4)
           AND a.starts_at < $6 AND a.ends_at > $5
           AND ($7::uuid IS NULL OR a.patient_id = $7)
           AND ($8::text IS NULL OR a.service_code = $8)
           AND ($9::uuid IS NULL OR EXISTS (
                SELECT 1 FROM appointment_resources ar WHERE ar.appointment_id = a.id AND ar.resource_id = $9))
           AND ($10::uuid IS NULL OR a.id > $10)
         ORDER BY a.id
         LIMIT $11",
        scheduling::APPOINTMENT_COLUMNS.replace("id,", "a.id,")
    ))
    .bind(ctx.tenant_id)
    .bind(scope_all)
    .bind(&scope_ids)
    .bind(&statuses)
    .bind(from)
    .bind(to)
    .bind(q.patient_id)
    .bind(q.service_code.as_deref().map(str::trim).filter(|s| !s.is_empty()))
    .bind(q.resource_id)
    .bind(q.after)
    .bind(limit + 1)
    .fetch_all(&state.pool)
    .await?;
    let mut items = Vec::with_capacity(rows.len());
    for r in rows.iter().take(limit as usize) {
        let a = scheduling::appointment_from_row(r)?;
        let mut v = scheduling::appointment_json(&a);
        v["resources"] = appointment_resources(&state.pool, a.id).await?;
        items.push(v);
    }
    let next_after = if rows.len() as i64 > limit {
        items.last().and_then(|v| v.get("id").cloned())
    } else {
        None
    };
    Ok(Json(json!({ "items": items, "next_after": next_after })))
}

pub async fn appointment_resources<'e, E>(exec: E, appointment_id: Uuid) -> Result<Value, ApiError>
where
    E: sqlx::Executor<'e, Database = sqlx::Postgres>,
{
    let rows = sqlx::query(
        "SELECT ar.resource_id, ar.role, sr.name, sr.resource_type_code
         FROM appointment_resources ar JOIN schedulable_resources sr ON sr.id = ar.resource_id
         WHERE ar.appointment_id = $1 ORDER BY ar.role, sr.name",
    )
    .bind(appointment_id)
    .fetch_all(exec)
    .await?;
    Ok(Value::Array(
        rows.iter()
            .map(|r| {
                json!({
                    "resource_id": r.get::<Uuid, _>("resource_id"),
                    "role": r.get::<String, _>("role"),
                    "name": r.get::<String, _>("name"),
                    "resource_type_code": r.get::<String, _>("resource_type_code"),
                })
            })
            .collect(),
    ))
}

pub async fn appointment_detail_json(
    conn: &mut PgConnection,
    a: &AppointmentRow,
) -> Result<Value, ApiError> {
    let mut v = scheduling::appointment_json(a);
    v["resources"] = appointment_resources(&mut *conn, a.id).await?;
    let service = sqlx::query(
        "SELECT name_en, name_es, config FROM catalog_entries
         WHERE tenant_id = $1 AND kind = 'clinical_service' AND code = $2",
    )
    .bind(a.tenant_id)
    .bind(&a.service_code)
    .fetch_optional(&mut *conn)
    .await?;
    if let Some(s) = service {
        let config: Value = s.get("config");
        v["service"] = json!({
            "code": a.service_code,
            "name_en": s.get::<String, _>("name_en"),
            "name_es": s.get::<String, _>("name_es"),
            "preparation_en": config.get("preparation_en").cloned().unwrap_or(Value::Null),
            "preparation_es": config.get("preparation_es").cloned().unwrap_or(Value::Null),
        });
    }
    let facility: Option<String> = sqlx::query_scalar("SELECT name FROM facilities WHERE id = $1")
        .bind(a.facility_id)
        .fetch_optional(&mut *conn)
        .await?;
    v["facility_name"] = json!(facility);
    if let Some(offer_id) = a.offer_id {
        let expl: Option<Value> =
            sqlx::query_scalar("SELECT explanation FROM appointment_offers WHERE id = $1")
                .bind(offer_id)
                .fetch_optional(&mut *conn)
                .await?
                .flatten();
        v["explanation"] = expl.unwrap_or(Value::Null);
    }
    Ok(v)
}

async fn get_appointment(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let a = load_appointment_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "appointment",
        Some(appointment_ctx(&a)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    Ok(Json(appointment_detail_json(&mut conn, &a).await?))
}

pub async fn appointment_history_json(
    conn: &mut PgConnection,
    a: &AppointmentRow,
) -> Result<Vec<Value>, ApiError> {
    let rows = sqlx::query(
        "SELECT from_status, to_status, starts_at_before, starts_at_after, reason_code, note, override, actor,
                version, recorded_at
         FROM appointment_history WHERE tenant_id = $1 AND appointment_id = $2 ORDER BY recorded_at, id",
    )
    .bind(a.tenant_id)
    .bind(a.id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .iter()
        .map(|r| {
            json!({
                "from_status": r.get::<Option<String>, _>("from_status"),
                "to_status": r.get::<String, _>("to_status"),
                "starts_at_before": r.get::<Option<DateTime<Utc>>, _>("starts_at_before"),
                "starts_at_after": r.get::<Option<DateTime<Utc>>, _>("starts_at_after"),
                "reason_code": r.get::<Option<String>, _>("reason_code"),
                "note": r.get::<Option<String>, _>("note"),
                "override": r.get::<bool, _>("override"),
                "actor": r.get::<String, _>("actor"),
                "version": r.get::<i64, _>("version"),
                "recorded_at": r.get::<DateTime<Utc>, _>("recorded_at"),
            })
        })
        .collect())
}

async fn appointment_history_route(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let a = load_appointment_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "appointment",
        Some(appointment_ctx(&a)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    let items = appointment_history_json(&mut conn, &a).await?;
    Ok(Json(json!({ "items": items })))
}

/// RFC 5545 event for one confirmed appointment: scheduling facts only.
pub async fn appointment_ics_for(
    state: &AppState,
    ctx: &AuthContext,
    a: &AppointmentRow,
    language: &str,
) -> Result<Response, ApiError> {
    if a.status != AppointmentStatus::Confirmed {
        return Err(ApiError::conflict(
            "not_confirmed",
            "only confirmed appointments can be exported",
        ));
    }
    let mut conn = state.pool.acquire().await?;
    let facility: Option<String> = sqlx::query_scalar("SELECT name FROM facilities WHERE id = $1")
        .bind(a.facility_id)
        .fetch_optional(&mut *conn)
        .await?;
    let service = sqlx::query(
        "SELECT name_en, name_es, config FROM catalog_entries
         WHERE tenant_id = $1 AND kind = 'clinical_service' AND code = $2",
    )
    .bind(a.tenant_id)
    .bind(&a.service_code)
    .fetch_optional(&mut *conn)
    .await?;
    let (summary, preparation) = match &service {
        Some(s) => {
            let config: Value = s.get("config");
            let key = if language == "es" {
                "preparation_es"
            } else {
                "preparation_en"
            };
            let name: String = if language == "es" {
                s.get("name_es")
            } else {
                s.get("name_en")
            };
            (
                name,
                config
                    .get(key)
                    .and_then(Value::as_str)
                    .map(str::to_string)
                    .unwrap_or_default(),
            )
        }
        None => (a.service_code.clone(), String::new()),
    };
    let description = if language == "es" {
        format!(
            "Cita confirmada. {}{}",
            if a.modality_code == "telehealth" {
                "Consulta remota. "
            } else {
                ""
            },
            preparation
        )
    } else {
        format!(
            "Confirmed appointment. {}{}",
            if a.modality_code == "telehealth" {
                "Remote consultation. "
            } else {
                ""
            },
            preparation
        )
    };
    let ics = wellos_domain::ics::appointment_event(
        &format!("appointment-{}@wellos", a.id),
        a.starts_at,
        a.ends_at,
        &summary,
        facility.as_deref().unwrap_or(""),
        description.trim(),
        Utc::now(),
    );
    audit::emit(
        &mut *conn,
        ctx,
        "appointment.ics.exported",
        &state.cell,
        json!({ "appointment_id": a.id, "patient_id": a.patient_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let mut resp = ics.into_response();
    resp.headers_mut().insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("text/calendar; charset=utf-8"),
    );
    resp.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_str(&format!(
            "attachment; filename=\"appointment-{}.ics\"",
            a.id
        ))
        .map_err(ApiError::internal)?,
    );
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    *resp.status_mut() = StatusCode::OK;
    Ok(resp)
}

#[derive(Debug, Deserialize)]
pub struct LangQuery {
    pub lang: Option<String>,
}

async fn appointment_ics(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Query(q): Query<LangQuery>,
) -> Result<Response, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let a = load_appointment_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_READ,
        "appointment",
        Some(appointment_ctx(&a)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let lang = if q.lang.as_deref() == Some("es") {
        "es"
    } else {
        "en"
    };
    appointment_ics_for(&state, &ctx, &a, lang).await
}

#[derive(Debug, Default, Deserialize)]
pub struct ConfirmBody {
    pub version: Option<i64>,
}

/// Record the patient's attendance confirmation on a confirmed appointment
/// and withdraw the pending confirmation reminders.
pub async fn confirm_attendance_for(
    state: &AppState,
    ctx: &AuthContext,
    a: AppointmentRow,
    body: ConfirmBody,
    by_patient: bool,
) -> Result<AppointmentRow, ApiError> {
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Scheduling).await?;
    let mut tx = state.pool.begin().await?;
    let a = scheduling::lock_appointment(&mut tx, a.id).await?;
    require_version(a.version, body.version)?;
    if a.status != AppointmentStatus::Confirmed {
        return Err(ApiError::conflict(
            "invalid_transition",
            format!(
                "an appointment in status {} cannot be confirmed by the patient",
                a.status.as_str()
            ),
        ));
    }
    if a.patient_confirmed_at.is_some() {
        tx.commit().await?;
        return Ok(a);
    }
    let updated = sqlx::query(
        "UPDATE appointments SET patient_confirmed_at = now(), version = version + 1, updated_at = now()
         WHERE id = $1 AND version = $2",
    )
    .bind(a.id)
    .bind(a.version)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 1 {
        return Err(scheduling::stale());
    }
    sqlx::query(
        "UPDATE notifications SET status = 'cancelled', updated_at = now()
         WHERE tenant_id = $1 AND appointment_id = $2 AND status IN ('scheduled','failed')
           AND kind IN ('confirmation_request','confirmation_follow_up','no_response_follow_up')",
    )
    .bind(a.tenant_id)
    .bind(a.id)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        ctx,
        "appointment.patient_confirmed",
        &state.cell,
        json!({ "appointment_id": a.id, "patient_id": a.patient_id, "by_patient": by_patient }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    let a = scheduling::load_appointment(&mut tx, a.id).await?;
    tx.commit().await?;
    Ok(a)
}

async fn confirm_attendance(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<ConfirmBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let mut conn = state.pool.acquire().await?;
    let a = load_appointment_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "appointment",
        Some(appointment_ctx(&a)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let a = confirm_attendance_for(&state, &ctx, a, body, false).await?;
    Ok(Json(scheduling::appointment_json(&a)))
}

#[derive(Debug, Default, Deserialize)]
pub struct RescheduleOptionsBody {
    pub version: Option<i64>,
    pub reason: Option<String>,
    #[serde(default)]
    pub constraints: ConstraintsInput,
    pub origin: Option<Origin>,
    pub ranking: Option<bool>,
    pub language: Option<String>,
}

/// Open a reschedule: a new access request for the same patient and
/// service (linked through `reschedule_of`) is submitted and matched. The
/// existing appointment stays confirmed until an option is accepted with
/// `reschedule_of`, which moves the visit atomically.
/// Transactional core of [`reschedule_options_for`]: opens the reschedule
/// access request (`reschedule_of` bound to the locked appointment) with
/// its history and audit rows. Policy/window checks belong to the caller.
pub async fn open_reschedule_request_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    a: &AppointmentRow,
    channel: &str,
    reason: Option<String>,
    mut input: ConstraintsInput,
) -> Result<RequestRow, ApiError> {
    let by_patient = channel != "staff";
    let base = Constraints {
        service_code: Some(a.service_code.clone()),
        modality_codes: vec![a.modality_code.clone()],
        facility_ids: vec![a.facility_id],
        reschedule_of: Some(a.id),
        ..Constraints::default()
    };
    input.service_code = None;
    let constraints = apply_constraints(tx, a.tenant_id, &base, input).await?;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO access_requests (id, tenant_id, patient_id, facility_id, status, channel, free_text,
             constraints, missing_info, urgency, urgency_source, created_by)
         VALUES ($1,$2,$3,$4,'submitted',$5,NULL,$6,'{}','routine','default',$7)",
    )
    .bind(id)
    .bind(a.tenant_id)
    .bind(a.patient_id)
    .bind(constraints.facility_ids.first().copied())
    .bind(channel)
    .bind(serde_json::to_value(&constraints).map_err(ApiError::internal)?)
    .bind(ctx.user_id)
    .execute(&mut **tx)
    .await?;
    let actor = scheduling::actor_label(ctx);
    request_history(tx, a.tenant_id, id, None, "draft", None, &actor).await?;
    request_history(
        tx,
        a.tenant_id,
        id,
        Some("draft"),
        "submitted",
        Some(RESCHEDULE_REASON),
        &actor,
    )
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "appointment.reschedule_requested",
        &state.cell,
        json!({ "appointment_id": a.id, "patient_id": a.patient_id, "access_request_id": id,
                "reason": reason, "by_patient": by_patient }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    load_request(tx, a.tenant_id, id).await
}

pub async fn reschedule_options_for(
    state: &AppState,
    ctx: &AuthContext,
    a: AppointmentRow,
    body: RescheduleOptionsBody,
    channel: &str,
) -> Result<MatchResult, ApiError> {
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Scheduling).await?;
    let by_patient = channel != "staff";
    if a.status.apply(AppointmentTransition::Reschedule).is_err() {
        return Err(ApiError::conflict(
            "invalid_transition",
            format!(
                "an appointment in status {} cannot be rescheduled",
                a.status.as_str()
            ),
        ));
    }
    let reason = scheduling::clean_text(body.reason, "reason", MAX_REASON)?;
    let mut tx = state.pool.begin().await?;
    let a = scheduling::lock_appointment(&mut tx, a.id).await?;
    require_version(a.version, body.version)?;
    let policy = scheduling::load_policy(&mut tx, a.tenant_id).await?;
    if by_patient
        && !wellos_domain::access::within_patient_window(
            Utc::now(),
            a.starts_at,
            policy.reschedule_window_hours as i64,
        )
    {
        return Err(ApiError::conflict(
            "reschedule_window_closed",
            format!(
                "appointments can be rescheduled online up to {} hours before they start; contact the facility",
                policy.reschedule_window_hours
            ),
        ));
    }
    let r = open_reschedule_request_in(&mut tx, ctx, state, &a, channel, reason, body.constraints)
        .await?;
    tx.commit().await?;
    run_matcher_for(
        state,
        ctx,
        r,
        MatchBody {
            version: None,
            origin: body.origin,
            ranking: body.ranking,
            language: body.language,
        },
        if by_patient { "patient" } else { "staff" },
    )
    .await
}

async fn reschedule_options(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<RescheduleOptionsBody>,
) -> Result<Json<Value>, ApiError> {
    let body = body.0.unwrap_or_default();
    let mut conn = state.pool.acquire().await?;
    let a = load_appointment_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "appointment",
        Some(appointment_ctx(&a)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let m = reschedule_options_for(&state, &ctx, a, body, "staff").await?;
    Ok(Json(match_result_json(&m)))
}

#[derive(Debug, Default, Deserialize)]
pub struct CloseBody {
    pub version: Option<i64>,
    pub reason_code: Option<String>,
    pub note: Option<String>,
    pub override_reason: Option<String>,
}

/// Cancel, no-show or fulfil an appointment with the linked visit kept
/// consistent in the same transaction (`scheduling::close_appointment`).
pub async fn close_appointment_for(
    state: &AppState,
    ctx: &AuthContext,
    a: AppointmentRow,
    t: AppointmentTransition,
    body: CloseBody,
    by_patient: bool,
) -> Result<AppointmentRow, ApiError> {
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Scheduling).await?;
    let reason_code = scheduling::clean_text(body.reason_code, "reason_code", 64)?;
    let note = scheduling::clean_text(body.note, "note", scheduling::MAX_NOTE)?;
    let override_reason =
        scheduling::clean_text(body.override_reason, "override_reason", MAX_REASON)?;
    if by_patient && override_reason.is_some() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "only staff can override scheduling policy",
        ));
    }
    if t == AppointmentTransition::Cancel && reason_code.is_none() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "reason_code is required to cancel",
        ));
    }
    let mut tx = state.pool.begin().await?;
    let a = close_appointment_in(
        &mut tx,
        ctx,
        state,
        a.id,
        body.version,
        t,
        scheduling::CloseInput {
            reason_code,
            note,
            override_reason,
            by_patient,
        },
    )
    .await?;
    tx.commit().await?;
    Ok(a)
}

/// Transaction-scoped core of [`close_appointment_for`]: policy windows and
/// override requirements are enforced here, then the appointment and its
/// linked visit close together. The caller owns the transaction.
pub async fn close_appointment_in(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    appointment_id: Uuid,
    version: Option<i64>,
    t: AppointmentTransition,
    input: scheduling::CloseInput,
) -> Result<AppointmentRow, ApiError> {
    let by_patient = input.by_patient;
    let override_reason = input.override_reason.clone();
    let a = scheduling::lock_appointment(tx, appointment_id).await?;
    require_version(a.version, version)?;
    let policy = scheduling::load_policy(tx, a.tenant_id).await?;
    let now = Utc::now();
    // Staff cancelling inside the patient window act on the patient's
    // behalf and must say why.
    if t == AppointmentTransition::Cancel
        && !by_patient
        && override_reason.is_none()
        && !wellos_domain::access::within_patient_window(
            now,
            a.starts_at,
            policy.cancellation_window_hours as i64,
        )
    {
        return Err(ApiError::conflict(
            "override_required",
            format!(
                "cancelling less than {} hours ahead requires an override reason",
                policy.cancellation_window_hours
            ),
        ));
    }
    if matches!(
        t,
        AppointmentTransition::MarkNoShow | AppointmentTransition::Fulfil
    ) && a.starts_at > now
        && override_reason.is_none()
    {
        return Err(ApiError::conflict(
            "override_required",
            "this appointment has not started yet; an override reason is required",
        ));
    }
    scheduling::close_appointment(tx, ctx, state, &a, t, input, &policy).await
}

async fn close_route(
    state: AppState,
    ctx: AuthContext,
    id: Uuid,
    t: AppointmentTransition,
    body: CloseBody,
) -> Result<Json<Value>, ApiError> {
    let mut conn = state.pool.acquire().await?;
    let a = load_appointment_scoped(&mut conn, ctx.tenant_id, id).await?;
    let allowed = guard(
        &state,
        &ctx,
        actions::SCHEDULING_MANAGE,
        "appointment",
        Some(appointment_ctx(&a)),
    )
    .await?;
    allowed.record(&mut conn, &ctx, &state.cell).await?;
    drop(conn);
    let a = close_appointment_for(&state, &ctx, a, t, body, false).await?;
    Ok(Json(scheduling::appointment_json(&a)))
}

async fn cancel_appointment(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<CloseBody>,
) -> Result<Json<Value>, ApiError> {
    close_route(
        state,
        ctx,
        id,
        AppointmentTransition::Cancel,
        body.0.unwrap_or_default(),
    )
    .await
}

async fn no_show_appointment(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<CloseBody>,
) -> Result<Json<Value>, ApiError> {
    close_route(
        state,
        ctx,
        id,
        AppointmentTransition::MarkNoShow,
        body.0.unwrap_or_default(),
    )
    .await
}

async fn fulfil_appointment(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    body: OptionalJson<CloseBody>,
) -> Result<Json<Value>, ApiError> {
    close_route(
        state,
        ctx,
        id,
        AppointmentTransition::Fulfil,
        body.0.unwrap_or_default(),
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_information_requires_service() {
        let c = Constraints::default();
        assert_eq!(missing_information(&c), vec!["service".to_string()]);
        let c = Constraints {
            service_code: Some("general_medicine".into()),
            ..Constraints::default()
        };
        assert!(missing_information(&c).is_empty());
    }

    #[test]
    fn clear_triage_reason_prefix_carries_urgency() {
        assert_eq!(
            body_urgency("urgency:priority; seen by nurse"),
            Some("priority".to_string())
        );
        assert_eq!(body_urgency("seen by nurse"), None);
    }

    #[test]
    fn constraints_round_trip_without_origin() {
        let c = Constraints {
            service_code: Some("x".into()),
            continuity_required: true,
            ..Constraints::default()
        };
        let v = serde_json::to_value(&c).unwrap();
        assert!(v.get("origin").is_none());
        let back: Constraints = serde_json::from_value(v).unwrap();
        assert_eq!(back, c);
    }
}
