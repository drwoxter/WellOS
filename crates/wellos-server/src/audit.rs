//! Append-only audit log and transactional outbox writers.
//!
//! Audit entries and outbox events contain identifiers and metadata only —
//! never clinical values, names, or other PHI-like content.

use crate::auth::AuthContext;
use chrono::Utc;
use serde_json::Value;
use sqlx::{PgExecutor, PgPool};
use uuid::Uuid;

#[allow(clippy::too_many_arguments)]
pub async fn record<'e, E: PgExecutor<'e>>(
    exec: E,
    ctx: &AuthContext,
    action: &str,
    resource_type: Option<&str>,
    resource_id: Option<String>,
    decision: &str,
    reason: Option<&str>,
) -> sqlx::Result<()> {
    sqlx::query(
        "INSERT INTO audit_events
         (id, tenant_id, actor, action, resource_type, resource_id, decision, reason,
          purpose_of_use, break_glass, break_glass_reason, correlation_id)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12)",
    )
    .bind(Uuid::now_v7())
    .bind(ctx.tenant_id)
    .bind(ctx.actor())
    .bind(action)
    .bind(resource_type)
    .bind(resource_id)
    .bind(decision)
    .bind(reason)
    .bind(ctx.purpose_of_use.as_str())
    .bind(ctx.break_glass_reason.is_some())
    .bind(ctx.break_glass_reason.as_deref())
    .bind(ctx.correlation_id)
    .execute(exec)
    .await?;
    Ok(())
}

/// Convenience wrapper: audit a denial on the pool (outside any transaction,
/// so the denial is recorded even when the request is rejected).
pub async fn record_denial(
    pool: &PgPool,
    ctx: &AuthContext,
    action: &str,
    resource_type: Option<&str>,
    resource_id: Option<String>,
    reason: &str,
) -> sqlx::Result<()> {
    record(
        pool,
        ctx,
        action,
        resource_type,
        resource_id,
        "deny",
        Some(reason),
    )
    .await?;
    // Denials are also domain events.
    emit(
        pool,
        ctx,
        "policy.access_denied",
        "cell-dev-1",
        serde_json::json!({ "action": action, "reason": reason }),
        None,
    )
    .await
}

/// Write an event to the transactional outbox. Call inside the same
/// transaction as the state change it describes.
pub async fn emit<'e, E: PgExecutor<'e>>(
    exec: E,
    ctx: &AuthContext,
    event_type: &str,
    cell: &str,
    resource_refs: Value,
    causation_id: Option<Uuid>,
) -> sqlx::Result<()> {
    debug_assert!(
        wellos_domain::events::is_known_event_type(event_type),
        "unknown event type {event_type}"
    );
    sqlx::query(
        "INSERT INTO outbox_events
         (id, event_type, schema_version, tenant_id, cell, actor, correlation_id,
          causation_id, occurred_at, source, resource_refs)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11)",
    )
    .bind(Uuid::now_v7())
    .bind(event_type)
    .bind("1.0")
    .bind(ctx.tenant_id)
    .bind(cell)
    .bind(ctx.actor())
    .bind(ctx.correlation_id)
    .bind(causation_id)
    .bind(Utc::now())
    .bind("wellos-server")
    .bind(resource_refs)
    .execute(exec)
    .await?;
    Ok(())
}
