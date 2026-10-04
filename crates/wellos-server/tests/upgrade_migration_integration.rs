//! Upgrade-migration gate for `0015_dmind_access.sql`.
//!
//! Unlike the in-place legacy test in `access_scheduling_integration.rs`
//! (which replays the migration block inside an already migrated database),
//! this test performs the real upgrade path an existing deployment takes:
//!
//! 1. a fresh database is migrated only through `0014`;
//! 2. representative scheduled visits are created in the pre-0015 schema;
//! 3. the remaining migrations are applied through the normal
//!    `wellos_server::run_migrations` mechanism;
//! 4. visit counts, appointment links, statuses and history are verified;
//! 5. a second run is proven to be a no-op.
//!
//! It needs `CREATEDB` on the configured role (true for the local compose
//! database and the CI service container) and uses its own throwaway
//! database, so it never touches the shared test dataset.

use std::borrow::Cow;
use std::str::FromStr;

use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection, PgPool};
use uuid::Uuid;

const LAST_PRE_ACCESS_MIGRATION: i64 = 14;
const GATE_DATABASE: &str = "wellos_upgrade_gate";

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

/// The shipped migration set truncated at `0014`: exactly what a deployment
/// that predates dMind Access has applied.
fn migrator_through_0014() -> sqlx::migrate::Migrator {
    let mut migrator = sqlx::migrate!("./migrations");
    let kept: Vec<_> = migrator
        .iter()
        .filter(|m| m.version <= LAST_PRE_ACCESS_MIGRATION)
        .cloned()
        .collect();
    assert_eq!(
        kept.len(),
        usize::try_from(LAST_PRE_ACCESS_MIGRATION).unwrap(),
        "migrations 0001..0014 are the pre-Access schema"
    );
    migrator.migrations = Cow::Owned(kept);
    migrator
}

type Ts = chrono::DateTime<chrono::Utc>;
type VisitRow = (Uuid, String, Option<Ts>, i64);
type AppointmentRow = (Uuid, String, String, Option<Uuid>, String, Option<Ts>);
type AppointmentSnapshot = (Uuid, String, i64, Option<Uuid>);
type AppointmentTimestamps = (Option<Ts>, Option<Ts>, Option<Ts>, Option<String>);

struct LegacyVisit {
    id: Uuid,
    visit_status: &'static str,
    expected_appointment: Option<&'static str>,
}

async fn scalar_i64(pool: &PgPool, sql: &str) -> i64 {
    sqlx::query_scalar(sql).fetch_one(pool).await.unwrap()
}

