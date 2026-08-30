//! Development identity abstraction.
//!
//! The production target is OIDC/OAuth 2.1 (see ADR-0006). For the local
//! reference environment, identity is asserted with `Authorization: Bearer
//! dev-<username>` against seeded synthetic users. The abstraction boundary
//! ([`AuthContext`]) is what the rest of the system depends on, so the token
//! mechanism can be replaced without touching domain code.

use crate::error::ApiError;
use crate::state::AppState;
use axum::async_trait;
use axum::extract::FromRequestParts;
use axum::http::request::Parts;
use sqlx::PgPool;
use uuid::Uuid;

#[derive(Debug, Clone)]
pub struct AuthContext {
    pub user_id: Uuid,
    pub tenant_id: Uuid,
    pub username: String,
    pub display_name: String,
    pub is_service: bool,
    pub roles: Vec<String>,
    /// Purpose of use asserted by the caller (default: treatment).
    pub purpose_of_use: String,
    /// Break-glass reason, if the caller activated break-glass access.
    pub break_glass_reason: Option<String>,
    pub correlation_id: Uuid,
}

impl AuthContext {
    pub fn has_role(&self, role: &str) -> bool {
        self.roles.iter().any(|r| r == role)
    }

    pub fn actor(&self) -> String {
        format!("user:{}", self.username)
    }
}

/// `X-Purpose-Of-Use` is caller-asserted context, so only a closed vocabulary
/// is accepted; anything else is rejected rather than recorded verbatim.
fn validate_purpose(purpose: Option<String>) -> Result<String, ApiError> {
    const ALLOWED: &[&str] = &["treatment", "operations", "emergency", "quality"];
    let purpose = purpose.unwrap_or_else(|| "treatment".to_string());
    if !ALLOWED.contains(&purpose.as_str()) {
        return Err(ApiError::bad_request(
            "invalid_purpose_of_use",
            "x-purpose-of-use must be one of: treatment, operations, emergency, quality",
        ));
    }
    Ok(purpose)
}

pub async fn load_auth(
    pool: &PgPool,
    token: &str,
    purpose_of_use: Option<String>,
    break_glass_reason: Option<String>,
) -> Result<AuthContext, ApiError> {
    // Two development token prefixes: `dev-` for interactive human users,
    // `svc-` for machine identities. Neither authenticates the other kind of
    // principal; real deployments replace both (see ADR-0006).
    let (username, service_token) = if let Some(u) = token.strip_prefix("dev-") {
        (u, false)
    } else if let Some(u) = token.strip_prefix("svc-") {
        (u, true)
    } else {
        return Err(ApiError::unauthorized());
    };
    let row: Option<(Uuid, Uuid, String, String, bool)> = sqlx::query_as(
        "SELECT id, tenant_id, username, display_name, is_service FROM users WHERE username = $1",
    )
    .bind(username)
    .fetch_optional(pool)
    .await?;
    let (user_id, tenant_id, username, display_name, is_service) =
        row.ok_or_else(ApiError::unauthorized)?;
    if is_service != service_token {
        return Err(ApiError::unauthorized());
    }
    let roles: Vec<(String,)> =
        sqlx::query_as("SELECT role FROM role_assignments WHERE user_id = $1")
            .bind(user_id)
            .fetch_all(pool)
            .await?;
    Ok(AuthContext {
        user_id,
        tenant_id,
        username,
        display_name,
        is_service,
        roles: roles.into_iter().map(|(r,)| r).collect(),
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
        load_auth(&state.pool, token, purpose, break_glass).await
    }
}
