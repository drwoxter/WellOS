//! Upgrade-migration gate for `0019_care_operations.sql`.
//!
//! A deployment that runs the dMind evaluation release (`0018`) already holds
//! tenants, facilities, users and patients but no operational hierarchy,
//! episodes or placements. This test performs the real upgrade path:
//!
//! 1. a fresh database is migrated only through `0018`;
//! 2. a tenant, a facility, a user and a patient are created in the pre-0019
//!    schema;
//! 3. the remaining migrations are applied through the normal
//!    `wellos_server::run_migrations` mechanism;
//! 4. the Care Operations tables, the position/state invariant, the
//!    one-open-episode-per-type rule and the append-only history guards are
//!    verified against those pre-existing rows (no backfill is expected: the
//!    hierarchy is runtime-configured data, so an upgraded facility simply
//!    has no positions until an operations manager creates them);
//! 5. a second migration run is proven a no-op.
//!
//! It uses its own throwaway database (`CREATEDB` is available on the local
//! compose role and the CI service container).

use std::borrow::Cow;
use std::str::FromStr;

use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, PgPool};
use uuid::Uuid;

const LAST_PRE_CARE_OPS_MIGRATION: i64 = 18;
const GATE_DATABASE: &str = "wellos_upgrade_gate_0019";

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

