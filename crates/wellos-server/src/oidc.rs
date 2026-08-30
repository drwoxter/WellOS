//! OIDC discovery and JWKS key management.
//!
//! Keys come either from a statically configured JWKS document or from the
//! issuer's discovery metadata (`/.well-known/openid-configuration`). The
//! remote path pins the issuer (metadata `issuer` must match the configured
//! value exactly), requires HTTPS outside local development, caches the JWKS
//! with a bounded refresh interval, and refreshes early when an unknown `kid`
//! appears — rate-limited so unknown-kid storms cannot hammer the IdP.
//! Refresh failures keep the last known good keys; signatures are still fully
//! validated against them, and unknown keys simply fail authentication.

use jsonwebtoken::jwk::{Jwk, JwkSet};
use serde::Deserialize;
use std::time::{Duration, Instant};
use tokio::sync::RwLock;

/// Signing keys for OIDC token validation.
#[derive(Clone)]
pub enum JwksKeys {
    /// A statically configured JWKS document (no network fetches).
    Static(JwkSet),
    /// Keys resolved from issuer discovery metadata and cached.
    Remote(std::sync::Arc<RemoteJwks>),
}

impl JwksKeys {
    /// Find the key for `kid` (or the single key when no `kid` is present).
    pub async fn find_key(&self, kid: Option<&str>) -> Option<Jwk> {
        match self {
            Self::Static(jwks) => select_key(jwks, kid).cloned(),
            Self::Remote(remote) => remote.find_key(kid).await,
        }
    }
}

fn select_key<'a>(jwks: &'a JwkSet, kid: Option<&str>) -> Option<&'a Jwk> {
    match kid {
        Some(kid) => jwks.find(kid),
        None if jwks.keys.len() == 1 => jwks.keys.first(),
        None => None,
    }
}

#[derive(Deserialize)]
struct DiscoveryDocument {
    issuer: String,
    jwks_uri: String,
    authorization_endpoint: Option<String>,
    token_endpoint: Option<String>,
    end_session_endpoint: Option<String>,
    response_types_supported: Option<Vec<String>>,
    code_challenge_methods_supported: Option<Vec<String>>,
}

/// Endpoints for the browser Authorization Code + PKCE flow, resolved from
/// the pinned issuer's discovery metadata and validated against it.
#[derive(Clone, Debug)]
pub struct OidcEndpoints {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    pub end_session_endpoint: Option<String>,
}

struct CachedJwks {
    jwks: JwkSet,
    fetched_at: Instant,
}

struct RemoteState {
    jwks_uri: Option<String>,
    endpoints: Option<OidcEndpoints>,
    cache: Option<CachedJwks>,
    last_attempt: Option<Instant>,
}

/// JWKS resolved via OIDC discovery, cached with bounded refresh.
pub struct RemoteJwks {
    issuer: String,
    allow_http: bool,
    refresh: Duration,
    min_refresh: Duration,
    client: reqwest::Client,
    state: RwLock<RemoteState>,
}

