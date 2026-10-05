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
                t.service_request_id, t.patient_id, p.family_name, p.given_name, p.identifier, p.facility_id,
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
            "service_request_id": r.get::<Option<Uuid>,_>("service_request_id"),
            "patient_id": r.get::<Uuid,_>("patient_id"),
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

// --- Per-user cockpit layout ------------------------------------------------
//
// Presentation preferences only (widget order, hidden widgets, widget sizes
// and density), keyed by (tenant, user). The document is validated against the
// fixed widget catalogue so nothing but layout can ever be stored, and writes
// are versioned so a stale tab cannot silently overwrite a newer layout.

pub const WIDGETS: &[&str] = &[
    "ready",
    "alerts",
    "triage",
    "access",
    "drafts",
    "attention",
    "results",
    "tasks",
    "ai",
];
const SIZES: &[&str] = &["half", "full"];
const DENSITIES: &[&str] = &["compact", "expanded"];
const MAX_LAYOUT_BYTES: usize = 4096;

fn invalid(message: &str) -> ApiError {
    ApiError::bad_request("invalid_layout", message)
}

/// Accepts only a complete, well-formed layout and returns it normalised:
/// `order` lists every widget exactly once, `hidden` and `sizes` reference
/// known widgets only, `density` is one of the supported values.
pub fn validate_layout(layout: &Value) -> Result<Value, ApiError> {
    if layout.to_string().len() > MAX_LAYOUT_BYTES {
        return Err(invalid("layout too large"));
    }
    let obj = layout
        .as_object()
        .ok_or_else(|| invalid("layout must be an object"))?;
    for key in obj.keys() {
        if !matches!(key.as_str(), "order" | "hidden" | "sizes" | "density") {
            return Err(invalid("unknown layout field"));
        }
    }
    let order: Vec<&str> = obj
        .get("order")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("order must be an array"))?
        .iter()
        .map(|w| {
            w.as_str()
                .ok_or_else(|| invalid("order entries must be strings"))
        })
        .collect::<Result<_, _>>()?;
    if order.len() != WIDGETS.len() || WIDGETS.iter().any(|w| !order.contains(w)) {
        return Err(invalid("order must list every widget exactly once"));
    }
    let mut seen = std::collections::HashSet::new();
    if order.iter().any(|w| !seen.insert(*w)) {
        return Err(invalid("order must list every widget exactly once"));
    }
    let hidden: Vec<&str> = obj
        .get("hidden")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("hidden must be an array"))?
        .iter()
        .map(|w| {
            w.as_str()
                .ok_or_else(|| invalid("hidden entries must be strings"))
        })
        .collect::<Result<_, _>>()?;
    if hidden.iter().any(|w| !WIDGETS.contains(w)) {
        return Err(invalid("hidden references an unknown widget"));
    }
    let mut hidden_norm: Vec<&str> = Vec::new();
    for w in hidden {
        if !hidden_norm.contains(&w) {
            hidden_norm.push(w);
        }
    }
    let mut sizes = serde_json::Map::new();
    if let Some(raw) = obj.get("sizes") {
        let map = raw
            .as_object()
            .ok_or_else(|| invalid("sizes must be an object"))?;
        for (w, size) in map {
            if !WIDGETS.contains(&w.as_str()) {
                return Err(invalid("sizes references an unknown widget"));
            }
            let size = size
                .as_str()
                .filter(|s| SIZES.contains(s))
                .ok_or_else(|| invalid("unsupported widget size"))?;
            sizes.insert(w.clone(), json!(size));
        }
    }
    let density = obj
        .get("density")
        .and_then(Value::as_str)
        .filter(|d| DENSITIES.contains(d))
        .ok_or_else(|| invalid("unsupported density"))?;
    Ok(json!({
        "order": order,
        "hidden": hidden_norm,
        "sizes": sizes,
        "density": density,
    }))
}

/// Anyone who can populate at least one cockpit widget may keep a layout;
/// service credentials have no dashboard.
async fn authorize_preferences(state: &AppState, ctx: &AuthContext) -> Result<(), ApiError> {
    if ctx.is_service {
        return Err(ApiError::forbidden("service principals have no dashboard"));
    }
    let resource = ResourceCtx {
        tenant_id: ctx.tenant_id,
        patient_id: None,
        facility_id: None,
    };
    let worklist =
        crate::policy::authorize(&state.pool, ctx, actions::WORKLIST_READ, Some(&resource)).await?;
    let action = if worklist.allowed {
        actions::WORKLIST_READ
    } else {
        actions::VISIT_READ
    };
    guard(state, ctx, action, "dashboard_preferences", Some(resource))
        .await?
        .record_on_pool(state, ctx)
        .await?;
    Ok(())
}

fn preferences_json(row: Option<(Value, i32, chrono::DateTime<chrono::Utc>)>) -> Value {
    match row {
        None => json!({ "layout": Value::Null, "version": 0, "updated_at": Value::Null }),
        Some((layout, version, updated_at)) => json!({
            "layout": layout,
            "version": version,
            "updated_at": updated_at,
        }),
    }
}

pub async fn get_preferences(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Value>, ApiError> {
    authorize_preferences(&state, &ctx).await?;
    let row: Option<(Value, i32, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT layout, version, updated_at FROM dashboard_preferences
         WHERE tenant_id = $1 AND user_id = $2",
    )
    .bind(ctx.tenant_id)
    .bind(ctx.user_id)
    .fetch_optional(&state.pool)
    .await
    .map_err(ApiError::internal)?;
    Ok(Json(preferences_json(row)))
}

#[derive(serde::Deserialize)]
pub struct PutPreferences {
    pub layout: Value,
    /// Version last seen by the client; `0` when no layout was stored yet.
    #[serde(default)]
    pub version: i32,
}

pub async fn put_preferences(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<PutPreferences>,
) -> Result<Json<Value>, ApiError> {
    authorize_preferences(&state, &ctx).await?;
    let layout = validate_layout(&body.layout)?;
    let mut tx = state.pool.begin().await.map_err(ApiError::internal)?;
    let current: Option<(Value, i32, chrono::DateTime<chrono::Utc>)> = sqlx::query_as(
        "SELECT layout, version, updated_at FROM dashboard_preferences
         WHERE tenant_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(ctx.tenant_id)
    .bind(ctx.user_id)
    .fetch_optional(&mut *tx)
    .await
    .map_err(ApiError::internal)?;
    let current_version = current.as_ref().map_or(0, |c| c.1);
    if current_version != body.version {
        return Err(ApiError::conflict(
            "layout_conflict",
            "the dashboard layout changed elsewhere; reload before saving",
        ));
    }
    let row: (Value, i32, chrono::DateTime<chrono::Utc>) = sqlx::query_as(
        "INSERT INTO dashboard_preferences (tenant_id, user_id, layout, version)
         VALUES ($1, $2, $3, 1)
         ON CONFLICT (tenant_id, user_id) DO UPDATE
            SET layout = EXCLUDED.layout,
                version = dashboard_preferences.version + 1,
                updated_at = now()
         RETURNING layout, version, updated_at",
    )
    .bind(ctx.tenant_id)
    .bind(ctx.user_id)
    .bind(&layout)
    .fetch_one(&mut *tx)
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await.map_err(ApiError::internal)?;
    Ok(Json(preferences_json(Some(row))))
}
