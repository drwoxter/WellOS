//! Closed-loop transitions: review, notification, closure, worklist, detail.

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, facility_scope, roles, ResourceCtx};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::Json;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;
use wellos_domain::result_loop::{LoopState, LoopTransition};

#[derive(Deserialize)]
pub struct TransitionBody {
    /// Optimistic concurrency: expected current version.
    pub version: i64,
    pub note: Option<String>,
}

struct SrCtx {
    tenant_id: Uuid,
    patient_id: Uuid,
    facility_id: Uuid,
    state: LoopState,
    version: i64,
}

async fn load_sr(state: &AppState, id: Uuid) -> Result<SrCtx, ApiError> {
    // Facility context is derived through the trusted patient relationship,
    // never from client input.
    let sr = sqlx::query(
        "SELECT sr.tenant_id, sr.patient_id, sr.loop_state, sr.version, p.facility_id
         FROM service_requests sr JOIN patients p ON p.id = sr.patient_id
         WHERE sr.id = $1",
    )
    .bind(id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)?;
    Ok(SrCtx {
        tenant_id: sr.get("tenant_id"),
        patient_id: sr.get("patient_id"),
        facility_id: sr.get("facility_id"),
        state: LoopState::parse(sr.get::<String, _>("loop_state").as_str())
            .ok_or_else(|| ApiError::internal("invalid loop state"))?,
        version: sr.get("version"),
    })
}

#[allow(clippy::too_many_arguments)]
async fn transition(
    state: &AppState,
    ctx: &AuthContext,
    id: Uuid,
    body: &TransitionBody,
    action: &'static str,
    t: LoopTransition,
    event: &'static str,
    kind: &'static str,
    note_required: bool,
) -> Result<Json<Value>, ApiError> {
    let sr = load_sr(state, id).await?;
    let allowed = guard(
        state,
        ctx,
        action,
        "service_request",
        Some(ResourceCtx {
            tenant_id: sr.tenant_id,
            patient_id: Some(sr.patient_id),
            facility_id: Some(sr.facility_id),
        }),
    )
    .await?;
    let note = body
        .note
        .as_deref()
        .map(str::trim)
        .filter(|n| !n.is_empty());
    if note_required && note.is_none() {
        return Err(ApiError::bad_request(
            "documentation_required",
            format!("a {kind} note documenting this step is required"),
        ));
    }
    if note.is_some_and(|n| n.len() > 4000) {
        return Err(ApiError::bad_request(
            "validation_failed",
            "note exceeds 4000 characters",
        ));
    }
    // The expected version must match the same snapshot the state was read
    // from, so a concurrently committed transition cannot be replayed by
    // guessing the next version number.
    if body.version != sr.version {
        return Err(ApiError::conflict(
            "version_conflict",
            "service request was modified concurrently",
        ));
    }
    let next = sr
        .state
        .apply(t)
        .map_err(|e| ApiError::conflict("invalid_loop_transition", e.to_string()))?;

    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, ctx, &state.cell).await?;
    let updated = sqlx::query(
        "UPDATE service_requests SET loop_state = $1, version = version + 1
         WHERE id = $2 AND version = $3 AND loop_state = $4",
    )
    .bind(next.as_str())
    .bind(id)
    .bind(sr.version)
    .bind(sr.state.as_str())
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "version_conflict",
            "service request was modified concurrently",
        ));
    }
    if event == "result_loop.closed" {
        // Closing the loop completes open follow-up tasks and alerts.
        sqlx::query(
            "UPDATE follow_up_tasks SET status='completed', completed_by=$1
             WHERE tenant_id=$2 AND service_request_id=$3 AND status IN ('open','overdue')",
        )
        .bind(ctx.user_id)
        .bind(sr.tenant_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE alerts SET status='resolved', closed_at=now()
             WHERE tenant_id=$1 AND status='open'
               AND observation_id IN (SELECT id FROM observations WHERE service_request_id=$2)",
        )
        .bind(sr.tenant_id)
        .bind(id)
        .execute(&mut *tx)
        .await?;
    }
    if let Some(note) = note {
        // Clinical documentation for the step is part of the same transaction
        // as the state change.
        sqlx::query(
            "INSERT INTO loop_notes (id, tenant_id, service_request_id, kind, note, created_by)
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(Uuid::now_v7())
        .bind(sr.tenant_id)
        .bind(id)
        .bind(kind)
        .bind(note)
        .bind(ctx.user_id)
        .execute(&mut *tx)
        .await?;
    }
    audit::emit(
        &mut *tx,
        ctx,
        event,
        &state.cell,
        json!({ "service_request_id": id, "note_present": note.is_some() }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({
        "id": id,
        "loop_state": next.as_str(),
        "version": body.version + 1
    })))
}

