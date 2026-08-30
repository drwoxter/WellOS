use dmind_gateway::fake::FakeProvider;
use std::sync::Arc;
use wellos_server::state::AppState;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();

    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wellos:wellos_dev@localhost:5432/wellos".into());
    let bind_addr = std::env::var("WELLOS_BIND_ADDR").unwrap_or_else(|_| "127.0.0.1:8080".into());

    let pool = wellos_server::connect_pool(&database_url).await?;
    wellos_server::run_migrations(&pool).await?;

    // Only the deterministic fake provider is wired in the development
    // baseline; external providers require explicit configuration, consent,
    // and policy routes (see ADR-0007).
    let gateway = Arc::new(FakeProvider::new());
    let state = AppState::new(pool, gateway);

    let app = wellos_server::app(state);
    let listener = tokio::net::TcpListener::bind(&bind_addr).await?;
    tracing::info!(%bind_addr, "wellos-server listening");
    axum::serve(listener, app).await?;
    Ok(())
}
