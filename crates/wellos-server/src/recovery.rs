//! Waitlist and cancellation recovery.
//!
//! A future appointment that is cancelled opens a *cancellation event* for
//! its freed slot. Eligibility is deterministic
//! (`wellos_domain::recovery::eligible_entries`): only consented, active
//! waitlist entries whose stated preferences, notice and personal calendar
//! fit the slot are considered, ordered by urgency, longest wait, fewest
//! declined offers. The first eligible patient receives a time-limited
//! offer through the shared offer machinery; acceptance runs through the
//! same atomic hold → `scheduling::confirm_offer` path as any other offer,
//! so a freed slot can never be double-booked. Decline or expiry advances
//! to the next eligible patient (`on_offer_closed`).
//!
//! dMind (`cancellation-recovery.v1`) may reorder the *not yet offered*
//! remainder within the fairness floors (`enforce_floors`); it cannot add,
//! drop or book anybody. Everything works identically with AI disabled.

use crate::aigov;
use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::notify;
use crate::scheduling::{self, OfferRow, Policy};
use crate::state::AppState;
use chrono::{DateTime, Duration, Utc};
use dmind_gateway::access::{RecoveryRankingRequest, CANCELLATION_RECOVERY_TEMPLATE};
use dmind_gateway::{hash_json, GatewayError};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wellos_domain::access::{OfferStatus, OfferTransition, Urgency, WeeklyWindow};
use wellos_domain::access_ai::{CancellationRecoveryV1, CANCELLATION_RECOVERY_SCHEMA};
use wellos_domain::ai::{ArtifactStatus, ProviderInfo};
use wellos_domain::matcher::Interval;
use wellos_domain::recovery::{eligible_entries, EligibleEntry, FreedSlot, WaitlistFact};

/// Most waitlist entries examined for one freed slot.
pub const MAX_ENTRIES_CONSIDERED: i64 = 200;
/// Most sequential offers made for one freed slot before it is left to the
/// matcher (each offer is time-limited, so this bounds the cascade).
pub const MAX_OFFERS_PER_EVENT: i32 = 10;
/// Waitlist offers expire well before the slot so a decline still leaves
/// time for the next patient.
pub const OFFER_SLOT_MARGIN_MINUTES: i64 = 30;
/// Among equal urgency, dMind may demote a patient below one who has
/// waited up to this much less (near-tie convenience); never more.
pub const MAX_WAIT_DEMOTION_HOURS: i64 = 24;
pub const OFFERED_TO: &str = "waitlist";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EligibleRecord {
    pub entry_id: Uuid,
    pub patient_id: Uuid,
    pub urgency: String,
    pub waited_hours: i64,
    /// Deterministic fairness rank (1 = first).
    pub rank: usize,
    pub reasons: Vec<String>,
    /// Position after dMind reordering (equals `rank` when deterministic).
    pub position: usize,
    pub explanation: Option<String>,
    pub offer_id: Option<Uuid>,
    /// `offered`, `accepted`, `declined`, `expired`, `revoked`, `skipped`.
    pub outcome: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EventRow {
    pub id: Uuid,
    pub tenant_id: Uuid,
    pub appointment_id: Uuid,
    pub facility_id: Uuid,
    pub service_code: String,
    pub modality_code: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub resources: Vec<wellos_domain::matcher::BookingPlan>,
    pub status: String,
    pub eligible: Vec<EligibleRecord>,
    pub excluded: Vec<Value>,
    pub ranking_mode: String,
    pub ranking_artifact_id: Option<Uuid>,
    pub current_offer_id: Option<Uuid>,
    pub offers_made: i32,
    pub override_by: Option<Uuid>,
    pub override_reason: Option<String>,
    pub closed_reason: Option<String>,
    pub version: i64,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

const EVENT_COLUMNS: &str =
    "id, tenant_id, appointment_id, facility_id, service_code, modality_code,
    starts_at, ends_at, resources, status, eligible, excluded, ranking_mode, ranking_artifact_id,
    current_offer_id, offers_made, override_by, override_reason, closed_reason, version,
    created_at, updated_at";

fn event_from_row(r: &sqlx::postgres::PgRow) -> Result<EventRow, ApiError> {
    Ok(EventRow {
        id: r.get("id"),
        tenant_id: r.get("tenant_id"),
        appointment_id: r.get("appointment_id"),
        facility_id: r.get("facility_id"),
        service_code: r.get("service_code"),
        modality_code: r.get("modality_code"),
        starts_at: r.get("starts_at"),
        ends_at: r.get("ends_at"),
        resources: serde_json::from_value(r.get::<Value, _>("resources"))
            .map_err(ApiError::internal)?,
        status: r.get("status"),
        eligible: serde_json::from_value(r.get::<Value, _>("eligible"))
            .map_err(ApiError::internal)?,
        excluded: serde_json::from_value(r.get::<Value, _>("excluded"))
            .map_err(ApiError::internal)?,
        ranking_mode: r.get("ranking_mode"),
        ranking_artifact_id: r.get("ranking_artifact_id"),
        current_offer_id: r.get("current_offer_id"),
        offers_made: r.get("offers_made"),
        override_by: r.get("override_by"),
        override_reason: r.get("override_reason"),
        closed_reason: r.get("closed_reason"),
        version: r.get("version"),
        created_at: r.get("created_at"),
        updated_at: r.get("updated_at"),
    })
}

pub async fn load_event(conn: &mut PgConnection, id: Uuid) -> Result<EventRow, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {EVENT_COLUMNS} FROM cancellation_events WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    event_from_row(&row)
}

pub async fn lock_event(conn: &mut PgConnection, id: Uuid) -> Result<EventRow, ApiError> {
    let row = sqlx::query(&format!(
        "SELECT {EVENT_COLUMNS} FROM cancellation_events WHERE id = $1 FOR UPDATE"
    ))
    .bind(id)
    .fetch_optional(&mut *conn)
    .await?
    .ok_or_else(ApiError::not_found)?;
    event_from_row(&row)
}

pub fn event_json(e: &EventRow) -> Value {
    json!({
        "id": e.id,
        "tenant_id": e.tenant_id,
        "appointment_id": e.appointment_id,
        "facility_id": e.facility_id,
        "service_code": e.service_code,
        "modality_code": e.modality_code,
        "starts_at": e.starts_at,
        "ends_at": e.ends_at,
        "status": e.status,
        "eligible": e.eligible,
        "excluded_count": e.excluded.len(),
        "excluded": e.excluded,
        "ranking_mode": e.ranking_mode,
        "ranking_artifact_id": e.ranking_artifact_id,
        "current_offer_id": e.current_offer_id,
        "offers_made": e.offers_made,
        "override_by": e.override_by,
        "override_reason": e.override_reason,
        "closed_reason": e.closed_reason,
        "version": e.version,
        "created_at": e.created_at,
        "updated_at": e.updated_at,
    })
}

async fn save_eligible(
    conn: &mut PgConnection,
    event_id: Uuid,
    eligible: &[EligibleRecord],
) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE cancellation_events SET eligible = $2, version = version + 1, updated_at = now()
         WHERE id = $1",
    )
    .bind(event_id)
    .bind(serde_json::to_value(eligible).map_err(ApiError::internal)?)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