pub async fn review(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<TransitionBody>,
) -> Result<Json<Value>, ApiError> {
    transition(
        &state,
        &ctx,
        id,
        &body,
        actions::RESULT_REVIEW,
        LoopTransition::ResultReviewed,
        "result.reviewed",
        "review",
        false,
    )
    .await
}

pub async fn notify(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<TransitionBody>,
) -> Result<Json<Value>, ApiError> {
    transition(
        &state,
        &ctx,
        id,
        &body,
        actions::PATIENT_NOTIFY,
        LoopTransition::PatientNotified,
        "patient.notified",
        "notification",
        true,
    )
    .await
}

pub async fn close(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
    Json(body): Json<TransitionBody>,
) -> Result<Json<Value>, ApiError> {
    transition(
        &state,
        &ctx,
        id,
        &body,
        actions::LOOP_CLOSE,
        LoopTransition::LoopClosed,
        "result_loop.closed",
        "closure",
        true,
    )
    .await
}

#[derive(Deserialize)]
pub struct WorklistQuery {
    /// Only items with an open critical alert.
    pub critical: Option<bool>,
    /// Restrict to one workflow state (`ordered`..`notified`).
    pub state: Option<String>,
    /// Case-insensitive patient name/identifier match.
    pub query: Option<String>,
    /// Keyset cursor returned as `next_cursor` by the previous page; each
    /// page holds up to [`WORKLIST_PAGE_SIZE`] rows.
    pub cursor: Option<String>,
}

/// Rows returned per worklist page. Older rows stay reachable through the
/// `cursor` parameter rather than being cut off by a fixed cap.
pub const WORKLIST_PAGE_SIZE: i64 = 200;

/// Keyset cursor over the ordering tuple
/// `(snapshot priority DESC, created_at DESC, id DESC)`.
///
/// Priority (an open critical alert) is mutable, so ordering by the live
/// value would let rows cross the cursor boundary between page fetches and
/// be skipped or repeated. Instead the first page captures a snapshot
/// instant that later pages carry in the cursor, and priority is evaluated
/// *as of that instant* from the alert's immutable `created_at` and
/// monotonic `closed_at`: critical results sort first, and every row's
/// position stays fixed for the whole page sequence. A row that turns
/// critical after the snapshot still appears (in its routine position) and
/// moves to the top on the next refresh; the live `has_open_alert` value is
/// returned per row for display.
struct WorklistCursor {
    snapshot_at: chrono::DateTime<chrono::Utc>,
    priority: bool,
    created_at: chrono::DateTime<chrono::Utc>,
    id: Uuid,
}

/// Whether the request had an open critical alert as of `$3` (the snapshot
/// instant): the alert existed then and had not yet been closed.
const SNAP_PRIORITY_EXPR: &str = "CASE WHEN EXISTS (
        SELECT 1 FROM alerts a JOIN observations o ON a.observation_id = o.id
        WHERE o.service_request_id = sr.id
          AND a.created_at <= $3
          AND (a.closed_at IS NULL OR a.closed_at > $3)
    ) THEN 1 ELSE 0 END";

impl WorklistCursor {
    /// URL-safe encoding: `{snapshot_micros}.{priority}.{epoch_micros}.{uuid}`.
    fn encode(&self) -> String {
        format!(
            "{}.{}.{}.{}",
            self.snapshot_at.timestamp_micros(),
            i32::from(self.priority),
            self.created_at.timestamp_micros(),
            self.id
        )
    }

    fn decode(raw: &str) -> Option<Self> {
        let mut parts = raw.splitn(4, '.');
        let snap_micros: i64 = parts.next()?.parse().ok()?;
        let snapshot_at = chrono::DateTime::from_timestamp_micros(snap_micros)?;
        let priority = match parts.next()? {
            "0" => false,
            "1" => true,
            _ => return None,
        };
        let micros: i64 = parts.next()?.parse().ok()?;
        let created_at = chrono::DateTime::from_timestamp_micros(micros)?;
        let id = Uuid::parse_str(parts.next()?).ok()?;
        Some(Self {
            snapshot_at,
            priority,
            created_at,
            id,
        })
    }
}

