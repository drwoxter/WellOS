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
    let Some(seeded) = wellos_server::seeddata::seed(&pool).await? else {
        println!("database already seeded; skipping");
        return Ok(());
    };
    println!("seeded synthetic data:");
    println!("  tenant A (Hospital Demo Norte): {}", seeded.tenant_a);
    println!("  tenant B (Clínica Demo Sur):    {}", seeded.tenant_b);
    println!("  patient A: {}", seeded.patient_a);
    println!("dev sign-in tokens: dev-<username>, e.g. dev-dr.garcia");
    println!("  (dev tokens require WELLOS_ENV=development and WELLOS_DEV_AUTH=true)");
    // Development-only credential: random per seed run, hash-stored, expires
    // in 90 days. Printed once here so the local lab adapter can use it.
    println!(
        "lab adapter service credential (dev only, shown once): {}",
        seeded.lab_adapter_token
    );
    Ok(())
}
