//! Upgrade-migration gate for `0016_dmind_clinical_diagnostics.sql`.
//!
//! A deployment that runs dMind Access (`0015`) already holds legacy
//! laboratory orders in the pre-diagnostics shape: `service_requests` keyed by
//! LOINC with a result-loop state, numeric `observations` and
//! `rule_evaluations`. This test performs the real upgrade path:
//!
//! 1. a fresh database is migrated only through `0015`;
//! 2. representative legacy laboratory orders, observations and rule
//!    evaluations are created in the pre-0016 schema;
//! 3. the remaining migrations are applied through the normal
//!    `wellos_server::run_migrations` mechanism;
//! 4. orders, derived orderables, order history and observations are
//!    verified to be adopted losslessly;
//! 5. a second migration run and a second adoption call are proven no-ops.
//!
//! It uses its own throwaway database (`CREATEDB` is available on the local
//! compose role and the CI service container).

use std::borrow::Cow;
use std::str::FromStr;

use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, PgPool};
use uuid::Uuid;

const LAST_PRE_DIAGNOSTICS_MIGRATION: i64 = 15;
const GATE_DATABASE: &str = "wellos_upgrade_gate_0016";

type Ts = chrono::DateTime<chrono::Utc>;
/// `(code_loinc, loop_state, status, version, orderable_code, category_code,
/// order_status, fulfilment_mode, completed_at, source_system, orderable_id)`.
type AdoptedOrder = (
    Option<String>,
    String,
    String,
    i64,
    Option<String>,
    Option<String>,
    String,
    String,
    Option<Ts>,
    Option<String>,
    Option<Uuid>,
);

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

fn migrator_through_0015() -> sqlx::migrate::Migrator {
    let mut migrator = sqlx::migrate!("./migrations");
    let kept: Vec<_> = migrator
        .iter()
        .filter(|m| m.version <= LAST_PRE_DIAGNOSTICS_MIGRATION)
        .cloned()
        .collect();
    assert_eq!(
        kept.len(),
        usize::try_from(LAST_PRE_DIAGNOSTICS_MIGRATION).unwrap(),
        "migrations 0001..0015 are the pre-diagnostics schema"
    );
    migrator.migrations = Cow::Owned(kept);
    migrator
}

