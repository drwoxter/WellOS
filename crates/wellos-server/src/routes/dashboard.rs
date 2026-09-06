//! Clinician cockpit: the data behind the dashboard widgets. Every list is
//! scoped by the caller's trusted facility assignments and mirrors the
//! central policy in display-only capability hints; the guarded detail
//! routes remain authoritative. Nothing here is cached in the browser.

use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, facility_scope, roles, ResourceCtx};
use crate::routes::brief::{task_order, ACTIONABLE_TASK_STATUSES};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

fn in_scope(scope: &Option<Vec<Uuid>>, facility: Uuid) -> bool {
    match scope {
        None => true,
        Some(ids) => ids.contains(&facility),
    }
}

pub async fn cockpit(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::WORKLIST_READ,
        "dashboard",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
            facility_id: None,
        }),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;

    let worklist_scope = facility_scope(&ctx, actions::WORKLIST_READ);
    let read_scope = facility_scope(&ctx, actions::PATIENT_READ);
    let encounter_scope = facility_scope(&ctx, actions::ENCOUNTER_START);
    let needs_relationship = ctx.has_role(roles::PHYSICIAN) && !ctx.has_role(roles::CLINICAL_ADMIN);
    // An empty assignment list means "nothing", not "everything".
    let scope_ids: Option<Vec<Uuid>> = worklist_scope.clone();
    let (scope_all, scope_list) = match &scope_ids {
        None => (true, Vec::new()),
        Some(ids) => (false, ids.clone()),
    };

    // Own consultations still in progress (resumable drafts).
    let drafts = sqlx::query(
        "SELECT e.id, e.started_at, p.id AS patient_id, p.family_name, p.given_name, p.identifier,
                n.reason_for_encounter, COALESCE(n.updated_at, e.started_at) AS updated_at
         FROM encounters e
         JOIN patients p ON p.id = e.patient_id
         LEFT JOIN encounter_notes n ON n.tenant_id = e.tenant_id AND n.encounter_id = e.id
         WHERE e.tenant_id = $1 AND e.practitioner_id = $2
           AND e.status = 'in_progress' AND e.encounter_type = 'consultation'
         ORDER BY COALESCE(n.updated_at, e.started_at) DESC LIMIT 10",
    )
    .bind(ctx.tenant_id)
    .bind(ctx.user_id)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "started_at": r.get::<chrono::DateTime<chrono::Utc>,_>("started_at"),
            "updated_at": r.get::<chrono::DateTime<chrono::Utc>,_>("updated_at"),
            "reason": r.get::<Option<String>,_>("reason_for_encounter"),
            "patient": {
                "id": r.get::<Uuid,_>("patient_id"),
                "family_name": r.get::<String,_>("family_name"),
                "given_name": r.get::<String,_>("given_name"),
                "identifier": r.get::<String,_>("identifier"),
            },
        })
    })
    .collect::<Vec<_>>();

    // Patients with open critical alerts or actionable (open/overdue) tasks.
    let attention = sqlx::query(&format!(
        "SELECT p.id, p.family_name, p.given_name, p.identifier, p.facility_id,
                (SELECT count(*) FROM alerts a
                 WHERE a.tenant_id = p.tenant_id AND a.patient_id = p.id AND a.status = 'open') AS open_alerts,
                (SELECT count(*) FROM follow_up_tasks t
                 WHERE t.tenant_id = p.tenant_id AND t.patient_id = p.id
                   AND t.status IN {ACTIONABLE_TASK_STATUSES}) AS open_tasks,
                (SELECT max(x.at) FROM (
                    SELECT a.created_at AS at FROM alerts a
                    WHERE a.tenant_id = p.tenant_id AND a.patient_id = p.id AND a.status = 'open'
                    UNION ALL
                    SELECT t.created_at FROM follow_up_tasks t
                    WHERE t.tenant_id = p.tenant_id AND t.patient_id = p.id
                      AND t.status IN {ACTIONABLE_TASK_STATUSES}
                 ) x) AS latest_at,
                EXISTS (SELECT 1 FROM encounters e
                        WHERE e.tenant_id = p.tenant_id AND e.patient_id = p.id
                          AND e.practitioner_id = $2) AS has_relationship,
                (SELECT e.id FROM encounters e
                 WHERE e.tenant_id = p.tenant_id AND e.patient_id = p.id
                   AND e.practitioner_id = $2 AND e.status = 'in_progress'
                   AND e.encounter_type = 'consultation'
                 ORDER BY e.started_at DESC LIMIT 1) AS open_consultation_id
         FROM patients p
         WHERE p.tenant_id = $1 AND ($3 OR p.facility_id = ANY($4))
           AND (EXISTS (SELECT 1 FROM alerts a WHERE a.tenant_id = p.tenant_id
                        AND a.patient_id = p.id AND a.status = 'open')
             OR EXISTS (SELECT 1 FROM follow_up_tasks t WHERE t.tenant_id = p.tenant_id
                        AND t.patient_id = p.id AND t.status IN {ACTIONABLE_TASK_STATUSES}))
         ORDER BY open_alerts DESC, latest_at DESC LIMIT 10"
    ))
    .bind(ctx.tenant_id)
    .bind(ctx.user_id)
    .bind(scope_all)
    .bind(&scope_list)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        let facility: Uuid = r.get("facility_id");
        let has_relationship: bool = r.get("has_relationship");
        json!({
            "patient": {
                "id": r.get::<Uuid,_>("id"),
                "family_name": r.get::<String,_>("family_name"),
                "given_name": r.get::<String,_>("given_name"),
                "identifier": r.get::<String,_>("identifier"),
            },
            "open_alerts": r.get::<i64,_>("open_alerts"),
            "open_tasks": r.get::<i64,_>("open_tasks"),
            "latest_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("latest_at"),
            "open_consultation_id": r.get::<Option<Uuid>,_>("open_consultation_id"),
            "can_open_chart": in_scope(&read_scope, facility) && (!needs_relationship || has_relationship),
            "can_start_encounter": in_scope(&encounter_scope, facility),
        })
    })
    .collect::<Vec<_>>();

    // Actionable follow-up tasks: overdue first, then urgent/high priority.
    let tasks = sqlx::query(&format!(
        "SELECT t.id, t.description, t.priority, t.status, t.due_at, t.created_at,
                t.service_request_id, p.family_name, p.given_name, p.identifier, p.facility_id,
                EXISTS (SELECT 1 FROM encounters e
                        WHERE e.tenant_id = p.tenant_id AND e.patient_id = p.id
                          AND e.practitioner_id = $2) AS has_relationship
         FROM follow_up_tasks t JOIN patients p ON p.id = t.patient_id
         WHERE t.tenant_id = $1 AND t.status IN {ACTIONABLE_TASK_STATUSES}
           AND ($3 OR p.facility_id = ANY($4))
         ORDER BY {} LIMIT 10",
        task_order("t.")
    ))
    .bind(ctx.tenant_id)
    .bind(ctx.user_id)
    .bind(scope_all)
    .bind(&scope_list)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        let facility: Uuid = r.get("facility_id");
        let has_relationship: bool = r.get("has_relationship");
        json!({
            "id": r.get::<Uuid,_>("id"),
            "description": r.get::<String,_>("description"),
            "priority": r.get::<String,_>("priority"),
            "status": r.get::<String,_>("status"),
            "due_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("due_at"),
            "created_at": r.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
            "service_request_id": r.get::<Uuid,_>("service_request_id"),
            "patient": {
                "family_name": r.get::<String,_>("family_name"),
                "given_name": r.get::<String,_>("given_name"),
                "identifier": r.get::<String,_>("identifier"),
            },
            "can_open_detail": in_scope(&read_scope, facility) && (!needs_relationship || has_relationship),
        })
    })
    .collect::<Vec<_>>();

    // Recent dMind activity: artifact type, status and provenance only — no
    // generated text leaves this endpoint.
    let ai_activity = sqlx::query(
        "SELECT a.id, a.artifact_type, a.status, a.model, a.model_version, a.generated_at,
                a.reviewed_at, a.review_decision, a.encounter_id, a.service_request_id,
                p.family_name, p.given_name, p.identifier, p.facility_id,
                EXISTS (SELECT 1 FROM encounters e
                        WHERE e.tenant_id = p.tenant_id AND e.patient_id = p.id
                          AND e.practitioner_id = $2) AS has_relationship
         FROM ai_artifacts a JOIN patients p ON p.id = a.patient_id
         WHERE a.tenant_id = $1 AND ($3 OR p.facility_id = ANY($4))
         ORDER BY COALESCE(a.reviewed_at, a.generated_at) DESC LIMIT 8",
    )
    .bind(ctx.tenant_id)
    .bind(ctx.user_id)
    .bind(scope_all)
    .bind(&scope_list)
    .fetch_all(&state.pool)
    .await?
    .iter()
    .map(|r| {
        let facility: Uuid = r.get("facility_id");
        let has_relationship: bool = r.get("has_relationship");
        let can_open = in_scope(&read_scope, facility) && (!needs_relationship || has_relationship);
        json!({
            "id": r.get::<Uuid,_>("id"),
            "artifact_type": r.get::<String,_>("artifact_type"),
            "status": r.get::<String,_>("status"),
            "model": r.get::<Option<String>,_>("model"),
            "model_version": r.get::<Option<String>,_>("model_version"),
            "generated_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("generated_at"),
            "reviewed_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("reviewed_at"),
            "review_decision": r.get::<Option<String>,_>("review_decision"),
            "encounter_id": r.get::<Option<Uuid>,_>("encounter_id"),
            "service_request_id": r.get::<Option<Uuid>,_>("service_request_id"),
            "patient": {
                "family_name": r.get::<String,_>("family_name"),
                "given_name": r.get::<String,_>("given_name"),
                "identifier": r.get::<String,_>("identifier"),
            },
            "can_open": can_open,
        })
    })
    .collect::<Vec<_>>();

    Ok(Json(json!({
        "draft_consultations": drafts,
        "attention": attention,
        "pending_tasks": tasks,
        "ai_activity": ai_activity,
        "generated_at": chrono::Utc::now(),
    })))
}
