//! Synthetic seed. Runs only when all three hold: `WELLOS_ENV` is
//! `development` or `test`, `WELLOS_ALLOW_SYNTHETIC_SEED=true`, and the
//! binary was built with `--features dev-fixtures` (this file is not compiled
//! otherwise). Staging and production are refused unconditionally.

use wellos_server::runtime::RuntimeConfig;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let runtime = RuntimeConfig::from_env()?;
    if runtime.env.is_deployed() {
        anyhow::bail!(
            "synthetic seeding is refused with WELLOS_ENV={}: it is only permitted in development or test",
            runtime.env
        );
    }
    if !runtime.allow_synthetic_seed {
        anyhow::bail!(
            "synthetic seeding requires WELLOS_ALLOW_SYNTHETIC_SEED=true (WELLOS_ENV={})",
            runtime.env
        );
    }
    let database_url = wellos_server::database_url(runtime.env)?;
    let pool = wellos_server::connect_pool(&database_url).await?;
    wellos_server::run_migrations(&pool).await?;
    let Some(seeded) = wellos_server::seeddata::seed(&pool, &runtime).await? else {
        println!("database already seeded; skipping (synthetic seed is idempotent)");
        return Ok(());
    };
    println!("seeded SYNTHETIC data (environment: {}):", runtime.env);
    println!("  tenant A (Hospital Demo Norte): {}", seeded.tenant_a);
    println!("  tenant B (Clínica Demo Sur):    {}", seeded.tenant_b);
    println!("  patient A: {}", seeded.patient_a);
    println!("dev sign-in tokens: dev-<username>, e.g. dev-dr.garcia");
    println!(
        "  (dev tokens require WELLOS_DEV_AUTH=true on a dev-fixtures build in development/test)"
    );
    // Development-only credential: random per seed run, hash-stored, expires
    // in 90 days. Printed once here so the local lab adapter can use it.
    println!(
        "lab adapter service credential (synthetic, shown once): {}",
        seeded.lab_adapter_token
    );
    Ok(())
}