// ---------------------------------------------------------------------------
// Eligibility
// ---------------------------------------------------------------------------

/// Load the deterministic facts about every active waitlist entry for the
/// event's service (bounded), including calendar busy time when the patient
/// has consented to calendar use.
async fn waitlist_facts(
    conn: &mut PgConnection,
    e: &EventRow,
    policy: &Policy,
) -> Result<Vec<WaitlistFact>, ApiError> {
    let rows = sqlx::query(
        "SELECT w.id, w.patient_id, w.urgency, w.joined_at, w.facility_ids, w.modality_codes,
                w.acceptable_windows, w.earliest, w.latest, w.min_notice_hours, w.offers_declined,
                COALESCE(p.time_zone, $3) AS time_zone
         FROM waitlist_entries w
         LEFT JOIN patient_scheduling_preferences p
                ON p.tenant_id = w.tenant_id AND p.patient_id = w.patient_id
         WHERE w.tenant_id = $1 AND w.service_code = $2 AND w.status = 'active'
         ORDER BY w.joined_at, w.id
         LIMIT $4",
    )
    .bind(e.tenant_id)
    .bind(&e.service_code)
    .bind(&policy.time_zone)
    .bind(MAX_ENTRIES_CONSIDERED)
    .fetch_all(&mut *conn)
    .await?;
    let mut out = Vec::with_capacity(rows.len());
    for r in rows {
        let patient_id: Uuid = r.get("patient_id");
        let consented =
            scheduling::consent_active(conn, e.tenant_id, patient_id, scheduling::CONSENT_WAITLIST)
                .await?;
        let mut busy: Vec<Interval> = Vec::new();
        if consented {
            if scheduling::consent_active(
                conn,
                e.tenant_id,
                patient_id,
                scheduling::CONSENT_CALENDAR,
            )
            .await?
            {
                busy = sqlx::query(
                    "SELECT b.starts_at, b.ends_at FROM patient_busy_intervals b
                     JOIN patient_calendar_sources s ON s.id = b.source_id
                     WHERE b.tenant_id = $1 AND b.patient_id = $2 AND s.status = 'connected'
                       AND b.ends_at > $3 AND b.starts_at < $4",
                )
                .bind(e.tenant_id)
                .bind(patient_id)
                .bind(e.starts_at)
                .bind(e.ends_at)
                .fetch_all(&mut *conn)
                .await?
                .iter()
                .map(|b| Interval::new(b.get("starts_at"), b.get("ends_at")))
                .collect();
            }
            // The patient's own live appointments are busy time too.
            let own: Vec<Interval> = sqlx::query(
                "SELECT starts_at, ends_at FROM appointments
                 WHERE tenant_id = $1 AND patient_id = $2 AND status IN ('confirmed','rescheduled')
                   AND ends_at > $3 AND starts_at < $4",
            )
            .bind(e.tenant_id)
            .bind(patient_id)
            .bind(e.starts_at)
            .bind(e.ends_at)
            .fetch_all(&mut *conn)
            .await?
            .iter()
            .map(|b| Interval::new(b.get("starts_at"), b.get("ends_at")))
            .collect();
            busy.extend(own);
        }
        let windows: Vec<WeeklyWindow> =
            serde_json::from_value(r.get::<Value, _>("acceptable_windows")).unwrap_or_default();
        out.push(WaitlistFact {
            entry_id: r.get("id"),
            patient_id,
            urgency: scheduling::urgency_of(&r.get::<String, _>("urgency")),
            joined_at: r.get("joined_at"),
            facility_ids: r.get("facility_ids"),
            modality_codes: r.get("modality_codes"),
            acceptable_windows: windows,
            earliest: r.get("earliest"),
            latest: r.get("latest"),
            min_notice_hours: r.get("min_notice_hours"),
            time_zone: r.get("time_zone"),
            busy_intervals: busy,
            offers_declined: r.get("offers_declined"),
            consented,
        });
    }
    Ok(out)
}

