#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wellos:wellos_dev@localhost:5432/wellos".into());
    let pool = wellos_server::connect_pool(&database_url).await?;
    wellos_server::run_migrations(&pool).await?;
    println!("migrations applied");
    Ok(())
}
