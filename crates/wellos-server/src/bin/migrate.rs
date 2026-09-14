use wellos_server::runtime::RuntimeEnv;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let env = RuntimeEnv::from_env()?;
    let database_url = wellos_server::database_url(env)?;
    let pool = wellos_server::connect_pool(&database_url).await?;
    wellos_server::run_migrations(&pool).await?;
    println!("migrations applied ({env})");
    Ok(())
}
