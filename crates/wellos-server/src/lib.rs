pub mod audit;
pub mod auth;
pub mod error;
pub mod oidc;
pub mod policy;
pub mod routes;
pub mod seeddata;
pub mod state;

use axum::Router;
use state::AppState;

pub fn app(state: AppState) -> Router {
    routes::router(state)
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
