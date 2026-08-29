use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, ResourceCtx};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

pub async fn health() -> Json<Value> {
    Json(json!({ "status": "ok" }))
}

pub async fn ready(State(state): State<AppState>) -> Result<Json<Value>, ApiError> {
    let db_ok = sqlx::query("SELECT 1").execute(&state.pool).await.is_ok();
    Ok(Json(json!({
        "status": if db_ok { "ready" } else { "degraded" },
        "dependencies": { "database": db_ok, "model_gateway": "async-optional" }
    })))
}

/// Tenant metadata for UI theming (public brand tokens only).
pub async fn tenant_meta(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Value>, ApiError> {
    let row = sqlx::query("SELECT name, brand, cell FROM tenants WHERE id = $1")
        .bind(ctx.tenant_id)
        .fetch_one(&state.pool)
        .await?;
    let facilities = sqlx::query("SELECT id, name FROM facilities WHERE tenant_id = $1")
        .bind(ctx.tenant_id)
        .fetch_all(&state.pool)
        .await?
        .iter()
        .map(|r| json!({ "id": r.get::<Uuid,_>("id"), "name": r.get::<String,_>("name") }))
        .collect::<Vec<_>>();
    Ok(Json(json!({
        "tenant": {
            "id": ctx.tenant_id,
            "name": row.get::<String,_>("name"),
            "brand": row.get::<Value,_>("brand"),
            "cell": row.get::<String,_>("cell"),
        },
        "user": {
            "username": ctx.username,
            "display_name": ctx.display_name,
            "roles": ctx.roles,
        },
        "facilities": facilities,
    })))
}

pub async fn audit_log(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::AUDIT_READ,
        "audit",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
        }),
    )
    .await?;
    let rows = sqlx::query(
        "SELECT actor, action, resource_type, resource_id, decision, reason,
                purpose_of_use, break_glass, correlation_id, recorded_at
         FROM audit_events WHERE tenant_id = $1 ORDER BY recorded_at DESC LIMIT 500",
    )
    .bind(ctx.tenant_id)
    .fetch_all(&state.pool)
    .await?;
    let events: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "actor": r.get::<String,_>("actor"),
                "action": r.get::<String,_>("action"),
                "resource_type": r.get::<Option<String>,_>("resource_type"),
                "resource_id": r.get::<Option<String>,_>("resource_id"),
                "decision": r.get::<String,_>("decision"),
                "reason": r.get::<Option<String>,_>("reason"),
                "purpose_of_use": r.get::<Option<String>,_>("purpose_of_use"),
                "break_glass": r.get::<bool,_>("break_glass"),
                "correlation_id": r.get::<Option<Uuid>,_>("correlation_id"),
                "recorded_at": r.get::<chrono::DateTime<chrono::Utc>,_>("recorded_at"),
            })
        })
        .collect();
    Ok(Json(json!({ "events": events })))
}

/// Deterministic escalation of overdue unreviewed results (A0 automation).
pub async fn escalate_overdue(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::JOBS_RUN,
        "job",
        Some(ResourceCtx {
            tenant_id: ctx.tenant_id,
            patient_id: None,
        }),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    let rows = sqlx::query(
        "UPDATE follow_up_tasks SET status = 'overdue', priority = 'urgent'
         WHERE tenant_id = $1 AND status = 'open' AND due_at < now()
         RETURNING id, service_request_id",
    )
    .bind(ctx.tenant_id)
    .fetch_all(&mut *tx)
    .await?;
    for r in &rows {
        audit::emit(
            &mut *tx,
            &ctx,
            "follow_up.overdue",
            &state.cell,
            json!({
                "follow_up_task_id": r.get::<Uuid,_>("id"),
                "service_request_id": r.get::<Uuid,_>("service_request_id")
            }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
    }
    tx.commit().await?;
    Ok(Json(json!({ "escalated": rows.len() })))
}
