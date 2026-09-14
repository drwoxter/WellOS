use wellos_server::runtime::{model_gateway_from_env, scribe_provider_from_env, RuntimeConfig};
use wellos_server::state::{AppState, AuthConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    // One typed runtime: WELLOS_ENV is mandatory, fixtures are refused in
    // staging/production and AI providers default to disabled.
    let runtime = RuntimeConfig::from_env()?;
    let env = runtime.env;
    let database_url = wellos_server::database_url(env)?;
    // Browser origins must be explicit outside local environments; the
    // localhost default is a development convenience only.
    if env.is_deployed() && std::env::var("WELLOS_ALLOWED_ORIGINS").is_err() {
        anyhow::bail!("WELLOS_ALLOWED_ORIGINS is required with WELLOS_ENV={env}");
    }
    let bind_addr = std::env::var("WELLOS_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());

    // Fail closed before touching the network: dev tokens outside a local
    // fixture build, or a missing identity provider, abort startup.
    let auth = AuthConfig::from_env_for(env)?;
    if auth.dev_auth_enabled {
        tracing::warn!(
            "development authentication enabled (WELLOS_DEV_AUTH=true); synthetic identities only"
        );
    }
    // OIDC discovery and the initial JWKS fetch happen before serving
    // traffic: an unreachable or mispinned provider aborts startup.
    auth.initialize().await?;

    // Providers are constructed before the database so a misconfigured real
    // provider aborts startup instead of degrading to fixtures.
    let gateway = model_gateway_from_env(&runtime)?;
    let scribe = scribe_provider_from_env(&runtime)?;
    let model_status = gateway.status();
    let scribe_status = scribe.status();
    tracing::info!(
        environment = %env,
        model_provider = runtime.model_provider.as_str(),
        model_state = ?model_status.state,
        scribe_provider = runtime.scribe_provider.as_str(),
        scribe_state = ?scribe_status.state,
        fixtures_compiled = wellos_server::runtime::DEV_FIXTURES_ENABLED,
        "runtime configuration resolved"
    );

    let pool = wellos_server::connect_pool(&database_url).await?;
    wellos_server::run_migrations(&pool).await?;

    let state = AppState::from_runtime(pool, gateway, scribe, auth, runtime);

    let app = wellos_server::app(state);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!(%bind_addr, "wellos-server listening");
    // ConnectInfo carries the socket peer address for the anonymous
    // login/callback rate limiter.
    axum::serve(
        listener,
        app.into_make_service_with_connect_info::<std::net::SocketAddr>(),
    )
    .await?;
    Ok(())
}
