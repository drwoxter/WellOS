//! Opaque server-side browser sessions for the web BFF.
//!
//! The BFF exchanges a validated bearer credential for a random `wss_`
//! session identifier plus a CSRF secret; only hashes are stored. The
//! session identifier lives in an HttpOnly cookie; the access credential is
//! never stored in the browser. Sessions carry an absolute expiration, an
//! inactivity timeout (enforced in the authentication path), logout
//! revocation, and rotation (fixation protection).

use crate::audit;
use crate::auth::{generate_secret, hash_service_secret, AuthContext};
use crate::error::ApiError;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use serde_json::{json, Value};
use uuid::Uuid;

async fn insert_session(
    state: &AppState,
    ctx: &AuthContext,
) -> Result<(Uuid, String, String, chrono::DateTime<chrono::Utc>), ApiError> {
    let session_id = Uuid::now_v7();
    let token = generate_secret("wss_");
    let csrf = generate_secret("wsc_");
    let expires_at =
        chrono::Utc::now() + chrono::Duration::seconds(state.auth.session_absolute_secs);
    sqlx::query(
        "INSERT INTO web_sessions (id, tenant_id, user_id, token_hash, csrf_hash, expires_at)
         VALUES ($1, $2, $3, $4, $5, $6)",
    )
    .bind(session_id)
    .bind(ctx.tenant_id)
    .bind(ctx.user_id)
    .bind(hash_service_secret(&token))
    .bind(hash_service_secret(&csrf))
    .bind(expires_at)
    .execute(&state.pool)
    .await?;
    Ok((session_id, token, csrf, expires_at))
}

/// Exchange a validated non-session credential (dev token or OIDC JWT) for a
/// fresh opaque session. Sessions cannot mint further sessions, and machine
/// principals never get browser sessions.
pub async fn create(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Value>, ApiError> {
    if ctx.is_service || ctx.web_session_id.is_some() {
        return Err(ApiError::forbidden(
            "browser sessions are issued to human credentials only".to_string(),
        ));
    }
    let (session_id, token, csrf, expires_at) = insert_session(&state, &ctx).await?;
    audit::record(
        &state.pool,
        &ctx,
        "session.create",
        Some("web_session"),
        Some(session_id.to_string()),
        "allow",
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "session_token": token,
        "csrf_token": csrf,
        "expires_at": expires_at,
    })))
}

/// Validate the current session (the authentication path already enforced
/// hash match, revocation, absolute expiry, and inactivity).
pub async fn get(State(state): State<AppState>, ctx: AuthContext) -> Result<Json<Value>, ApiError> {
    let Some(session_id) = ctx.web_session_id else {
        return Err(ApiError::unauthorized());
    };
    let (expires_at,): (chrono::DateTime<chrono::Utc>,) =
        sqlx::query_as("SELECT expires_at FROM web_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_one(&state.pool)
            .await?;
    Ok(Json(json!({
        "authenticated": true,
        "username": ctx.username,
        "display_name": ctx.display_name,
        "expires_at": expires_at,
    })))
}

/// Rotate the session identifier and CSRF secret (fixation protection):
/// revoke the current session and issue a fresh one atomically enough that
/// the old identifier can never outlive the response.
pub async fn rotate(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Value>, ApiError> {
    let Some(session_id) = ctx.web_session_id else {
        return Err(ApiError::unauthorized());
    };
    sqlx::query("UPDATE web_sessions SET revoked_at = now() WHERE id = $1")
        .bind(session_id)
        .execute(&state.pool)
        .await?;
    let (new_id, token, csrf, expires_at) = insert_session(&state, &ctx).await?;
    audit::record(
        &state.pool,
        &ctx,
        "session.rotate",
        Some("web_session"),
        Some(new_id.to_string()),
        "allow",
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(Json(json!({
        "session_token": token,
        "csrf_token": csrf,
        "expires_at": expires_at,
    })))
}

/// Logout: revoke the current session server-side.
pub async fn delete(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Value>, ApiError> {
    let Some(session_id) = ctx.web_session_id else {
        return Err(ApiError::unauthorized());
    };
    sqlx::query("UPDATE web_sessions SET revoked_at = now() WHERE id = $1")
        .bind(session_id)
        .execute(&state.pool)
        .await?;
    audit::record(
        &state.pool,
        &ctx,
        "session.revoke",
        Some("web_session"),
        Some(session_id.to_string()),
        "allow",
        None,
    )
    .await
    .map_err(ApiError::internal)?;
    Ok(Json(json!({ "authenticated": false })))
}
