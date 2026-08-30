//! Browser OIDC login: Authorization Code + PKCE (S256) through the BFF.
//!
//! `POST /api/v1/auth/oidc/login` creates a single-use, short-lived,
//! server-side login transaction (state hash, nonce hash, PKCE verifier) and
//! returns the provider authorization URL. `POST /api/v1/auth/oidc/callback`
//! atomically claims the transaction by state, exchanges the code
//! server-side at the discovery-validated token endpoint, validates the
//! returned identity token through the existing OIDC boundary (signature,
//! issuer, audience, expiry, MFA policy, nonce binding), resolves the local
//! user from PostgreSQL by `(issuer, subject)`, and issues only the opaque
//! `wss_` session and `wsc_` CSRF secret. Provider tokens never appear in
//! URLs, cookies, logs, audit payloads, or responses. Both endpoints are
//! anonymous and rate-limited by a hashed client-address key.

use crate::auth::{generate_secret, hash_service_secret, resolve_oidc_user, validate_oidc_token};
use crate::error::ApiError;
use crate::oidc::{JwksKeys, OidcEndpoints};
use crate::ratelimit;
use crate::routes::session::insert_session;
use crate::state::{AppState, OidcConfig, OidcLoginConfig};
use axum::extract::{ConnectInfo, State};
use axum::http::HeaderMap;
use axum::Json;
use base64::Engine;
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::net::SocketAddr;
use uuid::Uuid;

fn login_unavailable() -> ApiError {
    ApiError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "oidc_login_not_configured",
        "browser OIDC login is not configured",
    )
}

/// The generic callback failure: one bounded, non-enumerating error for
/// state mismatch, replay, expiry, PKCE/nonce mismatch, provider errors,
/// and exchange failures. Details are never echoed back to the browser.
fn login_failed() -> ApiError {
    ApiError::unauthorized()
}

fn login_config(state: &AppState) -> Result<(&OidcConfig, &OidcLoginConfig), ApiError> {
    let cfg = state.auth.oidc.as_ref().ok_or_else(login_unavailable)?;
    let login = cfg.login.as_ref().ok_or_else(login_unavailable)?;
    Ok((cfg, login))
}

async fn discovered_endpoints(cfg: &OidcConfig) -> Result<OidcEndpoints, ApiError> {
    match &cfg.keys {
        JwksKeys::Remote(remote) => remote.endpoints().await.ok_or_else(login_unavailable),
        JwksKeys::Static(_) => Err(login_unavailable()),
    }
}

/// PKCE S256 challenge: BASE64URL(SHA256(verifier)), no padding.
fn code_challenge_s256(verifier: &str) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
}

