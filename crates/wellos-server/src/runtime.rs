//! Typed, fail-closed runtime configuration.
//!
//! `WELLOS_ENV` is mandatory and must be one of `development`, `test`,
//! `staging` or `production`. Everything that distinguishes a local fixture
//! environment from a real deployment is derived from it here, once, so no
//! other module needs to inspect the raw variable:
//!
//! - fixtures (fake AI providers, synthetic seed, development users) require
//!   a `development`/`test` environment *and* a `dev-fixtures` build;
//! - `staging`/`production` refuse development authentication, fake
//!   providers and synthetic seeding unconditionally;
//! - AI providers default to `disabled`; a real provider is selected only by
//!   explicit configuration and its failures are never replaced by fixtures.

use std::sync::Arc;
use std::time::Duration;

use dmind_gateway::openai::{OpenAiCompatibleModel, OpenAiCompatibleModelConfig};
use dmind_gateway::scribe::{
    DisabledTranscription, OpenAiCompatibleConfig, OpenAiCompatibleTranscription,
    TranscriptionProvider,
};
use dmind_gateway::{DisabledGateway, ModelGateway};

pub const DEV_FIXTURES_ENABLED: bool = cfg!(feature = "dev-fixtures");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeEnv {
    Development,
    Test,
    Staging,
    Production,
}

impl RuntimeEnv {
    pub fn parse(raw: &str) -> anyhow::Result<Self> {
        match raw {
            "development" => Ok(Self::Development),
            "test" => Ok(Self::Test),
            "staging" => Ok(Self::Staging),
            "production" => Ok(Self::Production),
            other => anyhow::bail!(
                "WELLOS_ENV must be one of development, test, staging, production (got '{other}')"
            ),
        }
    }

    /// Read `WELLOS_ENV`; a missing value is an error, never development.
    pub fn from_env() -> anyhow::Result<Self> {
        match std::env::var("WELLOS_ENV") {
            Ok(raw) => Self::parse(raw.trim()),
            Err(_) => anyhow::bail!(
                "WELLOS_ENV is required (development, test, staging or production); \
                 refusing to guess an environment"
            ),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Development => "development",
            Self::Test => "test",
            Self::Staging => "staging",
            Self::Production => "production",
        }
    }

    /// Local environments: fixtures, development authentication, loopback
    /// `http://` endpoints and the default database URL are permitted here
    /// (subject to explicit flags) and nowhere else.
    pub fn is_local(&self) -> bool {
        matches!(self, Self::Development | Self::Test)
    }

    pub fn is_deployed(&self) -> bool {
        !self.is_local()
    }
}

impl std::fmt::Display for RuntimeEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderKind {
    Disabled,
    Fake,
    OpenAiCompatible,
}

impl ProviderKind {
    fn parse(var: &str, raw: &str) -> anyhow::Result<Self> {
        match raw {
            "disabled" => Ok(Self::Disabled),
            "fake" => Ok(Self::Fake),
            "openai_compatible" => Ok(Self::OpenAiCompatible),
            other => anyhow::bail!(
                "{var} must be 'disabled', 'fake' or 'openai_compatible' (got '{other}')"
            ),
        }
    }

    fn from_env(var: &str) -> anyhow::Result<Self> {
        match std::env::var(var) {
            Ok(raw) => Self::parse(var, raw.trim()),
            Err(_) => Ok(Self::Disabled),
        }
    }

    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Disabled => "disabled",
            Self::Fake => "fake",
            Self::OpenAiCompatible => "openai_compatible",
        }
    }
}

/// Per-tenant and per-task hourly ceilings on external model executions.
/// Artifact reuse (identical request → existing valid artifact) does not
/// count against them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiQuotas {
    pub tenant_per_hour: i64,
    pub task_per_hour: i64,
}

#[derive(Debug, Clone)]
pub struct RuntimeConfig {
    pub env: RuntimeEnv,
    pub model_provider: ProviderKind,
    pub scribe_provider: ProviderKind,
    /// `WELLOS_ALLOW_SYNTHETIC_SEED=true`; only honoured in local
    /// environments of a `dev-fixtures` build.
    pub allow_synthetic_seed: bool,
    /// Accepted BCP-47 language tags for consultation recordings.
    pub scribe_languages: Vec<String>,
    pub ai_quotas: AiQuotas,
}

