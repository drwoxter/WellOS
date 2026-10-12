//! Upgrade-migration gate for `0018_dmind_evaluation.sql`.
//!
//! A deployment that runs the Experience Reset (`0017`) holds synthetic and
//! non-synthetic tenants but no evaluation tables. This test performs the
//! real upgrade path:
//!
//! 1. a fresh database is migrated only through `0017`;
//! 2. a synthetic tenant, a non-synthetic tenant and a user are created in the
//!    pre-0018 schema;
//! 3. the remaining migrations are applied through the normal
//!    `wellos_server::run_migrations` mechanism;
//! 4. the evaluation tables, the synthetic-only guard, the append-only guard
//!    and the run state machine are verified against those pre-existing rows;
//! 5. a second migration run is proven a no-op.
//!
//! It uses its own throwaway database (`CREATEDB` is available on the local
//! compose role and the CI service container).
use std::borrow::Cow;
use std::str::FromStr;

use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, PgPool};
use uuid::Uuid;

const LAST_PRE_EVALUATION_MIGRATION: i64 = 17;
const GATE_DATABASE: &str = "wellos_upgrade_gate_0018";

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wellos:wellos_dev@localhost:5432/wellos".to_string())
}

async fn admin_connection() -> PgConnection {
    PgConnection::connect_with(&PgConnectOptions::from_str(&database_url()).unwrap())
        .await
        .unwrap()
}

async fn recreate_gate_database() -> PgPool {
    let mut admin = admin_connection().await;
    sqlx::query(&format!(
        "DROP DATABASE IF EXISTS {GATE_DATABASE} WITH (FORCE)"
    ))
    .execute(&mut admin)
    .await
    .unwrap();
    sqlx::query(&format!("CREATE DATABASE {GATE_DATABASE}"))
        .execute(&mut admin)
        .await
        .unwrap();
    let options = PgConnectOptions::from_str(&database_url())
        .unwrap()
        .database(GATE_DATABASE);
    sqlx::postgres::PgPoolOptions::new()
        .max_connections(4)
        .connect_with(options)
        .await
        .unwrap()
}

async fn drop_gate_database(pool: PgPool) {
    pool.close().await;
    let mut admin = admin_connection().await;
    sqlx::query(&format!(
        "DROP DATABASE IF EXISTS {GATE_DATABASE} WITH (FORCE)"
    ))
    .execute(&mut admin)
    .await
    .unwrap();
}

fn migrator_through_0017() -> sqlx::migrate::Migrator {
    let mut migrator = sqlx::migrate!("./migrations");
    let kept: Vec<_> = migrator
        .iter()
        .filter(|m| m.version <= LAST_PRE_EVALUATION_MIGRATION)
        .cloned()
        .collect();
    assert_eq!(
        kept.len(),
        usize::try_from(LAST_PRE_EVALUATION_MIGRATION).unwrap(),
        "migrations 0001..0017 are the pre-evaluation schema"
    );
    migrator.migrations = Cow::Owned(kept);
    migrator
}

async fn scalar_i64(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

async fn table_exists(pool: &PgPool, table: &str) -> bool {
    sqlx::query_scalar::<_, bool>(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_schema = 'public' AND table_name = $1)",
    )
    .bind(table)
    .fetch_one(pool)
    .await
    .unwrap()
}

fn error_text(err: sqlx::Error) -> String {
    match err {
        sqlx::Error::Database(db) => db.message().to_string(),
        other => other.to_string(),
    }
}