pub async fn worklist(
    State(state): State<AppState>,
    ctx: AuthContext,
    axum::extract::Query(params): axum::extract::Query<WorklistQuery>,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::WORKLIST_READ,
        "worklist",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: None,
        }),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;
    let state_filter = match params.state.as_deref() {
        None | Some("all") => None,
        Some(s) => Some(
            LoopState::parse(s)
                .filter(|st| *st != LoopState::Closed)
                .ok_or_else(|| ApiError::bad_request("invalid_state", "unknown workflow state"))?
                .as_str()
                .to_string(),
        ),
    };
    let query_filter = params
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(|q| {
            let escaped = q
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_");
            format!("%{escaped}%")
        });
    // The worklist is filtered to the caller's facility scope, resolved from
    // trusted role assignments (tenant-wide only for allowlisted roles).
    // Criticality/state/patient filters run in SQL before the row cap so a
    // filtered view always covers every matching open result, not just the
    // newest rows.
    let scope = facility_scope(&ctx, actions::WORKLIST_READ);
    if matches!(&scope, Some(ids) if ids.is_empty()) {
        return Ok(Json(
            json!({ "items": [], "has_more": false, "next_cursor": null }),
        ));
    }
    let cursor =
        match params.cursor.as_deref() {
            None => None,
            Some(raw) => Some(WorklistCursor::decode(raw).ok_or_else(|| {
                ApiError::bad_request("invalid_cursor", "malformed worklist cursor")
            })?),
        };
    // The first page fixes the snapshot instant; later pages reuse the one
    // carried in the cursor so the whole sequence shares one stable ordering.
    let snapshot_at = cursor
        .as_ref()
        .map(|c| c.snapshot_at)
        .unwrap_or_else(chrono::Utc::now);
    let mut sql = format!(
        "SELECT sr.id, sr.display, sr.code_loinc, sr.loop_state, sr.version, sr.created_at,
                p.family_name, p.given_name, p.identifier, p.facility_id,
                {SNAP_PRIORITY_EXPR} AS snap_priority,
                EXISTS (SELECT 1 FROM alerts a JOIN observations o ON a.observation_id = o.id
                        WHERE o.service_request_id = sr.id AND a.status = 'open') AS has_open_alert,
                EXISTS (SELECT 1 FROM encounters e
                        WHERE e.tenant_id = sr.tenant_id AND e.patient_id = p.id
                          AND e.practitioner_id = $2) AS has_relationship
         FROM service_requests sr JOIN patients p ON p.id = sr.patient_id
         WHERE sr.tenant_id = $1 AND sr.loop_state <> 'closed'",
    );
    let mut arg = 3;
    if scope.is_some() {
        arg += 1;
        sql.push_str(&format!(" AND p.facility_id = ANY(${arg})"));
    }
    if state_filter.is_some() {
        arg += 1;
        sql.push_str(&format!(" AND sr.loop_state = ${arg}"));
    }
    if params.critical == Some(true) {
        sql.push_str(
            " AND EXISTS (SELECT 1 FROM alerts a JOIN observations o ON a.observation_id = o.id
                          WHERE o.service_request_id = sr.id AND a.status = 'open')",
        );
    }
    if query_filter.is_some() {
        arg += 1;
        // Match both display orders ("Family Given" and "Given Family") so a
        // name copied from the UI finds the row regardless of word order.
        sql.push_str(&format!(
            " AND ((p.family_name || ' ' || p.given_name || ' ' || p.identifier) ILIKE ${arg}
                   OR (p.given_name || ' ' || p.family_name || ' ' || p.identifier) ILIKE ${arg})"
        ));
    }
    if cursor.is_some() {
        // Keyset predicate: strictly after the cursor tuple in the DESC
        // ordering below (row-value comparison, all columns descending).
        sql.push_str(&format!(
            " AND ({SNAP_PRIORITY_EXPR}, sr.created_at, sr.id) < (${}, ${}, ${})",
            arg + 1,
            arg + 2,
            arg + 3
        ));
    }
    // Deterministic ordering (id tie-breaker) over columns that are stable
    // for the snapshot instant, so pages never skip or repeat rows even when
    // alert priority changes between fetches; one extra row detects whether
    // more pages exist.
    sql.push_str(&format!(
        " ORDER BY snap_priority DESC, sr.created_at DESC, sr.id DESC LIMIT {}",
        WORKLIST_PAGE_SIZE + 1
    ));
    let mut q = sqlx::query(&sql)
        .bind(ctx.tenant_id)
        .bind(ctx.user_id)
        .bind(snapshot_at);
    if let Some(ids) = &scope {
        q = q.bind(ids);
    }
    if let Some(s) = &state_filter {
        q = q.bind(s);
    }
    if let Some(pat) = &query_filter {
        q = q.bind(pat);
    }
    if let Some(c) = &cursor {
        q = q.bind(i32::from(c.priority)).bind(c.created_at).bind(c.id);
    }
    let mut rows = q.fetch_all(&state.pool).await?;
    let has_more = rows.len() as i64 > WORKLIST_PAGE_SIZE;
    rows.truncate(WORKLIST_PAGE_SIZE as usize);
    let next_cursor = if has_more {
        rows.last().map(|r| {
            WorklistCursor {
                snapshot_at,
                priority: r.get::<i32, _>("snap_priority") == 1,
                created_at: r.get::<chrono::DateTime<chrono::Utc>, _>("created_at"),
                id: r.get::<Uuid, _>("id"),
            }
            .encode()
        })
    } else {
        None
    };
    // Display-only hint mirroring the detail endpoint's PATIENT_READ policy
    // (role grant at the result's facility; care relationship for physicians
    // without an administrative role). The detail guard stays authoritative.
    let read_scope = facility_scope(&ctx, actions::PATIENT_READ);
    let read_needs_relationship =
        ctx.has_role(roles::PHYSICIAN) && !ctx.has_role(roles::CLINICAL_ADMIN);
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
            let patient_facility: Uuid = r.get("facility_id");
            let at_facility = match &read_scope {
                None => true,
                Some(ids) => ids.contains(&patient_facility),
            };
            let can_open_detail =
                at_facility && (!read_needs_relationship || r.get::<bool, _>("has_relationship"));
            json!({
                "id": r.get::<Uuid,_>("id"),
                "display": r.get::<String,_>("display"),
                "code_loinc": r.get::<String,_>("code_loinc"),
                "loop_state": r.get::<String,_>("loop_state"),
                "version": r.get::<i64,_>("version"),
                "created_at": r.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
                "patient": {
                    "family_name": r.get::<String,_>("family_name"),
                    "given_name": r.get::<String,_>("given_name"),
                    "identifier": r.get::<String,_>("identifier"),
                },
                "has_open_alert": r.get::<bool,_>("has_open_alert"),
                "can_open_detail": can_open_detail,
            })
        })
        .collect();
    Ok(Json(
        json!({ "items": items, "has_more": has_more, "next_cursor": next_cursor }),
    ))
}