impl RuntimeConfig {
    /// Resolve and validate the runtime from the environment. Every rule
    /// here fails closed: a violated constraint aborts startup instead of
    /// degrading to a weaker configuration.
    pub fn from_env() -> anyhow::Result<Self> {
        let env = RuntimeEnv::from_env()?;
        Self::from_env_for(env)
    }

    pub fn from_env_for(env: RuntimeEnv) -> anyhow::Result<Self> {
        let model_provider = ProviderKind::from_env("DMIND_MODEL_PROVIDER")?;
        let scribe_provider = ProviderKind::from_env("WELLOS_SCRIBE_PROVIDER")?;
        let allow_synthetic_seed = parse_bool("WELLOS_ALLOW_SYNTHETIC_SEED")?.unwrap_or(false);
        for (var, kind) in [
            ("DMIND_MODEL_PROVIDER", model_provider),
            ("WELLOS_SCRIBE_PROVIDER", scribe_provider),
        ] {
            if kind == ProviderKind::Fake {
                fixtures_allowed(env, &format!("{var}=fake"))?;
            }
        }
        if allow_synthetic_seed {
            fixtures_allowed(env, "WELLOS_ALLOW_SYNTHETIC_SEED=true")?;
        }
        let scribe_languages = parse_languages("WELLOS_SCRIBE_LANGUAGES", &["en", "es"])?;
        let ai_quotas = AiQuotas {
            tenant_per_hour: parse_positive_i64("DMIND_QUOTA_TENANT_PER_HOUR", 600)?,
            task_per_hour: parse_positive_i64("DMIND_QUOTA_TASK_PER_HOUR", 200)?,
        };
        Ok(Self {
            env,
            model_provider,
            scribe_provider,
            allow_synthetic_seed,
            scribe_languages,
            ai_quotas,
        })
    }

    /// Fixture runtime used by the test suites and `AppState::new`.
    #[cfg(feature = "dev-fixtures")]
    pub fn test_fixtures() -> Self {
        Self {
            env: RuntimeEnv::Test,
            model_provider: ProviderKind::Fake,
            scribe_provider: ProviderKind::Fake,
            allow_synthetic_seed: true,
            scribe_languages: vec!["en".into(), "es".into()],
            ai_quotas: AiQuotas {
                tenant_per_hour: 100_000,
                task_per_hour: 100_000,
            },
        }
    }

    /// True when the synthetic seed may run: local environment, explicit
    /// flag and fixture build.
    pub fn synthetic_seed_permitted(&self) -> bool {
        self.env.is_local() && self.allow_synthetic_seed && DEV_FIXTURES_ENABLED
    }

    /// True when model output in this process comes from the deterministic
    /// fixture provider and must be persisted and displayed as synthetic.
    pub fn synthetic_output(&self) -> bool {
        self.model_provider == ProviderKind::Fake
    }

    pub fn language_allowed(&self, tag: &str) -> bool {
        self.scribe_languages
            .iter()
            .any(|l| l.eq_ignore_ascii_case(tag))
    }
}

/// Fixtures need all three: a local environment, a `dev-fixtures` build and
/// the explicit setting the caller is validating.
pub fn fixtures_allowed(env: RuntimeEnv, what: &str) -> anyhow::Result<()> {
    if env.is_deployed() {
        anyhow::bail!(
            "{what} is refused with WELLOS_ENV={env}: synthetic fixtures are only \
             permitted in development or test"
        );
    }
    if !DEV_FIXTURES_ENABLED {
        anyhow::bail!(
            "{what} requires a build with the `dev-fixtures` feature \
             (cargo ... --features dev-fixtures); this binary has no fixtures compiled in"
        );
    }
    Ok(())
}

