pub mod aigov;
pub mod audit;
pub mod auth;
pub mod error;
pub mod oidc;
pub mod policy;
pub mod ratelimit;
pub mod routes;
pub mod runtime;
#[cfg(feature = "dev-fixtures")]
pub mod seeddata;
pub mod state;

use axum::Router;
use state::AppState;

pub fn app(state: AppState) -> Router {
    routes::router(state)
}

/// Resolve `DATABASE_URL`. The synthetic development database is the
/// fallback only in local environments; deployed environments must configure
/// it explicitly.
pub fn database_url(env: runtime::RuntimeEnv) -> anyhow::Result<String> {
    match std::env::var("DATABASE_URL") {
        Ok(url) if !url.trim().is_empty() => Ok(url),
        _ if env.is_local() => Ok("postgres://wellos:wellos_dev@localhost:5432/wellos".into()),
        _ => anyhow::bail!("DATABASE_URL is required with WELLOS_ENV={env}"),
    }
}

pub async fn connect_pool(database_url: &str) -> anyhow::Result<sqlx::PgPool> {
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(10)
        .connect(database_url)
        .await?;
    Ok(pool)
}

pub async fn run_migrations(pool: &sqlx::PgPool) -> anyhow::Result<()> {
    sqlx::migrate!("./migrations").run(pool).await?;
    Ok(())
}