async fn scalar_i64(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

async fn snapshot(pool: &PgPool) -> (i64, i64, i64, i64, i64, i64) {
    (
        scalar_i64(pool, "SELECT COUNT(*) FROM _sqlx_migrations").await,
        scalar_i64(pool, "SELECT COUNT(*) FROM catalog_entries").await,
        scalar_i64(pool, "SELECT COUNT(*) FROM catalog_entry_history").await,
        scalar_i64(pool, "SELECT COUNT(*) FROM service_request_history").await,
        scalar_i64(pool, "SELECT COUNT(*) FROM observations").await,
        scalar_i64(
            pool,
            "SELECT COALESCE(SUM(version), 0)::bigint FROM service_requests",
        )
        .await,
    )
}

struct LegacyOrder {
    id: Uuid,
    loinc: &'static str,
    display: &'static str,
    status: &'static str,
    loop_state: &'static str,
    /// `(value, unit, critical)` when a result was received.
    result: Option<(&'static str, &'static str, bool)>,
    expected_order_status: &'static str,
}

#[tokio::test]
async fn pre_0016_database_adopts_legacy_laboratory_orders_exactly_once() {
    let pool = recreate_gate_database().await;

    // 1. Access schema only.
    migrator_through_0015().run(&pool).await.unwrap();
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM _sqlx_migrations").await,
        LAST_PRE_DIAGNOSTICS_MIGRATION
    );
    let has_reports: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'diagnostic_reports')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!has_reports, "0016 must not be applied yet");

    // 2. Legacy data as the pre-0016 result loop created it.
    let tenant = Uuid::now_v7();
    let facility = Uuid::now_v7();
    let physician = Uuid::now_v7();
    let patient = Uuid::now_v7();
    let encounter = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO tenants (id, cell, name, data_class) VALUES ($1, 'cell-test', 'Upgrade Gate Tenant', 'synthetic')",
    )
    .bind(tenant)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO facilities (id, tenant_id, name) VALUES ($1, $2, 'Upgrade Gate Clinic')",
    )
    .bind(facility)
    .bind(tenant)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO users (id, tenant_id, username, display_name, oidc_subject)
         VALUES ($1, $2, 'upgrade.gate.physician', 'Upgrade Gate Physician', 'synthetic|upgrade-gate-0016')",
    )
    .bind(physician)
    .bind(tenant)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO patients (id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier)
         VALUES ($1, $2, $3, 'Synthetic', 'Upgrade', DATE '1975-05-05', 'male', 'SYN-UPGRADE-0016')",
    )
    .bind(patient)
    .bind(tenant)
    .bind(facility)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO encounters (id, tenant_id, facility_id, patient_id, practitioner_id, status)
         VALUES ($1, $2, $3, $4, $5, 'completed')",
    )
    .bind(encounter)
    .bind(tenant)
    .bind(facility)
    .bind(patient)
    .bind(physician)
    .execute(&pool)
    .await
    .unwrap();

    let orders = [
        LegacyOrder {
            id: Uuid::now_v7(),
            loinc: "2823-3",
            display: "Potassium [Moles/volume] in Serum or Plasma",
            status: "active",
            loop_state: "ordered",
            result: None,
            expected_order_status: "placed",
        },
        LegacyOrder {
            id: Uuid::now_v7(),
            loinc: "2823-3",
            display: "Potassium [Moles/volume] in Serum or Plasma",
            status: "active",
            loop_state: "received",
            result: Some(("6.8", "mmol/L", true)),
            expected_order_status: "completed",
        },
        LegacyOrder {
            id: Uuid::now_v7(),
            loinc: "2345-7",
            display: "Glucose [Mass/volume] in Serum or Plasma",
            status: "active",
            loop_state: "closed",
            result: Some(("92", "mg/dL", false)),
            expected_order_status: "completed",
        },
        LegacyOrder {
            id: Uuid::now_v7(),
            loinc: "2345-7",
            display: "Glucose [Mass/volume] in Serum or Plasma",
            status: "cancelled",
            loop_state: "ordered",
            result: None,
            expected_order_status: "cancelled",
        },
    ];
    for (idx, order) in orders.iter().enumerate() {
        sqlx::query(
            "INSERT INTO service_requests (id, tenant_id, encounter_id, patient_id, requester_id, code_loinc, display,
                                           status, loop_state, version, created_at)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, 3, TIMESTAMPTZ '2026-02-01 08:00+00' + make_interval(mins => $10))",
        )
        .bind(order.id)
        .bind(tenant)
        .bind(encounter)
        .bind(patient)
        .bind(physician)
        .bind(order.loinc)
        .bind(order.display)
        .bind(order.status)
        .bind(order.loop_state)
        .bind(i32::try_from(idx).unwrap())
        .execute(&pool)
        .await
        .unwrap();
        if let Some((value, unit, critical)) = order.result {
            let observation = Uuid::now_v7();
            sqlx::query(
                "INSERT INTO observations (id, tenant_id, service_request_id, patient_id, code_loinc, value_num, unit,
                                           reference_range, status, source_system, idempotency_key, effective_at, received_at)
                 VALUES ($1, $2, $3, $4, $5, $6::numeric, $7, '3.5-5.1', 'final', 'legacy-lis', $8,
                         TIMESTAMPTZ '2026-02-01 09:00+00', TIMESTAMPTZ '2026-02-01 09:30+00')",
            )
            .bind(observation)
            .bind(tenant)
            .bind(order.id)
            .bind(patient)
            .bind(order.loinc)
            .bind(value)
            .bind(unit)
            .bind(format!("legacy-{}", order.id))
            .execute(&pool)
            .await
            .unwrap();
            let outcome = if critical {
                serde_json::json!({ "Critical": { "threshold": "6.0", "direction": "high" } })
            } else {
                serde_json::json!({ "Normal": {} })
            };
            sqlx::query(
                "INSERT INTO rule_evaluations (id, tenant_id, observation_id, rule_id, rule_version, outcome)
                 VALUES ($1, $2, $3, 'critical-potassium', 'v1', $4)",
            )
            .bind(Uuid::now_v7())
            .bind(tenant)
            .bind(observation)
            .bind(outcome)
            .execute(&pool)
            .await
            .unwrap();
        }
    }
    let observations_before: Vec<(Uuid, Uuid, String, rust_decimal::Decimal, String, String)> =
        sqlx::query_as(
            "SELECT id, service_request_id, code_loinc, value_num, unit, status FROM observations ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(observations_before.len(), 2);
    let orders_before = scalar_i64(&pool, "SELECT COUNT(*) FROM service_requests").await;

    // 3. The normal upgrade path.
    wellos_server::run_migrations(&pool).await.unwrap();
    let applied_after = scalar_i64(&pool, "SELECT COUNT(*) FROM _sqlx_migrations").await;
    assert!(
        applied_after > LAST_PRE_DIAGNOSTICS_MIGRATION,
        "0016 applied"
    );

    // 4a. One orderable per (tenant, LOINC), version 1 with its history row.
    let orderables: Vec<(String, String, serde_json::Value, i64, Option<Uuid>)> = sqlx::query_as(
        "SELECT code, name_en, config, version, created_by FROM catalog_entries
         WHERE tenant_id = $1 AND kind = 'diagnostic_orderable' ORDER BY code",
    )
    .bind(tenant)
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(orderables.len(), 2, "{orderables:?}");
    assert_eq!(orderables[0].0, "loinc-2345-7");
    assert_eq!(orderables[1].0, "loinc-2823-3");
    for (code, name, config, version, created_by) in &orderables {
        assert_eq!(*version, 1);
        assert_eq!(*created_by, Some(physician));
        assert_eq!(config["legacy_migrated"], true, "{code}");
        assert_eq!(config["category_code"], "laboratory");
        assert_eq!(config["result_type"], "quantity");
        let loinc = code.trim_start_matches("loinc-");
        assert_eq!(config["components"][0]["code"], loinc, "{config}");
        assert_eq!(config["components"][0]["display"], name.as_str());
    }
    assert_eq!(
        scalar_i64(
            &pool,
            "SELECT COUNT(*) FROM catalog_entry_history h JOIN catalog_entries c ON c.id = h.entry_id
             WHERE c.kind = 'diagnostic_orderable' AND h.version = 1
               AND h.change_reason = 'migrated from legacy laboratory order'"
        )
        .await,
        2
    );

    // 4b. Every legacy order is adopted in place: same id, LOINC, loop state
    //     and version; generalized fields derived deterministically.
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM service_requests").await,
        orders_before
    );
    for order in &orders {
        let row: AdoptedOrder = sqlx::query_as(
            "SELECT sr.code_loinc, sr.loop_state, sr.status, sr.version, sr.orderable_code, sr.category_code,
                    sr.order_status, sr.fulfilment_mode, sr.completed_at, sr.source_system, c.id
             FROM service_requests sr LEFT JOIN catalog_entries c ON c.id = sr.orderable_id
             WHERE sr.id = $1",
        )
        .bind(order.id)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0.as_deref(), Some(order.loinc));
        assert_eq!(row.1, order.loop_state, "loop state preserved");
        assert_eq!(row.2, order.status, "legacy status preserved");
        assert_eq!(row.3, 3, "version preserved");
        assert_eq!(
            row.4.as_deref(),
            Some(format!("loinc-{}", order.loinc).as_str())
        );
        assert_eq!(row.5.as_deref(), Some("laboratory"));
        assert_eq!(row.6, order.expected_order_status, "{}", order.id);
        assert_eq!(row.7, "immediate");
        assert_eq!(row.8.is_some(), order.expected_order_status == "completed");
        assert_eq!(row.9.as_deref(), Some("legacy_lab_order"));
        assert!(row.10.is_some(), "orderable linked");
        let history: Vec<(Option<String>, String, i64, String, String, serde_json::Value)> = sqlx::query_as(
            "SELECT from_status, to_status, version, reason, actor, details FROM service_request_history
             WHERE service_request_id = $1",
        )
        .bind(order.id)
        .fetch_all(&pool)
        .await
        .unwrap();
        assert_eq!(history.len(), 1, "exactly one migration history row");
        let h = &history[0];
        assert!(h.0.is_none());
        assert_eq!(h.1, order.expected_order_status);
        assert_eq!(h.2, 3);
        assert_eq!(h.3, "migrated from legacy laboratory order");
        assert_eq!(h.4, "migration:0016");
        assert_eq!(h.5["loop_state"], order.loop_state);
        assert_eq!(h.5["code_loinc"], order.loinc);
    }

    // 4c. Observations are untouched apart from the typed-value defaults and
    //     the deterministic critical interpretation.
    let observations_after: Vec<(Uuid, Uuid, String, rust_decimal::Decimal, String, String)> =
        sqlx::query_as(
            "SELECT id, service_request_id, code_loinc, value_num, unit, status FROM observations ORDER BY id",
        )
        .fetch_all(&pool)
        .await
        .unwrap();
    assert_eq!(
        observations_after, observations_before,
        "no observation rewritten"
    );
    let interpretations: Vec<(String, String)> = sqlx::query_as(
        "SELECT o.code_loinc, o.interpretation FROM observations o ORDER BY o.code_loinc",
    )
    .fetch_all(&pool)
    .await
    .unwrap();
    assert_eq!(
        interpretations,
        vec![
            ("2345-7".to_string(), "unknown".to_string()),
            ("2823-3".to_string(), "critical".to_string()),
        ]
    );
    assert_eq!(
        scalar_i64(
            &pool,
            "SELECT COUNT(*) FROM observations WHERE value_type <> 'quantity'"
        )
        .await,
        0
    );
    let completed_at: Option<Ts> =
        sqlx::query_scalar("SELECT completed_at FROM service_requests WHERE id = $1")
            .bind(orders[1].id)
            .fetch_one(&pool)
            .await
            .unwrap();
    assert_eq!(
        completed_at.map(|t| t.to_rfc3339()),
        Some("2026-02-01T09:30:00+00:00".to_string()),
        "completion is the first received result"
    );

    // 5. Idempotent: a second migration run and a second adoption call change nothing.
    let before = snapshot(&pool).await;
    wellos_server::run_migrations(&pool).await.unwrap();
    sqlx::query("SELECT wellos_adopt_legacy_lab_orders()")
        .execute(&pool)
        .await
        .unwrap();
    assert_eq!(snapshot(&pool).await, before, "second run is a no-op");

    drop_gate_database(pool).await;
}
