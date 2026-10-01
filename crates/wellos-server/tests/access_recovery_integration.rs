//! dMind Access v1 — cancellation recovery, waitlist fairness, notification
//! durability, capacity forecasting and transport/location integration
//! tests. Every scenario drives the public HTTP API with synthetic
//! identities against a real PostgreSQL schema; nothing here touches an
//! external network.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Datelike, Duration, NaiveDate, Utc};
use http_body_util::BodyExt;
use serde_json::{json, Value};
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

const REG: &str = "dev-reg.rivera";
const ADMIN: &str = "dev-admin.silva";
const PRIVACY: &str = "dev-privacy.wolf";
const GARCIA: &str = "dev-dr.garcia";
const OTHER_TENANT: &str = "dev-dr.sur";

async fn call_with_purpose(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    purpose: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
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
    call_with_purpose(state, method, path, token, purpose, body).await
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

async fn main_facility(state: &AppState) -> String {
    let (st, meta) = call(state, "GET", "/api/v1/meta/tenant", REG, None).await;
    assert_eq!(st, StatusCode::OK);
    s(&meta["facilities"][0]["id"])
}

async fn tenant_of_facility(state: &AppState, facility: &str) -> Uuid {
    let fid: Uuid = facility.parse().unwrap();
    sqlx::query_scalar("SELECT tenant_id FROM facilities WHERE id = $1")
        .bind(fid)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

async fn register_patient(state: &AppState, facility: &str) -> String {
    let (st, patient) = call(
        state,
        "POST",
        "/api/v1/patients",
        REG,
        Some(json!({
            "facility_id": facility,
            "family_name": "Recovery",
            "given_name": "Synthetic",
            "birth_date": "1975-06-21",
            "sex": "male",
            "identifier": uniq("MRN-REC"),
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{patient}");
    s(&patient["id"])
}

async fn create_service(state: &AppState) -> String {
    let service = uniq("svc").replace('-', "_");
    let (st, v) = call(
        state,
        "POST",
        "/api/v1/catalog",
        ADMIN,
        Some(json!({
            "kind": "clinical_service",
            "code": service,
            "name_en": "Runtime recovery service",
            "name_es": "Servicio de recuperación",
            "config": {
                "duration_minutes": 30,
                "modality_codes": ["in_person"],
                "required_resource_types": ["professional"],
                "preparation_en": "Bring your medication list.",
                "preparation_es": "Traiga su lista de medicación."
            },
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    service
}

async fn create_resource(
    state: &AppState,
    facility: &str,
    type_code: &str,
    service: Option<&str>,
) -> String {
    let mut body = json!({
        "facility_id": facility,
        "resource_type_code": type_code,
        "name": uniq(&format!("Resource {type_code}")),
        "languages": ["es", "en"],
    });
    if let Some(svc) = service {
        body["services"] = json!([{ "service_code": svc, "modality_codes": ["in_person"] }]);
    }
    let (st, res) = call(
        state,
        "POST",
        "/api/v1/scheduling/resources",
        ADMIN,
        Some(body),
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

async fn create_professional(state: &AppState, facility: &str, service: &str) -> String {
    create_resource(state, facility, "professional", Some(service)).await
}

/// Creates a synthetic `patient_representative` user holding an active
/// self grant for `patient` and returns its dev token.
async fn grant_self(state: &AppState, patient: &str) -> String {
    let username = uniq("rep").to_lowercase();
    let tenant: Uuid =
        sqlx::query_scalar("SELECT tenant_id FROM users WHERE username = 'reg.rivera'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let uid = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, tenant_id, username, display_name, is_service, oidc_subject)
         VALUES ($1,$2,$3,'Synthetic Representative',false,$4)",
    )
    .bind(uid)
    .bind(tenant)
    .bind(&username)
    .bind(format!("synthetic|{username}"))
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO role_assignments (id, tenant_id, user_id, role, facility_id)
         VALUES ($1,$2,$3,'patient_representative',NULL)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(uid)
    .execute(&state.pool)
    .await
    .unwrap();
    let (st, g) = call(
        state,
        "POST",
        "/api/v1/patient-grants",
        REG,
        Some(json!({
            "user_id": uid,
            "patient_id": patient,
            "relationship": "self",
            "verification_note": "Synthetic: identity document checked in person",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{g}");
    format!("dev-{username}")
}

async fn set_consent(state: &AppState, patient: &str, purpose: &str, status: &str) {
    let (st, v) = call(
        state,
        "POST",
        "/api/v1/consents",
        PRIVACY,
        Some(json!({ "patient_id": patient, "purpose": purpose, "status": status })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

/// Book a confirmed appointment for `patient` starting no earlier than
/// `days_ahead` days from now through the genuine request → match → accept
/// path. Returns the appointment JSON.
async fn book(
    state: &AppState,
    facility: &str,
    patient: &str,
    service: &str,
    days_ahead: i64,
) -> Value {
    let (st, req) = call(
        state,
        "POST",
        "/api/v1/access-requests",
        REG,
        Some(json!({
            "patient_id": patient,
            "facility_id": facility,
            "free_text": "Routine follow-up",
            "constraints": {
                "service_code": service,
                "facility_ids": [facility],
                "earliest": Utc::now() + Duration::days(days_ahead),
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
    let offer = &m["offers"][0];
    assert!(offer.is_object(), "{m}");
    let (st, a) = call(
        state,
        "POST",
        &format!("/api/v1/offers/{}/accept", s(&offer["id"])),
        REG,
        Some(json!({ "version": offer["version"], "idempotency_key": uniq("book") })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{a}");
    assert_eq!(a["status"], "confirmed");
    a
}

async fn join_waitlist(state: &AppState, facility: &str, patient: &str, service: &str) -> Value {
    let (st, e) = call(
        state,
        "POST",
        "/api/v1/waitlist",
        REG,
        Some(json!({
            "patient_id": patient,
            "consent_confirmed": true,
            "service_code": service,
            "facility_ids": [facility],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{e}");
    e
}

async fn backdate_join(state: &AppState, entry: &str, days: i64) {
    let id: Uuid = entry.parse().unwrap();
    sqlx::query(
        "UPDATE waitlist_entries SET joined_at = now() - make_interval(days => $2) WHERE id = $1",
    )
    .bind(id)
    .bind(days as i32)
    .execute(&state.pool)
    .await
    .unwrap();
}

/// Urgency on a waitlist entry is set by deterministic or human triage,
/// never by the patient; tests set it directly as triage would.
async fn set_urgency(state: &AppState, entry: &str, urgency: &str) {
    let id: Uuid = entry.parse().unwrap();
    sqlx::query("UPDATE waitlist_entries SET urgency = $2, urgency_source = 'human' WHERE id = $1")
        .bind(id)
        .bind(urgency)
        .execute(&state.pool)
        .await
        .unwrap();
}

async fn cancel(state: &AppState, appointment: &Value) -> Value {
    let (st, c) = call(
        state,
        "POST",
        &format!("/api/v1/appointments/{}/cancel", s(&appointment["id"])),
        REG,
        Some(json!({
            "version": appointment["version"],
            "reason_code": "patient_request",
            "override_reason": "Patient phoned to cancel",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["status"], "cancelled");
    c
}

/// Walks the bounded, keyset-paginated staff list until the recovery event
/// for `appointment_id` appears (the shared test database accumulates
/// events across runs), then loads the full detail.
async fn event_for_appointment(state: &AppState, appointment_id: &str) -> Value {
    let mut after: Option<String> = None;
    let ev = loop {
        let path = match &after {
            Some(a) => format!("/api/v1/recovery-events?status=all&limit=100&after={a}"),
            None => "/api/v1/recovery-events?status=all&limit=100".to_string(),
        };
        let (st, list) = call(state, "GET", &path, REG, None).await;
        assert_eq!(st, StatusCode::OK, "{list}");
        if let Some(ev) = list["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|e| e["appointment_id"] == appointment_id)
        {
            break ev.clone();
        }
        match list["next_after"].as_str() {
            Some(next) => after = Some(next.to_string()),
            None => panic!("recovery event opened for the cancelled appointment"),
        }
    };
    let (st, full) = call(
        state,
        "GET",
        &format!("/api/v1/recovery-events/{}", s(&ev["id"])),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{full}");
    full
}

async fn entry(state: &AppState, id: &str) -> Value {
    let (st, e) = call(state, "GET", &format!("/api/v1/waitlist/{id}"), REG, None).await;
    assert_eq!(st, StatusCode::OK, "{e}");
    e
}

async fn notification_kinds(state: &AppState, patient: &str) -> Vec<(String, String)> {
    let pid: Uuid = patient.parse().unwrap();
    sqlx::query_as::<_, (String, String)>(
        "SELECT kind, status FROM notifications WHERE patient_id = $1 ORDER BY created_at",
    )
    .bind(pid)
    .fetch_all(&state.pool)
    .await
    .unwrap()
}

/// The worker sweeps every tenant, so tests that inspect worker state
/// serialize their ticks; the notification test holds the lock for its
/// whole delivery sequence.
static WORKER: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

async fn tick_unlocked(state: &AppState) -> Value {
    let (st, out) = call(state, "POST", "/api/v1/scheduling/worker/tick", REG, None).await;
    assert_eq!(st, StatusCode::OK, "{out}");
    out
}

async fn tick(state: &AppState) -> Value {
    let _worker = WORKER.lock().await;
    tick_unlocked(state).await
}

#[tokio::test]
async fn cancellation_offers_the_freed_slot_fairly_and_cascades_on_decline() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;

    let holder = register_patient(&state, &facility).await;
    let longest = register_patient(&state, &facility).await;
    let recent = register_patient(&state, &facility).await;
    let urgent = register_patient(&state, &facility).await;
    let no_consent = register_patient(&state, &facility).await;

    let appointment = book(&state, &facility, &holder, &service, 3).await;

    let e_long = join_waitlist(&state, &facility, &longest, &service).await;
    let e_recent = join_waitlist(&state, &facility, &recent, &service).await;
    let e_urgent = join_waitlist(&state, &facility, &urgent, &service).await;
    let e_nc = join_waitlist(&state, &facility, &no_consent, &service).await;
    backdate_join(&state, &s(&e_long["id"]), 10).await;
    backdate_join(&state, &s(&e_recent["id"]), 1).await;
    // Urgency established by human triage outranks waiting time.
    let urgent_id: Uuid = s(&e_urgent["id"]).parse().unwrap();
    sqlx::query(
        "UPDATE waitlist_entries SET urgency = 'urgent', urgency_source = 'human' WHERE id = $1",
    )
    .bind(urgent_id)
    .execute(&state.pool)
    .await
    .unwrap();
    // A revoked consent excludes the entry deterministically.
    set_consent(&state, &no_consent, "waitlist_offers", "revoked").await;

    cancel(&state, &appointment).await;

    let ev = event_for_appointment(&state, &s(&appointment["id"])).await;
    assert_eq!(ev["status"], "offered", "{ev}");
    assert_eq!(ev["ranking_mode"], "deterministic");
    let eligible = ev["eligible"].as_array().unwrap();
    let order: Vec<String> = eligible.iter().map(|r| s(&r["entry_id"])).collect();
    assert_eq!(
        order,
        vec![s(&e_urgent["id"]), s(&e_long["id"]), s(&e_recent["id"])],
        "urgency first, then longest wait: {ev}"
    );
    let excluded = ev["excluded"].as_array().unwrap();
    assert!(
        excluded
            .iter()
            .any(|x| x["entry_id"] == e_nc["id"] && x["reason"] == "no_consent"),
        "{ev}"
    );
    assert!(eligible[0]["offer_id"].is_string(), "{ev}");
    assert!(eligible[0]["reasons"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| r == "urgency:urgent"));

    // The urgent patient holds the live offer; they decline it.
    let urgent_entry = entry(&state, &s(&e_urgent["id"])).await;
    assert_eq!(urgent_entry["entry"]["status"], "offered");
    let offer = &urgent_entry["current_offer"];
    assert_eq!(offer["status"], "offered", "{urgent_entry}");
    assert_eq!(offer["starts_at"], appointment["starts_at"]);
    assert!(notification_kinds(&state, &urgent)
        .await
        .iter()
        .any(|(k, _)| k == "waitlist_offer"),);
    let (st, declined) = call(
        &state,
        "POST",
        &format!("/api/v1/offers/{}/decline", s(&offer["id"])),
        REG,
        Some(json!({ "version": offer["version"], "reason": "Patient prefers to wait" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{declined}");
    assert_eq!(declined["status"], "declined");

    // The cascade moves to the longest-waiting patient without staff action.
    let ev = event_for_appointment(&state, &s(&appointment["id"])).await;
    assert_eq!(ev["status"], "offered");
    assert_eq!(ev["offers_made"], 2, "{ev}");
    let urgent_entry = entry(&state, &s(&e_urgent["id"])).await;
    assert_eq!(urgent_entry["entry"]["status"], "active");
    assert_eq!(urgent_entry["entry"]["offers_declined"], 1);
    let long_entry = entry(&state, &s(&e_long["id"])).await;
    assert_eq!(long_entry["entry"]["status"], "offered", "{long_entry}");
    let offer = long_entry["current_offer"].clone();

    // Accepting books through the shared offer path: appointment + visit,
    // waitlist entry fulfilled, event filled, later candidates untouched.
    let (st, booked) = call(
        &state,
        "POST",
        &format!("/api/v1/offers/{}/accept", s(&offer["id"])),
        REG,
        Some(json!({ "version": offer["version"], "idempotency_key": uniq("wl") })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{booked}");
    assert_eq!(booked["status"], "confirmed");
    assert_eq!(booked["booked_via"], "waitlist");
    assert_eq!(booked["patient_id"], longest);
    assert_eq!(booked["starts_at"], appointment["starts_at"]);
    assert!(booked["visit_id"].is_string(), "{booked}");
    let ev = event_for_appointment(&state, &s(&appointment["id"])).await;
    assert_eq!(ev["status"], "filled", "{ev}");
    let long_entry = entry(&state, &s(&e_long["id"])).await;
    assert_eq!(long_entry["entry"]["status"], "fulfilled");
    assert_eq!(
        long_entry["entry"]["fulfilled_appointment_id"],
        booked["id"]
    );
    let recent_entry = entry(&state, &s(&e_recent["id"])).await;
    assert_eq!(recent_entry["entry"]["status"], "active");
    assert!(recent_entry["current_offer"].is_null());

    // The audit trail covers opening, both offers, the decline-driven
    // revocation path and the fill; every entry references the event only
    // by identifiers.
    let tenant = tenant_of_facility(&state, &facility).await;
    let ev_id: Uuid = s(&ev["id"]).parse().unwrap();
    let events: Vec<(String, Value)> = sqlx::query_as(
        "SELECT event_type, resource_refs FROM outbox_events WHERE tenant_id = $1
           AND resource_refs::text LIKE '%' || $2 || '%' ORDER BY occurred_at",
    )
    .bind(tenant)
    .bind(ev_id.to_string())
    .fetch_all(&state.pool)
    .await
    .unwrap();
    let types: Vec<&str> = events.iter().map(|(t, _)| t.as_str()).collect();
    assert_eq!(
        types
            .iter()
            .filter(|t| **t == "waitlist.recovery.opened")
            .count(),
        1,
        "{types:?}"
    );
    assert_eq!(
        types
            .iter()
            .filter(|t| **t == "waitlist.recovery.offered")
            .count(),
        2,
        "{types:?}"
    );
    assert!(
        events
            .iter()
            .any(|(t, r)| t == "waitlist.recovery.closed" && r["status"] == "filled"),
        "{events:?}"
    );
    for (_, refs) in &events {
        let text = refs.to_string();
        assert!(
            !text.contains("Recovery"),
            "no names in outbox payloads: {text}"
        );
    }
}

#[tokio::test]
async fn expired_offer_cascades_via_worker_and_exhausts_cleanly() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let holder = register_patient(&state, &facility).await;
    let first = register_patient(&state, &facility).await;
    let second = register_patient(&state, &facility).await;

    let appointment = book(&state, &facility, &holder, &service, 3).await;
    let e1 = join_waitlist(&state, &facility, &first, &service).await;
    let e2 = join_waitlist(&state, &facility, &second, &service).await;
    backdate_join(&state, &s(&e1["id"]), 5).await;
    backdate_join(&state, &s(&e2["id"]), 2).await;

    cancel(&state, &appointment).await;
    let ev = event_for_appointment(&state, &s(&appointment["id"])).await;
    let offer1 = ev["current_offer"].clone();
    assert_eq!(offer1["waitlist_entry_id"], e1["id"], "{ev}");
    assert!(offer1["offer_expires_at"].is_string(), "{offer1}");

    // Force the offer past its expiry, then let the worker run.
    let oid: Uuid = s(&offer1["id"]).parse().unwrap();
    sqlx::query(
        "UPDATE appointment_offers SET offer_expires_at = now() - interval '1 minute' WHERE id = $1",
    )
    .bind(oid)
    .execute(&state.pool)
    .await
    .unwrap();
    // Ticks are tenant-wide and other tests may sweep concurrently, so the
    // assertion is on the resulting state, not on this tick's counters.
    tick(&state).await;
    let expired: String = sqlx::query_scalar("SELECT status FROM appointment_offers WHERE id = $1")
        .bind(oid)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(expired, "expired");

    let ev = event_for_appointment(&state, &s(&appointment["id"])).await;
    assert_eq!(ev["status"], "offered", "{ev}");
    assert_eq!(ev["offers_made"], 2);
    assert_eq!(ev["current_offer"]["waitlist_entry_id"], e2["id"]);
    let first_entry = entry(&state, &s(&e1["id"])).await;
    assert_eq!(first_entry["entry"]["status"], "active");
    assert_eq!(
        first_entry["entry"]["offers_declined"], 0,
        "expiry is not a decline"
    );
    let kinds = notification_kinds(&state, &first).await;
    assert!(
        kinds.iter().any(|(k, _)| k == "waitlist_offer"),
        "{kinds:?}"
    );
    assert!(
        kinds.iter().any(|(k, _)| k == "waitlist_offer_expired"),
        "{kinds:?}"
    );

    // The last candidate leaves the waitlist: the event exhausts and the
    // slot stays open capacity for the matcher.
    let second_entry = entry(&state, &s(&e2["id"])).await;
    let (st, left) = call(
        &state,
        "POST",
        &format!("/api/v1/waitlist/{}/leave", s(&e2["id"])),
        REG,
        Some(json!({
            "version": second_entry["entry"]["version"],
            "reason": "Patient found care elsewhere",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{left}");
    let ev = event_for_appointment(&state, &s(&appointment["id"])).await;
    assert_eq!(ev["status"], "exhausted", "{ev}");
    assert!(ev["current_offer"].is_null());
    let second_entry = entry(&state, &s(&e2["id"])).await;
    assert_eq!(second_entry["entry"]["status"], "left");
}

#[tokio::test]
async fn concurrent_acceptance_of_one_waitlist_offer_books_exactly_once() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let holder = register_patient(&state, &facility).await;
    let waiting = register_patient(&state, &facility).await;

    let appointment = book(&state, &facility, &holder, &service, 3).await;
    let e = join_waitlist(&state, &facility, &waiting, &service).await;
    cancel(&state, &appointment).await;
    let ev = event_for_appointment(&state, &s(&appointment["id"])).await;
    let offer = ev["current_offer"].clone();
    assert_eq!(offer["waitlist_entry_id"], e["id"]);
    let path = format!("/api/v1/offers/{}/accept", s(&offer["id"]));

    let (ra, rb) = tokio::join!(
        call(
            &state,
            "POST",
            &path,
            REG,
            Some(json!({ "version": offer["version"], "idempotency_key": uniq("a") })),
        ),
        call(
            &state,
            "POST",
            &path,
            REG,
            Some(json!({ "version": offer["version"], "idempotency_key": uniq("b") })),
        ),
    );
    let outcomes = [ra, rb];
    let created = outcomes
        .iter()
        .filter(|(st, _)| *st == StatusCode::CREATED)
        .count();
    let conflicts = outcomes
        .iter()
        .filter(|(st, _)| *st == StatusCode::CONFLICT)
        .count();
    assert_eq!(created, 1, "{outcomes:?}");
    assert_eq!(conflicts, 1, "{outcomes:?}");

    let pid: Uuid = waiting.parse().unwrap();
    let live: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM appointments WHERE patient_id = $1 AND status = 'confirmed'",
    )
    .bind(pid)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(live, 1);
    let ev = event_for_appointment(&state, &s(&appointment["id"])).await;
    assert_eq!(ev["status"], "filled");
    let wl = entry(&state, &s(&e["id"])).await;
    assert_eq!(wl["entry"]["status"], "fulfilled");
}

#[tokio::test]
async fn staff_override_requires_a_reason_and_is_bounded_by_scope() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let holder = register_patient(&state, &facility).await;
    let a_patient = register_patient(&state, &facility).await;
    let b_patient = register_patient(&state, &facility).await;
    let c_patient = register_patient(&state, &facility).await;

    let appointment = book(&state, &facility, &holder, &service, 3).await;
    // A and C are urgent (A waited longer); B is routine.
    let ea = join_waitlist(&state, &facility, &a_patient, &service).await;
    let eb = join_waitlist(&state, &facility, &b_patient, &service).await;
    let ec = join_waitlist(&state, &facility, &c_patient, &service).await;
    backdate_join(&state, &s(&ea["id"]), 6).await;
    backdate_join(&state, &s(&eb["id"]), 8).await;
    backdate_join(&state, &s(&ec["id"]), 1).await;
    set_urgency(&state, &s(&ea["id"]), "urgent").await;
    set_urgency(&state, &s(&ec["id"]), "urgent").await;
    cancel(&state, &appointment).await;
    let ev = event_for_appointment(&state, &s(&appointment["id"])).await;
    let eid = s(&ev["id"]);
    assert_eq!(ev["current_offer"]["waitlist_entry_id"], ea["id"], "{ev}");

    // dMind ranking is bounded to the pending eligible entries and never
    // books: the live offer is untouched and the response separates the
    // governed outcome from the event.
    let (st, ranked) = call(
        &state,
        "POST",
        &format!("/api/v1/recovery-events/{eid}/rank"),
        REG,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ranked}");
    assert!(
        matches!(
            ranked["ranking"]["mode"].as_str(),
            Some("dmind") | Some("deterministic") | Some("reused")
        ),
        "{ranked}"
    );
    let ranked_event = &ranked["event"];
    assert_eq!(ranked_event["status"], "offered");
    assert_eq!(ranked_event["current_offer"]["waitlist_entry_id"], ea["id"]);
    if ranked["ranking"]["artifact_id"].is_string() {
        assert_eq!(
            ranked["ranking"]["synthetic"], true,
            "fake provider is synthetic"
        );
    }
    let pending_ids = |e: &Value| -> Vec<String> {
        let mut pending: Vec<&Value> = e["eligible"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r["outcome"].is_null())
            .collect();
        pending.sort_by_key(|r| r["position"].as_u64().unwrap_or(u64::MAX));
        pending.iter().map(|r| s(&r["entry_id"])).collect()
    };
    // The routine patient can never be ranked above the urgent one.
    assert_eq!(
        pending_ids(ranked_event),
        vec![s(&ec["id"]), s(&eb["id"])],
        "{ranked_event}"
    );

    // An override without a reason is rejected.
    let (st, denied) = call(
        &state,
        "POST",
        &format!("/api/v1/recovery-events/{eid}/override"),
        REG,
        Some(json!({ "waitlist_entry_id": eb["id"], "reason": "   " })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{denied}");
    assert_eq!(code(&denied), "validation_failed");

    // An entry that is not an eligible candidate of this event cannot be
    // injected, and the patient currently holding the offer cannot be
    // "overridden" into the queue again.
    for bogus in [json!(Uuid::now_v7()), ea["id"].clone()] {
        let (st, denied) = call(
            &state,
            "POST",
            &format!("/api/v1/recovery-events/{eid}/override"),
            REG,
            Some(json!({ "waitlist_entry_id": bogus, "reason": "typo" })),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{denied}");
        assert_eq!(code(&denied), "validation_failed");
    }

    // Staff cannot move the routine patient ahead of a waiting urgent one:
    // the fairness floor holds against human overrides too.
    let (st, floor) = call(
        &state,
        "POST",
        &format!("/api/v1/recovery-events/{eid}/override"),
        REG,
        Some(json!({
            "waitlist_entry_id": eb["id"],
            "reason": "Patient B is at the desk",
            "version": ranked_event["version"],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{floor}");
    assert_eq!(code(&floor), "fairness_floor");

    // Other tenants see nothing.
    let (st, _) = call_with_purpose(
        &state,
        "GET",
        &format!("/api/v1/recovery-events/{eid}"),
        OTHER_TENANT,
        "operations",
        None,
    )
    .await;
    assert!(
        st == StatusCode::NOT_FOUND || st == StatusCode::FORBIDDEN,
        "{st}"
    );

    // A stale version is refused; with the current one, staff may reorder
    // the pending queue among equals. The live offer to A stays in place
    // and the override is recorded with its reason.
    let (st, stale) = call(
        &state,
        "POST",
        &format!("/api/v1/recovery-events/{eid}/override"),
        REG,
        Some(json!({
            "waitlist_entry_id": ec["id"],
            "reason": "Patient C confirmed availability at the desk",
            "version": ranked_event["version"].as_i64().unwrap() + 40,
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{stale}");
    assert_eq!(code(&stale), "stale_version");
    let (st, over) = call(
        &state,
        "POST",
        &format!("/api/v1/recovery-events/{eid}/override"),
        REG,
        Some(json!({
            "waitlist_entry_id": ec["id"],
            "reason": "Patient C confirmed availability at the desk",
            "version": ranked_event["version"],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{over}");
    assert_eq!(over["ranking_mode"], "human_override");
    assert_eq!(
        over["override_reason"],
        "Patient C confirmed availability at the desk"
    );
    assert_eq!(
        over["current_offer"]["waitlist_entry_id"], ea["id"],
        "{over}"
    );
    assert_eq!(pending_ids(&over), vec![s(&ec["id"]), s(&eb["id"])]);

    // Revoking A's offer needs a reason and cascades to the overridden
    // choice; A's entry stays active and is not counted as a decline.
    let (st, denied) = call(
        &state,
        "POST",
        &format!("/api/v1/recovery-events/{eid}/revoke-offer"),
        REG,
        Some(json!({ "reason": "" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{denied}");
    let (st, revoked) = call(
        &state,
        "POST",
        &format!("/api/v1/recovery-events/{eid}/revoke-offer"),
        REG,
        Some(json!({ "reason": "Patient A unreachable by phone" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{revoked}");
    assert_eq!(revoked["cascaded"], 1, "{revoked}");
    assert_eq!(revoked["event"]["status"], "offered");
    assert_eq!(
        revoked["event"]["current_offer"]["waitlist_entry_id"], ec["id"],
        "{revoked}"
    );
    assert_eq!(revoked["event"]["offers_made"], 2);
    let a = entry(&state, &s(&ea["id"])).await;
    assert_eq!(a["entry"]["status"], "active");
    assert_eq!(a["entry"]["offers_declined"], 0);

    // Closing by staff needs a reason, releases the live offer and stops
    // the cascade; C and B remain active on the waitlist.
    let (st, denied) = call(
        &state,
        "POST",
        &format!("/api/v1/recovery-events/{eid}/close"),
        REG,
        Some(json!({ "reason": " " })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{denied}");
    let (st, closed) = call(
        &state,
        "POST",
        &format!("/api/v1/recovery-events/{eid}/close"),
        REG,
        Some(json!({ "reason": "Slot repurposed for an urgent walk-in" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{closed}");
    assert_eq!(closed["status"], "closed");
    assert_eq!(
        closed["closed_reason"],
        "Slot repurposed for an urgent walk-in"
    );
    assert!(closed["current_offer"].is_null());
    for e in [&eb, &ec] {
        let row = entry(&state, &s(&e["id"])).await;
        assert_eq!(row["entry"]["status"], "active", "{row}");
        assert!(row["current_offer"].is_null());
    }
    // A closed event accepts no further overrides.
    let (st, denied) = call(
        &state,
        "POST",
        &format!("/api/v1/recovery-events/{eid}/override"),
        REG,
        Some(json!({ "waitlist_entry_id": eb["id"], "reason": "late" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{denied}");
    assert_eq!(code(&denied), "invalid_transition");

    // Overrides and revocations are audited with their reasons.
    let tenant = tenant_of_facility(&state, &facility).await;
    let types: Vec<String> = sqlx::query_scalar(
        "SELECT event_type FROM outbox_events WHERE tenant_id = $1
           AND resource_refs::text LIKE '%' || $2 || '%' ORDER BY occurred_at",
    )
    .bind(tenant)
    .bind(&eid)
    .fetch_all(&state.pool)
    .await
    .unwrap();
    for needed in [
        "waitlist.recovery.overridden",
        "appointment.offer.revoked",
        "waitlist.recovery.closed",
    ] {
        assert!(
            types.iter().any(|t| t == needed),
            "{needed} missing: {types:?}"
        );
    }
}

#[tokio::test]
async fn notifications_are_idempotent_retry_with_backoff_and_dead_letter() {
    let _worker = WORKER.lock().await;
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let patient = register_patient(&state, &facility).await;
    let appointment = book(&state, &facility, &patient, &service, 3).await;

    let kinds = notification_kinds(&state, &patient).await;
    assert!(
        kinds.iter().any(|(k, _)| k == "booking_confirmation"),
        "{kinds:?}"
    );
    assert!(kinds.iter().any(|(k, _)| k == "reminder"), "{kinds:?}");
    let pid: Uuid = patient.parse().unwrap();

    // Make the confirmation due and deliver it; a second pass must not
    // produce a second delivery for the same channel.
    sqlx::query(
        "UPDATE notifications SET scheduled_for = now() - interval '1 minute'
         WHERE patient_id = $1 AND kind = 'booking_confirmation'",
    )
    .bind(pid)
    .execute(&state.pool)
    .await
    .unwrap();
    tick_unlocked(&state).await;
    tick_unlocked(&state).await;
    let (status, attempts): (String, i32) = sqlx::query_as(
        "SELECT status, attempts FROM notifications WHERE patient_id = $1 AND kind = 'booking_confirmation'",
    )
    .bind(pid)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(status, "delivered");
    assert_eq!(attempts, 1);
    let deliveries: Vec<(String, String)> = sqlx::query_as(
        "SELECT d.channel, d.status FROM notification_deliveries d
         JOIN notifications n ON n.id = d.notification_id
         WHERE n.patient_id = $1 AND n.kind = 'booking_confirmation' ORDER BY d.channel",
    )
    .bind(pid)
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert!(
        deliveries
            .iter()
            .any(|(c, st)| c == "in_app" && st == "delivered"),
        "{deliveries:?}"
    );
    let in_app = deliveries.iter().filter(|(c, _)| c == "in_app").count();
    assert_eq!(in_app, 1, "exactly one in-app delivery: {deliveries:?}");

    // The persisted payload is PHI-minimized (identifiers, times, codes):
    // no patient name, no medical-record number. Rendering happens in the
    // worker at delivery time.
    let payload: Value = sqlx::query_scalar(
        "SELECT payload FROM notifications WHERE patient_id = $1 AND kind = 'booking_confirmation'",
    )
    .bind(pid)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let text = payload.to_string();
    assert!(
        !text.contains("Recovery") && !text.contains("Synthetic"),
        "{text}"
    );
    assert!(!text.contains("MRN-"), "{text}");
    assert_eq!(payload["appointment_id"], appointment["id"], "{text}");

    // A channel that keeps failing retries with growing backoff and finally
    // dead-letters; delivered channels are never re-sent. The webhook
    // adapter is pointed at a closed loopback port, so nothing leaves the
    // machine and every attempt fails as unreachable.
    let mut runtime = wellos_server::runtime::RuntimeConfig::test_fixtures();
    runtime.notifications.dev_sink = false;
    runtime.notifications.webhook = Some(wellos_server::runtime::WebhookConfig {
        url: "http://127.0.0.1:9/push".into(),
        secret: "synthetic-webhook-secret".into(),
        timeout: std::time::Duration::from_secs(2),
    });
    let webhook_state = AppState::from_runtime(
        state.pool.clone(),
        Arc::new(dmind_gateway::fake::FakeProvider::new()),
        Arc::new(dmind_gateway::scribe::FakeTranscription::new()),
        wellos_server::state::AuthConfig::development(),
        runtime,
    );
    let sealed = webhook_state
        .runtime
        .location
        .keyring
        .as_ref()
        .unwrap()
        .seal(b"https://push.example.invalid/device/synthetic");
    let tenant = tenant_of_facility(&state, &facility).await;
    sqlx::query(
        "INSERT INTO patient_scheduling_preferences (patient_id, tenant_id, push_endpoint_enc, channels)
         VALUES ($1, $2, $3, ARRAY['in_app','webhook'])
         ON CONFLICT (patient_id) DO UPDATE SET push_endpoint_enc = EXCLUDED.push_endpoint_enc,
             channels = EXCLUDED.channels",
    )
    .bind(pid)
    .bind(tenant)
    .bind(&sealed)
    .execute(&state.pool)
    .await
    .unwrap();
    let reminder: Uuid = sqlx::query_scalar(
        "SELECT id FROM notifications WHERE patient_id = $1 AND kind = 'reminder' ORDER BY scheduled_for LIMIT 1",
    )
    .bind(pid)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE notifications SET scheduled_for = now() - interval '1 minute',
             channels = ARRAY['in_app','webhook']
         WHERE id = $1",
    )
    .bind(reminder)
    .execute(&state.pool)
    .await
    .unwrap();
    let mut seen_next: Vec<Option<chrono::DateTime<Utc>>> = Vec::new();
    for round in 0..4 {
        if round > 0 {
            // Backoff is honoured: the row is not due until next_attempt_at.
            let before: i32 = sqlx::query_scalar(
                "UPDATE notifications SET next_attempt_at = now() + interval '1 hour'
                 WHERE id = $1 RETURNING attempts",
            )
            .bind(reminder)
            .fetch_one(&state.pool)
            .await
            .unwrap();
            tick_unlocked(&webhook_state).await;
            let after: i32 = sqlx::query_scalar("SELECT attempts FROM notifications WHERE id = $1")
                .bind(reminder)
                .fetch_one(&state.pool)
                .await
                .unwrap();
            assert_eq!(before, after, "no retry before the backoff elapses");
        }
        sqlx::query(
            "UPDATE notifications SET next_attempt_at = now() - interval '1 second' WHERE id = $1",
        )
        .bind(reminder)
        .execute(&state.pool)
        .await
        .unwrap();
        tick_unlocked(&webhook_state).await;
        let (status, attempts, next): (String, i32, Option<chrono::DateTime<Utc>>) =
            sqlx::query_as(
                "SELECT status, attempts, next_attempt_at FROM notifications WHERE id = $1",
            )
            .bind(reminder)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        seen_next.push(next);
        if status == "dead" {
            assert_eq!(attempts, 3, "dead-lettered after max_attempts");
            break;
        }
        assert_eq!(status, "failed", "attempt {attempts}");
        assert!(next.is_some(), "a failed attempt schedules a retry");
        let (last_error,): (Option<String>,) =
            sqlx::query_as("SELECT last_error_code FROM notifications WHERE id = $1")
                .bind(reminder)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(last_error.as_deref(), Some("webhook_unreachable"));
    }
    let (status,): (String,) = sqlx::query_as("SELECT status FROM notifications WHERE id = $1")
        .bind(reminder)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(status, "dead", "{seen_next:?}");
    let in_app_deliveries: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM notification_deliveries WHERE notification_id = $1 AND channel = 'in_app' AND status = 'delivered'",
    )
    .bind(reminder)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        in_app_deliveries, 1,
        "in-app delivered once despite webhook retries"
    );
    let webhook_attempts: Vec<(i32, String, Option<String>)> = sqlx::query_as(
        "SELECT attempt, status, error_code FROM notification_deliveries
         WHERE notification_id = $1 AND channel = 'webhook' ORDER BY attempt",
    )
    .bind(reminder)
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(webhook_attempts.len(), 3, "{webhook_attempts:?}");
    assert!(webhook_attempts
        .iter()
        .all(|(_, st, code)| { st == "failed" && code.as_deref() == Some("webhook_unreachable") }));
    let dead_audits: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outbox_events WHERE event_type = 'notification.dead_lettered'
           AND resource_refs->>'notification_id' = $1",
    )
    .bind(reminder.to_string())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(dead_audits, 1);

    // With external delivery disabled (the production default) the webhook
    // channel is skipped, not failed, and the notification still completes
    // in-app.
    let mut disabled = wellos_server::runtime::RuntimeConfig::test_fixtures();
    disabled.notifications.dev_sink = false;
    let disabled_state = AppState::from_runtime(
        state.pool.clone(),
        Arc::new(dmind_gateway::fake::FakeProvider::new()),
        Arc::new(dmind_gateway::scribe::FakeTranscription::new()),
        wellos_server::state::AuthConfig::development(),
        disabled,
    );
    let preparation: Uuid = sqlx::query_scalar(
        "SELECT id FROM notifications WHERE patient_id = $1 AND kind = 'preparation' LIMIT 1",
    )
    .bind(pid)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE notifications SET scheduled_for = now() - interval '1 minute',
             channels = ARRAY['in_app','webhook','email'] WHERE id = $1",
    )
    .bind(preparation)
    .execute(&state.pool)
    .await
    .unwrap();
    tick_unlocked(&disabled_state).await;
    let prep: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT channel, status, error_code FROM notification_deliveries
         WHERE notification_id = $1 ORDER BY channel",
    )
    .bind(preparation)
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        prep,
        vec![
            (
                "email".into(),
                "skipped".into(),
                Some("no_email_address".into())
            ),
            ("in_app".into(), "delivered".into(), None),
            (
                "webhook".into(),
                "skipped".into(),
                Some("webhook_delivery_disabled".into())
            ),
        ]
    );
    let (prep_status,): (String,) =
        sqlx::query_as("SELECT status FROM notifications WHERE id = $1")
            .bind(preparation)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(prep_status, "delivered");

    // Cancelling the appointment cancels the pending reminders and enqueues
    // a cancellation notice.
    let (st, fresh) = call(
        &state,
        "GET",
        &format!("/api/v1/appointments/{}", s(&appointment["id"])),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    cancel(&state, &fresh).await;
    let kinds = notification_kinds(&state, &patient).await;
    assert!(kinds.iter().any(|(k, _)| k == "cancellation"), "{kinds:?}");
    assert!(
        kinds
            .iter()
            .filter(|(k, _)| k == "reminder" || k == "preparation" || k == "confirmation_request")
            .all(|(_, st)| st == "cancelled" || st == "dead" || st == "delivered"),
        "{kinds:?}"
    );

    // Email: with a sealed address but no SMTP configured the channel is
    // skipped as disabled; with SMTP pointed at a closed loopback port the
    // attempt fails (never leaving the machine) and is retried, while the
    // in-app copy is delivered exactly once.
    let sealed_email = disabled_state
        .runtime
        .location
        .keyring
        .as_ref()
        .unwrap()
        .seal(b"synthetic.patient@example.invalid");
    sqlx::query(
        "UPDATE patient_scheduling_preferences SET contact_email_enc = $2, channels = ARRAY['in_app','email']
         WHERE patient_id = $1",
    )
    .bind(pid)
    .bind(&sealed_email)
    .execute(&state.pool)
    .await
    .unwrap();
    let cancellation: Uuid = sqlx::query_scalar(
        "SELECT id FROM notifications WHERE patient_id = $1 AND kind = 'cancellation' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(pid)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "UPDATE notifications SET scheduled_for = now() - interval '1 minute',
             channels = ARRAY['in_app','email'] WHERE id = $1",
    )
    .bind(cancellation)
    .execute(&state.pool)
    .await
    .unwrap();
    tick_unlocked(&disabled_state).await;
    let email_rows: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT channel, status, error_code FROM notification_deliveries
         WHERE notification_id = $1 ORDER BY channel",
    )
    .bind(cancellation)
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        email_rows,
        vec![
            (
                "email".into(),
                "skipped".into(),
                Some("email_delivery_disabled".into())
            ),
            ("in_app".into(), "delivered".into(), None),
        ]
    );

    let mut smtp_runtime = wellos_server::runtime::RuntimeConfig::test_fixtures();
    smtp_runtime.notifications.dev_sink = false;
    smtp_runtime.notifications.smtp = Some(wellos_server::runtime::SmtpConfig {
        host: "127.0.0.1".into(),
        port: 9,
        username: "synthetic".into(),
        password: "synthetic".into(),
        from: "WellOS <no-reply@example.invalid>".into(),
        tls: wellos_server::runtime::SmtpTls::None,
        timeout: std::time::Duration::from_secs(2),
    });
    let smtp_state = AppState::from_runtime(
        state.pool.clone(),
        Arc::new(dmind_gateway::fake::FakeProvider::new()),
        Arc::new(dmind_gateway::scribe::FakeTranscription::new()),
        wellos_server::state::AuthConfig::development(),
        smtp_runtime,
    );
    sqlx::query(
        "UPDATE notifications SET status = 'scheduled', attempts = 0, next_attempt_at = NULL,
             scheduled_for = now() - interval '1 minute' WHERE id = $1",
    )
    .bind(cancellation)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "DELETE FROM notification_deliveries WHERE notification_id = $1 AND channel = 'email'",
    )
    .bind(cancellation)
    .execute(&state.pool)
    .await
    .unwrap();
    tick_unlocked(&smtp_state).await;
    let (status, attempts, next, last_error): (
        String,
        i32,
        Option<chrono::DateTime<Utc>>,
        Option<String>,
    ) = sqlx::query_as(
        "SELECT status, attempts, next_attempt_at, last_error_code FROM notifications WHERE id = $1",
    )
    .bind(cancellation)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!((status.as_str(), attempts), ("failed", 1));
    assert!(next.is_some(), "SMTP failure schedules a retry");
    assert_eq!(last_error.as_deref(), Some("smtp_send_failed"));
    let deliveries: Vec<(String, String, Option<String>)> = sqlx::query_as(
        "SELECT channel, status, error_code FROM notification_deliveries
         WHERE notification_id = $1 ORDER BY channel, attempt",
    )
    .bind(cancellation)
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        deliveries,
        vec![
            (
                "email".into(),
                "failed".into(),
                Some("smtp_send_failed".into())
            ),
            ("in_app".into(), "delivered".into(), None),
        ],
        "in-app is not re-sent when only email fails"
    );
}

#[tokio::test]
async fn capacity_forecast_reports_insufficient_history_then_seasonal_pressure() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    let professional = create_professional(&state, &facility, &service).await;
    let tenant = tenant_of_facility(&state, &facility).await;
    let fid: Uuid = facility.parse().unwrap();

    // Purpose matters: capacity review is an operations/quality activity.
    let (st, denied) = call_with_purpose(
        &state,
        "POST",
        "/api/v1/capacity/forecasts",
        ADMIN,
        "treatment",
        Some(json!({ "facility_id": facility, "service_code": service })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{denied}");

    let (st, f) = call(
        &state,
        "POST",
        "/api/v1/capacity/forecasts",
        ADMIN,
        Some(json!({ "facility_id": facility, "service_code": service, "horizon_days": 14 })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{f}");
    assert_eq!(f["status"], "insufficient_history");
    assert_eq!(f["forecast_version"], "capacity-forecast.v1");
    assert_eq!(f["forecast"]["required_weeks"], 4);

    // Twelve weeks of synthetic fulfilled history: Mondays are twice as busy.
    let patient = register_patient(&state, &facility).await;
    let pid: Uuid = patient.parse().unwrap();
    let booked_by: Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = 'reg.rivera'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let today = Utc::now().date_naive();
    for days_back in 1..=84i64 {
        let date = today - Duration::days(days_back);
        let n = if date.weekday() == chrono::Weekday::Mon {
            4
        } else {
            2
        };
        for k in 0..n {
            let start = date.and_hms_opt(9 + k as u32, 0, 0).unwrap().and_utc();
            let status = if k == 0 && days_back % 7 == 0 {
                "no_show"
            } else {
                "fulfilled"
            };
            sqlx::query(
                "INSERT INTO appointments (id, tenant_id, facility_id, patient_id, service_code,
                     status, starts_at, ends_at, time_zone, booked_via, booked_by)
                 VALUES ($1,$2,$3,$4,$5,$6,$7,$8,'UTC','staff_direct',$9)",
            )
            .bind(Uuid::now_v7())
            .bind(tenant)
            .bind(fid)
            .bind(pid)
            .bind(&service)
            .bind(status)
            .bind(start)
            .bind(start + Duration::minutes(30))
            .bind(booked_by)
            .execute(&state.pool)
            .await
            .unwrap();
        }
    }

    // A tenant-configured seasonal period doubles demand across the horizon;
    // a one-day closure removes capacity. Nothing about a specific region is
    // hardcoded: both are operational-calendar rows.
    let horizon_start = today + Duration::days(1);
    let (st, season) = call(
        &state,
        "POST",
        "/api/v1/scheduling/calendar",
        ADMIN,
        Some(json!({
            "facility_id": facility,
            "kind": "seasonal_period",
            "name": "Summer pressure (synthetic)",
            "starts_on": horizon_start,
            "ends_on": horizon_start + Duration::days(13),
            "demand_multiplier": 1.5,
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{season}");
    let closure_day = horizon_start + Duration::days(3);
    let (st, closure) = call(
        &state,
        "POST",
        "/api/v1/scheduling/calendar",
        ADMIN,
        Some(json!({
            "facility_id": facility,
            "kind": "closure",
            "name": "Local holiday closure (synthetic)",
            "starts_on": closure_day,
            "ends_on": closure_day,
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{closure}");
    // Professional on leave for one horizon day.
    let leave_day = horizon_start + Duration::days(5);
    let (st, res) = call(
        &state,
        "GET",
        &format!("/api/v1/scheduling/resources/{professional}"),
        ADMIN,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{res}");
    let (st, exc) = call(
        &state,
        "POST",
        &format!("/api/v1/scheduling/resources/{professional}/exceptions"),
        ADMIN,
        Some(json!({
            "kind": "leave",
            "starts_at": leave_day.and_hms_opt(0, 0, 0).unwrap().and_utc(),
            "ends_at": (leave_day + Duration::days(1)).and_hms_opt(0, 0, 0).unwrap().and_utc(),
            "reason_code": "annual_leave",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{exc}");

    let (st, f) = call(
        &state,
        "POST",
        "/api/v1/capacity/forecasts",
        ADMIN,
        Some(json!({
            "facility_id": facility,
            "service_code": service,
            "horizon_start": horizon_start,
            "horizon_days": 14,
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{f}");
    assert_eq!(f["status"], "ready", "{f}");
    let fc = &f["forecast"];
    assert!(fc["history_weeks"].as_u64().unwrap() >= 4);
    let confidence = fc["confidence"].as_f64().unwrap();
    assert!(confidence > 0.0 && confidence <= 1.0);
    let days = fc["days"].as_array().unwrap();
    assert_eq!(days.len(), 14);
    let by_date = |d: NaiveDate| {
        days.iter()
            .find(|x| x["date"] == d.to_string())
            .cloned()
            .expect("day present")
    };
    let monday = (0..7)
        .map(|i| horizon_start + Duration::days(i))
        .find(|d| d.weekday() == chrono::Weekday::Mon && *d != closure_day && *d != leave_day)
        .or_else(|| {
            (7..14)
                .map(|i| horizon_start + Duration::days(i))
                .find(|d| d.weekday() == chrono::Weekday::Mon)
        })
        .unwrap();
    let tuesday = (0..14)
        .map(|i| horizon_start + Duration::days(i))
        .find(|d| d.weekday() == chrono::Weekday::Tue && *d != closure_day && *d != leave_day)
        .unwrap();
    let mon = by_date(monday);
    let tue = by_date(tuesday);
    assert!(
        mon["expected_demand"].as_f64().unwrap() > tue["expected_demand"].as_f64().unwrap(),
        "weekday baseline learned from history: {mon} vs {tue}"
    );
    let has = |d: &Value, code: &str| {
        d["factors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["code"] == code)
    };
    assert!(has(&mon, "weekday_baseline"), "{mon}");
    assert!(has(&mon, "calendar:seasonal_period"), "{mon}");
    assert!(has(&mon, "no_show_rate"), "{mon}");
    assert!(
        mon["demand_high"].as_f64().unwrap() >= mon["expected_demand"].as_f64().unwrap()
            && mon["demand_low"].as_f64().unwrap() <= mon["expected_demand"].as_f64().unwrap()
    );
    let closed = by_date(closure_day);
    assert!(has(&closed, "calendar:closure"), "{closed}");
    assert_eq!(
        closed["available_capacity"].as_f64().unwrap(),
        0.0,
        "{closed}"
    );
    assert!(
        closed["gap"].as_f64().unwrap() < 0.0,
        "closure creates a shortfall"
    );
    let leave = by_date(leave_day);
    assert!(has(&leave, "resource_exceptions"), "{leave}");
    assert!(
        leave["available_capacity"].as_f64().unwrap() < tue["available_capacity"].as_f64().unwrap(),
        "{leave} vs {tue}"
    );
    assert!(!fc["pressure_days"].as_array().unwrap().is_empty(), "{fc}");
    assert!(fc["recommendations"].as_array().is_some());

    // The governed explanation cites the forecast, never alters it.
    let fid_str = s(&f["id"]);
    let before: Value = sqlx::query_scalar("SELECT output FROM capacity_forecasts WHERE id = $1")
        .bind(fid_str.parse::<Uuid>().unwrap())
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let (st, ex) = call(
        &state,
        "POST",
        &format!("/api/v1/capacity/forecasts/{fid_str}/explain"),
        ADMIN,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ex}");
    assert_eq!(ex["explanation"]["mode"], "dmind", "{ex}");
    assert_eq!(ex["explanation"]["synthetic"], true, "{ex}");
    assert!(ex["explanation"]["artifact_id"].is_string(), "{ex}");
    assert!(ex["explanation"]["explanation"].is_object(), "{ex}");
    assert_eq!(ex["forecast"]["id"], f["id"]);
    let artifact_id: Uuid = s(&ex["explanation"]["artifact_id"]).parse().unwrap();
    let (artifact_type, synthetic, forecast_ref): (String, bool, Option<Uuid>) = sqlx::query_as(
        "SELECT artifact_type, synthetic, capacity_forecast_id FROM ai_artifacts WHERE id = $1",
    )
    .bind(artifact_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(artifact_type, "capacity_explanation");
    assert!(synthetic);
    assert_eq!(forecast_ref, Some(fid_str.parse::<Uuid>().unwrap()));
    // The same forecast + language reuses the governed artifact.
    let (st, again) = call(
        &state,
        "POST",
        &format!("/api/v1/capacity/forecasts/{fid_str}/explain"),
        ADMIN,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{again}");
    assert_eq!(again["explanation"]["reused"], true, "{again}");
    // Reuse follows the governed pattern: a new scoped artifact row that
    // links to the prior one instead of another provider execution.
    let again_id: Uuid = s(&again["explanation"]["artifact_id"]).parse().unwrap();
    assert_ne!(again_id, artifact_id);
    let (reused_from, again_synthetic): (Option<Uuid>, bool) =
        sqlx::query_as("SELECT reused_from, synthetic FROM ai_artifacts WHERE id = $1")
            .bind(again_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(reused_from, Some(artifact_id));
    assert!(again_synthetic);
    let executions: i64 =
        sqlx::query_scalar("SELECT count(*) FROM ai_executions WHERE artifact_id IN ($1, $2)")
            .bind(artifact_id)
            .bind(again_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(executions, 1, "reuse must not spend a provider execution");
    let after: Value = sqlx::query_scalar("SELECT output FROM capacity_forecasts WHERE id = $1")
        .bind(fid_str.parse::<Uuid>().unwrap())
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(before, after, "explanation must not alter the forecast");

    // Listing is tenant scoped and paginated; another tenant sees nothing.
    let (st, list) = call(
        &state,
        "GET",
        &format!("/api/v1/capacity/forecasts?facility_id={facility}&limit=5"),
        ADMIN,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{list}");
    let forecasts = list["forecasts"].as_array().unwrap();
    assert!(!forecasts.is_empty() && forecasts.len() <= 5, "{list}");
    assert!(
        forecasts.iter().all(|f| f["facility_id"] == facility),
        "{list}"
    );
    assert_eq!(list["forecast_version"], "capacity-forecast.v1");
    let (st, _) = call_with_purpose(
        &state,
        "GET",
        &format!("/api/v1/capacity/forecasts/{fid_str}"),
        OTHER_TENANT,
        "operations",
        None,
    )
    .await;
    assert!(
        st == StatusCode::NOT_FOUND || st == StatusCode::FORBIDDEN,
        "{st}"
    );
}

async fn accessibility_code(state: &AppState) -> String {
    let code = uniq("wheelchair").replace('-', "_");
    let (st, v) = call(
        state,
        "POST",
        "/api/v1/catalog",
        ADMIN,
        Some(json!({
            "kind": "accessibility_capability",
            "code": code,
            "name_en": "Wheelchair accessible vehicle",
            "name_es": "Vehículo adaptado para silla de ruedas",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    code
}

#[tokio::test]
async fn transport_requires_consent_encrypts_addresses_and_purges_live_locations() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let vehicle = create_resource(&state, &facility, "accessible_vehicle", None).await;
    let patient = register_patient(&state, &facility).await;
    let appointment = book(&state, &facility, &patient, &service, 3).await;
    let aid = s(&appointment["id"]);
    let need = accessibility_code(&state).await;

    let body = json!({
        "appointment_id": aid,
        "requirements": [need],
        "origin_area_code": "07800",
        "pickup_address": "Carrer Synthetic 12, 2A",
    });
    // Without transport consent the request is refused.
    let (st, denied) = call(&state, "POST", "/api/v1/transport", REG, Some(body.clone())).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{denied}");
    assert_eq!(code(&denied), "consent_required");
    set_consent(&state, &patient, "transport_coordination", "active").await;

    // Unknown accessibility codes are rejected (catalog is the source of truth).
    let mut bad = body.clone();
    bad["requirements"] = json!(["not_a_code"]);
    let (st, denied) = call(&state, "POST", "/api/v1/transport", REG, Some(bad)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{denied}");
    assert_eq!(code(&denied), "unknown_accessibility_code");

    let (st, t) = call(&state, "POST", "/api/v1/transport", REG, Some(body.clone())).await;
    assert_eq!(st, StatusCode::CREATED, "{t}");
    assert_eq!(t["status"], "requested");
    assert_eq!(t["requirements"][0], need);
    assert!(
        t.get("pickup_address").is_none(),
        "address never in the summary: {t}"
    );
    let tid = s(&t["id"]);
    let tid_uuid: Uuid = tid.parse().unwrap();

    // A second active request for the same appointment is a conflict.
    let (st, dup) = call(&state, "POST", "/api/v1/transport", REG, Some(body.clone())).await;
    assert_eq!(st, StatusCode::CONFLICT, "{dup}");
    assert_eq!(code(&dup), "transport_exists");

    // The stored address is ciphertext, and the sensitive read is audited.
    let stored: Vec<u8> =
        sqlx::query_scalar("SELECT pickup_address_enc FROM transport_requests WHERE id = $1")
            .bind(tid_uuid)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(!String::from_utf8_lossy(&stored).contains("Carrer Synthetic"));
    let (st, addr) = call(
        &state,
        "GET",
        &format!("/api/v1/transport/{tid}/address"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{addr}");
    assert_eq!(addr["pickup_address"], "Carrer Synthetic 12, 2A");
    let tenant = tenant_of_facility(&state, &facility).await;
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outbox_events WHERE tenant_id = $1 AND event_type = 'transport.address.read'
           AND resource_refs->>'transport_request_id' = $2",
    )
    .bind(tenant)
    .bind(&tid)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(audited, 1);

    // Scheduling books the vehicle; a pickup window and vehicle are mandatory.
    let (st, denied) = call(
        &state,
        "POST",
        &format!("/api/v1/transport/{tid}/transition"),
        REG,
        Some(json!({ "status": "scheduled", "version": t["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{denied}");
    let starts: chrono::DateTime<Utc> = s(&appointment["starts_at"]).parse().unwrap();
    let window = json!({
        "status": "scheduled",
        "version": t["version"],
        "vehicle_resource_id": vehicle,
        "pickup_window_start": starts - Duration::minutes(90),
        "pickup_window_end": starts - Duration::minutes(30),
    });
    let (st, scheduled) = call(
        &state,
        "POST",
        &format!("/api/v1/transport/{tid}/transition"),
        REG,
        Some(window.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{scheduled}");
    assert_eq!(scheduled["status"], "scheduled");
    assert_eq!(scheduled["vehicle_resource_id"], vehicle);

    // The same vehicle cannot be double-booked over an overlapping window.
    let other = register_patient(&state, &facility).await;
    set_consent(&state, &other, "transport_coordination", "active").await;
    let other_appt = book(&state, &facility, &other, &service, 3).await;
    let (st, t2) = call(
        &state,
        "POST",
        "/api/v1/transport",
        REG,
        Some(json!({ "appointment_id": other_appt["id"], "origin_area_code": "07800" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{t2}");
    let (st, clash) = call(
        &state,
        "POST",
        &format!("/api/v1/transport/{}/transition", s(&t2["id"])),
        REG,
        Some(json!({
            "status": "scheduled",
            "version": t2["version"],
            "vehicle_resource_id": vehicle,
            "pickup_window_start": starts - Duration::minutes(60),
            "pickup_window_end": starts - Duration::minutes(15),
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{clash}");
    assert_eq!(code(&clash), "vehicle_unavailable");

    // Live location: refused before the episode is under way? scheduled is
    // open; the coordinates are ciphertext at rest and expire.
    let (st, loc) = call(
        &state,
        "POST",
        &format!("/api/v1/transport/{tid}/location"),
        REG,
        Some(json!({ "latitude": 38.9089, "longitude": 1.4321 })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{loc}");
    let raw: Vec<u8> = sqlx::query_scalar(
        "SELECT coordinates_enc FROM transport_live_locations WHERE transport_request_id = $1 LIMIT 1",
    )
    .bind(tid_uuid)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(!String::from_utf8_lossy(&raw).contains("38.9"));
    let (st, positions) = call(
        &state,
        "GET",
        &format!("/api/v1/transport/{tid}/location"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{positions}");
    assert_eq!(positions["positions"].as_array().unwrap().len(), 1);
    let location_reads: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outbox_events WHERE tenant_id = $1 AND event_type = 'transport.location.read'
           AND resource_refs->>'transport_request_id' = $2",
    )
    .bind(tenant)
    .bind(&tid)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(location_reads, 1);

    // Expired positions are purged by the worker.
    sqlx::query(
        "UPDATE transport_live_locations SET expires_at = now() - interval '1 second' WHERE transport_request_id = $1",
    )
    .bind(tid_uuid)
    .execute(&state.pool)
    .await
    .unwrap();
    tick(&state).await;
    let remaining: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM transport_live_locations WHERE transport_request_id = $1",
    )
    .bind(tid_uuid)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(remaining, 0);

    // Progress the episode; completion releases the vehicle booking and
    // deletes remaining live positions; the address is no longer readable.
    let mut version = scheduled["version"].clone();
    for next in ["en_route", "picked_up", "completed"] {
        let (st, moved) = call(
            &state,
            "POST",
            &format!("/api/v1/transport/{tid}/transition"),
            REG,
            Some(json!({ "status": next, "version": version })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{next}: {moved}");
        assert_eq!(moved["status"], next);
        version = moved["version"].clone();
    }
    let active_bookings: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM resource_bookings WHERE resource_id = $1 AND status = 'active'",
    )
    .bind(vehicle.parse::<Uuid>().unwrap())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(active_bookings, 0);
    let (st, gone) = call(
        &state,
        "GET",
        &format!("/api/v1/transport/{tid}/address"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{gone}");

    // The second request cannot be scheduled by someone outside the facility
    // scope, nor seen by another tenant.
    let (st, _) = call_with_purpose(
        &state,
        "GET",
        &format!("/api/v1/transport/{}", s(&t2["id"])),
        OTHER_TENANT,
        "operations",
        None,
    )
    .await;
    assert!(
        st == StatusCode::NOT_FOUND || st == StatusCode::FORBIDDEN,
        "{st}"
    );
    // Physicians have no transport permission at all.
    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/transport/{}", s(&t2["id"])),
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Cancelling the second appointment cancels its transport in the same
    // transaction and emits a transport status notice.
    let (st, fresh) = call(
        &state,
        "GET",
        &format!("/api/v1/appointments/{}", s(&other_appt["id"])),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    cancel(&state, &fresh).await;
    let (st, t2_after) = call(
        &state,
        "GET",
        &format!("/api/v1/transport/{}", s(&t2["id"])),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t2_after}");
    assert_eq!(t2_after["status"], "cancelled");
    let kinds = notification_kinds(&state, &other).await;
    assert!(
        kinds.iter().any(|(k, _)| k == "transport_status"),
        "{kinds:?}"
    );
}

#[tokio::test]
async fn emergency_transport_needs_a_human_coordinator_decision() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let patient = register_patient(&state, &facility).await;
    set_consent(&state, &patient, "transport_coordination", "active").await;
    let appointment = book(&state, &facility, &patient, &service, 3).await;
    let ambulance = create_resource(&state, &facility, "ambulance", None).await;

    // Staff with transport.coordinate may flag an emergency; it is recorded
    // as their decision and audited. The API never dispatches on its own.
    let (st, t) = call(
        &state,
        "POST",
        "/api/v1/transport",
        REG,
        Some(json!({
            "appointment_id": appointment["id"],
            "emergency": true,
            "origin_area_code": "07800",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{t}");
    assert_eq!(t["emergency"], true);
    assert_eq!(t["status"], "requested", "no automatic dispatch");
    let tenant = tenant_of_facility(&state, &facility).await;
    let decided: Vec<Value> = sqlx::query_scalar(
        "SELECT resource_refs FROM outbox_events WHERE tenant_id = $1
           AND event_type = 'transport.requested'
           AND resource_refs->>'transport_request_id' = $2",
    )
    .bind(tenant)
    .bind(s(&t["id"]))
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(decided.len(), 1, "{decided:?}");
    assert_eq!(decided[0]["emergency"], true);
    assert!(decided[0]["authorized_by"].is_string(), "{decided:?}");
    assert_eq!(decided[0]["channel"], "staff");
    assert_eq!(t["authorized_by"], decided[0]["authorized_by"]);

    // A patient (self-service) can never flag an emergency: that path is
    // reserved for an authorized human coordinator.
    let other = register_patient(&state, &facility).await;
    set_consent(&state, &other, "transport_coordination", "active").await;
    let other_appt = book(&state, &facility, &other, &service, 3).await;
    let patient_token = grant_self(&state, &other).await;
    let (st, denied) = call_with_purpose(
        &state,
        "POST",
        "/api/v1/me/transport",
        &patient_token,
        "treatment",
        Some(json!({
            "appointment_id": other_appt["id"],
            "emergency": true,
            "origin_area_code": "07800",
        })),
    )
    .await;
    // The patient body has no emergency field at all: the flag is rejected
    // at the schema boundary and no transport request is created.
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{denied}");
    let created: i64 =
        sqlx::query_scalar("SELECT count(*) FROM transport_requests WHERE appointment_id = $1")
            .bind(s(&other_appt["id"]).parse::<Uuid>().unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(created, 0);

    // The ambulance is only ever assigned by a coordinator transition.
    let starts: chrono::DateTime<Utc> = s(&appointment["starts_at"]).parse().unwrap();
    let (st, sched) = call(
        &state,
        "POST",
        &format!("/api/v1/transport/{}/transition", s(&t["id"])),
        REG,
        Some(json!({
            "status": "scheduled",
            "version": t["version"],
            "vehicle_resource_id": ambulance,
            "pickup_window_start": starts - Duration::minutes(45),
            "pickup_window_end": starts - Duration::minutes(15),
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sched}");
    assert_eq!(sched["vehicle_resource_id"], ambulance);

    // Cancelling or failing requires a reason.
    let (st, denied) = call(
        &state,
        "POST",
        &format!("/api/v1/transport/{}/transition", s(&t["id"])),
        REG,
        Some(json!({ "status": "cancelled", "version": sched["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{denied}");
    let (st, cancelled) = call(
        &state,
        "POST",
        &format!("/api/v1/transport/{}/transition", s(&t["id"])),
        REG,
        Some(json!({
            "status": "cancelled",
            "version": sched["version"],
            "reason": "Family will bring the patient",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{cancelled}");
    assert_eq!(cancelled["status"], "cancelled");
}

#[tokio::test]
async fn transport_fails_closed_without_an_encryption_key() {
    let base = test_state().await;
    let facility = main_facility(&base).await;
    let service = create_service(&base).await;
    create_professional(&base, &facility, &service).await;
    let patient = register_patient(&base, &facility).await;
    set_consent(&base, &patient, "transport_coordination", "active").await;
    let appointment = book(&base, &facility, &patient, &service, 3).await;

    let mut runtime = wellos_server::runtime::RuntimeConfig::test_fixtures();
    runtime.location.keyring = None;
    let state = AppState::from_runtime(
        base.pool.clone(),
        Arc::new(dmind_gateway::fake::FakeProvider::new()),
        Arc::new(dmind_gateway::scribe::FakeTranscription::new()),
        wellos_server::state::AuthConfig::development(),
        runtime,
    );

    // Requests carrying an address cannot be stored without a key.
    let (st, denied) = call(
        &state,
        "POST",
        "/api/v1/transport",
        REG,
        Some(json!({
            "appointment_id": appointment["id"],
            "pickup_address": "Carrer Synthetic 12",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{denied}");
    assert_eq!(code(&denied), "encryption_unavailable");
    let stored: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM transport_requests WHERE appointment_id = $1")
            .bind(s(&appointment["id"]).parse::<Uuid>().unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(stored, 0, "nothing persisted in plaintext");

    // Area-only requests still work (no sensitive field to protect)...
    let (st, t) = call(
        &state,
        "POST",
        "/api/v1/transport",
        REG,
        Some(json!({ "appointment_id": appointment["id"], "origin_area_code": "07800" })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{t}");
    // ...but live coordinates fail closed too.
    let (st, denied) = call(
        &state,
        "POST",
        &format!("/api/v1/transport/{}/location", s(&t["id"])),
        REG,
        Some(json!({ "latitude": 38.9, "longitude": 1.43 })),
    )
    .await;
    assert!(
        st == StatusCode::SERVICE_UNAVAILABLE || st == StatusCode::CONFLICT,
        "{st} {denied}"
    );
    let positions: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM transport_live_locations WHERE transport_request_id = $1",
    )
    .bind(s(&t["id"]).parse::<Uuid>().unwrap())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(positions, 0, "no plaintext coordinates persisted");
}