/// Build the model gateway selected by `DMIND_MODEL_PROVIDER`. A real
/// provider that cannot be constructed is a startup error; nothing here ever
/// substitutes the fake provider for a failed real one.
pub fn model_gateway_from_env(cfg: &RuntimeConfig) -> anyhow::Result<Arc<dyn ModelGateway>> {
    match cfg.model_provider {
        ProviderKind::Disabled => Ok(Arc::new(DisabledGateway::disabled(
            "DMIND_MODEL_PROVIDER=disabled",
        ))),
        ProviderKind::Fake => fake_model_gateway(cfg.env),
        ProviderKind::OpenAiCompatible => {
            let endpoint = required("DMIND_MODEL_ENDPOINT")?;
            let allowed_hosts = std::env::var("DMIND_MODEL_ALLOWED_HOSTS").ok();
            validate_external_endpoint(
                "DMIND_MODEL_ENDPOINT",
                "DMIND_MODEL_ALLOWED_HOSTS",
                &endpoint,
                allowed_hosts.as_deref(),
                cfg.env.is_local(),
            )?;
            let model = required("DMIND_MODEL_NAME")?;
            let api_key = required_secret("DMIND_MODEL_API_KEY")?;
            let connect_secs = parse_positive_i64("DMIND_MODEL_CONNECT_TIMEOUT_SECS", 5)?;
            let timeout_secs = parse_positive_i64("DMIND_MODEL_TIMEOUT_SECS", 60)?;
            let max_retries = parse_bounded_u32("DMIND_MODEL_MAX_RETRIES", 2, 5)?;
            let max_response_kib = parse_positive_i64("DMIND_MODEL_MAX_RESPONSE_KIB", 512)?;
            let max_concurrency = parse_positive_i64("DMIND_MODEL_MAX_CONCURRENCY", 8)?;
            let queue_secs = parse_positive_i64("DMIND_MODEL_QUEUE_TIMEOUT_SECS", 10)?;
            let model = OpenAiCompatibleModel::new(OpenAiCompatibleModelConfig {
                endpoint,
                model,
                api_key,
                connect_timeout: Duration::from_secs(connect_secs as u64),
                timeout: Duration::from_secs(timeout_secs as u64),
                max_retries,
                retry_backoff: Duration::from_millis(500),
                max_response_bytes: (max_response_kib as usize) * 1024,
                max_concurrency: max_concurrency as usize,
                queue_timeout: Duration::from_secs(queue_secs as u64),
            })
            .map_err(|e| anyhow::anyhow!("model provider: {e}"))?;
            Ok(Arc::new(model))
        }
    }
}

#[cfg(feature = "dev-fixtures")]
fn fake_model_gateway(env: RuntimeEnv) -> anyhow::Result<Arc<dyn ModelGateway>> {
    fixtures_allowed(env, "DMIND_MODEL_PROVIDER=fake")?;
    Ok(Arc::new(dmind_gateway::fake::FakeProvider::new()))
}

#[cfg(not(feature = "dev-fixtures"))]
fn fake_model_gateway(env: RuntimeEnv) -> anyhow::Result<Arc<dyn ModelGateway>> {
    fixtures_allowed(env, "DMIND_MODEL_PROVIDER=fake")?;
    anyhow::bail!("fake model provider is not compiled into this binary")
}

/// Build the speech-to-text provider selected by `WELLOS_SCRIBE_PROVIDER`.
pub fn scribe_provider_from_env(
    cfg: &RuntimeConfig,
) -> anyhow::Result<Arc<dyn TranscriptionProvider>> {
    match cfg.scribe_provider {
        ProviderKind::Disabled => Ok(Arc::new(DisabledTranscription::disabled(
            "WELLOS_SCRIBE_PROVIDER=disabled",
        ))),
        ProviderKind::Fake => fake_scribe_provider(cfg.env),
        ProviderKind::OpenAiCompatible => {
            let endpoint = required("WELLOS_SCRIBE_ENDPOINT")?;
            let allowed_hosts = std::env::var("WELLOS_SCRIBE_ALLOWED_HOSTS").ok();
            validate_external_endpoint(
                "WELLOS_SCRIBE_ENDPOINT",
                "WELLOS_SCRIBE_ALLOWED_HOSTS",
                &endpoint,
                allowed_hosts.as_deref(),
                cfg.env.is_local(),
            )?;
            let model = required("WELLOS_SCRIBE_MODEL")?;
            let api_key = required_secret("WELLOS_SCRIBE_API_KEY")?;
            let timeout_secs = parse_positive_i64("WELLOS_SCRIBE_TIMEOUT_SECS", 60)?;
            let max_retries = parse_bounded_u32("WELLOS_SCRIBE_MAX_RETRIES", 2, 5)?;
            let provider = OpenAiCompatibleTranscription::new(OpenAiCompatibleConfig {
                endpoint,
                model,
                api_key,
                timeout: Duration::from_secs(timeout_secs as u64),
                max_retries,
                retry_backoff: Duration::from_millis(500),
            })
            .map_err(|e| anyhow::anyhow!("scribe provider: {e}"))?;
            Ok(Arc::new(provider))
        }
    }
}