fn migrator_through_0018() -> sqlx::migrate::Migrator {
    let mut migrator = sqlx::migrate!("./migrations");
    let kept: Vec<_> = migrator
        .iter()
        .filter(|m| m.version <= LAST_PRE_CARE_OPS_MIGRATION)
        .cloned()
        .collect();
    assert_eq!(
        kept.len(),
        usize::try_from(LAST_PRE_CARE_OPS_MIGRATION).unwrap(),
        "migrations 0001..0018 are the pre-care-operations schema"
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

const CARE_OPS_TABLES: &[&str] = &[
    "operational_locations",
    "operational_location_history",
    "operational_location_periods",
    "care_episodes",
    "care_episode_links",
    "care_episode_history",
    "placement_requests",
    "placement_candidates",
    "placements",
    "placement_history",
    "care_coordination_tasks",
    "care_coordination_task_dependencies",
    "care_coordination_task_history",
    "care_handoffs",
    "care_escalations",
    "theatre_sessions",
    "perioperative_cases",
    "perioperative_case_history",
    "workforce_shifts",
    "workforce_coverage_requirements",
    "workforce_shift_assignments",
    "workforce_shift_absences",
    "care_operations_forecasts",
    "care_operations_snapshots",
];

#[tokio::test]
async fn pre_0019_database_gains_care_operations_tables_and_guards() {
    let pool = recreate_gate_database().await;
    migrator_through_0018().run(&pool).await.unwrap();
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM _sqlx_migrations").await,
        LAST_PRE_CARE_OPS_MIGRATION
    );
    for table in CARE_OPS_TABLES {
        assert!(
            !table_exists(&pool, table).await,
            "{table} must not pre-exist"
        );
    }

    // Pre-0019 rows: a tenant, a facility, a user and a patient.
    let tenant = Uuid::now_v7();
    let facility = Uuid::now_v7();
    let user = Uuid::now_v7();
    let patient = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO tenants (id, cell, name, data_class) VALUES ($1, 'cell-test', 'Upgrade Gate 0019', 'synthetic')",
    )
    .bind(tenant)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO facilities (id, tenant_id, name) VALUES ($1, $2, 'Upgrade Gate Hospital')",
    )
    .bind(facility)
    .bind(tenant)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO users (id, tenant_id, username, display_name, oidc_subject)
         VALUES ($1, $2, 'upgrade.gate.ops', 'Upgrade Gate Ops', 'synthetic|upgrade-gate-0019')",
    )
    .bind(user)
    .bind(tenant)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO patients (id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier)
         VALUES ($1, $2, $3, 'Gate', 'Synthetic', '1970-01-01', 'other', 'SYN-GATE-0019')",
    )
    .bind(patient)
    .bind(tenant)
    .bind(facility)
    .execute(&pool)
    .await
    .unwrap();

    wellos_server::run_migrations(&pool).await.unwrap();
    let applied_after = scalar_i64(&pool, "SELECT COUNT(*) FROM _sqlx_migrations").await;
    assert!(applied_after > LAST_PRE_CARE_OPS_MIGRATION);
    for table in CARE_OPS_TABLES {
        assert!(table_exists(&pool, table).await, "{table} created by 0019");
        assert_eq!(
            scalar_i64(&pool, &format!("SELECT COUNT(*) FROM {table}")).await,
            0,
            "{table} is not backfilled: the hierarchy is runtime-configured data"
        );
    }
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM facilities").await,
        1
    );
    assert_eq!(scalar_i64(&pool, "SELECT COUNT(*) FROM patients").await, 1);

    // A pre-existing facility can receive the new hierarchy.
    let unit = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO operational_locations (id, tenant_id, facility_id, level, type_code, code, name_en, name_es, created_by)
         VALUES ($1, $2, $3, 'unit', 'ward', 'GATE-U1', 'Gate Unit', 'Unidad Gate', $4)",
    )
    .bind(unit)
    .bind(tenant)
    .bind(facility)
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    // A position must carry a position state, and only positions may.
    let position_without_state = sqlx::query(
        "INSERT INTO operational_locations (id, tenant_id, facility_id, parent_id, level, type_code, code, name_en, name_es, created_by)
         VALUES ($1, $2, $3, $4, 'position', 'bed', 'GATE-B1', 'Gate Bed 1', 'Cama Gate 1', $5)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(facility)
    .bind(unit)
    .bind(user)
    .execute(&pool)
    .await
    .expect_err("a position without position_state is refused");
    assert!(error_text(position_without_state).contains("check"));
    let unit_with_state = sqlx::query(
        "INSERT INTO operational_locations (id, tenant_id, facility_id, level, type_code, code, name_en, name_es, position_state, created_by)
         VALUES ($1, $2, $3, 'unit', 'ward', 'GATE-U2', 'Gate Unit 2', 'Unidad Gate 2', 'available', $4)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(facility)
    .bind(user)
    .execute(&pool)
    .await
    .expect_err("a non-position with position_state is refused");
    assert!(error_text(unit_with_state).contains("check"));
    let bed = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO operational_locations (id, tenant_id, facility_id, parent_id, level, type_code, code, name_en, name_es, position_state, created_by)
         VALUES ($1, $2, $3, $4, 'position', 'bed', 'GATE-B1', 'Gate Bed 1', 'Cama Gate 1', 'available', $5)",
    )
    .bind(bed)
    .bind(tenant)
    .bind(facility)
    .bind(unit)
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();

    // A pre-existing patient can open one episode per type; a second open one
    // of the same type is refused, and a terminal one does not block.
    let episode = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO care_episodes (id, tenant_id, facility_id, patient_id, episode_type_code, status, started_at, created_by)
         VALUES ($1, $2, $3, $4, 'inpatient', 'active', now(), $5)",
    )
    .bind(episode)
    .bind(tenant)
    .bind(facility)
    .bind(patient)
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    let duplicate = sqlx::query(
        "INSERT INTO care_episodes (id, tenant_id, facility_id, patient_id, episode_type_code, status, created_by)
         VALUES ($1, $2, $3, $4, 'inpatient', 'planned', $5)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(facility)
    .bind(patient)
    .bind(user)
    .execute(&pool)
    .await
    .expect_err("one open episode per type per patient");
    assert!(error_text(duplicate).contains("care_episodes_one_open_per_type"));
    sqlx::query(
        "INSERT INTO care_episodes (id, tenant_id, facility_id, patient_id, episode_type_code, status, started_at, ended_at, created_by)
         VALUES ($1, $2, $3, $4, 'emergency', 'completed', now() - interval '2 days', now() - interval '1 day', $5)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(facility)
    .bind(patient)
    .bind(user)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO care_episodes (id, tenant_id, facility_id, patient_id, episode_type_code, status, created_by)
         VALUES ($1, $2, $3, $4, 'emergency', 'planned', $5)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(facility)
    .bind(patient)
    .bind(user)
    .execute(&pool)
    .await
    .expect("a completed episode does not block a new one of the same type");
    let invalid_status = sqlx::query(
        "INSERT INTO care_episodes (id, tenant_id, facility_id, patient_id, episode_type_code, status, created_by)
         VALUES ($1, $2, $3, $4, 'rehabilitation', 'discharged', $5)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(facility)
    .bind(patient)
    .bind(user)
    .execute(&pool)
    .await
    .expect_err("episode status is constrained to the state machine");
    assert!(error_text(invalid_status).contains("check"));

    // History is append-only.
    let history = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO care_episode_history (id, tenant_id, episode_id, from_status, to_status, change_kind, actor, version)
         VALUES ($1, $2, $3, NULL, 'active', 'created', 'upgrade-gate', 1)",
    )
    .bind(history)
    .bind(tenant)
    .bind(episode)
    .execute(&pool)
    .await
    .unwrap();
    let mutated =
        sqlx::query("UPDATE care_episode_history SET to_status = 'completed' WHERE id = $1")
            .bind(history)
            .execute(&pool)
            .await
            .expect_err("history rows cannot be updated");
    assert!(error_text(mutated).contains("append-only"));
    let deleted = sqlx::query("DELETE FROM care_episode_history WHERE id = $1")
        .bind(history)
        .execute(&pool)
        .await
        .expect_err("history rows cannot be deleted");
    assert!(error_text(deleted).contains("append-only"));

    // Second migration run is a no-op and keeps the rows.
    wellos_server::run_migrations(&pool).await.unwrap();
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM _sqlx_migrations").await,
        applied_after
    );
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM care_episodes").await,
        3
    );
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM operational_locations").await,
        2
    );

    drop_gate_database(pool).await;
}
