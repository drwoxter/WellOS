#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wellos:wellos_dev@localhost:5432/wellos".into());
    let pool = wellos_server::connect_pool(&database_url).await?;
    wellos_server::run_migrations(&pool).await?;
    let existing: (i64,) = sqlx::query_as("SELECT count(*) FROM tenants")
        .fetch_one(&pool)
        .await?;
    if existing.0 > 0 {
        println!("database already seeded; skipping");
        return Ok(());
    }
    let seeded = wellos_server::seeddata::seed(&pool).await?;
    println!("seeded synthetic data:");
    println!("  tenant A (Hospital Demo Norte): {}", seeded.tenant_a);
    println!("  tenant B (Clínica Demo Sur):    {}", seeded.tenant_b);
    println!("  patient A: {}", seeded.patient_a);
    println!("dev sign-in tokens: dev-<username>, e.g. dev-dr.garcia");
    Ok(())
}
