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
//! - `wss_<secret>`: opaque server-side browser sessions issued by
//!   `/api/v1/auth/session`. Only a hash is stored; sessions carry absolute
//!   expiration, inactivity timeout, revocation, and a CSRF secret that
//!   state-changing requests must echo in `x-csrf-token`.
//! - OIDC/OAuth 2.1 JWTs: validated against the configured keys (static
//!   JWKS or discovery-resolved, cached, rotating JWKS), issuer, audience,
//!   expiration, not-before, and issued-at (with bounded clock skew), plus
//!   an optional MFA (`amr`/`acr`) requirement. Only the stable
//!   (issuer, `sub`) pair is trusted: tenant, roles, and permissions are
//!   resolved from the local database, never from client claims.
//!
//! The rest of the system depends only on [`AuthContext`], so identity
//! providers can be swapped without touching domain code.

use crate::error::ApiError;
use crate::policy::Purpose;
use crate::state::{identity_provider_not_configured, AppState, OidcConfig};
use axum::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use axum::http::Method;
use jsonwebtoken::{decode, decode_header, Algorithm, DecodingKey, Validation};
use rand::RngCore;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use uuid::Uuid;

/// A tenant-scoped role assignment. `facility_id = None` grants tenant-wide
/// access only for explicitly allowlisted administrative/emergency roles
/// (see `policy::null_facility_is_tenant_wide`); ordinary clinical roles
/// require explicit facility assignments.
#[derive(Debug, Clone)]
pub struct RoleAssignment {
    pub role: String,
    pub facility_id: Option<Uuid>,
}

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub is_service: bool,
    pub roles: Vec<String>,
    /// Facility-scoped role assignments (source of the derived `roles`).
    pub assignments: Vec<RoleAssignment>,
    /// Scopes granted to a service credential (empty for humans).
    pub scopes: Vec<String>,
    /// Purpose of use asserted by the caller (default: treatment).
    pub purpose_of_use: Purpose,
    /// Break-glass reason, if the caller activated break-glass access.
    pub break_glass_reason: Option<String>,
    /// Set when the caller authenticated with an opaque browser session.
    pub web_session_id: Option<Uuid>,
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

/// Generate an opaque high-entropy secret with the given prefix
/// (`wsk_` for service credentials, `wss_` for browser sessions).
pub fn generate_secret(prefix: &str) -> String {
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    format!("{prefix}{}", hex::encode(bytes))
}

pub(crate) struct Principal {
    pub(crate) user_id: Uuid,
    pub(crate) tenant_id: Uuid,
    pub(crate) username: String,
    pub(crate) display_name: String,
    pub(crate) is_service: bool,
    pub(crate) scopes: Vec<String>,
    pub(crate) web_session_id: Option<Uuid>,
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
        web_session_id: None,
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
        web_session_id: None,
    })
}

/// Opaque browser sessions: hashed lookup with revocation, absolute expiry,
/// and inactivity timeout, all enforced (and activity recorded) atomically.
async fn web_session_principal(state: &AppState, token: &str) -> Result<Principal, ApiError> {
    let hash = hash_service_secret(token);
    let row: Option<(Uuid, Uuid, Uuid)> = sqlx::query_as(
        "UPDATE web_sessions SET last_seen_at = now()
         WHERE token_hash = $1
           AND revoked_at IS NULL
           AND expires_at > now()
           AND last_seen_at > now() - make_interval(secs => $2)
         RETURNING id, tenant_id, user_id",
    )
    .bind(&hash)
    .bind(state.auth.session_idle_secs as f64)
    .fetch_optional(&state.pool)
    .await?;
    let (session_id, tenant_id, user_id) = row.ok_or_else(ApiError::unauthorized)?;
    let user: Option<(String, String, bool)> =
        sqlx::query_as("SELECT username, display_name, is_service FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_optional(&state.pool)
            .await?;
    let (username, display_name, is_service) = user.ok_or_else(ApiError::unauthorized)?;
    // Sessions are issued to humans only.
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
        web_session_id: Some(session_id),
    })
}

