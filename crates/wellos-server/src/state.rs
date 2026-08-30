use crate::error::ApiError;
use dmind_gateway::ModelGateway;
use jsonwebtoken::jwk::JwkSet;
use sqlx::PgPool;
use std::sync::Arc;

/// Identity-provider configuration for OIDC/OAuth 2.1 bearer tokens.
/// Signature keys, issuer, and audience come from deployment configuration;
/// tenant, roles, and permissions are always resolved from the local
/// database, never from client-asserted claims.
#[derive(Clone)]
pub struct OidcConfig {
    pub issuer: String,
    pub audience: String,
    pub jwks: JwkSet,
    /// Clock-skew tolerance in seconds for exp/nbf/iat validation.
    pub leeway_secs: u64,
}

impl OidcConfig {
    /// Build from environment. All-or-nothing: partial configuration is a
    /// startup error rather than a silently disabled provider.
    ///
    /// - `WELLOS_OIDC_ISSUER`: expected `iss` claim.
    /// - `WELLOS_OIDC_AUDIENCE`: expected `aud` claim.
    /// - `WELLOS_OIDC_JWKS_JSON` or `WELLOS_OIDC_JWKS_PATH`: the JWKS document.
    /// - `WELLOS_OIDC_LEEWAY_SECS`: optional clock skew (default 60).
    pub fn from_env() -> anyhow::Result<Option<Self>> {
        let issuer = std::env::var("WELLOS_OIDC_ISSUER").ok();
        let audience = std::env::var("WELLOS_OIDC_AUDIENCE").ok();
        let jwks_json = std::env::var("WELLOS_OIDC_JWKS_JSON").ok();
        let jwks_path = std::env::var("WELLOS_OIDC_JWKS_PATH").ok();
        if issuer.is_none() && audience.is_none() && jwks_json.is_none() && jwks_path.is_none() {
            return Ok(None);
        }
        let issuer = issuer.ok_or_else(|| anyhow::anyhow!("WELLOS_OIDC_ISSUER is required"))?;
        let audience =
            audience.ok_or_else(|| anyhow::anyhow!("WELLOS_OIDC_AUDIENCE is required"))?;
        let raw = match (jwks_json, jwks_path) {
            (Some(json), _) => json,
            (None, Some(path)) => std::fs::read_to_string(path)?,
            (None, None) => anyhow::bail!(
                "WELLOS_OIDC_JWKS_JSON or WELLOS_OIDC_JWKS_PATH is required when OIDC is configured"
            ),
        };
        let jwks: JwkSet = serde_json::from_str(&raw)?;
        let leeway_secs = std::env::var("WELLOS_OIDC_LEEWAY_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        Ok(Some(Self {
            issuer,
            audience,
            jwks,
            leeway_secs,
        }))
    }
}

/// Authentication configuration resolved once at startup (fail closed).
#[derive(Clone)]
pub struct AuthConfig {
    /// Predictable `dev-<username>` tokens: local development only.
    pub dev_auth_enabled: bool,
    pub oidc: Option<OidcConfig>,
    /// Maximum break-glass activations per user per hour.
    pub break_glass_hourly_limit: i64,
}

impl AuthConfig {
    /// Development/test defaults: dev tokens enabled, no OIDC provider.
    pub fn development() -> Self {
        Self {
            dev_auth_enabled: true,
            oidc: None,
            break_glass_hourly_limit: 5,
        }
    }

    /// Resolve from environment, failing closed:
    /// - dev auth requires `WELLOS_ENV=development` AND `WELLOS_DEV_AUTH=true`;
    /// - enabling dev auth outside development aborts startup;
    /// - with dev auth disabled, a configured OIDC provider is mandatory.
    pub fn from_env() -> anyhow::Result<Self> {
        let env = std::env::var("WELLOS_ENV").unwrap_or_else(|_| "development".to_string());
        let dev_flag = std::env::var("WELLOS_DEV_AUTH")
            .map(|v| v == "true")
            .unwrap_or(false);
        if dev_flag && env != "development" {
            anyhow::bail!(
                "WELLOS_DEV_AUTH=true is only permitted with WELLOS_ENV=development \
                 (current: {env}); refusing to start with predictable tokens enabled"
            );
        }
        let dev_auth_enabled = dev_flag && env == "development";
        let oidc = OidcConfig::from_env()?;
        if !dev_auth_enabled && oidc.is_none() {
            anyhow::bail!(
                "no identity provider configured: set WELLOS_OIDC_ISSUER, \
                 WELLOS_OIDC_AUDIENCE and WELLOS_OIDC_JWKS_JSON/`_PATH` (or, for local \
                 development only, WELLOS_ENV=development with WELLOS_DEV_AUTH=true)"
            );
        }
        // Malformed or non-positive limits abort startup: emergency access
        // must never be silently disabled by a configuration typo.
        let break_glass_hourly_limit = match std::env::var("WELLOS_BREAK_GLASS_HOURLY_LIMIT") {
            Ok(raw) => {
                let parsed: i64 = raw.parse().map_err(|_| {
                    anyhow::anyhow!("WELLOS_BREAK_GLASS_HOURLY_LIMIT must be a positive integer")
                })?;
                if parsed < 1 {
                    anyhow::bail!("WELLOS_BREAK_GLASS_HOURLY_LIMIT must be at least 1");
                }
                parsed
            }
            Err(_) => 5,
        };
        Ok(Self {
            dev_auth_enabled,
            oidc,
            break_glass_hourly_limit,
        })
    }
}

/// Error returned when no identity provider can handle the presented
/// credential: a configuration problem, never a silent dev fallback.
pub fn identity_provider_not_configured() -> ApiError {
    ApiError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "identity_provider_not_configured",
        "no identity provider is configured for this credential type",
    )
}

#[derive(Clone)]
pub struct AppState {
    pub pool: PgPool,
    pub gateway: Arc<dyn ModelGateway>,
    /// Whether routing patient data to external (off-cell) AI providers is
    /// permitted by deployment configuration. Development default: false.
    pub allow_external_ai: bool,
    /// Regional cell identifier for events and provenance.
    pub cell: String,
    pub auth: Arc<AuthConfig>,
}

impl AppState {
    /// Development/test constructor (dev tokens on, no OIDC). Production
    /// entry points must use [`AppState::with_auth`] with
    /// [`AuthConfig::from_env`].
    pub fn new(pool: PgPool, gateway: Arc<dyn ModelGateway>) -> Self {
        Self::with_auth(pool, gateway, AuthConfig::development())
    }

    pub fn with_auth(pool: PgPool, gateway: Arc<dyn ModelGateway>, auth: AuthConfig) -> Self {
        Self {
            pool,
            gateway,
            allow_external_ai: false,
            cell: "cell-dev-1".to_string(),
            auth: Arc::new(auth),
        }
    }
}