/// Begin a browser login: mint state/nonce/verifier, persist the server-side
/// transaction, and return the provider authorization URL. The raw verifier
/// and nonce never reach the browser; the state value is opaque and single-use.
pub async fn start(
    State(state): State<AppState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
) -> Result<Json<Value>, ApiError> {
    ratelimit::enforce_for_client(&state, &headers, peer.map(|p| p.0)).await?;
    let (cfg, login) = login_config(&state)?;
    let endpoints = discovered_endpoints(cfg).await?;

    // Opportunistic retention cleanup: expired login transactions and stale
    // rate-limit windows carry no long-term value and hold no secrets worth
    // keeping (verifiers are useless once the transaction expires).
    sqlx::query("DELETE FROM login_transactions WHERE expires_at < now() - interval '1 hour'")
        .execute(&state.pool)
        .await?;
    sqlx::query("DELETE FROM rate_limit_windows WHERE window_start < now() - interval '1 hour'")
        .execute(&state.pool)
        .await?;

    let oauth_state = generate_secret("wst_");
    let nonce = generate_secret("wsn_");
    let verifier = generate_secret("");
    let expires_at = chrono::Utc::now() + chrono::Duration::seconds(login.login_txn_secs);
    sqlx::query(
        "INSERT INTO login_transactions (id, state_hash, nonce_hash, code_verifier, expires_at)
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(Uuid::now_v7())
    .bind(hash_service_secret(&oauth_state))
    .bind(hash_service_secret(&nonce))
    .bind(&verifier)
    .bind(expires_at)
    .execute(&state.pool)
    .await?;

    let mut authorize_url = url::Url::parse(&endpoints.authorization_endpoint)
        .map_err(|_| ApiError::internal("invalid authorization endpoint"))?;
    authorize_url
        .query_pairs_mut()
        .append_pair("response_type", "code")
        .append_pair("client_id", &login.client_id)
        .append_pair("redirect_uri", &login.redirect_uri)
        .append_pair("scope", "openid")
        .append_pair("state", &oauth_state)
        .append_pair("nonce", &nonce)
        .append_pair("code_challenge", &code_challenge_s256(&verifier))
        .append_pair("code_challenge_method", "S256");
    // Only the provider authorization URL is returned; the state round-trips
    // through the provider and is verified against the stored hash.
    Ok(Json(json!({ "authorize_url": authorize_url.to_string() })))
}

#[derive(Deserialize)]
pub struct CallbackBody {
    pub code: Option<String>,
    pub state: Option<String>,
    /// Provider error code, when the provider redirected back with an error.
    pub error: Option<String>,
}

#[derive(Deserialize)]
struct TokenResponse {
    id_token: Option<String>,
}

/// Complete a browser login. The transaction is claimed atomically (single
/// use), the code is exchanged server-side, and the identity token passes the
/// full existing validation boundary plus the nonce binding before any local
/// session is issued.
pub async fn callback(
    State(state): State<AppState>,
    peer: Option<ConnectInfo<SocketAddr>>,
    headers: HeaderMap,
    Json(body): Json<CallbackBody>,
) -> Result<Json<Value>, ApiError> {
    ratelimit::enforce_for_client(&state, &headers, peer.map(|p| p.0)).await?;
    let (cfg, login) = login_config(&state)?;
    let endpoints = discovered_endpoints(cfg).await?;

    let (Some(code), Some(oauth_state)) = (&body.code, &body.state) else {
        // Provider error responses and malformed callbacks fail identically;
        // the provider error code is not clinical data but is still not echoed.
        let _ = &body.error;
        return Err(login_failed());
    };
    if code.is_empty() || code.len() > 4096 || oauth_state.len() > 256 {
        return Err(login_failed());
    }

    // Atomic single-use claim: replayed, expired, or unknown states all fail
    // the same way, and concurrent replays cannot both claim the row.
    let txn: Option<(String, String)> = sqlx::query_as(
        "UPDATE login_transactions SET used_at = now()
         WHERE state_hash = $1 AND used_at IS NULL AND expires_at > now()
         RETURNING nonce_hash, code_verifier",
    )
    .bind(hash_service_secret(oauth_state))
    .fetch_optional(&state.pool)
    .await?;
    let (nonce_hash, code_verifier) = txn.ok_or_else(login_failed)?;

    // Server-side code exchange at the discovery-validated token endpoint.
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| ApiError::internal("http client"))?;
    let mut form: Vec<(&str, &str)> = vec![
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", &login.redirect_uri),
        ("client_id", &login.client_id),
        ("code_verifier", &code_verifier),
    ];
    if let Some(secret) = &login.client_secret {
        form.push(("client_secret", secret));
    }
    let response = client
        .post(&endpoints.token_endpoint)
        .form(&form)
        .send()
        .await
        .map_err(|_| login_failed())?;
    if !response.status().is_success() {
        return Err(login_failed());
    }
    let tokens: TokenResponse = response.json().await.map_err(|_| login_failed())?;
    let id_token = tokens.id_token.ok_or_else(login_failed)?;

    // Full existing validation boundary: signature/issuer/audience/expiry/
    // MFA policy — then the nonce binding against the stored hash.
    let claims = validate_oidc_token(cfg, &id_token).await?;
    let nonce_ok = claims
        .nonce
        .as_deref()
        .map(|n| hash_service_secret(n) == nonce_hash)
        .unwrap_or(false);
    if !nonce_ok {
        return Err(login_failed());
    }

    // Local identity only: tenant/roles/permissions come from PostgreSQL.
    let principal = resolve_oidc_user(&state, cfg, &claims.sub).await?;
    if principal.is_service {
        return Err(login_failed());
    }

    let mut tx = state.pool.begin().await?;
    let (session_id, token, csrf, expires_at) =
        insert_session(&mut tx, &state, principal.tenant_id, principal.user_id).await?;
    // Audit the login without any token material.
    sqlx::query(
        "INSERT INTO audit_events
         (id, tenant_id, actor, action, resource_type, resource_id,
          decision, reason, purpose_of_use, correlation_id)
         VALUES ($1, $2, $3, 'session.create', 'web_session', $4, 'allow',
                 'oidc_login', 'treatment', $5)",
    )
    .bind(Uuid::now_v7())
    .bind(principal.tenant_id)
    .bind(format!("user:{}", principal.username))
    .bind(session_id.to_string())
    .bind(Uuid::now_v7())
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(Json(json!({
        "session_token": token,
        "csrf_token": csrf,
        "expires_at": expires_at,
    })))
}