#[derive(Deserialize)]
pub(crate) struct OidcClaims {
    pub(crate) sub: String,
    iat: Option<u64>,
    amr: Option<serde_json::Value>,
    acr: Option<serde_json::Value>,
    /// Replay binding for the browser login flow (compared against the
    /// login transaction's stored nonce hash).
    pub(crate) nonce: Option<String>,
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

/// OIDC bearer tokens: signature via configured keys, then provider-aware
/// (issuer, subject) mapping to a local identity. The legacy single-provider
/// `users.oidc_subject` mapping is honored as a fallback and migrated into
/// `user_identities` on first use.
async fn oidc_principal(state: &AppState, token: &str) -> Result<Principal, ApiError> {
    let Some(cfg) = &state.auth.oidc else {
        return Err(identity_provider_not_configured());
    };
    let claims = validate_oidc_token(cfg, token).await?;
    resolve_oidc_user(state, cfg, &claims.sub).await
}

/// Map a validated `(configured issuer, subject)` pair to the local user.
/// Tenant, roles, and permissions come only from PostgreSQL; the legacy
/// single-provider `users.oidc_subject` mapping is honored as a fallback and
/// migrated into `user_identities` on first use.
pub(crate) async fn resolve_oidc_user(
    state: &AppState,
    cfg: &OidcConfig,
    sub: &str,
) -> Result<Principal, ApiError> {
    let claims_sub = sub;
    let row: Option<(Uuid, Uuid, String, String, bool)> = sqlx::query_as(
        "SELECT u.id, u.tenant_id, u.username, u.display_name, u.is_service
         FROM user_identities ui JOIN users u ON u.id = ui.user_id
         WHERE ui.issuer = $1 AND ui.subject = $2",
    )
    .bind(&cfg.issuer)
    .bind(claims_sub)
    .fetch_optional(&state.pool)
    .await?;
    let row = match row {
        Some(row) => Some(row),
        None => {
            let legacy: Option<(Uuid, Uuid, String, String, bool)> = sqlx::query_as(
                "SELECT id, tenant_id, username, display_name, is_service
                 FROM users WHERE oidc_subject = $1",
            )
            .bind(claims_sub)
            .fetch_optional(&state.pool)
            .await?;
            if let Some((user_id, ..)) = &legacy {
                sqlx::query(
                    "INSERT INTO user_identities (id, user_id, issuer, subject)
                     VALUES ($1, $2, $3, $4)
                     ON CONFLICT (issuer, subject) DO NOTHING",
                )
                .bind(Uuid::now_v7())
                .bind(user_id)
                .bind(&cfg.issuer)
                .bind(claims_sub)
                .execute(&state.pool)
                .await?;
            }
            legacy
        }
    };
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
        web_session_id: None,
    })
}

/// MFA policy: an accepted signal must appear in the *validated* `amr`
/// (array of strings) or `acr` (string) claim. Anything else — missing,
/// malformed, or unaccepted values — fails closed. Nothing outside the
/// signed token (headers, roles, email) can satisfy this requirement.
fn mfa_satisfied(cfg: &OidcConfig, claims: &OidcClaims) -> bool {
    match &claims.amr {
        Some(serde_json::Value::Array(items)) => {
            if !items.iter().all(|v| v.is_string()) {
                return false;
            }
            if items
                .iter()
                .filter_map(|v| v.as_str())
                .any(|m| cfg.accepted_amr.iter().any(|a| a == m))
            {
                return true;
            }
        }
        Some(_) => return false,
        None => {}
    }
    match &claims.acr {
        Some(serde_json::Value::String(acr)) => cfg.accepted_acr.iter().any(|a| a == acr),
        _ => false,
    }
}

/// A JWK may only verify tokens whose header algorithm matches the key's
/// declared algorithm (when present), and only keys published for signature
/// verification are accepted.
fn jwk_matches_algorithm(jwk: &jsonwebtoken::jwk::Jwk, alg: Algorithm) -> bool {
    if let Some(u) = &jwk.common.public_key_use {
        if *u != jsonwebtoken::jwk::PublicKeyUse::Signature {
            return false;
        }
    }
    if let Some(jwk_alg) = jwk.common.key_algorithm {
        use jsonwebtoken::jwk::KeyAlgorithm as K;
        return matches!(
            (jwk_alg, alg),
            (K::RS256, Algorithm::RS256)
                | (K::RS384, Algorithm::RS384)
                | (K::RS512, Algorithm::RS512)
                | (K::ES256, Algorithm::ES256)
                | (K::ES384, Algorithm::ES384)
                | (K::EdDSA, Algorithm::EdDSA)
        );
    }
    true
}

