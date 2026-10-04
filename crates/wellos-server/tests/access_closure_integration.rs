//! Facility closures as a hard scheduling constraint: `kind='closure'`
//! operational-calendar events (facility-specific or tenant-wide) remove
//! matcher candidates on the closed local dates, are revalidated inside the
//! hold/confirm transaction, revoke affected live offers and holds when they
//! are created, preserve confirmed appointments as staff conflicts and
//! restore availability once deactivated.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Duration, NaiveDate, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::collections::BTreeSet;
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;
use wellos_server::state::AppState;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wellos:wellos_dev@localhost:5432/wellos".to_string())
}

async fn test_state() -> AppState {
    let pool = wellos_server::connect_pool(&database_url()).await.unwrap();
    wellos_server::run_migrations(&pool).await.unwrap();
    let seeded: Option<(i64,)> = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_optional(&pool)
        .await
        .unwrap();
    if seeded.map(|(n,)| n).unwrap_or(0) == 0 {
        wellos_server::seeddata::seed(
            &pool,
            &wellos_server::runtime::RuntimeConfig::test_fixtures(),
        )
        .await
        .unwrap();
    }
    let gateway = Arc::new(dmind_gateway::fake::FakeProvider::new());
    AppState::new(pool, gateway)
}

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let purpose = if token == ADMIN {
        "operations"
    } else {
        "treatment"
    };
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("x-purpose-of-use", purpose)
        .body(match body {
            Some(v) => Body::from(v.to_string()),
            None => Body::empty(),
        })
        .unwrap();
    let res = wellos_server::app(state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = res.status();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn uniq(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7().simple())
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

const REG: &str = "dev-reg.rivera";
const ADMIN: &str = "dev-admin.silva";

async fn main_facility(state: &AppState) -> String {
    let (st, meta) = call(state, "GET", "/api/v1/meta/tenant", REG, None).await;
    assert_eq!(st, StatusCode::OK);
    s(&meta["facilities"][0]["id"])
}

/// A second facility of the same tenant (the synthetic "North Annex").
async fn other_facility(state: &AppState, main: &str) -> String {
    let main_id: Uuid = main.parse().unwrap();
    let id: Uuid = sqlx::query_scalar(
        "SELECT id FROM facilities WHERE tenant_id = (SELECT tenant_id FROM facilities WHERE id = $1)
           AND id <> $1 ORDER BY name LIMIT 1",
    )
    .bind(main_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    id.to_string()
}

async fn facility_tz(state: &AppState, facility: &str) -> Tz {
    let id: Uuid = facility.parse().unwrap();
    let tz: Option<String> =
        sqlx::query_scalar("SELECT time_zone FROM facility_scheduling WHERE facility_id = $1")
            .bind(id)
            .fetch_optional(&state.pool)
            .await
            .unwrap();
    tz.unwrap_or_else(|| "UTC".into()).parse().unwrap()
}

async fn register_patient(state: &AppState, facility: &str) -> String {
    let (st, patient) = call(
        state,
        "POST",
        "/api/v1/patients",
        REG,
        Some(json!({
            "facility_id": facility,
            "family_name": "Closure",
            "given_name": "Synthetic",
            "birth_date": "1979-06-15",
            "sex": "male",
            "identifier": uniq("MRN-CLO"),
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{patient}");
    s(&patient["id"])
}

async fn create_service(state: &AppState) -> String {
    let service = uniq("clo").replace('-', "_");
    let (st, v) = call(
        state,
        "POST",
        "/api/v1/catalog",
        ADMIN,
        Some(json!({
            "kind": "clinical_service",
            "code": service,
            "name_en": "Closure test service",
            "name_es": "Servicio de prueba de cierre",
            "config": { "duration_minutes": 30, "modality_codes": ["in_person"], "required_resource_types": ["professional"] },
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    service
}

/// Professional at `facility` delivering `service` every day 08:00-18:00.
async fn create_professional(state: &AppState, facility: &str, service: &str) -> String {
    let (st, res) = call(
        state,
        "POST",
        "/api/v1/scheduling/resources",
        ADMIN,
        Some(json!({
            "facility_id": facility,
            "resource_type_code": "professional",
            "name": uniq("Dr Closure"),
            "languages": ["es", "en"],
            "services": [{ "service_code": service, "modality_codes": ["in_person"] }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{res}");
    let id = s(&res["id"]);
    let version = res["version"].as_i64().unwrap();
    let rules: Vec<Value> = (1..=7)
        .map(|d| json!({ "weekday": d, "start_local": "08:00:00", "end_local": "18:00:00" }))
        .collect();
    let (st, res) = call(
        state,
        "POST",
        &format!("/api/v1/scheduling/resources/{id}/availability"),
        ADMIN,
        Some(json!({ "version": version, "rules": rules })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{res}");
    id
}

/// Submit a request for `service` restricted to `facilities` and the UTC
/// window `[earliest, latest)`, then run the matcher. Each call uses a fresh
/// patient so requests never interfere with each other. Returns the match
/// response (offers, matcher_run_id) and the request id.
async fn request_and_match(
    state: &AppState,
    facility: &str,
    facilities: &[&str],
    service: &str,
    earliest: DateTime<Utc>,
    latest: DateTime<Utc>,
) -> (Value, String) {
    let patient = register_patient(state, facility).await;
    let (st, req) = call(
        state,
        "POST",
        "/api/v1/access-requests",
        REG,
        Some(json!({
            "patient_id": patient,
            "facility_id": facility,
            "free_text": "Routine review (closure test)",
            "constraints": {
                "service_code": service,
                "facility_ids": facilities,
                "earliest": earliest,
                "latest": latest,
            },
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{req}");
    let rid = s(&req["id"]);
    let (st, m) = call(
        state,
        "POST",
        &format!("/api/v1/access-requests/{rid}/match"),
        REG,
        Some(json!({ "version": req["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{m}");
    (m, rid)
}

fn offers(m: &Value) -> Vec<Value> {
    m["offers"].as_array().cloned().unwrap_or_default()
}

fn starts(o: &Value) -> DateTime<Utc> {
    s(&o["starts_at"]).parse().unwrap()
}

fn local_date(o: &Value, tz: Tz) -> NaiveDate {
    starts(o).with_timezone(&tz).date_naive()
}

fn local_dates(os: &[Value], tz: Tz) -> BTreeSet<NaiveDate> {
    os.iter().map(|o| local_date(o, tz)).collect()
}

/// Local day of the first offer that is a plain 24-hour day with at least
/// `min_offers` offers, so a test's closure never coincides with the DST day
/// another test closes.
fn plain_day(os: &[Value], tz: Tz, min_offers: usize) -> NaiveDate {
    local_dates(os, tz)
        .into_iter()
        .find(|d| {
            (local_midnight(tz, *d + Duration::days(1)) - local_midnight(tz, *d)).num_hours() == 24
                && os.iter().filter(|o| local_date(o, tz) == *d).count() >= min_offers
        })
        .expect("a plain local day with enough offers")
}

/// UTC instant of local midnight on `d` in `tz` (DST-safe).
fn local_midnight(tz: Tz, d: NaiveDate) -> DateTime<Utc> {
    tz.from_local_datetime(&d.and_time(NaiveTime::MIN))
        .earliest()
        .or_else(|| {
            tz.from_local_datetime(&d.and_time(NaiveTime::from_hms_opt(1, 0, 0).unwrap()))
                .earliest()
        })
        .unwrap()
        .with_timezone(&Utc)
}

async fn rejected_summary(state: &AppState, m: &Value) -> Value {
    let run_id = s(&m["matcher_run_id"]);
    let (st, run) = call(
        state,
        "GET",
        &format!("/api/v1/matcher-runs/{run_id}"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{run}");
    run["rejected_summary"].clone()
}

async fn create_closure(
    state: &AppState,
    facility: Option<&str>,
    starts_on: NaiveDate,
    ends_on: NaiveDate,
) -> Value {
    let (st, ev) = call(
        state,
        "POST",
        "/api/v1/scheduling/calendar",
        ADMIN,
        Some(json!({
            "facility_id": facility,
            "kind": "closure",
            "name": "Closure (synthetic test)",
            "starts_on": starts_on,
            "ends_on": ends_on,
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ev}");
    assert_eq!(ev["kind"], "closure");
    ev
}

async fn deactivate_closure(state: &AppState, id: &str) {
    let (st, v) = call(
        state,
        "POST",
        &format!("/api/v1/scheduling/calendar/{id}/deactivate"),
        ADMIN,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

async fn offer_status(state: &AppState, oid: &str) -> String {
    let id: Uuid = oid.parse().unwrap();
    sqlx::query_scalar("SELECT status FROM appointment_offers WHERE id = $1")
        .bind(id)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

async fn active_holds_for_offer(state: &AppState, oid: &str) -> i64 {
    let id: Uuid = oid.parse().unwrap();
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM resource_bookings WHERE offer_id = $1 AND status = 'active'",
    )
    .bind(id)
    .fetch_one(&state.pool)
    .await
    .unwrap()
}

/// Insert an active closure row directly, bypassing the route's own
/// invalidation, to prove that hold/confirm revalidate the calendar
/// transactionally.
async fn insert_closure_row(
    state: &AppState,
    facility: &str,
    starts_on: NaiveDate,
    ends_on: NaiveDate,
) -> Uuid {
    let fid: Uuid = facility.parse().unwrap();
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO operational_calendar_events
             (id, tenant_id, facility_id, kind, name, starts_on, ends_on, demand_multiplier,
              capacity_multiplier, created_by)
         SELECT $1, f.tenant_id, $2, 'closure', 'Closure (direct row)', $3, $4, 1.0, 0.0,
                (SELECT u.id FROM users u WHERE u.tenant_id = f.tenant_id ORDER BY u.id LIMIT 1)
         FROM facilities f WHERE f.id = $2",
    )
    .bind(id)
    .bind(fid)
    .bind(starts_on)
    .bind(ends_on)
    .execute(&state.pool)
    .await
    .unwrap();
    id
}

/// Window `[now + offset days, + 6 days)`: far enough out that other suites'
/// near-term candidates never meet these closures, and at least nine days
/// apart between tests so their closures never overlap.
fn window(offset_days: i64) -> (DateTime<Utc>, DateTime<Utc>) {
    let from = Utc::now() + Duration::days(offset_days);
    (from, from + Duration::days(6))
}

#[tokio::test]
async fn facility_closure_removes_candidates_and_deactivation_restores_them() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let tz = facility_tz(&state, &facility).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let (from, to) = window(20);

    let (m, _) = request_and_match(&state, &facility, &[&facility], &service, from, to).await;
    let before = offers(&m);
    assert!(!before.is_empty(), "{m}");
    let closed_day = plain_day(&before, tz, 1);

    let closure = create_closure(&state, Some(&facility), closed_day, closed_day).await;
    let cid = s(&closure["id"]);

    let (m2, _) = request_and_match(&state, &facility, &[&facility], &service, from, to).await;
    let after = offers(&m2);
    assert!(!after.is_empty(), "other days stay available: {m2}");
    assert!(
        !local_dates(&after, tz).contains(&closed_day),
        "closed local day still offered: {:?}",
        local_dates(&after, tz)
    );
    let rejected = rejected_summary(&state, &m2).await;
    assert!(
        rejected["facility_closed"].as_u64().unwrap_or(0) > 0,
        "closure must be accounted as a hard rejection: {rejected}"
    );

    deactivate_closure(&state, &cid).await;
    let (m3, _) = request_and_match(&state, &facility, &[&facility], &service, from, to).await;
    assert!(
        local_dates(&offers(&m3), tz).contains(&closed_day),
        "deactivated closure must not remove availability: {:?}",
        local_dates(&offers(&m3), tz)
    );
}

#[tokio::test]
async fn tenant_wide_closure_removes_every_facility_but_facility_closure_leaves_others_schedulable()
{
    let state = test_state().await;
    let main = main_facility(&state).await;
    let annex = other_facility(&state, &main).await;
    let tz_main = facility_tz(&state, &main).await;
    let tz_annex = facility_tz(&state, &annex).await;
    let service = create_service(&state).await;
    create_professional(&state, &main, &service).await;
    create_professional(&state, &annex, &service).await;
    let (from, to) = window(29);
    let both = [main.as_str(), annex.as_str()];

    let (m, _) = request_and_match(&state, &main, &both, &service, from, to).await;
    let before = offers(&m);
    let facilities_before: BTreeSet<String> = before.iter().map(|o| s(&o["facility_id"])).collect();
    assert_eq!(
        facilities_before.len(),
        2,
        "both facilities offer options before any closure: {m}"
    );
    let closed_day = plain_day(&before, tz_main, 1);

    // Tenant-wide: no facility offers the closed local day.
    let tenant_wide = create_closure(&state, None, closed_day, closed_day).await;
    let (m2, _) = request_and_match(&state, &main, &both, &service, from, to).await;
    for o in offers(&m2) {
        let tz = if s(&o["facility_id"]) == main {
            tz_main
        } else {
            tz_annex
        };
        assert_ne!(
            local_date(&o, tz),
            closed_day,
            "tenant-wide closure ignored: {o}"
        );
    }
    assert!(!offers(&m2).is_empty(), "{m2}");
    deactivate_closure(&state, &s(&tenant_wide["id"])).await;

    // Facility-specific over the whole window: only the annex remains.
    let main_closure = create_closure(
        &state,
        Some(&main),
        from.date_naive() - Duration::days(1),
        to.date_naive() + Duration::days(1),
    )
    .await;
    let (m3, _) = request_and_match(&state, &main, &both, &service, from, to).await;
    let after = offers(&m3);
    assert!(!after.is_empty(), "annex must stay schedulable: {m3}");
    assert!(
        after.iter().all(|o| s(&o["facility_id"]) == annex),
        "closed facility still offered: {m3}"
    );
    let rejected = rejected_summary(&state, &m3).await;
    assert!(
        rejected["facility_closed"].as_u64().unwrap_or(0) > 0,
        "{rejected}"
    );
    deactivate_closure(&state, &s(&main_closure["id"])).await;
}

#[tokio::test]
async fn closure_created_after_offers_revokes_live_offers_and_releases_holds() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let tz = facility_tz(&state, &facility).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let (from, to) = window(38);

    let (m, _) = request_and_match(&state, &facility, &[&facility], &service, from, to).await;
    let os = offers(&m);
    assert!(os.len() >= 2, "{m}");
    let closed_day = plain_day(&os, tz, 1);
    let held = os.iter().find(|o| local_date(o, tz) == closed_day).unwrap();
    let hid = s(&held["id"]);
    let (st, h) = call(
        &state,
        "POST",
        &format!("/api/v1/offers/{hid}/hold"),
        REG,
        Some(json!({ "version": held["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{h}");
    assert_eq!(active_holds_for_offer(&state, &hid).await, 1);
    let same_day: Vec<&Value> = os
        .iter()
        .filter(|o| local_date(o, tz) == closed_day)
        .collect();
    let other_day: Vec<&Value> = os
        .iter()
        .filter(|o| local_date(o, tz) != closed_day)
        .collect();

    let closure = create_closure(&state, Some(&facility), closed_day, closed_day).await;
    let revoked: BTreeSet<String> = closure["revoked_offer_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(s)
        .collect();
    for o in &same_day {
        assert!(revoked.contains(&s(&o["id"])), "offer not revoked: {o}");
        assert_eq!(offer_status(&state, &s(&o["id"])).await, "revoked");
    }
    for o in &other_day {
        assert!(
            !revoked.contains(&s(&o["id"])),
            "unaffected offer revoked: {o}"
        );
        assert_eq!(offer_status(&state, &s(&o["id"])).await, "offered");
    }
    assert_eq!(
        active_holds_for_offer(&state, &hid).await,
        0,
        "the hold inside the closure is released"
    );
    assert_eq!(closure["conflicting_appointment_ids"], json!([]));

    // A revoked offer can no longer be held or accepted.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/offers/{hid}/accept"),
        REG,
        Some(json!({ "idempotency_key": uniq("acc") })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    deactivate_closure(&state, &s(&closure["id"])).await;
}

#[tokio::test]
async fn hold_and_confirm_revalidate_closures_transactionally() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let tz = facility_tz(&state, &facility).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let (from, to) = window(47);

    let (m, _) = request_and_match(&state, &facility, &[&facility], &service, from, to).await;
    let os = offers(&m);
    let closed_day = plain_day(&os, tz, 2);
    let on_day: Vec<&Value> = os
        .iter()
        .filter(|o| local_date(o, tz) == closed_day)
        .collect();
    let stale_hold = on_day[0];
    let stale_accept = on_day[1];
    // Hold one before the closure exists (this is the held → confirm path).
    let (st, h) = call(
        &state,
        "POST",
        &format!("/api/v1/offers/{}/hold", s(&stale_accept["id"])),
        REG,
        Some(json!({ "version": stale_accept["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{h}");

    // Closure row written outside the route: nothing was invalidated yet.
    let row = insert_closure_row(&state, &facility, closed_day, closed_day).await;
    assert_eq!(offer_status(&state, &s(&stale_hold["id"])).await, "offered");
    assert_eq!(offer_status(&state, &s(&stale_accept["id"])).await, "held");

    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/offers/{}/hold", s(&stale_hold["id"])),
        REG,
        Some(json!({ "version": stale_hold["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(code(&v), "facility_closed", "{v}");
    assert_eq!(offer_status(&state, &s(&stale_hold["id"])).await, "revoked");
    assert_eq!(
        active_holds_for_offer(&state, &s(&stale_hold["id"])).await,
        0
    );

    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/offers/{}/accept", s(&stale_accept["id"])),
        REG,
        Some(json!({ "version": h["version"], "idempotency_key": uniq("acc") })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(code(&v), "facility_closed", "{v}");
    assert_eq!(
        offer_status(&state, &s(&stale_accept["id"])).await,
        "revoked"
    );
    assert_eq!(
        active_holds_for_offer(&state, &s(&stale_accept["id"])).await,
        0,
        "the stale hold is released"
    );
    let appointments: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM appointments WHERE id IN (SELECT appointment_id FROM appointment_offers WHERE id = $1 AND appointment_id IS NOT NULL)",
    )
    .bind(s(&stale_accept["id"]).parse::<Uuid>().unwrap())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(appointments, 0, "no appointment inside a closure");

    // Direct staff booking on the closed day is refused as well.
    let patient = register_patient(&state, &facility).await;
    let slot = local_midnight(tz, closed_day) + Duration::hours(10);
    let (st, v) = call(
        &state,
        "POST",
        "/api/v1/visits",
        REG,
        Some(json!({
            "patient_id": patient,
            "arrival_kind": "scheduled",
            "service": service,
            "scheduled_at": slot,
            "reason": "closure test",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(code(&v), "facility_closed", "{v}");

    deactivate_closure(&state, &row.to_string()).await;
    // After deactivation a fresh run offers the day again.
    let (m2, _) = request_and_match(&state, &facility, &[&facility], &service, from, to).await;
    assert!(local_dates(&offers(&m2), tz).contains(&closed_day), "{m2}");
}

#[tokio::test]
async fn confirmed_appointment_inside_new_closure_is_preserved_and_flagged_for_staff() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let tz = facility_tz(&state, &facility).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let (from, to) = window(11);

    let (m, _) = request_and_match(&state, &facility, &[&facility], &service, from, to).await;
    let os = offers(&m);
    let closed_day = plain_day(&os, tz, 1);
    let chosen = os.iter().find(|o| local_date(o, tz) == closed_day).unwrap();
    let (st, acc) = call(
        &state,
        "POST",
        &format!("/api/v1/offers/{}/accept", s(&chosen["id"])),
        REG,
        Some(json!({ "version": chosen["version"], "idempotency_key": uniq("acc") })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{acc}");
    let aid = s(&acc["id"]);
    let visit_id = s(&acc["visit_id"]);

    let closure = create_closure(&state, Some(&facility), closed_day, closed_day).await;
    let cid = s(&closure["id"]);
    let flagged: Vec<String> = closure["conflicting_appointment_ids"]
        .as_array()
        .unwrap()
        .iter()
        .map(s)
        .collect();
    assert!(flagged.contains(&aid), "{closure}");

    // Never silently cancelled: appointment and linked visit are untouched.
    let (st, a) = call(
        &state,
        "GET",
        &format!("/api/v1/appointments/{aid}"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    assert_eq!(a["status"], "confirmed");
    assert_eq!(a["starts_at"], chosen["starts_at"]);
    let visit_status: String = sqlx::query_scalar("SELECT status FROM visits WHERE id = $1")
        .bind(visit_id.parse::<Uuid>().unwrap())
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(visit_status, "scheduled");

    // Staff see the conflict and must decide.
    let (st, c) = call(
        &state,
        "GET",
        &format!("/api/v1/scheduling/calendar/{cid}/conflicts"),
        ADMIN,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["requires_human_decision"], true);
    let items = c["items"].as_array().unwrap();
    let item = items
        .iter()
        .find(|i| s(&i["id"]) == aid)
        .expect("conflict listed");
    assert_eq!(item["status"], "confirmed");
    assert!(
        item["patient"].is_object(),
        "staff need the patient summary: {item}"
    );

    // Other tenants cannot see it; deactivation clears the conflict.
    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/scheduling/calendar/{cid}/conflicts"),
        "dev-dr.sur",
        None,
    )
    .await;
    assert!(
        st == StatusCode::NOT_FOUND || st == StatusCode::FORBIDDEN,
        "{st}"
    );
    deactivate_closure(&state, &cid).await;
    let (st, c2) = call(
        &state,
        "GET",
        &format!("/api/v1/scheduling/calendar/{cid}/conflicts"),
        ADMIN,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c2}");
    assert_eq!(c2["requires_human_decision"], false);
    assert_eq!(c2["items"], json!([]));
}

#[tokio::test]
async fn closure_boundaries_follow_facility_local_midnight_including_dst_days() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let tz = facility_tz(&state, &facility).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;

    // Prefer a DST transition day inside the horizon (23 or 25 local hours)
    // on which the facility is open; otherwise any open day proves the
    // local-midnight boundaries.
    let today = Utc::now().with_timezone(&tz).date_naive();
    let day_hours = |d: NaiveDate| {
        (local_midnight(tz, d + Duration::days(1)) - local_midnight(tz, d)).num_hours()
    };
    let dst_days: Vec<NaiveDate> = (3..55)
        .map(|i| today + Duration::days(i))
        .filter(|d| day_hours(*d) != 24)
        .collect();
    let fallback: Vec<NaiveDate> = (54..58).map(|i| today + Duration::days(i)).collect();
    let mut closed_day = None;
    for d in dst_days.into_iter().chain(fallback) {
        let (open, _) = request_and_match(
            &state,
            &facility,
            &[&facility],
            &service,
            local_midnight(tz, d),
            local_midnight(tz, d + Duration::days(1)),
        )
        .await;
        if !offers(&open).is_empty() {
            closed_day = Some(d);
            break;
        }
    }
    let closed_day = closed_day.expect("an open day inside the horizon");
    let hours = day_hours(closed_day);
    assert!((23..=25).contains(&hours), "{hours}");

    let closure = create_closure(&state, Some(&facility), closed_day, closed_day).await;
    let start = local_midnight(tz, closed_day);
    let end = local_midnight(tz, closed_day + Duration::days(1));
    assert_eq!((end - start).num_hours(), hours);

    // Exactly the local day is closed: nothing inside [start, end) ...
    let (closed, _) =
        request_and_match(&state, &facility, &[&facility], &service, start, end).await;
    assert!(offers(&closed).is_empty(), "{closed}");
    assert!(
        rejected_summary(&state, &closed).await["facility_closed"]
            .as_u64()
            .unwrap_or(0)
            > 0
    );
    // ... while the neighbouring local days before and after stay open, and
    // every offer starts outside the closed interval.
    let (around, _) = request_and_match(
        &state,
        &facility,
        &[&facility],
        &service,
        start - Duration::days(1),
        end + Duration::days(1),
    )
    .await;
    let os = offers(&around);
    assert!(!os.is_empty(), "{around}");
    for o in &os {
        let t = starts(o);
        assert!(t < start || t >= end, "offer inside closed local day: {o}");
    }
    let dates = local_dates(&os, tz);
    assert!(!dates.contains(&closed_day), "{dates:?}");
    deactivate_closure(&state, &s(&closure["id"])).await;
}