fn record_from(e: &EligibleEntry) -> EligibleRecord {
    EligibleRecord {
        entry_id: e.entry_id,
        patient_id: e.patient_id,
        urgency: e.urgency.as_str().to_string(),
        waited_hours: e.waited_hours,
        rank: e.rank,
        reasons: e.reasons.clone(),
        position: e.rank,
        explanation: None,
        offer_id: None,
        outcome: None,
    }
}

/// Compute deterministic eligibility for a freshly opened event and make
/// the first offer. Called inside the cancelling transaction so the freed
/// slot is offered atomically with the cancellation itself.
pub async fn start_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    event_id: Uuid,
) -> Result<Option<Uuid>, ApiError> {
    let e = lock_event(tx, event_id).await?;
    if e.status != "open" {
        return Ok(None);
    }
    let policy = scheduling::load_policy(tx, e.tenant_id).await?;
    let facts = waitlist_facts(tx, &e, &policy).await?;
    let slot = FreedSlot {
        facility_id: e.facility_id,
        modality_code: e.modality_code.clone(),
        starts_at: e.starts_at,
        ends_at: e.ends_at,
    };
    let out = eligible_entries(Utc::now(), &slot, &facts);
    let eligible: Vec<EligibleRecord> = out.eligible.iter().map(record_from).collect();
    let excluded: Vec<Value> = out
        .excluded
        .iter()
        .map(|(id, reason)| json!({ "entry_id": id, "reason": reason }))
        .collect();
    sqlx::query(
        "UPDATE cancellation_events SET eligible = $2, excluded = $3, version = version + 1,
             updated_at = now() WHERE id = $1",
    )
    .bind(e.id)
    .bind(serde_json::to_value(&eligible).map_err(ApiError::internal)?)
    .bind(serde_json::to_value(&excluded).map_err(ApiError::internal)?)
    .execute(&mut **tx)
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "waitlist.recovery.opened",
        &state.cell,
        json!({
            "cancellation_event_id": e.id,
            "eligibility_version": out.version,
            "eligible": eligible.len(),
            "excluded": excluded.len(),
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    advance(tx, ctx, state, e.id, &policy).await
}

// ---------------------------------------------------------------------------
// Offer cascade
// ---------------------------------------------------------------------------

fn offer_expiry(now: DateTime<Utc>, slot_start: DateTime<Utc>, policy: &Policy) -> DateTime<Utc> {
    let by_policy = now + Duration::minutes(policy.offer_ttl_minutes as i64);
    let by_slot = slot_start
        - Duration::hours(policy.min_notice_hours as i64)
        - Duration::minutes(OFFER_SLOT_MARGIN_MINUTES);
    by_policy.min(by_slot)
}

async fn close_event(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    e: &EventRow,
    status: &str,
    reason: &str,
) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE cancellation_events SET status = $2, closed_reason = $3, current_offer_id = NULL,
             version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(e.id)
    .bind(status)
    .bind(reason)
    .execute(&mut **tx)
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "waitlist.recovery.closed",
        &state.cell,
        json!({ "cancellation_event_id": e.id, "status": status, "reason": reason,
                "offers_made": e.offers_made }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(())
}

/// Offer the freed slot to the next eligible patient who is still active
/// and consented. Returns the new offer id, or `None` when the event is
/// exhausted or closed. Caller holds the event lock.
pub async fn advance(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    event_id: Uuid,
    policy: &Policy,
) -> Result<Option<Uuid>, ApiError> {
    let e = lock_event(tx, event_id).await?;
    if !matches!(e.status.as_str(), "open" | "offered") || e.current_offer_id.is_some() {
        return Ok(None);
    }
    let now = Utc::now();
    if offer_expiry(now, e.starts_at, policy) <= now {
        close_event(tx, ctx, state, &e, "closed", "slot_too_close").await?;
        return Ok(None);
    }
    if e.offers_made >= MAX_OFFERS_PER_EVENT {
        close_event(tx, ctx, state, &e, "exhausted", "max_offers_reached").await?;
        return Ok(None);
    }
    let mut eligible = e.eligible.clone();
    eligible.sort_by_key(|r| (r.position, r.rank));
    for idx in 0..eligible.len() {
        if eligible[idx].outcome.is_some() {
            continue;
        }
        let rec = eligible[idx].clone();
        // Re-check the live entry: it may have paused, left or been
        // fulfilled since eligibility was computed.
        let live: Option<(String, i64)> = sqlx::query_as(
            "SELECT status, version FROM waitlist_entries WHERE id = $1 AND tenant_id = $2 FOR UPDATE",
        )
        .bind(rec.entry_id)
        .bind(e.tenant_id)
        .fetch_optional(&mut **tx)
        .await?;
        let still_active = matches!(live, Some((ref s, _)) if s == "active")
            && scheduling::consent_active(
                tx,
                e.tenant_id,
                rec.patient_id,
                scheduling::CONSENT_WAITLIST,
            )
            .await?;
        if !still_active {
            eligible[idx].outcome = Some("skipped".into());
            continue;
        }
        let offer_id = Uuid::now_v7();
        let expires = offer_expiry(now, e.starts_at, policy);
        sqlx::query(
            "INSERT INTO appointment_offers (id, tenant_id, patient_id, facility_id,
                 cancellation_event_id, waitlist_entry_id, status, service_code, modality_code,
                 starts_at, ends_at, resources, score, explanation, rank, offered_to,
                 offer_expires_at, created_by)
             VALUES ($1,$2,$3,$4,$5,$6,'offered',$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17)",
        )
        .bind(offer_id)
        .bind(e.tenant_id)
        .bind(rec.patient_id)
        .bind(e.facility_id)
        .bind(e.id)
        .bind(rec.entry_id)
        .bind(&e.service_code)
        .bind(&e.modality_code)
        .bind(e.starts_at)
        .bind(e.ends_at)
        .bind(serde_json::to_value(&e.resources).map_err(ApiError::internal)?)
        .bind(json!({
            "source": "cancellation_recovery",
            "urgency": rec.urgency,
            "waited_hours": rec.waited_hours,
            "fairness_rank": rec.rank,
            "reasons": rec.reasons,
        }))
        .bind(rec.explanation.as_ref().map(
            |t| json!({ "text": t, "artifact_id": e.ranking_artifact_id, "mode": e.ranking_mode }),
        ))
        .bind(rec.position as i32)
        .bind(OFFERED_TO)
        .bind(expires)
        .bind((ctx.user_id != Uuid::nil()).then_some(ctx.user_id))
        .execute(&mut **tx)
        .await?;
        let actor = scheduling::actor_label(ctx);
        scheduling::offer_history(
            tx,
            e.tenant_id,
            offer_id,
            None,
            "offered",
            Some("cancellation_recovery"),
            &actor,
        )
        .await?;
        sqlx::query(
            "UPDATE waitlist_entries SET status = 'offered', current_appointment_id = NULL,
                 version = version + 1, updated_at = now() WHERE id = $1",
        )
        .bind(rec.entry_id)
        .execute(&mut **tx)
        .await?;
        eligible[idx].offer_id = Some(offer_id);
        eligible[idx].outcome = Some("offered".into());
        sqlx::query(
            "UPDATE cancellation_events SET status = 'offered', current_offer_id = $2,
                 offers_made = offers_made + 1, eligible = $3, version = version + 1,
                 updated_at = now() WHERE id = $1",
        )
        .bind(e.id)
        .bind(offer_id)
        .bind(serde_json::to_value(&eligible).map_err(ApiError::internal)?)
        .execute(&mut **tx)
        .await?;
        let o = scheduling::load_offer(tx, offer_id).await?;
        notify::schedule_offer(tx, ctx, state, &o, policy, notify::KIND_WAITLIST_OFFER).await?;
        audit::emit(
            &mut **tx,
            ctx,
            "waitlist.recovery.offered",
            &state.cell,
            json!({
                "cancellation_event_id": e.id,
                "offer_id": offer_id,
                "waitlist_entry_id": rec.entry_id,
                "patient_id": rec.patient_id,
                "position": rec.position,
                "fairness_rank": rec.rank,
                "ranking_mode": e.ranking_mode,
                "offer_expires_at": expires,
            }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
        return Ok(Some(offer_id));
    }
    save_eligible(tx, e.id, &eligible).await?;
    close_event(tx, ctx, state, &e, "exhausted", "no_eligible_entries").await?;
    Ok(None)
}

/// A recovery offer was declined, expired or revoked: return the entry to
/// the waitlist (counting a decline) and offer the slot to the next eligible
/// patient. Returns how many new offers were made (0 or 1).
pub async fn on_offer_closed(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    offer_id: Uuid,
    reason: &str,
) -> Result<usize, ApiError> {
    let o = scheduling::load_offer(tx, offer_id).await?;
    let Some(event_id) = o.cancellation_event_id else {
        return Ok(0);
    };
    let e = lock_event(tx, event_id).await?;
    if let Some(entry_id) = o.waitlist_entry_id {
        let declined = if reason == "declined" { 1 } else { 0 };
        sqlx::query(
            "UPDATE waitlist_entries SET status = 'active', offers_declined = offers_declined + $2,
                 version = version + 1, updated_at = now()
             WHERE id = $1 AND status = 'offered'",
        )
        .bind(entry_id)
        .bind(declined)
        .execute(&mut **tx)
        .await?;
    }
    if e.current_offer_id != Some(offer_id) {
        return Ok(0);
    }
    let policy = scheduling::load_policy(tx, e.tenant_id).await?;
    if reason == "expired" && o.waitlist_entry_id.is_some() {
        notify::schedule_offer(
            tx,
            ctx,
            state,
            &o,
            &policy,
            notify::KIND_WAITLIST_OFFER_EXPIRED,
        )
        .await?;
    }
    let mut eligible = e.eligible.clone();
    for rec in eligible.iter_mut() {
        if rec.offer_id == Some(offer_id) {
            rec.outcome = Some(reason.to_string());
        }
    }
    sqlx::query(
        "UPDATE cancellation_events SET current_offer_id = NULL, eligible = $2,
             version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(e.id)
    .bind(serde_json::to_value(&eligible).map_err(ApiError::internal)?)
    .execute(&mut **tx)
    .await?;
    if !matches!(e.status.as_str(), "open" | "offered") {
        return Ok(0);
    }
    Ok(advance(tx, ctx, state, e.id, &policy).await?.is_some() as usize)
}

/// A recovery offer was accepted (inside `scheduling::confirm_offer`): the
/// entry is fulfilled, the event filled, and any other live offer for the
/// same event revoked.
pub async fn on_offer_accepted(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    o: &OfferRow,
    appointment_id: Uuid,
) -> Result<(), ApiError> {
    let Some(event_id) = o.cancellation_event_id else {
        return Ok(());
    };
    let e = lock_event(tx, event_id).await?;
    if let Some(entry_id) = o.waitlist_entry_id {
        sqlx::query(
            "UPDATE waitlist_entries SET status = 'fulfilled', fulfilled_appointment_id = $2,
                 version = version + 1, updated_at = now()
             WHERE id = $1 AND status IN ('offered','active','paused')",
        )
        .bind(entry_id)
        .bind(appointment_id)
        .execute(&mut **tx)
        .await?;
        audit::emit(
            &mut **tx,
            ctx,
            "waitlist.left",
            &state.cell,
            json!({ "waitlist_entry_id": entry_id, "reason": "fulfilled",
                    "appointment_id": appointment_id }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
    }
    let mut eligible = e.eligible.clone();
    for rec in eligible.iter_mut() {
        if rec.offer_id == Some(o.id) {
            rec.outcome = Some("accepted".into());
        }
    }
    sqlx::query(
        "UPDATE cancellation_events SET status = 'filled', current_offer_id = NULL, eligible = $2,
             closed_reason = 'accepted', version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(e.id)
    .bind(serde_json::to_value(&eligible).map_err(ApiError::internal)?)
    .execute(&mut **tx)
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "waitlist.recovery.closed",
        &state.cell,
        json!({ "cancellation_event_id": e.id, "status": "filled", "offer_id": o.id,
                "appointment_id": appointment_id, "offers_made": e.offers_made }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(())
}

/// Staff override: move one not-yet-offered eligible entry to the front of
/// the remaining order, with a mandatory reason, and (when nobody currently
/// holds the offer) offer the slot to them immediately. Urgency floors still
/// apply: a human may promote, never demote below a higher urgency.
pub async fn override_next(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    event_id: Uuid,
    entry_id: Uuid,
    reason: &str,
    expected_version: Option<i64>,
) -> Result<EventRow, ApiError> {
    let e = lock_event(tx, event_id).await?;
    if let Some(v) = expected_version {
        if v != e.version {
            return Err(ApiError::conflict(
                "stale_version",
                "the recovery event changed; reload and retry",
            ));
        }
    }
    if !matches!(e.status.as_str(), "open" | "offered") {
        return Err(ApiError::conflict(
            "invalid_transition",
            format!(
                "a recovery event in status {} cannot be overridden",
                e.status
            ),
        ));
    }
    let mut eligible = e.eligible.clone();
    let Some(target) = eligible
        .iter()
        .find(|r| r.entry_id == entry_id && r.outcome.is_none())
    else {
        return Err(ApiError::bad_request(
            "validation_failed",
            "entry is not an eligible, not-yet-offered candidate of this event",
        ));
    };
    let target_urgency = scheduling::urgency_of(&target.urgency);
    if eligible.iter().any(|r| {
        r.outcome.is_none() && scheduling::urgency_of(&r.urgency).rank() > target_urgency.rank()
    }) {
        return Err(ApiError::conflict(
            "fairness_floor",
            "a higher-urgency patient is still waiting for this slot",
        ));
    }
    // Renumber: target first, the rest keep their relative order.
    let mut pending: Vec<&mut EligibleRecord> = eligible
        .iter_mut()
        .filter(|r| r.outcome.is_none())
        .collect();
    pending.sort_by_key(|r| (r.entry_id != entry_id, r.position, r.rank));
    let base = e.offers_made as usize + 1;
    for (i, r) in pending.iter_mut().enumerate() {
        r.position = base + i;
        if r.entry_id == entry_id {
            r.explanation = Some(format!("Staff override: {reason}"));
        }
    }
    sqlx::query(
        "UPDATE cancellation_events SET eligible = $2, ranking_mode = 'human_override',
             override_by = $3, override_reason = $4, version = version + 1, updated_at = now()
         WHERE id = $1",
    )
    .bind(e.id)
    .bind(serde_json::to_value(&eligible).map_err(ApiError::internal)?)
    .bind(ctx.user_id)
    .bind(reason)
    .execute(&mut **tx)
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "waitlist.recovery.overridden",
        &state.cell,
        json!({ "cancellation_event_id": e.id, "waitlist_entry_id": entry_id,
                "reason": reason, "previous_mode": e.ranking_mode }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    if e.current_offer_id.is_none() {
        let policy = scheduling::load_policy(tx, e.tenant_id).await?;
        advance(tx, ctx, state, e.id, &policy).await?;
    }
    load_event(tx, e.id).await
}

/// Staff closes an event by hand (the freed slot is handled otherwise):
/// a live offer is revoked first and the cascade is *not* continued.
pub async fn close_by_staff(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    event_id: Uuid,
    reason: &str,
) -> Result<EventRow, ApiError> {
    let e = lock_event(tx, event_id).await?;
    if !matches!(e.status.as_str(), "open" | "offered") {
        return Err(ApiError::conflict(
            "invalid_transition",
            format!("a recovery event in status {} cannot be closed", e.status),
        ));
    }
    let mut eligible = e.eligible.clone();
    if let Some(offer_id) = e.current_offer_id {
        let o = scheduling::lock_offer(tx, offer_id).await?;
        if matches!(o.status, OfferStatus::Offered | OfferStatus::Held) {
            scheduling::release_offer_bookings(tx, o.id).await?;
            scheduling::transition_offer(
                tx,
                &o,
                OfferTransition::Revoke,
                None,
                Some(reason),
                &scheduling::actor_label(ctx),
            )
            .await?;
            if let Some(entry_id) = o.waitlist_entry_id {
                sqlx::query(
                    "UPDATE waitlist_entries SET status = 'active', version = version + 1,
                         updated_at = now()
                     WHERE id = $1 AND status = 'offered'",
                )
                .bind(entry_id)
                .execute(&mut **tx)
                .await?;
            }
            audit::emit(
                &mut **tx,
                ctx,
                "appointment.offer.revoked",
                &state.cell,
                json!({ "offer_id": o.id, "reason": reason, "cancellation_event_id": e.id }),
                None,
            )
            .await
            .map_err(ApiError::internal)?;
            for rec in eligible.iter_mut() {
                if rec.offer_id == Some(o.id) {
                    rec.outcome = Some("revoked".into());
                }
            }
        }
    }
    save_eligible(tx, e.id, &eligible).await?;
    close_event(tx, ctx, state, &e, "closed", reason).await?;
    load_event(tx, e.id).await
}

/// Revoke the live offer of an event (staff decision with reason); the
/// cascade continues with the next eligible patient.
pub async fn revoke_current(
    tx: &mut sqlx::Transaction<'_, sqlx::Postgres>,
    ctx: &AuthContext,
    state: &AppState,
    event_id: Uuid,
    reason: &str,
) -> Result<usize, ApiError> {
    let e = lock_event(tx, event_id).await?;
    let Some(offer_id) = e.current_offer_id else {
        return Ok(0);
    };
    let o = scheduling::lock_offer(tx, offer_id).await?;
    if !matches!(o.status, OfferStatus::Offered | OfferStatus::Held) {
        return Ok(0);
    }
    scheduling::release_offer_bookings(tx, o.id).await?;
    scheduling::transition_offer(
        tx,
        &o,
        OfferTransition::Revoke,
        None,
        Some(reason),
        &scheduling::actor_label(ctx),
    )
    .await?;
    audit::emit(
        &mut **tx,
        ctx,
        "appointment.offer.revoked",
        &state.cell,
        json!({ "offer_id": o.id, "reason": reason, "cancellation_event_id": e.id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    on_offer_closed(tx, ctx, state, o.id, "revoked").await
}

// ---------------------------------------------------------------------------
// dMind ranking of the remainder
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize)]
pub struct RankingOutcome {
    pub mode: &'static str,
    pub artifact_id: Option<Uuid>,
    pub synthetic: Option<bool>,
    pub reused: bool,
    pub reason: Option<String>,
}

fn pending_entries(e: &EventRow) -> Vec<EligibleEntry> {
    let mut pending: Vec<&EligibleRecord> =
        e.eligible.iter().filter(|r| r.outcome.is_none()).collect();
    pending.sort_by_key(|r| (r.position, r.rank));
    pending
        .iter()
        .enumerate()
        .map(|(i, r)| EligibleEntry {
            entry_id: r.entry_id,
            patient_id: r.patient_id,
            urgency: Urgency::parse(&r.urgency).unwrap_or(Urgency::Routine),
            waited_hours: r.waited_hours,
            rank: i + 1,
            reasons: r.reasons.clone(),
        })
        .collect()
}

/// One bounded `cancellation-recovery.v1` call over the not-yet-offered
/// remainder. The proposed order is accepted only when it is an exact
/// permutation within the fairness floors; otherwise the deterministic order
/// stands and the refusal is audited. Never books or notifies anybody.
pub async fn rank_event(
    state: &AppState,
    ctx: &AuthContext,
    event_id: Uuid,
    language: &str,
) -> Result<RankingOutcome, ApiError> {
    let deterministic = |reason: &str| RankingOutcome {
        mode: "deterministic",
        artifact_id: None,
        synthetic: None,
        reused: false,
        reason: Some(reason.to_string()),
    };
    let mut conn = state.pool.acquire().await?;
    let e = load_event(&mut conn, event_id).await?;
    let facility_name: Option<String> =
        sqlx::query_scalar("SELECT name FROM facilities WHERE id = $1")
            .bind(e.facility_id)
            .fetch_optional(&mut *conn)
            .await?;
    let service = scheduling::load_service(&mut conn, e.tenant_id, &e.service_code)
        .await
        .ok();
    let patient_id: Uuid = sqlx::query_scalar("SELECT patient_id FROM appointments WHERE id = $1")
        .bind(e.appointment_id)
        .fetch_one(&mut *conn)
        .await?;
    drop(conn);
    if !matches!(e.status.as_str(), "open" | "offered") {
        return Err(ApiError::conflict(
            "invalid_transition",
            format!("a recovery event in status {} cannot be ranked", e.status),
        ));
    }
    let entries = pending_entries(&e);
    if entries.len() < 2 {
        return Ok(deterministic("nothing_to_rank"));
    }
    let status = state.gateway.status();
    if !status.state.is_callable() {
        return Ok(deterministic(match status.state {
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
        .unwrap_or_else(|| e.service_code.clone());
    let req = RecoveryRankingRequest {
        template: CANCELLATION_RECOVERY_TEMPLATE.to_string(),
        language: language.to_string(),
        facts: vec![
            (
                "slot:time".into(),
                format!("Freed slot {} to {} (UTC)", e.starts_at, e.ends_at),
            ),
            (
                "slot:service".into(),
                format!("Service {service_label} ({})", e.service_code),
            ),
            (
                "slot:facility".into(),
                format!(
                    "Facility {}",
                    facility_name.unwrap_or_else(|| "facility".into())
                ),
            ),
            (
                "slot:modality".into(),
                format!("Modality {}", e.modality_code),
            ),
            (
                "policy:fairness".into(),
                format!(
                    "Urgency floors are absolute; among equal urgency a patient may only be placed below one who waited up to {MAX_WAIT_DEMOTION_HOURS} hours less"
                ),
            ),
        ],
        entries: entries.clone(),
        max_wait_demotion_hours: MAX_WAIT_DEMOTION_HOURS,
    };
    let hash = hash_json(&req);
    let scope = aigov::ReuseScope::CancellationRecovery {
        cancellation_event_id: e.id,
    };
    audit::record(
        &state.pool,
        ctx,
        "ai.artifact.requested",
        Some("cancellation_event"),
        Some(e.id.to_string()),
        "allow",
        Some(CANCELLATION_RECOVERY_TEMPLATE),
    )
    .await
    .map_err(ApiError::internal)?;
    let plan = match aigov::plan(
        state,
        e.tenant_id,
        patient_id,
        scope,
        &hash,
        CANCELLATION_RECOVERY_SCHEMA,
    )
    .await
    {
        Ok(p) => p,
        Err(err) => {
            let reason = match err.code {
                "ai_external_consent_required" => "consent_required",
                "ai_quota_exceeded" => "quota_exceeded",
                "ai_disabled" => "disabled",
                _ => "unavailable",
            };
            audit::record(
                &state.pool,
                ctx,
                "ai.generation.skipped",
                Some("cancellation_event"),
                Some(e.id.to_string()),
                "deny",
                Some(reason),
            )
            .await
            .map_err(ApiError::internal)?;
            return Ok(deterministic(reason));
        }
    };
    let synthetic = plan.model_synthetic();
    let (output, provider, prompt_version, usage, reused_from, execution_id): (
        CancellationRecoveryV1,
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
            match state.gateway.rank_cancellation_recovery(&req).await {
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
                        Some("cancellation_event"),
                        Some(e.id.to_string()),
                        "deny",
                        Some(code),
                    )
                    .await
                    .map_err(ApiError::internal)?;
                    return Ok(deterministic(code));
                }
            }
        }
    };
    // Server-side re-validation: exact permutation, cited facts, floors.
    if let Err(err) = output.validate(&req.entry_ids(), &req.fact_refs()) {
        return refuse(state, ctx, e.id, &err).await;
    }
    let accepted = match wellos_domain::recovery::enforce_floors(
        &entries,
        &output.ordered_entry_ids,
        MAX_WAIT_DEMOTION_HOURS,
    ) {
        Ok(a) => a,
        Err(err) => return refuse(state, ctx, e.id, &err).await,
    };

    let mut tx = state.pool.begin().await?;
    let e = lock_event(&mut tx, event_id).await?;
    // The pending set must be unchanged since the request was built.
    let now_pending: Vec<Uuid> = pending_entries(&e).iter().map(|x| x.entry_id).collect();
    let mut sorted_now = now_pending.clone();
    sorted_now.sort();
    let mut sorted_req = req.entry_ids();
    sorted_req.sort();
    if sorted_now != sorted_req {
        return Ok(deterministic("event_changed"));
    }
    let artifact_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, cancellation_event_id, artifact_type, autonomy_level, status,
          model, model_version, route, template, input_hash, output, output_schema,
          citations, limitations, generated_at)
         VALUES ($1,$2,$3,$4,'cancellation_recovery','A1',$5,$6,$7,$8,$9,$10,$11,$12,$13,$14, now())",
    )
    .bind(artifact_id)
    .bind(e.tenant_id)
    .bind(patient_id)
    .bind(e.id)
    .bind(ArtifactStatus::AwaitingReview.as_str())
    .bind(&provider.model)
    .bind(&provider.model_version)
    .bind(&provider.provider)
    .bind(CANCELLATION_RECOVERY_TEMPLATE)
    .bind(&hash)
    .bind(serde_json::to_value(&output).map_err(ApiError::internal)?)
    .bind(CANCELLATION_RECOVERY_SCHEMA)
    .bind(serde_json::to_value(&output.cited_sources).map_err(ApiError::internal)?)
    .bind(serde_json::to_value(&output.limitations).map_err(ApiError::internal)?)
    .execute(&mut *tx)
    .await?;
    let mut input_refs = req.fact_refs();
    input_refs.extend(req.entry_ids().iter().map(|id| format!("entry:{id}")));
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
    let mut eligible = e.eligible.clone();
    let base = e.offers_made as usize + 1;
    for (i, id) in accepted.iter().enumerate() {
        if let Some(rec) = eligible
            .iter_mut()
            .find(|r| r.entry_id == *id && r.outcome.is_none())
        {
            rec.position = base + i;
            rec.explanation = output
                .explanations
                .iter()
                .find(|x| x.entry_id == *id)
                .map(|x| x.explanation.clone());
        }
    }
    sqlx::query(
        "UPDATE cancellation_events SET eligible = $2, ranking_mode = 'dmind',
             ranking_artifact_id = $3, version = version + 1, updated_at = now() WHERE id = $1",
    )
    .bind(e.id)
    .bind(serde_json::to_value(&eligible).map_err(ApiError::internal)?)
    .bind(artifact_id)
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        ctx,
        "ai.artifact.generated",
        &state.cell,
        json!({
            "artifact_id": artifact_id,
            "cancellation_event_id": e.id,
            "template": CANCELLATION_RECOVERY_TEMPLATE,
            "prompt_version": prompt_version,
            "model": provider.model,
            "input_hash": hash,
            "reused_from": reused_from,
            "synthetic": synthetic,
            "ranked": accepted.len(),
        }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(RankingOutcome {
        mode: "dmind",
        artifact_id: Some(artifact_id),
        synthetic: Some(synthetic),
        reused: reused_from.is_some(),
        reason: None,
    })
}

async fn refuse(
    state: &AppState,
    ctx: &AuthContext,
    event_id: Uuid,
    err: &str,
) -> Result<RankingOutcome, ApiError> {
    audit::record(
        &state.pool,
        ctx,
        "ai.generation.failed",
        Some("cancellation_event"),
        Some(event_id.to_string()),
        "deny",
        Some(&format!("invalid_output: {err}")),
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(RankingOutcome {
        mode: "deterministic",
        artifact_id: None,
        synthetic: None,
        reused: false,
        reason: Some("invalid_output".into()),
    })
}
