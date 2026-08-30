//! Service-credential administration (privacy/security roles only).
//!
//! Secrets are random and high-entropy; only a one-way hash is stored, and
//! the plaintext is returned exactly once, at issuance or rotation. Scopes,
//! tenant, and the machine principal are resolved server-side, and every
//! operation is audited. No endpoint can return an existing secret.

use crate::audit;
use crate::auth::{generate_secret, hash_service_secret, AuthContext};
use crate::error::ApiError;
use crate::policy::{actions, role_allows, ResourceCtx};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::{Path, State};
use axum::Json;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

const MAX_EXPIRY_SECS: i64 = 366 * 24 * 3600;

fn tenant_resource(ctx: &AuthContext) -> Option<ResourceCtx> {
    Some(ResourceCtx {
        tenant_id: ctx.tenant_id,
        patient_id: None,
    })
}

/// Scopes may never exceed what the service principal's current roles
/// allow, so a credential cannot hold latent permissions that a later role
/// grant would silently activate.
async fn require_scopes_within_roles(
    pool: &sqlx::PgPool,
    tenant_id: Uuid,
    service_user_id: Uuid,
    scopes: &[String],
) -> Result<(), ApiError> {
    let roles: Vec<(String,)> =
        sqlx::query_as("SELECT role FROM role_assignments WHERE user_id = $1 AND tenant_id = $2")
            .bind(service_user_id)
            .bind(tenant_id)
            .fetch_all(pool)
            .await?;
    for scope in scopes {
        if !roles.iter().any(|(role,)| role_allows(role, scope)) {
            return Err(ApiError::bad_request(
                "scope_exceeds_role",
                format!("scope '{scope}' is not permitted by the service principal's roles"),
            ));
        }
    }
    Ok(())
}

#[derive(serde::Deserialize)]
pub struct IssueCredential {
    pub service_username: String,
    pub name: String,
    pub scopes: Vec<String>,
    pub expires_in_secs: Option<i64>,
}

/// Issue a credential for an existing machine principal in the caller's
/// tenant. The plaintext secret appears only in this response.
pub async fn issue(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<IssueCredential>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::SERVICE_CREDENTIAL_MANAGE,
        "service_credential",
        tenant_resource(&ctx),
    )
    .await?;
    let name = body.name.trim();
    if name.is_empty() || name.len() > 200 {
        return Err(ApiError::bad_request(
            "validation_failed",
            "name is required and must be at most 200 characters",
        ));
    }
    if body.scopes.is_empty()
        || body.scopes.len() > 20
        || body.scopes.iter().any(|s| !actions::is_known_action(s))
    {
        return Err(ApiError::bad_request(
            "validation_failed",
            "scopes must be a non-empty list of known actions",
        ));
    }
    let expires_at = match body.expires_in_secs {
        Some(secs) if (60..=MAX_EXPIRY_SECS).contains(&secs) => {
            Some(chrono::Utc::now() + chrono::Duration::seconds(secs))
        }
        Some(_) => {
            return Err(ApiError::bad_request(
                "validation_failed",
                "expires_in_secs must be between 60 seconds and one year",
            ))
        }
        None => None,
    };
    // Machine principal resolved server-side, within the caller's tenant.
    let user: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM users WHERE tenant_id = $1 AND username = $2 AND is_service = true",
    )
    .bind(ctx.tenant_id)
    .bind(body.service_username.trim())
    .fetch_optional(&state.pool)
    .await?;
    let (service_user_id,) = user.ok_or_else(ApiError::not_found)?;
    require_scopes_within_roles(&state.pool, ctx.tenant_id, service_user_id, &body.scopes).await?;
    let secret = generate_secret("wsk_");
    let cred_id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    sqlx::query(
        "INSERT INTO service_credentials (id, tenant_id, user_id, name, token_hash, scopes, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(cred_id)
    .bind(ctx.tenant_id)
    .bind(service_user_id)
    .bind(name)
    .bind(hash_service_secret(&secret))
    .bind(&body.scopes)
    .bind(expires_at)
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        &ctx,
        "service_credential.issue",
        Some("service_credential"),
        Some(cred_id.to_string()),
        "allow",
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({
        "id": cred_id,
        "name": name,
        "scopes": body.scopes,
        "expires_at": expires_at,
        "secret": secret,
    })))
}

