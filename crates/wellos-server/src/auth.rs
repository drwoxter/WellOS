//! Provider-neutral authentication boundary.
//!
//! Three credential types are recognized, dispatched by shape:
//!
//! - `dev-<username>`: predictable development tokens for seeded synthetic
//!   humans. Accepted only when explicitly enabled for local development
//!   (`WELLOS_ENV=development` and `WELLOS_DEV_AUTH=true`); production
//!   startup fails closed if they are enabled.
//! - `wsk_<secret>`: opaque high-entropy service credentials for machine
//!   principals. Only a one-way SHA-256 hash is stored; credentials carry
//!   explicit scopes, expiration, and revocation, and never authenticate
//!   human users.
//! - OIDC/OAuth 2.1 JWTs: validated against the configured JWKS, issuer,
//!   audience, expiration, not-before, and issued-at (with bounded clock
//!   skew). Only the stable `sub` is trusted: tenant, roles, and permissions
//!   are resolved from the local database, never from client claims.
//!
//! The rest of the system depends only on [`AuthContext`], so identity
//! providers can be swapped without touching domain code.

use crate::error::ApiError;
use crate::policy::Purpose;
use crate::state::{identity_provider_not_configured, AppState, OidcConfig};
use axum::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub is_service: bool,
    pub roles: Vec<String>,
    /// Scopes granted to a service credential (empty for humans).
    pub scopes: Vec<String>,
    /// Purpose of use asserted by the caller (default: treatment).
    pub purpose_of_use: Purpose,
    /// Break-glass reason, if the caller activated break-glass access.
    pub break_glass_reason: Option<String>,
    pub correlation_id: Uuid,
}

impl AuthContext {
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }

    pub fn has_scope(&self, scope: &str) -> bool {
        self.scopes.iter().any(|s| s == scope)
    }

    pub fn actor(&self) -> String {
        format!("user:{}", self.username)
    }
}

/// `X-Purpose-Of-Use` is caller-asserted context, restricted to a closed
/// vocabulary; the policy layer enforces which purposes permit which actions.
fn validate_purpose(purpose: Option<String>) -> Result<Purpose, ApiError> {
    match purpose {
        None => Ok(Purpose::Treatment),
        Some(p) => Purpose::parse(&p).ok_or_else(|| {
            ApiError::bad_request(
                "invalid_purpose_of_use",
                "x-purpose-of-use must be one of: treatment, operations, emergency, quality",
            )
        }),
    }
}

pub fn hash_service_secret(token: &str) -> String {
    hex::encode(Sha256::digest(token.as_bytes()))
}

struct Principal {
    user_id: Uuid,
    tenant_id: Uuid,
    username: String,
    display_name: String,
    is_service: bool,
    scopes: Vec<String>,
}

/// Development human tokens: local development only, humans only.
async fn dev_principal(state: &AppState, username: &str) -> Result<Principal, ApiError> {
    if !state.auth.dev_auth_enabled {
        return Err(ApiError::unauthorized());
    }
    let row: Option<(Uuid, Uuid, String, String, bool)> = sqlx::query_as(
        "SELECT id, tenant_id, username, display_name, is_service
         FROM users WHERE username = $1",
    )
    .bind(username)
    .fetch_optional(&state.pool)
    .await?;
    let (user_id, tenant_id, username, display_name, is_service) =
        row.ok_or_else(ApiError::unauthorized)?;
    // Dev tokens never authenticate machine principals.
    if is_service {
        return Err(ApiError::unauthorized());
    }
    Ok(Principal {
        user_id,
        tenant_id,
        username,
        display_name,
        is_service: false,
        scopes: Vec::new(),
    })
}

/// A service-credential row joined with its machine user.
type ServiceCredentialRow = (
    Uuid,
    Vec<String>,
    Option<chrono::DateTime<chrono::Utc>>,
    Option<chrono::DateTime<chrono::Utc>>,
    Uuid,
    Uuid,
    String,
    String,
    bool,
);

/// Service credentials: hashed lookup with expiry, revocation, and scopes.
async fn service_principal(state: &AppState, token: &str) -> Result<Principal, ApiError> {
    let hash = hash_service_secret(token);
    let row: Option<ServiceCredentialRow> = sqlx::query_as(
        "SELECT sc.id, sc.scopes, sc.expires_at, sc.revoked_at,
                u.id, u.tenant_id, u.username, u.display_name, u.is_service
         FROM service_credentials sc
         JOIN users u ON u.id = sc.user_id
         WHERE sc.token_hash = $1",
    )
    .bind(&hash)
    .fetch_optional(&state.pool)
    .await?;
    let (
        cred_id,
        scopes,
        expires_at,
        revoked_at,
        user_id,
        tenant_id,
        username,
        display_name,
        is_service,
    ) = row.ok_or_else(ApiError::unauthorized)?;
    // Service credentials never authenticate human users.
    if !is_service {
        return Err(ApiError::unauthorized());
    }
    if revoked_at.is_some() {
        return Err(ApiError::unauthorized());
    }
    if let Some(exp) = expires_at {
        if exp <= chrono::Utc::now() {
            return Err(ApiError::unauthorized());
        }
    }
    // Best-effort usage metadata; failures never block authentication.
    let _ = sqlx::query("UPDATE service_credentials SET last_used_at = now() WHERE id = $1")
        .bind(cred_id)
        .execute(&state.pool)
        .await;
    Ok(Principal {
        user_id,
        tenant_id,
        username,
        display_name,
        is_service: true,
        scopes,
    })
}

