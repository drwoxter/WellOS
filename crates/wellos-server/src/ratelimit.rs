//! Shared PostgreSQL-backed rate limiting.
//!
//! Fixed windows are incremented with a single atomic upsert, so concurrent
//! requests cannot bypass a limit and the counters are shared across API
//! replicas. Keys never contain raw client addresses, credentials, or
//! clinical data — only tenant/user identifiers and one-way hashes. Because
//! the limiter lives in the same PostgreSQL instance as the rest of the
//! system, an unavailable store surfaces as an internal error and the
//! request fails closed (nothing is served without a successful check).

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::state::AppState;
use axum::http::HeaderMap;
use sha2::{Digest, Sha256};
use sqlx::Row;

/// Endpoint families with independent limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Family {
    /// Anonymous OIDC login initiation and callback (per client address).
    Login,
    /// Patient search (stricter than ordinary reads).
    PatientSearch,
    /// Service-credential administration.
    CredentialAdmin,
    /// General authenticated API traffic.
    Api,
}

impl Family {
    fn as_str(self) -> &'static str {
        match self {
            Self::Login => "login",
            Self::PatientSearch => "patient_search",
            Self::CredentialAdmin => "credential_admin",
            Self::Api => "api",
        }
    }
}

/// Per-minute limits, resolved once at startup.
#[derive(Clone, Debug)]
pub struct RateConfig {
    pub login_per_min: i64,
    pub search_per_min: i64,
    pub cred_admin_per_min: i64,
    pub api_per_min: i64,
    /// Whether `x-forwarded-for` may be trusted for the client-address key
    /// (set only when a trusted reverse proxy sets it).
    pub trusted_proxy: bool,
}

impl RateConfig {
    pub fn limit(&self, family: Family) -> i64 {
        match family {
            Family::Login => self.login_per_min,
            Family::PatientSearch => self.search_per_min,
            Family::CredentialAdmin => self.cred_admin_per_min,
            Family::Api => self.api_per_min,
        }
    }
}

const WINDOW_SECS: f64 = 60.0;

/// Atomically count this request against `key`'s current window. Returns
/// `Ok(())` when within the limit, or `Err(retry_after_secs)` when exhausted.
async fn count(pool: &sqlx::PgPool, key: &str, limit: i64) -> Result<Result<(), u64>, sqlx::Error> {
    let row = sqlx::query(
        "INSERT INTO rate_limit_windows (key, window_start, count)
         VALUES ($1, to_timestamp(floor(extract(epoch FROM now()) / $2) * $2), 1)
         ON CONFLICT (key, window_start)
         DO UPDATE SET count = rate_limit_windows.count + 1
         RETURNING count,
                   GREATEST(extract(epoch FROM window_start
                            + make_interval(secs => $2) - now()), 0)::float8 AS remaining",
    )
    .bind(key)
    .bind(WINDOW_SECS)
    .fetch_one(pool)
    .await?;
    let n: i64 = row.get("count");
    if n <= limit {
        Ok(Ok(()))
    } else {
        let remaining: f64 = row.try_get("remaining").unwrap_or(WINDOW_SECS);
        Ok(Err(remaining.ceil().max(1.0) as u64))
    }
}

/// Enforce a per-principal limit for authenticated traffic. Denials are
/// audited (without secrets or clinical payloads) and returned as HTTP 429
/// with `Retry-After`.
pub async fn enforce_for_principal(
    state: &AppState,
    ctx: &AuthContext,
    family: Family,
) -> Result<(), ApiError> {
    let limit = state.auth.rate.limit(family);
    let key = format!("t:{}:u:{}:{}", ctx.tenant_id, ctx.user_id, family.as_str());
    match count(&state.pool, &key, limit).await? {
        Ok(()) => Ok(()),
        Err(retry_after) => {
            audit::record_denial(
                &state.pool,
                ctx,
                &format!("rate_limit.{}", family.as_str()),
                None,
                None,
                "rate_limited",
            )
            .await
            .map_err(ApiError::internal)?;
            Err(ApiError::too_many_requests(retry_after))
        }
    }
}

/// Enforce the anonymous login/callback limit keyed by a one-way hash of the
/// trusted client address. `x-forwarded-for` is honored only when an explicit
/// trusted-proxy deployment is configured; otherwise the socket peer address
/// is used. Raw addresses are never stored or logged.
pub async fn enforce_for_client(
    state: &AppState,
    headers: &HeaderMap,
    peer: Option<std::net::SocketAddr>,
) -> Result<(), ApiError> {
    let address = if state.auth.rate.trusted_proxy {
        headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(',').next())
            .map(|s| s.trim().to_string())
            .or_else(|| peer.map(|p| p.ip().to_string()))
    } else {
        peer.map(|p| p.ip().to_string())
    };
    // Without any client address (e.g. in-process testing), fall back to one
    // shared bucket: anonymous endpoints stay rate-limited rather than open.
    let address = address.unwrap_or_else(|| "unknown".to_string());
    let key = format!(
        "c:{}:login",
        hex::encode(Sha256::digest(address.as_bytes()))
    );
    match count(&state.pool, &key, state.auth.rate.limit(Family::Login)).await? {
        Ok(()) => Ok(()),
        Err(retry_after) => {
            // Anonymous denials have no tenant/principal for the audit table;
            // they are surfaced as structured logs with only the hashed key.
            tracing::warn!(family = "login", "anonymous rate limit denial");
            Err(ApiError::too_many_requests(retry_after))
        }
    }
}