/// Metadata listing: never includes hashes or secrets.
pub async fn list(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Value>, ApiError> {
    guard(
        &state,
        &ctx,
        actions::SERVICE_CREDENTIAL_READ,
        "service_credential",
        tenant_resource(&ctx),
    )
    .await?
    .record_on_pool(&state, &ctx)
    .await?;
    let rows = sqlx::query(
        "SELECT sc.id, sc.name, sc.scopes, sc.created_at, sc.expires_at,
                sc.revoked_at, sc.last_used_at, u.username AS service_username
         FROM service_credentials sc JOIN users u ON u.id = sc.user_id
         WHERE sc.tenant_id = $1 ORDER BY sc.created_at DESC LIMIT 500",
    )
    .bind(ctx.tenant_id)
    .fetch_all(&state.pool)
    .await?;
    let credentials: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid,_>("id"),
                "name": r.get::<String,_>("name"),
                "service_username": r.get::<String,_>("service_username"),
                "scopes": r.get::<Vec<String>,_>("scopes"),
                "created_at": r.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
                "expires_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("expires_at"),
                "revoked_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("revoked_at"),
                "last_used_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("last_used_at"),
            })
        })
        .collect();
    Ok(Json(json!({ "credentials": credentials })))
}

/// Rotate: revoke the old credential and issue a new secret for the same
/// principal, name, scopes, and expiry. The new plaintext appears only here.
pub async fn rotate(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::SERVICE_CREDENTIAL_MANAGE,
        "service_credential",
        tenant_resource(&ctx),
    )
    .await?;
    let secret = generate_secret("wsk_");
    let new_id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    // Same-tenant lookup; unknown or cross-tenant IDs get the same 404.
    let row = sqlx::query(
        "UPDATE service_credentials SET revoked_at = now()
         WHERE id = $1 AND tenant_id = $2 AND revoked_at IS NULL
         RETURNING user_id, name, scopes, expires_at",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or_else(ApiError::not_found)?;
    require_scopes_within_roles(
        &state.pool,
        ctx.tenant_id,
        row.get::<Uuid, _>("user_id"),
        &row.get::<Vec<String>, _>("scopes"),
    )
    .await?;
    sqlx::query(
        "INSERT INTO service_credentials (id, tenant_id, user_id, name, token_hash, scopes, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(new_id)
    .bind(ctx.tenant_id)
    .bind(row.get::<Uuid, _>("user_id"))
    .bind(row.get::<String, _>("name"))
    .bind(hash_service_secret(&secret))
    .bind(row.get::<Vec<String>, _>("scopes"))
    .bind(row.get::<Option<chrono::DateTime<chrono::Utc>>, _>("expires_at"))
    .execute(&mut *tx)
    .await?;
    audit::record(
        &mut *tx,
        &ctx,
        "service_credential.rotate",
        Some("service_credential"),
        Some(new_id.to_string()),
        "allow",
        Some(&format!("rotated_from:{id}")),
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({
        "id": new_id,
        "rotated_from": id,
        "secret": secret,
    })))
}

/// Revoke a credential immediately.
pub async fn revoke(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::SERVICE_CREDENTIAL_MANAGE,
        "service_credential",
        tenant_resource(&ctx),
    )
    .await?;
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    let updated = sqlx::query(
        "UPDATE service_credentials SET revoked_at = now()
         WHERE id = $1 AND tenant_id = $2 AND revoked_at IS NULL",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::not_found());
    }
    audit::record(
        &mut *tx,
        &ctx,
        "service_credential.revoke",
        Some("service_credential"),
        Some(id.to_string()),
        "allow",
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    tx.commit().await?;
    Ok(Json(json!({ "id": id, "revoked": true })))
}