#[derive(Deserialize)]
struct OidcClaims {
    sub: String,
    iat: Option<u64>,
}

/// Asymmetric signature algorithms accepted for OIDC tokens. Symmetric and
/// unsigned algorithms are never accepted.
const OIDC_ALGORITHMS: &[Algorithm] = &[
    Algorithm::RS256,
    Algorithm::RS384,
    Algorithm::RS512,
    Algorithm::ES256,
    Algorithm::ES384,
    Algorithm::EdDSA,
];

/// OIDC bearer tokens: signature via configured JWKS, then subject mapping.
async fn oidc_principal(state: &AppState, token: &str) -> Result<Principal, ApiError> {
    let Some(cfg) = &state.auth.oidc else {
        return Err(identity_provider_not_configured());
    };
    let claims = validate_oidc_token(cfg, token)?;
    let row: Option<(Uuid, Uuid, String, String, bool)> = sqlx::query_as(
        "SELECT id, tenant_id, username, display_name, is_service
         FROM users WHERE oidc_subject = $1",
    )
    .bind(&claims.sub)
    .fetch_optional(&state.pool)
    .await?;
    let (user_id, tenant_id, username, display_name, is_service) =
        row.ok_or_else(ApiError::unauthorized)?;
    // OIDC subjects are human identities; machine principals use service
    // credentials.
    if is_service {
        return Err(ApiError::unauthorized());
    }
    Ok(Principal {
        user_id,
        tenant_id,
        username,
        display_name,
        is_service: false,
        scopes: Vec::new(),
    })
}

fn validate_oidc_token(cfg: &OidcConfig, token: &str) -> Result<OidcClaims, ApiError> {
    let header = decode_header(token).map_err(|_| ApiError::unauthorized())?;
    if !OIDC_ALGORITHMS.contains(&header.alg) {
        return Err(ApiError::unauthorized());
    }
    // Select the signing key by `kid` when present, otherwise the single
    // configured key.
    let jwk = match &header.kid {
        Some(kid) => cfg.jwks.find(kid),
        None if cfg.jwks.keys.len() == 1 => cfg.jwks.keys.first(),
        None => None,
    }
    .ok_or_else(ApiError::unauthorized)?;
    let key = DecodingKey::from_jwk(jwk).map_err(|_| ApiError::unauthorized())?;
    let mut validation = Validation::new(header.alg);
    validation.set_issuer(&[&cfg.issuer]);
    validation.set_audience(&[&cfg.audience]);
    validation.set_required_spec_claims(&["exp", "iss", "aud", "sub"]);
    validation.validate_nbf = true;
    validation.leeway = cfg.leeway_secs;
    let data =
        decode::<OidcClaims>(token, &key, &validation).map_err(|_| ApiError::unauthorized())?;
    // `iat` in the future (beyond clock skew) means a malformed or spoofed
    // token; jsonwebtoken does not check this itself.
    if let Some(iat) = data.claims.iat {
        let now = chrono::Utc::now().timestamp().max(0) as u64;
        if iat > now + cfg.leeway_secs {
            return Err(ApiError::unauthorized());
        }
    }
    Ok(data.claims)
}

pub async fn load_auth(
    state: &AppState,
    token: &str,
    purpose_of_use: Option<String>,
    break_glass_reason: Option<String>,
) -> Result<AuthContext, ApiError> {
    let principal = if let Some(username) = token.strip_prefix("dev-") {
        dev_principal(state, username).await?
    } else if token.starts_with("wsk_") {
        service_principal(state, token).await?
    } else if token.split('.').count() == 3 {
        oidc_principal(state, token).await?
    } else {
        return Err(ApiError::unauthorized());
    };
    let roles: Vec<(String,)> =
        sqlx::query_as("SELECT role FROM role_assignments WHERE user_id = $1")
            .bind(principal.user_id)
            .fetch_all(&state.pool)
            .await?;
    Ok(AuthContext {
        user_id: principal.user_id,
        tenant_id: principal.tenant_id,
        username: principal.username,
        display_name: principal.display_name,
        is_service: principal.is_service,
        roles: roles.into_iter().map(|(r,)| r).collect(),
        scopes: principal.scopes,
        purpose_of_use: validate_purpose(purpose_of_use)?,
        break_glass_reason,
        correlation_id: Uuid::now_v7(),
    })
}

#[async_trait]
impl FromRequestParts<AppState> for AuthContext {
    type Rejection = ApiError;

    async fn from_request_parts(
        parts: &mut Parts,
        state: &AppState,
    ) -> Result<Self, Self::Rejection> {
        let header = parts
            .headers
            .get("authorization")
            .and_then(|v| v.to_str().ok())
            .ok_or_else(ApiError::unauthorized)?;
        let token = header
            .strip_prefix("Bearer ")
            .ok_or_else(ApiError::unauthorized)?;
        let purpose = parts
            .headers
            .get("x-purpose-of-use")
            .and_then(|v| v.to_str().ok())
            .map(|s| s.to_string());
        let break_glass = parts
            .headers
            .get("x-break-glass-reason")
            .and_then(|v| v.to_str().ok())
            .filter(|s| !s.trim().is_empty())
            .map(|s| s.to_string());
        load_auth(state, token, purpose, break_glass).await
    }
}
