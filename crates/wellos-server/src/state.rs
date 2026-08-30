use crate::error::ApiError;
use crate::oidc::{JwksKeys, RemoteJwks};
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
    pub keys: JwksKeys,
    /// Clock-skew tolerance in seconds for exp/nbf/iat validation.
    pub leeway_secs: u64,
    /// When true, tokens must carry an accepted MFA signal in validated
    /// `amr`/`acr` claims; missing or malformed signals fail closed.
    pub require_mfa: bool,
    /// `amr` values accepted as MFA proof.
    pub accepted_amr: Vec<String>,
    /// `acr` values accepted as MFA proof.
    pub accepted_acr: Vec<String>,
}

impl OidcConfig {
    /// Build from environment. All-or-nothing: partial configuration is a
    /// startup error rather than a silently disabled provider.
    ///
    /// - `WELLOS_OIDC_ISSUER`: expected `iss` claim (and discovery base).
    /// - `WELLOS_OIDC_AUDIENCE`: expected `aud` claim.
    /// - `WELLOS_OIDC_DISCOVERY=true`: resolve the JWKS URI from the issuer's
    ///   `/.well-known/openid-configuration` metadata (issuer-pinned,
    ///   HTTPS-only outside development), with cached, bounded refresh.
    /// - `WELLOS_OIDC_JWKS_JSON` or `WELLOS_OIDC_JWKS_PATH`: static JWKS
    ///   document (alternative to discovery).
    /// - `WELLOS_OIDC_JWKS_REFRESH_SECS`: cache lifetime (default 3600).
    /// - `WELLOS_OIDC_JWKS_MIN_REFRESH_SECS`: minimum interval between
    ///   refresh attempts, bounding unknown-`kid` refresh storms (default 30).
    /// - `WELLOS_OIDC_LEEWAY_SECS`: optional clock skew (default 60).
    /// - `WELLOS_OIDC_REQUIRE_MFA=true`: require an accepted MFA signal in
    ///   validated `amr`/`acr` claims.
    /// - `WELLOS_OIDC_ACCEPTED_AMR` / `WELLOS_OIDC_ACCEPTED_ACR`: accepted
    ///   claim values (comma-separated).
    pub fn from_env(is_development: bool) -> anyhow::Result<Option<Self>> {
        let issuer = std::env::var("WELLOS_OIDC_ISSUER").ok();
        let audience = std::env::var("WELLOS_OIDC_AUDIENCE").ok();
        let discovery = std::env::var("WELLOS_OIDC_DISCOVERY")
            .map(|v| v == "true")
            .unwrap_or(false);
        let jwks_json = std::env::var("WELLOS_OIDC_JWKS_JSON").ok();
        let jwks_path = std::env::var("WELLOS_OIDC_JWKS_PATH").ok();
        if issuer.is_none()
            && audience.is_none()
            && !discovery
            && jwks_json.is_none()
            && jwks_path.is_none()
        {
            return Ok(None);
        }
        let issuer = issuer.ok_or_else(|| anyhow::anyhow!("WELLOS_OIDC_ISSUER is required"))?;
        let audience =
            audience.ok_or_else(|| anyhow::anyhow!("WELLOS_OIDC_AUDIENCE is required"))?;
        let keys = if discovery {
            let refresh_secs = parse_secs("WELLOS_OIDC_JWKS_REFRESH_SECS", 3600)?;
            let min_refresh_secs = parse_secs("WELLOS_OIDC_JWKS_MIN_REFRESH_SECS", 30)?;
            let remote = RemoteJwks::new(
                issuer.clone(),
                is_development,
                refresh_secs,
                min_refresh_secs,
            )?;
            JwksKeys::Remote(Arc::new(remote))
        } else {
            let raw = match (jwks_json, jwks_path) {
                (Some(json), _) => json,
                (None, Some(path)) => std::fs::read_to_string(path)?,
                (None, None) => anyhow::bail!(
                    "WELLOS_OIDC_DISCOVERY=true, WELLOS_OIDC_JWKS_JSON or WELLOS_OIDC_JWKS_PATH is required when OIDC is configured"
                ),
            };
            let jwks: JwkSet = serde_json::from_str(&raw)?;
            JwksKeys::Static(jwks)
        };
        let leeway_secs = std::env::var("WELLOS_OIDC_LEEWAY_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(60);
        let require_mfa = std::env::var("WELLOS_OIDC_REQUIRE_MFA")
            .map(|v| v == "true")
            .unwrap_or(false);
        let accepted_amr = parse_list("WELLOS_OIDC_ACCEPTED_AMR", &["mfa", "otp", "hwk"]);
        let accepted_acr = parse_list(
            "WELLOS_OIDC_ACCEPTED_ACR",
            &["urn:mace:incommon:iap:silver", "phrh", "phr"],
        );
        Ok(Some(Self {
            issuer,
            audience,
            keys,
            leeway_secs,
            require_mfa,
            accepted_amr,
            accepted_acr,
        }))
    }
}

fn parse_secs(var: &str, default: u64) -> anyhow::Result<u64> {
    match std::env::var(var) {
        Ok(raw) => raw
            .parse()
            .map_err(|_| anyhow::anyhow!("{var} must be a non-negative integer")),
        Err(_) => Ok(default),
    }
}

fn parse_list(var: &str, default: &[&str]) -> Vec<String> {
    match std::env::var(var) {
        Ok(raw) => raw
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect(),
        Err(_) => default.iter().map(|s| s.to_string()).collect(),
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
    /// Absolute lifetime of a browser session in seconds.
    pub session_absolute_secs: i64,
    /// Inactivity timeout of a browser session in seconds.
    pub session_idle_secs: i64,
}

impl AuthConfig {
    /// Development/test defaults: dev tokens enabled, no OIDC provider.
    pub fn development() -> Self {
        Self {
            dev_auth_enabled: true,
            oidc: None,
            break_glass_hourly_limit: 5,
            session_absolute_secs: 8 * 3600,
            session_idle_secs: 30 * 60,
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
        let oidc = OidcConfig::from_env(env == "development")?;
        if !dev_auth_enabled && oidc.is_none() {
            anyhow::bail!(
                "no identity provider configured: set WELLOS_OIDC_ISSUER, \
                 WELLOS_OIDC_AUDIENCE and WELLOS_OIDC_DISCOVERY=true or \
                 WELLOS_OIDC_JWKS_JSON/`_PATH` (or, for local development only, \
                 WELLOS_ENV=development with WELLOS_DEV_AUTH=true)"
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
        let session_absolute_secs = parse_positive_secs("WELLOS_SESSION_ABSOLUTE_SECS", 8 * 3600)?;
        let session_idle_secs = parse_positive_secs("WELLOS_SESSION_IDLE_SECS", 30 * 60)?;
        Ok(Self {
            dev_auth_enabled,
            oidc,
            break_glass_hourly_limit,
            session_absolute_secs,
            session_idle_secs,
        })
    }

    /// Complete any network-backed identity configuration (OIDC discovery and
    /// the initial JWKS fetch). Startup fails closed when the configured
    /// provider is unreachable or its metadata does not pin the issuer.
    pub async fn initialize(&self) -> anyhow::Result<()> {
        if let Some(OidcConfig {
            keys: JwksKeys::Remote(remote),
            ..
        }) = &self.oidc
        {
            remote.initialize().await?;
        }
        Ok(())
    }
}

fn parse_positive_secs(var: &str, default: i64) -> anyhow::Result<i64> {
    match std::env::var(var) {
        Ok(raw) => {
            let parsed: i64 = raw
                .parse()
                .map_err(|_| anyhow::anyhow!("{var} must be a positive integer"))?;
            if parsed < 1 {
                anyhow::bail!("{var} must be at least 1");
            }
            Ok(parsed)
        }
        Err(_) => Ok(default),
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