#[tokio::test]
async fn pre_0015_database_upgrades_scheduled_visits_exactly_once() {
    let pool = recreate_gate_database().await;

    // 1. Pre-Access schema only.
    migrator_through_0014().run(&pool).await.unwrap();
    let applied = scalar_i64(&pool, "SELECT COUNT(*) FROM _sqlx_migrations").await;
    assert_eq!(applied, LAST_PRE_ACCESS_MIGRATION);
    let has_appointments: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM information_schema.tables WHERE table_name = 'appointments')",
    )
    .fetch_one(&pool)
    .await
    .unwrap();
    assert!(!has_appointments, "0015 must not be applied yet");

    // 2. Representative legacy data as the pre-0015 access board created it.
    let tenant = Uuid::now_v7();
    let facility = Uuid::now_v7();
    let staff = Uuid::now_v7();
    let patient = Uuid::now_v7();
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
         VALUES ($1, $2, 'upgrade.gate.registration', 'Upgrade Gate Registration', 'synthetic|upgrade-gate')",
    )
    .bind(staff)
    .bind(tenant)
    .execute(&pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO patients (id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier)
         VALUES ($1, $2, $3, 'Synthetic', 'Upgrade', DATE '1980-01-01', 'female', 'SYN-UPGRADE-0001')",
    )
    .bind(patient)
    .bind(tenant)
    .bind(facility)
    .execute(&pool)
    .await
    .unwrap();

    let visits = [
        LegacyVisit {
            id: Uuid::now_v7(),
            visit_status: "scheduled",
            expected_appointment: Some("confirmed"),
        },
        LegacyVisit {
            id: Uuid::now_v7(),
            visit_status: "cancelled",
            expected_appointment: Some("cancelled"),
        },
        LegacyVisit {
            id: Uuid::now_v7(),
            visit_status: "no_show",
            expected_appointment: Some("no_show"),
        },
        LegacyVisit {
            id: Uuid::now_v7(),
            visit_status: "completed",
            expected_appointment: Some("fulfilled"),
        },
        LegacyVisit {
            id: Uuid::now_v7(),
            visit_status: "closed",
            expected_appointment: Some("fulfilled"),
        },
        // Walk-ins are operational episodes, never scheduling records.
        LegacyVisit {
            id: Uuid::now_v7(),
            visit_status: "waiting_triage",
            expected_appointment: None,
        },
    ];
    for (idx, visit) in visits.iter().enumerate() {
        let scheduled = visit.expected_appointment.is_some();
        let closed_reason = match visit.visit_status {
            "cancelled" => Some("cancelled"),
            "no_show" => Some("no_show"),
            "closed" => Some("completed"),
            _ => None,
        };
        sqlx::query(
            "INSERT INTO visits (id, tenant_id, facility_id, patient_id, status, arrival_kind, service, reason,
                                 scheduled_at, arrived_at, completed_at, closed_at, closed_reason, created_by,
                                 created_at, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, 'general_medicine', 'legacy follow-up',
                     CASE WHEN $6 = 'scheduled' THEN TIMESTAMPTZ '2026-03-02 09:00+00' + make_interval(days => $7) END,
                     CASE WHEN $6 <> 'scheduled' THEN TIMESTAMPTZ '2026-03-01 08:00+00' END,
                     CASE WHEN $5 IN ('completed', 'closed') THEN TIMESTAMPTZ '2026-03-01 10:00+00' END,
                     CASE WHEN $8::text IS NOT NULL THEN TIMESTAMPTZ '2026-03-01 11:00+00' END,
                     $8, $9,
                     TIMESTAMPTZ '2026-02-20 12:00+00', TIMESTAMPTZ '2026-02-21 12:00+00')",
        )
        .bind(visit.id)
        .bind(tenant)
        .bind(facility)
        .bind(patient)
        .bind(visit.visit_status)
        .bind(if scheduled { "scheduled" } else { "walk_in" })
        .bind(i32::try_from(idx).unwrap())
        .bind(closed_reason)
        .bind(staff)
        .execute(&pool)
        .await
        .unwrap();
    }
    let visits_before = scalar_i64(&pool, "SELECT COUNT(*) FROM visits").await;
    assert_eq!(visits_before, visits.len() as i64);
    let visit_rows_before: Vec<VisitRow> =
        sqlx::query_as("SELECT id, status, scheduled_at, version FROM visits ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();

    // 3. The normal upgrade path.
    wellos_server::run_migrations(&pool).await.unwrap();
    let applied_after = scalar_i64(&pool, "SELECT COUNT(*) FROM _sqlx_migrations").await;
    assert!(applied_after > LAST_PRE_ACCESS_MIGRATION, "0015 applied");

    // 4. Visits are untouched apart from the new link; every scheduled visit
    //    has exactly one appointment with the mapped status and its history.
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM visits").await,
        visits_before
    );
    let visit_rows_after: Vec<VisitRow> =
        sqlx::query_as("SELECT id, status, scheduled_at, version FROM visits ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(visit_rows_after, visit_rows_before, "no visit history lost");

    let expected_appointments = visits
        .iter()
        .filter(|v| v.expected_appointment.is_some())
        .count() as i64;
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM appointments").await,
        expected_appointments
    );
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM appointment_history").await,
        expected_appointments
    );

    for visit in &visits {
        let rows: Vec<AppointmentRow> = sqlx::query_as(
            "SELECT a.id, a.status, a.booked_via, v.appointment_id, a.service_code, a.starts_at
                 FROM appointments a JOIN visits v ON v.id = a.visit_id WHERE a.visit_id = $1",
        )
        .bind(visit.id)
        .fetch_all(&pool)
        .await
        .unwrap();
        match visit.expected_appointment {
            None => {
                assert!(
                    rows.is_empty(),
                    "walk-in visit must not become an appointment"
                );
                let link: Option<Uuid> =
                    sqlx::query_scalar("SELECT appointment_id FROM visits WHERE id = $1")
                        .bind(visit.id)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                assert!(link.is_none());
            }
            Some(want) => {
                assert_eq!(rows.len(), 1, "exactly one appointment per scheduled visit");
                let (appt_id, status, via, link, service, starts_at) = &rows[0];
                assert_eq!(status, want, "visit {} -> appointment", visit.visit_status);
                assert_eq!(via, "migration");
                assert_eq!(service, "general_medicine");
                assert_eq!(
                    link.as_ref(),
                    Some(appt_id),
                    "visit links back to its appointment"
                );
                let scheduled_at: Option<Ts> =
                    sqlx::query_scalar("SELECT scheduled_at FROM visits WHERE id = $1")
                        .bind(visit.id)
                        .fetch_one(&pool)
                        .await
                        .unwrap();
                assert_eq!(
                    starts_at, &scheduled_at,
                    "appointment keeps the scheduled time"
                );
                let history: Vec<(Option<String>, String, String, String)> = sqlx::query_as(
                    "SELECT from_status, to_status, reason_code, actor FROM appointment_history WHERE appointment_id = $1",
                )
                .bind(appt_id)
                .fetch_all(&pool)
                .await
                .unwrap();
                assert_eq!(
                    history,
                    vec![(
                        None,
                        want.to_string(),
                        "migrated_from_scheduled_visit".to_string(),
                        "system:migration-0015".to_string()
                    )]
                );
                let (cancelled_at, fulfilled_at, no_show_at, cancellation_reason): AppointmentTimestamps = sqlx::query_as(
                    "SELECT cancelled_at, fulfilled_at, no_show_at, cancellation_reason FROM appointments WHERE id = $1",
                )
                .bind(appt_id)
                .fetch_one(&pool)
                .await
                .unwrap();
                match want {
                    "confirmed" => {
                        assert!(
                            cancelled_at.is_none()
                                && fulfilled_at.is_none()
                                && no_show_at.is_none()
                        );
                        assert!(cancellation_reason.is_none());
                    }
                    "cancelled" => {
                        assert!(cancelled_at.is_some());
                        assert_eq!(
                            cancellation_reason.as_deref(),
                            Some("migrated_visit_cancelled")
                        );
                    }
                    "no_show" => assert!(no_show_at.is_some() && cancelled_at.is_none()),
                    "fulfilled" => assert!(fulfilled_at.is_some() && cancelled_at.is_none()),
                    other => unreachable!("{other}"),
                }
            }
        }
    }

    // 5. Re-running the upgrade is a no-op.
    let appointments_snapshot: Vec<AppointmentSnapshot> =
        sqlx::query_as("SELECT id, status, version, visit_id FROM appointments ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    wellos_server::run_migrations(&pool).await.unwrap();
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM _sqlx_migrations").await,
        applied_after
    );
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM visits").await,
        visits_before
    );
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM appointments").await,
        expected_appointments
    );
    assert_eq!(
        scalar_i64(&pool, "SELECT COUNT(*) FROM appointment_history").await,
        expected_appointments
    );
    let appointments_again: Vec<AppointmentSnapshot> =
        sqlx::query_as("SELECT id, status, version, visit_id FROM appointments ORDER BY id")
            .fetch_all(&pool)
            .await
            .unwrap();
    assert_eq!(appointments_again, appointments_snapshot);

    drop_gate_database(pool).await;
}