/// Aggregate counts backing the dashboard status cards. Same authorization
/// and facility scoping as the worklist; returns only numbers.
pub async fn worklist_summary(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::WORKLIST_READ,
        "worklist",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: None,
        }),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;
    let scope = facility_scope(&ctx, actions::WORKLIST_READ);
    const SUMMARY_SQL: &str = "SELECT
            COUNT(*) FILTER (WHERE sr.loop_state <> 'closed' AND EXISTS (
                SELECT 1 FROM alerts a JOIN observations o ON a.observation_id = o.id
                WHERE o.service_request_id = sr.id AND a.status = 'open')) AS critical_open,
            COUNT(*) FILTER (WHERE sr.loop_state = 'received') AS awaiting_review,
            COUNT(*) FILTER (WHERE sr.loop_state = 'reviewed') AS awaiting_notification,
            COUNT(*) FILTER (WHERE sr.loop_state = 'notified') AS awaiting_closure,
            COUNT(*) FILTER (WHERE sr.loop_state = 'closed' AND EXISTS (
                SELECT 1 FROM loop_notes n WHERE n.service_request_id = sr.id
                    AND n.kind = 'closure'
                    AND n.created_at > now() - interval '7 days')) AS recently_closed
         FROM service_requests sr JOIN patients p ON p.id = sr.patient_id
         WHERE sr.tenant_id = $1";
    let row = match &scope {
        None => {
            sqlx::query(SUMMARY_SQL)
                .bind(ctx.tenant_id)
                .fetch_one(&state.pool)
                .await?
        }
        Some(ids) if ids.is_empty() => {
            return Ok(Json(json!({
                "critical_open": 0,
                "awaiting_review": 0,
                "awaiting_notification": 0,
                "awaiting_closure": 0,
                "recently_closed": 0,
            })));
        }
        Some(ids) => {
            sqlx::query(&format!("{SUMMARY_SQL} AND p.facility_id = ANY($2)"))
                .bind(ctx.tenant_id)
                .bind(ids)
                .fetch_one(&state.pool)
                .await?
        }
    };
    Ok(Json(json!({
        "critical_open": row.get::<i64,_>("critical_open"),
        "awaiting_review": row.get::<i64,_>("awaiting_review"),
        "awaiting_notification": row.get::<i64,_>("awaiting_notification"),
        "awaiting_closure": row.get::<i64,_>("awaiting_closure"),
        "recently_closed": row.get::<i64,_>("recently_closed"),
    })))
}