#[cfg(feature = "dev-fixtures")]
fn fake_scribe_provider(env: RuntimeEnv) -> anyhow::Result<Arc<dyn TranscriptionProvider>> {
    fixtures_allowed(env, "WELLOS_SCRIBE_PROVIDER=fake")?;
    Ok(Arc::new(dmind_gateway::scribe::FakeTranscription::new()))
}

#[cfg(not(feature = "dev-fixtures"))]
fn fake_scribe_provider(env: RuntimeEnv) -> anyhow::Result<Arc<dyn TranscriptionProvider>> {
    fixtures_allowed(env, "WELLOS_SCRIBE_PROVIDER=fake")?;
    anyhow::bail!("fake transcription provider is not compiled into this binary")
}

/// Destination policy shared by every external AI endpoint. Data and the
/// bearer credential are only ever sent to a host the operator named twice:
/// once in the endpoint variable and once in the exact-match allowlist. In
/// deployed environments the allowlist is mandatory, the scheme must be
/// `https`, and IP-literal or loopback hosts are refused; locally a loopback
/// `http://` mock is allowed for controlled test servers. The URL may not
/// embed credentials.
pub fn validate_external_endpoint(
    endpoint_var: &str,
    allowlist_var: &str,
    endpoint: &str,
    allowed_hosts: Option<&str>,
    is_local: bool,
) -> anyhow::Result<()> {
    let url = url::Url::parse(endpoint)
        .map_err(|_| anyhow::anyhow!("{endpoint_var} is not a valid absolute URL"))?;
    let host = match url.host() {
        Some(url::Host::Domain(d)) => d.to_ascii_lowercase(),
        Some(url::Host::Ipv4(_)) | Some(url::Host::Ipv6(_)) if is_local => {
            url.host_str().unwrap_or_default().to_ascii_lowercase()
        }
        Some(_) => anyhow::bail!("{endpoint_var} must name a DNS host, not an IP literal"),
        None => anyhow::bail!("{endpoint_var} must include a host"),
    };
    if !url.username().is_empty() || url.password().is_some() {
        anyhow::bail!("{endpoint_var} must not embed credentials");
    }
    let loopback = host == "localhost"
        || host.ends_with(".localhost")
        || matches!(url.host(), Some(url::Host::Ipv4(ip)) if ip.is_loopback())
        || matches!(url.host(), Some(url::Host::Ipv6(ip)) if ip.is_loopback());
    match url.scheme() {
        "https" => {}
        "http" if is_local && loopback => {}
        "http" => anyhow::bail!(
            "{endpoint_var} must be an https:// URL (http:// is only allowed for a loopback host in development/test)"
        ),
        _ => anyhow::bail!("{endpoint_var} must be an https:// URL"),
    }
    if loopback && !is_local {
        anyhow::bail!("{endpoint_var} must not point at a loopback host outside development/test");
    }
    let allowed: Vec<String> = allowed_hosts
        .unwrap_or_default()
        .split(',')
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect();
    if allowed.is_empty() {
        if is_local {
            return Ok(());
        }
        anyhow::bail!("{allowlist_var} is required outside development/test");
    }
    if allowed.iter().any(|h| h.contains('*') || h.contains('/')) {
        anyhow::bail!("{allowlist_var} entries must be exact host names (no wildcards or paths)");
    }
    // A bare entry matches only the scheme's default port; a non-default
    // port must be named explicitly as `host:port`.
    let host_port = match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host,
    };
    if !allowed.contains(&host_port) {
        anyhow::bail!("{endpoint_var} host is not in {allowlist_var}");
    }
    Ok(())
}

/// Parse a security-sensitive boolean flag: only the literal strings `true`
/// and `false` are accepted. A present-but-malformed value aborts startup
/// instead of silently weakening the configuration.
pub fn parse_bool(var: &str) -> anyhow::Result<Option<bool>> {
    match std::env::var(var) {
        Ok(raw) => match raw.trim() {
            "true" => Ok(Some(true)),
            "false" => Ok(Some(false)),
            _ => anyhow::bail!("{var} must be exactly 'true' or 'false'"),
        },
        Err(_) => Ok(None),
    }
}