impl RemoteJwks {
    pub fn new(
        issuer: String,
        allow_http: bool,
        refresh_secs: u64,
        min_refresh_secs: u64,
    ) -> anyhow::Result<Self> {
        require_safe_url(&issuer, allow_http, "OIDC issuer")?;
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            issuer,
            allow_http,
            refresh: Duration::from_secs(refresh_secs),
            min_refresh: Duration::from_secs(min_refresh_secs),
            client,
            state: RwLock::new(RemoteState {
                jwks_uri: None,
                endpoints: None,
                cache: None,
                last_attempt: None,
            }),
        })
    }

    /// Run discovery and the initial JWKS fetch. Called at startup so a
    /// misconfigured or unreachable provider aborts startup (fail closed).
    pub async fn initialize(&self) -> anyhow::Result<()> {
        let mut state = self.state.write().await;
        state.last_attempt = Some(Instant::now());
        self.initialize_locked(&mut state).await
    }

    async fn fetch_jwks(&self, jwks_uri: &str) -> anyhow::Result<JwkSet> {
        let jwks: JwkSet = self
            .client
            .get(jwks_uri)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        Ok(jwks)
    }

    /// Resolve a key, refreshing the cache when it has expired or when the
    /// requested `kid` is unknown. Refresh attempts are rate-limited by the
    /// minimum refresh interval; on failure the last known keys are kept.
    pub async fn find_key(&self, kid: Option<&str>) -> Option<Jwk> {
        {
            let state = self.state.read().await;
            if let Some(cache) = &state.cache {
                if cache.fetched_at.elapsed() < self.refresh {
                    if let Some(key) = select_key(&cache.jwks, kid) {
                        return Some(key.clone());
                    }
                }
            }
        }
        // Cache expired or kid unknown: attempt a bounded refresh.
        let mut state = self.state.write().await;
        let may_attempt = state
            .last_attempt
            .map(|t| t.elapsed() >= self.min_refresh)
            .unwrap_or(true);
        if may_attempt {
            state.last_attempt = Some(Instant::now());
            if let Some(jwks_uri) = state.jwks_uri.clone() {
                if let Ok(jwks) = self.fetch_jwks(&jwks_uri).await {
                    state.cache = Some(CachedJwks {
                        jwks,
                        fetched_at: Instant::now(),
                    });
                }
            } else if self.initialize_locked(&mut state).await.is_err() {
                tracing::warn!("OIDC JWKS refresh failed; keeping cached keys");
            }
        }
        state
            .cache
            .as_ref()
            .and_then(|c| select_key(&c.jwks, kid))
            .cloned()
    }

    /// Validated browser-flow endpoints, when the provider advertises them.
    pub async fn endpoints(&self) -> Option<OidcEndpoints> {
        self.state.read().await.endpoints.clone()
    }

    async fn initialize_locked(&self, state: &mut RemoteState) -> anyhow::Result<()> {
        let discovery_url = format!(
            "{}/.well-known/openid-configuration",
            self.issuer.trim_end_matches('/')
        );
        let doc: DiscoveryDocument = self
            .client
            .get(&discovery_url)
            .send()
            .await?
            .error_for_status()?
            .json()
            .await?;
        if doc.issuer.trim_end_matches('/') != self.issuer.trim_end_matches('/') {
            anyhow::bail!("OIDC discovery metadata issuer does not match the configured issuer");
        }
        require_safe_url(&doc.jwks_uri, self.allow_http, "OIDC jwks_uri")?;
        require_same_host(&self.issuer, &doc.jwks_uri)?;
        state.endpoints = validate_endpoints(&self.issuer, self.allow_http, &doc)?;
        let jwks = self.fetch_jwks(&doc.jwks_uri).await?;
        state.jwks_uri = Some(doc.jwks_uri);
        state.cache = Some(CachedJwks {
            jwks,
            fetched_at: Instant::now(),
        });
        Ok(())
    }
}

/// Validate the browser-flow endpoints from discovery metadata. Both the
/// authorization and token endpoints must be safe URLs on the pinned
/// issuer's host. When the provider advertises supported response types or
/// code challenge methods, they must include `code` and `S256`: WellOS only
/// performs the authorization-code flow with S256 PKCE.
fn validate_endpoints(
    issuer: &str,
    allow_http: bool,
    doc: &DiscoveryDocument,
) -> anyhow::Result<Option<OidcEndpoints>> {
    let (Some(authz), Some(token)) = (&doc.authorization_endpoint, &doc.token_endpoint) else {
        return Ok(None);
    };
    require_safe_url(authz, allow_http, "OIDC authorization_endpoint")?;
    require_same_host(issuer, authz)?;
    require_safe_url(token, allow_http, "OIDC token_endpoint")?;
    require_same_host(issuer, token)?;
    if let Some(types) = &doc.response_types_supported {
        if !types.iter().any(|t| t == "code") {
            anyhow::bail!("OIDC provider does not support the authorization-code flow");
        }
    }
    if let Some(methods) = &doc.code_challenge_methods_supported {
        if !methods.iter().any(|m| m == "S256") {
            anyhow::bail!("OIDC provider does not support S256 PKCE");
        }
    }
    let end_session = match &doc.end_session_endpoint {
        Some(url) => {
            require_safe_url(url, allow_http, "OIDC end_session_endpoint")?;
            require_same_host(issuer, url)?;
            Some(url.clone())
        }
        None => None,
    };
    Ok(Some(OidcEndpoints {
        authorization_endpoint: authz.clone(),
        token_endpoint: token.clone(),
        end_session_endpoint: end_session,
    }))
}

