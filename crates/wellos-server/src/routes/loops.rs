//! Closed-loop transitions: review, notification, closure, worklist, detail.

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, ResourceCtx};
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
    state: LoopState,
}

async fn load_sr(state: &AppState, id: Uuid) -> Result<SrCtx, ApiError> {
    let sr =
        sqlx::query("SELECT tenant_id, patient_id, loop_state FROM service_requests WHERE id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await?
            .ok_or_else(ApiError::not_found)?;
    Ok(SrCtx {
        tenant_id: sr.get("tenant_id"),
        patient_id: sr.get("patient_id"),
        state: LoopState::parse(sr.get::<String, _>("loop_state").as_str())
            .ok_or_else(|| ApiError::internal("invalid loop state"))?,
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
    let next = sr
        .state
        .apply(t)
        .map_err(|e| ApiError::conflict("invalid_loop_transition", e.to_string()))?;

    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, ctx, &state.cell).await?;
    let updated = sqlx::query(
        "UPDATE service_requests SET loop_state = $1, version = version + 1
         WHERE id = $2 AND version = $3",
    )
    .bind(next.as_str())
    .bind(id)
    .bind(body.version)
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
            "UPDATE alerts SET status='resolved'
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

pub async fn worklist(
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
        }),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;
    let rows = sqlx::query(
        "SELECT sr.id, sr.display, sr.code_loinc, sr.loop_state, sr.version, sr.created_at,
                p.family_name, p.given_name, p.identifier,
                EXISTS (SELECT 1 FROM alerts a JOIN observations o ON a.observation_id = o.id
                        WHERE o.service_request_id = sr.id AND a.status = 'open') AS has_open_alert
         FROM service_requests sr JOIN patients p ON p.id = sr.patient_id
         WHERE sr.tenant_id = $1
         ORDER BY has_open_alert DESC, sr.created_at DESC LIMIT 200",
    )
    .bind(ctx.tenant_id)
    .fetch_all(&state.pool)
    .await?;
    let items: Vec<Value> = rows
        .iter()
        .map(|r| {
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
            })
        })
        .collect();
    Ok(Json(json!({ "items": items })))
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

    let observations = sqlx::query(
        "SELECT id, code_loinc, value_num::text AS value_num, unit, reference_range, status,
                amends, source_system, effective_at, received_at
         FROM observations WHERE tenant_id=$1 AND service_request_id=$2 ORDER BY received_at",
    )
    .bind(sr.tenant_id)
    .bind(id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "code_loinc": r.get::<String,_>("code_loinc"),
            "value": r.get::<String,_>("value_num"),
            "unit": r.get::<String,_>("unit"),
            "reference_range": r.get::<Option<String>,_>("reference_range"),
            "status": r.get::<String,_>("status"),
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
    })))
}