pub fn parse_positive_i64(var: &str, default: i64) -> anyhow::Result<i64> {
    match std::env::var(var) {
        Ok(raw) => {
            let parsed: i64 = raw
                .trim()
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

fn parse_bounded_u32(var: &str, default: u32, max: u32) -> anyhow::Result<u32> {
    match std::env::var(var) {
        Ok(raw) => {
            let parsed: u32 = raw
                .trim()
                .parse()
                .map_err(|_| anyhow::anyhow!("{var} must be a non-negative integer"))?;
            if parsed > max {
                anyhow::bail!("{var} must be at most {max}");
            }
            Ok(parsed)
        }
        Err(_) => Ok(default),
    }
}

fn required(var: &str) -> anyhow::Result<String> {
    let v = std::env::var(var).map_err(|_| anyhow::anyhow!("{var} is required"))?;
    if v.trim().is_empty() {
        anyhow::bail!("{var} must not be empty");
    }
    Ok(v.trim().to_string())
}

fn required_secret(var: &str) -> anyhow::Result<String> {
    let v = std::env::var(var).map_err(|_| anyhow::anyhow!("{var} is required"))?;
    if v.trim().is_empty() {
        anyhow::bail!("{var} must not be empty");
    }
    Ok(v)
}

/// Comma-separated BCP-47 language tags. Validated structurally (RFC 5646
/// subtag shape) rather than against a closed list, so deployments choose
/// their clinical languages.
fn parse_languages(var: &str, default: &[&str]) -> anyhow::Result<Vec<String>> {
    let raw = match std::env::var(var) {
        Ok(raw) => raw,
        Err(_) => return Ok(default.iter().map(|s| s.to_string()).collect()),
    };
    let mut out: Vec<String> = Vec::new();
    for tag in raw.split(',').map(str::trim).filter(|t| !t.is_empty()) {
        if !is_bcp47_tag(tag) {
            anyhow::bail!("{var} contains an invalid BCP-47 language tag: '{tag}'");
        }
        if !out.iter().any(|t| t.eq_ignore_ascii_case(tag)) {
            out.push(tag.to_string());
        }
    }
    if out.is_empty() {
        anyhow::bail!("{var} must list at least one BCP-47 language tag");
    }
    Ok(out)
}

/// Structural BCP-47 check: a 2–3 letter primary language subtag (every
/// registered language; the 5–8 letter range is reserved and unused)
/// followed by optional 1–8 character alphanumeric subtags, separated by
/// hyphens.
pub fn is_bcp47_tag(tag: &str) -> bool {
    let mut parts = tag.split('-');
    let Some(primary) = parts.next() else {
        return false;
    };
    let primary_ok =
        (2..=3).contains(&primary.len()) && primary.chars().all(|c| c.is_ascii_alphabetic());
    if !primary_ok {
        return false;
    }
    parts.all(|p| (1..=8).contains(&p.len()) && p.chars().all(|c| c.is_ascii_alphanumeric()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn environment_is_typed_and_unknown_values_are_rejected() {
        assert_eq!(
            RuntimeEnv::parse("development").unwrap(),
            RuntimeEnv::Development
        );
        assert_eq!(RuntimeEnv::parse("test").unwrap(), RuntimeEnv::Test);
        assert_eq!(RuntimeEnv::parse("staging").unwrap(), RuntimeEnv::Staging);
        assert_eq!(
            RuntimeEnv::parse("production").unwrap(),
            RuntimeEnv::Production
        );
        for bad in ["", "dev", "prod", "Development", "local"] {
            assert!(RuntimeEnv::parse(bad).is_err(), "{bad:?}");
        }
        assert!(RuntimeEnv::Development.is_local());
        assert!(RuntimeEnv::Test.is_local());
        assert!(RuntimeEnv::Staging.is_deployed());
        assert!(RuntimeEnv::Production.is_deployed());
    }

    #[test]
    fn deployed_environments_refuse_fixtures_regardless_of_build() {
        for env in [RuntimeEnv::Staging, RuntimeEnv::Production] {
            let err = fixtures_allowed(env, "DMIND_MODEL_PROVIDER=fake").unwrap_err();
            assert!(err
                .to_string()
                .contains("only permitted in development or test"));
            assert!(fake_model_gateway(env).is_err());
            assert!(fake_scribe_provider(env).is_err());
        }
    }

    #[test]
    fn local_environments_allow_fixtures_only_with_the_feature() {
        let res = fixtures_allowed(RuntimeEnv::Development, "x");
        assert_eq!(res.is_ok(), DEV_FIXTURES_ENABLED);
    }

    #[test]
    fn provider_kind_defaults_to_disabled_and_rejects_unknown_values() {
        assert_eq!(
            ProviderKind::parse("V", "disabled").unwrap(),
            ProviderKind::Disabled
        );
        assert_eq!(
            ProviderKind::parse("V", "fake").unwrap(),
            ProviderKind::Fake
        );
        assert_eq!(
            ProviderKind::parse("V", "openai_compatible").unwrap(),
            ProviderKind::OpenAiCompatible
        );
        for bad in ["", "openai", "mock", "Fake", "true"] {
            assert!(ProviderKind::parse("V", bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn bcp47_tags_are_validated_structurally() {
        for ok in [
            "en",
            "es",
            "pt-BR",
            "zh-Hant-TW",
            "de-CH-1996",
            "ast",
            "sr-Latn",
        ] {
            assert!(is_bcp47_tag(ok), "{ok}");
        }
        for bad in [
            "",
            "e",
            "english",
            "en_US",
            "en-",
            "-en",
            "en--US",
            "12",
            "en-verylongsub",
        ] {
            assert!(!is_bcp47_tag(bad), "{bad}");
        }
    }

    const EP: &str = "https://stt.example.org/v1/audio/transcriptions";

    fn check(ep: &str, allow: Option<&str>, local: bool) -> anyhow::Result<()> {
        validate_external_endpoint("EP", "ALLOW", ep, allow, local)
    }

    #[test]
    fn deployed_endpoints_require_an_exact_host_allowlist() {
        let err = check(EP, None, false).unwrap_err();
        assert!(err.to_string().contains("ALLOW is required"));
        let err = check(EP, Some(" , "), false).unwrap_err();
        assert!(err.to_string().contains("ALLOW is required"));
        check(EP, Some("stt.example.org"), false).unwrap();
        check(EP, Some("other.example.org, STT.Example.ORG"), false).unwrap();
    }

    #[test]
    fn hosts_outside_the_allowlist_are_refused() {
        for ep in [
            "https://evil.example.net/v1/audio/transcriptions",
            "https://stt.example.org.evil.example.net/x",
            "https://sub.stt.example.org/x",
            "https://stt.example.org:8443/x",
        ] {
            let err = check(ep, Some("stt.example.org"), false).unwrap_err();
            assert!(err.to_string().contains("not in ALLOW"), "{ep}: {err}");
        }
        check(
            "https://stt.example.org:8443/x",
            Some("stt.example.org:8443"),
            false,
        )
        .unwrap();
    }

    #[test]
    fn allowlist_entries_are_exact_names_only() {
        let err = check(EP, Some("*.example.org"), false).unwrap_err();
        assert!(err.to_string().contains("exact host names"));
        let err = check(EP, Some("stt.example.org/v1"), false).unwrap_err();
        assert!(err.to_string().contains("exact host names"));
    }

    #[test]
    fn plain_http_ip_literals_loopback_and_embedded_credentials_are_refused_when_deployed() {
        let allow = Some("stt.example.org,localhost,10.0.0.5,127.0.0.1");
        for (ep, needle) in [
            ("http://stt.example.org/x", "https://"),
            ("ftp://stt.example.org/x", "https://"),
            ("https://10.0.0.5/x", "IP literal"),
            ("https://[::1]/x", "IP literal"),
            ("https://127.0.0.1/x", "IP literal"),
            ("https://localhost/x", "loopback"),
            ("https://stt.localhost/x", "loopback"),
            ("https://user:pw@stt.example.org/x", "embed credentials"),
            ("https:///x", "host"),
            ("not a url", "valid absolute URL"),
        ] {
            let err = check(ep, allow, false).unwrap_err();
            assert!(err.to_string().contains(needle), "{ep}: {err}");
        }
    }

    #[test]
    fn local_allows_a_loopback_http_mock_but_still_honours_an_allowlist() {
        check("http://127.0.0.1:9000/v1", None, true).unwrap();
        check("http://localhost:9000/v1", None, true).unwrap();
        check(EP, None, true).unwrap();
        let err = check("http://stt.example.org/x", None, true).unwrap_err();
        assert!(err
            .to_string()
            .contains("loopback host in development/test"));
        let err = check(EP, Some("other.example.org"), true).unwrap_err();
        assert!(err.to_string().contains("not in ALLOW"));
        let err = check("https://user:pw@stt.example.org/x", None, true).unwrap_err();
        assert!(err.to_string().contains("embed credentials"));
    }
}
