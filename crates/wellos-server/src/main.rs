use dmind_gateway::fake::FakeProvider;
use std::sync::Arc;
use wellos_server::state::{AppState, AuthConfig};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let env = std::env::var("WELLOS_ENV").unwrap_or_else(|_| "development".to_string());
    // The development database fallback exists only for explicit local
    // development; staging and production must configure DATABASE_URL.
    let database_url = match std::env::var("DATABASE_URL") {
        Ok(url) => url,
        Err(_) if env == "development" => {
            "postgres://wellos:wellos_dev@localhost:5432/wellos".into()
        }
        Err(_) => anyhow::bail!("DATABASE_URL is required when WELLOS_ENV is not development"),
    };
    // Browser origins must be explicit outside development; the localhost
    // default is a development convenience only.
    if env != "development" && std::env::var("WELLOS_ALLOWED_ORIGINS").is_err() {
        anyhow::bail!("WELLOS_ALLOWED_ORIGINS is required when WELLOS_ENV is not development");
    }
    let bind_addr = std::env::var("WELLOS_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());

    // Fail closed before touching the network: dev tokens outside
    // development, or a missing identity provider, abort startup.
    let auth = AuthConfig::from_env()?;
    if auth.dev_auth_enabled {
        tracing::warn!("development authentication enabled (WELLOS_DEV_AUTH=true); never use outside local development");
    }
    // OIDC discovery and the initial JWKS fetch happen before serving
    // traffic: an unreachable or mispinned provider aborts startup.
    auth.initialize().await?;

    let pool = wellos_server::connect_pool(&database_url).await?;
    wellos_server::run_migrations(&pool).await?;

    // Only the deterministic fake provider is wired in the development
    // baseline; external providers require explicit configuration, consent,
    // and policy routes (see ADR-0007).
    let gateway = Arc::new(FakeProvider::new());
    let state = AppState::with_auth(pool, gateway, auth);

    let app = wellos_server::app(state);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!(%bind_addr, "wellos-server listening");
    axum::serve(listener, app).await?;
    Ok(())
}