#[tokio::test]
async fn pre_0018_database_gains_synthetic_only_append_only_evaluation_tables() {
    let pool = recreate_gate_database().await;

    migrator_through_0017().run(&pool).await.unwrap();
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM _sqlx_migrations").await,
        LAST_PRE_EVALUATION_MIGRATION
    );
    for table in ["dmind_eval_suites", "dmind_eval_runs", "dmind_eval_reviews"] {
        assert!(
            !table_exists(&pool, table).await,
            "{table} must not pre-exist"
        );
    }

    let synthetic = Uuid::now_v7();
    let real = Uuid::now_v7();
    let admin = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO tenants (id, cell, name, data_class) VALUES ($1, 'cell-test', 'Upgrade Gate Synthetic', 'synthetic'), ($2, 'cell-test', 'Upgrade Gate Real', 'production')",
    )
    .bind(synthetic)
    .bind(real)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO users (id, tenant_id, username, display_name, oidc_subject)
         VALUES ($1, $2, 'upgrade.gate.admin', 'Upgrade Gate Admin', 'synthetic|upgrade-gate-0018')",
    )
    .bind(admin)
    .bind(synthetic)
    .execute(&pool)
    .await
    .unwrap();

    wellos_server::run_migrations(&pool).await.unwrap();
    let applied_after = scalar_i64(&pool, "SELECT COUNT(*) FROM _sqlx_migrations").await;
    assert!(applied_after > LAST_PRE_EVALUATION_MIGRATION);
    for table in [
        "dmind_eval_suites",
        "dmind_eval_cases",
        "dmind_eval_provider_fingerprints",
        "dmind_eval_runs",
        "dmind_eval_executions",
        "dmind_eval_metrics",
        "dmind_eval_reviews",
        "dmind_eval_run_summaries",
    ] {
        assert!(table_exists(&pool, table).await, "{table} created by 0018");
        assert_eq!(
            scalar_i64(&pool, &format!("SELECT COUNT(*) FROM {table}")).await,
            0,
            "{table} starts empty: no backfill of evaluation data"
        );
    }
    // Pre-existing rows are untouched by the upgrade.
    assert_eq!(scalar_i64(&pool, "SELECT COUNT(*) FROM tenants").await, 2);
    assert_eq!(scalar_i64(&pool, "SELECT COUNT(*) FROM users").await, 1);

    // Synthetic-only guard applies to tenants that existed before the upgrade.
    let suite_sql = "INSERT INTO dmind_eval_suites (id, tenant_id, suite_key, suite_version, rubric_version, languages, case_count, corpus_hash)
         VALUES ($1, $2, 'dmind-core', '1.0.0', 'dmind-eval-rubric.v1', ARRAY['en','es'], 55, 'hash')";
    let refused = sqlx::query(suite_sql)
        .bind(Uuid::now_v7())
        .bind(real)
        .execute(&pool)
        .await
        .expect_err("non-synthetic tenant must be refused");
    assert!(
        error_text(refused).contains("data_class is not synthetic"),
        "synthetic guard message"
    );
    let suite = Uuid::now_v7();
    sqlx::query(suite_sql)
        .bind(suite)
        .bind(synthetic)
        .execute(&pool)
        .await
        .unwrap();

    // Append-only guard.
    let mutated = sqlx::query("UPDATE dmind_eval_suites SET case_count = 1 WHERE id = $1")
        .bind(suite)
        .execute(&pool)
        .await
        .expect_err("suites are append-only");
    assert!(error_text(mutated).contains("append-only"));
    let deleted = sqlx::query("DELETE FROM dmind_eval_suites WHERE id = $1")
        .bind(suite)
        .execute(&pool)
        .await
        .expect_err("suites cannot be deleted");
    assert!(error_text(deleted).contains("append-only"));

    // Run state machine: planned -> running -> completed with version bumps;
    // skipping the version or jumping states is refused; runs are never deleted.
    let fingerprint = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO dmind_eval_provider_fingerprints (id, tenant_id, provider, model, model_version, external, synthetic_provider, config_hash)
         VALUES ($1, $2, 'fake', 'dmind-fake', 'v1', false, true, 'cfg-hash')",
    )
    .bind(fingerprint)
    .bind(synthetic)
    .execute(&pool)
    .await
    .unwrap();
    let run = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO dmind_eval_runs (id, tenant_id, suite_id, fingerprint_id, mode, environment, requested_by, requested_via, budget, gate, status)
         VALUES ($1, $2, $3, $4, 'offline', 'test', $5, 'cli', '{}'::jsonb, '{}'::jsonb, 'planned')",
    )
    .bind(run)
    .bind(synthetic)
    .bind(suite)
    .bind(fingerprint)
    .bind(admin)
    .execute(&pool)
    .await
    .unwrap();
    let jump =
        sqlx::query("UPDATE dmind_eval_runs SET status = 'completed', version = 2 WHERE id = $1")
            .bind(run)
            .execute(&pool)
            .await
            .expect_err("planned cannot jump to completed");
    assert!(error_text(jump).contains("cannot move from planned to completed"));
    let stale =
        sqlx::query("UPDATE dmind_eval_runs SET status = 'running', version = 1 WHERE id = $1")
            .bind(run)
            .execute(&pool)
            .await
            .expect_err("state change requires a version bump");
    assert!(error_text(stale).contains("requires version 2"));
    sqlx::query("UPDATE dmind_eval_runs SET status = 'running', version = 2 WHERE id = $1")
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    let retarget = sqlx::query(
        "UPDATE dmind_eval_runs SET mode = 'live', status = 'completed', version = 3 WHERE id = $1",
    )
    .bind(run)
    .execute(&pool)
    .await
    .expect_err("mode is immutable");
    assert!(error_text(retarget).contains("immutable except for its state"));
    sqlx::query("UPDATE dmind_eval_runs SET status = 'completed', version = 3 WHERE id = $1")
        .bind(run)
        .execute(&pool)
        .await
        .unwrap();
    let delete = sqlx::query("DELETE FROM dmind_eval_runs WHERE id = $1")
        .bind(run)
        .execute(&pool)
        .await
        .expect_err("runs cannot be deleted");
    assert!(error_text(delete).contains("cannot be deleted"));
    let real_run = sqlx::query(
        "INSERT INTO dmind_eval_runs (id, tenant_id, suite_id, fingerprint_id, mode, environment, requested_via, budget, gate, status, data_class)
         VALUES ($1, $2, $3, $4, 'offline', 'test', 'cli', '{}'::jsonb, '{}'::jsonb, 'planned', 'real')",
    )
    .bind(Uuid::now_v7())
    .bind(synthetic)
    .bind(suite)
    .bind(fingerprint)
    .execute(&pool)
    .await
    .expect_err("data_class is constrained to synthetic");
    assert!(error_text(real_run).contains("data_class"));

    // Second migration run is a no-op.
    wellos_server::run_migrations(&pool).await.unwrap();
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM _sqlx_migrations").await,
        applied_after
    );
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM dmind_eval_runs").await,
        1
    );

    drop_gate_database(pool).await;
}