pub(crate) async fn validate_oidc_token(
    cfg: &OidcConfig,
    token: &str,
) -> Result<OidcClaims, ApiError> {
    let header = decode_header(token).map_err(|_| ApiError::unauthorized())?;
    if !OIDC_ALGORITHMS.contains(&header.alg) {
        return Err(ApiError::unauthorized());
    }
    let jwk = cfg
        .keys
        .find_key(header.kid.as_deref())
        .await
        .ok_or_else(ApiError::unauthorized)?;
    if !jwk_matches_algorithm(&jwk, header.alg) {
        return Err(ApiError::unauthorized());
    }
    let key = DecodingKey::from_jwk(&jwk).map_err(|_| ApiError::unauthorized())?;
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
    if cfg.require_mfa && !mfa_satisfied(cfg, &data.claims) {
        return Err(ApiError::unauthorized());
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
    } else if token.starts_with("wss_") {
        web_session_principal(state, token).await?
    } else if token.split('.').count() == 3 {
        oidc_principal(state, token).await?
    } else {
        return Err(ApiError::unauthorized());
    };
    // Roles are tenant-scoped: an assignment recorded under another tenant
    // never grants privileges inside the principal's tenant. Facility scope
    // is loaded with each assignment and enforced centrally in policy.
    let assignments: Vec<(String, Option<Uuid>)> = sqlx::query_as(
        "SELECT role, facility_id FROM role_assignments WHERE user_id = $1 AND tenant_id = $2",
    )
    .bind(principal.user_id)
    .bind(principal.tenant_id)
    .fetch_all(&state.pool)
    .await?;
    let assignments: Vec<RoleAssignment> = assignments
        .into_iter()
        .map(|(role, facility_id)| RoleAssignment { role, facility_id })
        .collect();
    let mut roles: Vec<String> = assignments.iter().map(|a| a.role.clone()).collect();
    roles.sort();
    roles.dedup();
    Ok(AuthContext {
        user_id: principal.user_id,
        tenant_id: principal.tenant_id,
        username: principal.username,
        display_name: principal.display_name,
        is_service: principal.is_service,
        roles,
        assignments,
        scopes: principal.scopes,
        purpose_of_use: validate_purpose(purpose_of_use)?,
        break_glass_reason,
        web_session_id: principal.web_session_id,
        correlation_id: Uuid::now_v7(),
    })
}

/// CSRF enforcement for browser sessions: state-changing requests must echo
/// the session's CSRF secret in `x-csrf-token`. Bearer credentials
/// (dev/service/OIDC) are not cookie-attached, so they are not CSRF-exposed.
async fn enforce_csrf(
    state: &AppState,
    session_id: Uuid,
    method: &Method,
    csrf_token: Option<&str>,
) -> Result<(), ApiError> {
    if matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS) {
        return Ok(());
    }
    let Some(csrf_token) = csrf_token else {
        return Err(ApiError::forbidden("missing x-csrf-token".to_string()));
    };
    let hash = hash_service_secret(csrf_token);
    let row: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM web_sessions WHERE id = $1 AND csrf_hash = $2")
            .bind(session_id)
            .bind(&hash)
            .fetch_optional(&state.pool)
            .await?;
    if row.is_none() {
        return Err(ApiError::forbidden("invalid x-csrf-token".to_string()));
    }
    Ok(())
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
        let ctx = load_auth(state, token, purpose, break_glass).await?;
        if let Some(session_id) = ctx.web_session_id {
            let csrf = parts
                .headers
                .get("x-csrf-token")
                .and_then(|v| v.to_str().ok());
            enforce_csrf(state, session_id, &parts.method, csrf).await?;
        }
        Ok(ctx)
    }
}