/// HTTPS is mandatory for identity endpoints. Development may use plain
/// HTTP, but only toward loopback hosts: identity metadata and signing keys
/// are never fetched over cleartext networks.
fn require_safe_url(url: &str, allow_http: bool, what: &str) -> anyhow::Result<()> {
    if url.starts_with("https://") {
        return Ok(());
    }
    if allow_http && url.starts_with("http://") && is_loopback_host(url) {
        return Ok(());
    }
    anyhow::bail!("{what} must be an https:// URL (http:// is allowed only toward loopback hosts in development)");
}

/// The advertised `jwks_uri` must live on the pinned issuer's host: key
/// material is only ever fetched from the identity provider itself, so a
/// compromised or misconfigured discovery document cannot direct signing-key
/// fetches at unrelated (e.g. internal) services.
fn require_same_host(issuer: &str, jwks_uri: &str) -> anyhow::Result<()> {
    let issuer_host = url::Url::parse(issuer)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase));
    let jwks_host = url::Url::parse(jwks_uri)
        .ok()
        .and_then(|u| u.host_str().map(str::to_ascii_lowercase));
    match (issuer_host, jwks_host) {
        (Some(a), Some(b)) if a == b => Ok(()),
        _ => anyhow::bail!("OIDC jwks_uri host does not match the configured issuer host"),
    }
}

fn is_loopback_host(url: &str) -> bool {
    let Ok(parsed) = url::Url::parse(url) else {
        return false;
    };
    match parsed.host() {
        Some(url::Host::Domain(d)) => d.eq_ignore_ascii_case("localhost"),
        Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
        Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
        None => false,
    }
}

#[cfg(test)]
mod tests {
    use super::{require_safe_url, require_same_host};

    #[test]
    fn https_is_always_accepted() {
        assert!(require_safe_url("https://idp.example.test", false, "issuer").is_ok());
        assert!(require_safe_url("https://idp.example.test", true, "issuer").is_ok());
    }

    #[test]
    fn http_is_rejected_outside_development() {
        assert!(require_safe_url("http://localhost:9999", false, "issuer").is_err());
        assert!(require_safe_url("http://127.0.0.1:9999", false, "issuer").is_err());
    }

    #[test]
    fn development_http_is_limited_to_loopback() {
        assert!(require_safe_url("http://localhost:9999/x", true, "issuer").is_ok());
        assert!(require_safe_url("http://127.0.0.1:9999", true, "issuer").is_ok());
        assert!(require_safe_url("http://[::1]:9999", true, "issuer").is_ok());
        assert!(require_safe_url("http://idp.example.test", true, "issuer").is_err());
        assert!(require_safe_url("http://10.0.0.5", true, "issuer").is_err());
        assert!(require_safe_url("http://192.168.1.1:8080", true, "issuer").is_err());
    }

    #[test]
    fn jwks_uri_is_pinned_to_the_issuer_host() {
        assert!(
            require_same_host("https://idp.example.test", "https://idp.example.test/keys").is_ok()
        );
        assert!(require_same_host(
            "https://idp.example.test:8443",
            "https://IDP.EXAMPLE.TEST/oauth/jwks"
        )
        .is_ok());
        assert!(require_same_host(
            "https://idp.example.test",
            "https://other.example.test/keys"
        )
        .is_err());
        assert!(require_same_host(
            "https://idp.example.test",
            "https://169.254.169.254/latest/meta-data"
        )
        .is_err());
        assert!(require_same_host("https://idp.example.test", "not a url").is_err());
    }
}