pub async fn detail(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let sr = load_sr(&state, id).await?;
    guard(
        &state,
        &ctx,
        actions::PATIENT_READ,
        "service_request",
        Some(ResourceCtx {
            tenant_id: sr.tenant_id,
            patient_id: Some(sr.patient_id),
            facility_id: Some(sr.facility_id),
        }),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;

    let head = sqlx::query(
        "SELECT sr.id, sr.display, sr.code_loinc, sr.loop_state, sr.version, sr.created_at,
                sr.patient_id, p.family_name, p.given_name, p.identifier
         FROM service_requests sr JOIN patients p ON p.id = sr.patient_id WHERE sr.id = $1",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await?;

    // Display-only capability hints mirroring the central policy for the
    // consequential loop transitions: role grant at this result's facility
    // plus an established care relationship. The guards on the transition
    // endpoints stay authoritative.
    let (has_relationship,): (bool,) = sqlx::query_as(
        "SELECT EXISTS (SELECT 1 FROM encounters
         WHERE tenant_id = $1 AND patient_id = $2 AND practitioner_id = $3)",
    )
    .bind(sr.tenant_id)
    .bind(sr.patient_id)
    .bind(ctx.user_id)
    .fetch_one(&state.pool)
    .await?;
    let can_transition = |action: &str| -> bool {
        let at_facility = match facility_scope(&ctx, action) {
            None => true,
            Some(ids) => ids.contains(&sr.facility_id),
        };
        at_facility && has_relationship
    };
    let capabilities = json!({
        "review": can_transition(actions::RESULT_REVIEW),
        "notify": can_transition(actions::PATIENT_NOTIFY),
        "close": can_transition(actions::LOOP_CLOSE),
    });

    let observation_rows = sqlx::query(
        "SELECT id, code_loinc, value_num::text AS value_num, unit, reference_range, status,
                amends, source_system, effective_at, received_at
         FROM observations WHERE tenant_id=$1 AND service_request_id=$2 ORDER BY received_at",
    )
    .bind(sr.tenant_id)
    .bind(id)
    .fetch_all(&state.pool)
    .await?;
    // Observation rows are append-only: supersession is derived from the
    // amendment relationship instead of mutating historical rows.
    let amended_ids: std::collections::HashSet<Uuid> = observation_rows
        .iter()
        .filter_map(|r| r.get::<Option<Uuid>, _>("amends"))
        .collect();
    let observations = observation_rows
        .iter()
        .map(|r| {
            let obs_id = r.get::<Uuid, _>("id");
            let status = if amended_ids.contains(&obs_id) {
                "amended-superseded".to_string()
            } else {
                r.get::<String, _>("status")
            };
            json!({
                "id": obs_id,
                "code_loinc": r.get::<String,_>("code_loinc"),
                "value": r.get::<String,_>("value_num"),
                "unit": r.get::<String,_>("unit"),
                "reference_range": r.get::<Option<String>,_>("reference_range"),
                "status": status,
                "amends": r.get::<Option<Uuid>,_>("amends"),
                "source_system": r.get::<String,_>("source_system"),
                "effective_at": r.get::<chrono::DateTime<chrono::Utc>,_>("effective_at"),
                "received_at": r.get::<chrono::DateTime<chrono::Utc>,_>("received_at"),
            })
        })
        .collect::<Vec<_>>();

    let rule_evaluations = sqlx::query(
        "SELECT re.rule_id, re.rule_version, re.outcome, re.evaluated_at
         FROM rule_evaluations re JOIN observations o ON o.id = re.observation_id
         WHERE re.tenant_id=$1 AND o.service_request_id=$2 ORDER BY re.evaluated_at",
    )
    .bind(sr.tenant_id)
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "rule_id": r.get::<String,_>("rule_id"),
            "rule_version": r.get::<String,_>("rule_version"),
            "outcome": r.get::<Value,_>("outcome"),
            "evaluated_at": r.get::<chrono::DateTime<chrono::Utc>,_>("evaluated_at"),
        })
    })
    .collect::<Vec<_>>();

    let artifacts = sqlx::query(
        "SELECT id, observation_id, artifact_type, autonomy_level, status, model, model_version, route,
                template, input_hash, output, citations, limitations, review_decision,
                review_note, reviewed_at, generated_at
         FROM ai_artifacts WHERE tenant_id=$1 AND service_request_id=$2 ORDER BY created_at",
    )
    .bind(sr.tenant_id)
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "observation_id": r.get::<Option<Uuid>,_>("observation_id"),
            "artifact_type": r.get::<String,_>("artifact_type"),
            "autonomy_level": r.get::<String,_>("autonomy_level"),
            "status": r.get::<String,_>("status"),
            "model": r.get::<Option<String>,_>("model"),
            "model_version": r.get::<Option<String>,_>("model_version"),
            "route": r.get::<Option<String>,_>("route"),
            "template": r.get::<Option<String>,_>("template"),
            "input_hash": r.get::<Option<String>,_>("input_hash"),
            "output": r.get::<Option<Value>,_>("output"),
            "citations": r.get::<Value,_>("citations"),
            "limitations": r.get::<Value,_>("limitations"),
            "review_decision": r.get::<Option<String>,_>("review_decision"),
            "review_note": r.get::<Option<String>,_>("review_note"),
            "reviewed_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("reviewed_at"),
            "generated_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("generated_at"),
        })
    })
    .collect::<Vec<_>>();

    let tasks = sqlx::query(
        "SELECT id, description, priority, status, due_at FROM follow_up_tasks
         WHERE tenant_id=$1 AND service_request_id=$2 ORDER BY created_at",
    )
    .bind(sr.tenant_id)
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "description": r.get::<String,_>("description"),
            "priority": r.get::<String,_>("priority"),
            "status": r.get::<String,_>("status"),
            "due_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("due_at"),
        })
    })
    .collect::<Vec<_>>();

    let alerts = sqlx::query(
        "SELECT a.id, a.severity, a.message, a.status, a.created_at
         FROM alerts a JOIN observations o ON o.id = a.observation_id
         WHERE a.tenant_id=$1 AND o.service_request_id=$2 ORDER BY a.created_at",
    )
    .bind(sr.tenant_id)
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "severity": r.get::<String,_>("severity"),
            "message": r.get::<String,_>("message"),
            "status": r.get::<String,_>("status"),
            "created_at": r.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
        })
    })
    .collect::<Vec<_>>();

    let dq = sqlx::query(
        "SELECT dq.issue, dq.created_at FROM data_quality_issues dq
         WHERE dq.tenant_id=$1 AND dq.resource_type='observation'
           AND dq.resource_id IN (SELECT id FROM observations WHERE service_request_id=$2)",
    )
    .bind(sr.tenant_id)
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "issue": r.get::<String,_>("issue"),
            "created_at": r.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
        })
    })
    .collect::<Vec<_>>();

    let notes = sqlx::query(
        "SELECT n.kind, n.note, n.created_at, u.display_name AS author
         FROM loop_notes n JOIN users u ON u.id = n.created_by
         WHERE n.tenant_id=$1 AND n.service_request_id=$2 ORDER BY n.created_at",
    )
    .bind(sr.tenant_id)
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "kind": r.get::<String,_>("kind"),
            "note": r.get::<String,_>("note"),
            "author": r.get::<String,_>("author"),
            "created_at": r.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
        })
    })
    .collect::<Vec<_>>();

    Ok(Json(json!({
        "service_request": {
            "id": head.get::<Uuid,_>("id"),
            "display": head.get::<String,_>("display"),
            "code_loinc": head.get::<String,_>("code_loinc"),
            "loop_state": head.get::<String,_>("loop_state"),
            "version": head.get::<i64,_>("version"),
            "created_at": head.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
            "patient": {
                "id": head.get::<Uuid,_>("patient_id"),
                "family_name": head.get::<String,_>("family_name"),
                "given_name": head.get::<String,_>("given_name"),
                "identifier": head.get::<String,_>("identifier"),
            }
        },
        "observations": observations,
        "rule_evaluations": rule_evaluations,
        "ai_artifacts": artifacts,
        "follow_up_tasks": tasks,
        "alerts": alerts,
        "data_quality_issues": dq,
        "notes": notes,
        "capabilities": capabilities,
    })))
}
