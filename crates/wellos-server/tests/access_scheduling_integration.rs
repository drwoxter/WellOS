//! Integration tests for dMind Access v1: runtime catalogs, schedulable
//! resources, access requests, the deterministic matcher, offers and holds,
//! appointments and their linked visits, ICS export and the security
//! boundaries around them.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{DateTime, Duration, Utc};
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

async fn raw(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, axum::http::HeaderMap, Vec<u8>) {
    // Administration is an operations-purpose activity; everything else in
    // these tests runs under the default treatment purpose.
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
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    (status, headers, bytes.to_vec())
}

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let (status, _, bytes) = raw(state, method, path, token, body).await;
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn uniq(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::now_v7().simple())
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

const REG: &str = "dev-reg.rivera";
const NURSE: &str = "dev-nurse.kim";
const GARCIA: &str = "dev-dr.garcia";
const ADMIN: &str = "dev-admin.silva";
const OTHER_TENANT: &str = "dev-dr.sur";

async fn main_facility(state: &AppState) -> String {
    let (st, meta) = call(state, "GET", "/api/v1/meta/tenant", REG, None).await;
    assert_eq!(st, StatusCode::OK);
    s(&meta["facilities"][0]["id"])
}

async fn register_patient(state: &AppState, facility: &str) -> String {
    let (st, patient) = call(
        state,
        "POST",
        "/api/v1/patients",
        REG,
        Some(json!({
            "facility_id": facility,
            "family_name": "Scheduling",
            "given_name": "Synthetic",
            "birth_date": "1980-03-09",
            "sex": "female",
            "identifier": uniq("MRN-SCH"),
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{patient}");
    s(&patient["id"])
}

/// A professional resource at `facility` delivering `service` every day
/// 08:00-18:00 local time, so the matcher always has capacity in its window.
/// Each test owns a runtime clinical service (and therefore its own
/// capacity), so parallel tests never compete for the same slots.
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
            "name_en": "Runtime service",
            "name_es": "Servicio en tiempo de ejecución",
            "config": { "duration_minutes": 30, "modality_codes": ["in_person"], "required_resource_types": ["professional"] },
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    service
}

async fn create_professional(state: &AppState, facility: &str, service: &str) -> String {
    let (st, res) = call(
        state,
        "POST",
        "/api/v1/scheduling/resources",
        ADMIN,
        Some(json!({
            "facility_id": facility,
            "resource_type_code": "professional",
            "name": uniq("Dr Synthetic"),
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

async fn submit_request(state: &AppState, facility: &str, patient: &str, service: &str) -> Value {
    let (st, req) = call(
        state,
        "POST",
        "/api/v1/access-requests",
        REG,
        Some(json!({
            "patient_id": patient,
            "facility_id": facility,
            "free_text": "Routine follow-up, mornings preferred",
            "constraints": { "service_code": service, "facility_ids": [facility] },
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{req}");
    req
}

#[tokio::test]
async fn golden_path_request_match_hold_accept_visit_ics_cancel() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let patient = register_patient(&state, &facility).await;

    let req = submit_request(&state, &facility, &patient, &service).await;
    assert_eq!(req["status"], "submitted");
    assert_eq!(req["urgency"], "routine");
    let rid = s(&req["id"]);

    let (st, m) = call(
        &state,
        "POST",
        &format!("/api/v1/access-requests/{rid}/match"),
        REG,
        Some(json!({ "version": req["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{m}");
    assert_eq!(m["request"]["status"], "options_ready");
    assert_eq!(m["matcher_version"], "access-matcher.v1");
    let offers = m["offers"].as_array().unwrap();
    assert!(!offers.is_empty(), "{m}");
    assert!(offers.iter().all(|o| o["status"] == "offered"));
    assert!(
        offers.iter().all(|o| o["score"]["factors"].is_array()),
        "{}",
        offers[0]
    );
    let run_id = s(&m["matcher_run_id"]);

    let (st, run) = call(
        &state,
        "GET",
        &format!("/api/v1/matcher-runs/{run_id}"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{run}");
    assert_eq!(run["matcher_version"], "access-matcher.v1");
    assert!(run["source_facts"]["facts_hash"].as_str().is_some());

    // Offers and holds create no visit.
    let patient_uuid: Uuid = patient.parse().unwrap();
    let visit_count = |state: &AppState| {
        let pool = state.pool.clone();
        async move {
            sqlx::query_scalar::<_, i64>("SELECT COUNT(*) FROM visits WHERE patient_id = $1")
                .bind(patient_uuid)
                .fetch_one(&pool)
                .await
                .unwrap()
        }
    };
    assert_eq!(visit_count(&state).await, 0);

    let offer = &offers[0];
    let oid = s(&offer["id"]);
    let (st, held) = call(
        &state,
        "POST",
        &format!("/api/v1/offers/{oid}/hold"),
        REG,
        Some(json!({ "version": offer["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{held}");
    assert_eq!(held["status"], "held");
    assert!(held["hold_expires_at"].as_str().is_some());
    assert_eq!(
        visit_count(&state).await,
        0,
        "a hold must not create a visit"
    );

    let (st, acc) = call(
        &state,
        "POST",
        &format!("/api/v1/offers/{oid}/accept"),
        REG,
        Some(json!({ "version": held["version"], "idempotency_key": uniq("acc") })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{acc}");
    assert_eq!(acc["status"], "confirmed");
    assert_eq!(acc["starts_at"], offer["starts_at"]);
    let aid = s(&acc["id"]);
    let visit_id = s(&acc["visit_id"]);
    assert!(
        !visit_id.is_empty(),
        "confirmed appointment links a visit: {acc}"
    );

    // Sibling offers are revoked; the request is booked.
    let (st, after) = call(
        &state,
        "GET",
        &format!("/api/v1/access-requests/{rid}"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{after}");
    assert_eq!(after["request"]["status"], "booked");
    assert_eq!(s(&after["request"]["appointment_id"]), aid);
    let (st, offs) = call(
        &state,
        "GET",
        &format!("/api/v1/access-requests/{rid}/offers"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{offs}");
    for o in offs["items"].as_array().unwrap() {
        if s(&o["id"]) == oid {
            assert_eq!(o["status"], "accepted");
        } else {
            assert_eq!(o["status"], "revoked", "{o}");
        }
    }

    // Exactly one visit appeared, scheduled at the appointment time.
    let (st, visit) = call(
        &state,
        "GET",
        &format!("/api/v1/visits/{visit_id}"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{visit}");
    assert_eq!(visit["status"], "scheduled");
    assert_eq!(visit["scheduled_at"], acc["starts_at"]);
    assert_eq!(s(&visit["patient"]["id"]), patient);
    assert_eq!(visit_count(&state).await, 1, "exactly one linked visit");

    // The staff list (agenda/worklists) returns the confirmed appointment
    // with its resources and patient summary inside the requested window.
    let starts_at: DateTime<Utc> = s(&acc["starts_at"]).parse().unwrap();
    let (st, listed) = call(
        &state,
        "GET",
        &format!(
            "/api/v1/appointments?from={}&to={}&status=confirmed&patient_id={patient}",
            (starts_at - Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
            (starts_at + Duration::hours(1)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true),
        ),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{listed}");
    let listed_items = listed["items"].as_array().unwrap();
    assert_eq!(listed_items.len(), 1, "{listed}");
    assert_eq!(s(&listed_items[0]["id"]), aid);
    assert!(listed_items[0]["resources"].is_array());
    assert_eq!(s(&listed_items[0]["patient"]["id"]), patient, "{listed}");

    // ICS: one confirmed event, no clinical free text.
    let (st, headers, bytes) = raw(
        &state,
        "GET",
        &format!("/api/v1/appointments/{aid}/ics"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(headers["content-type"]
        .to_str()
        .unwrap()
        .starts_with("text/calendar"));
    let ics = String::from_utf8(bytes).unwrap();
    assert!(ics.contains("BEGIN:VEVENT"));
    assert_eq!(ics.matches("BEGIN:VEVENT").count(), 1);
    assert!(ics.contains("STATUS:CONFIRMED"));
    assert!(!ics.contains("Routine follow-up"));
    assert!(
        !ics.contains("Synthetic"),
        "patient name must not leak: {ics}"
    );

    // The slot is inside the policy notice window: staff need an override
    // reason, and the reason is mandatory.
    let (st, denied) = call(
        &state,
        "POST",
        &format!("/api/v1/appointments/{aid}/cancel"),
        REG,
        Some(json!({ "version": acc["version"], "reason_code": "patient_request" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{denied}");
    assert_eq!(code(&denied), "override_required");

    // Cancel with an override reason; the visit follows in the same transaction.
    let (st, cancelled) = call(
        &state,
        "POST",
        &format!("/api/v1/appointments/{aid}/cancel"),
        REG,
        Some(json!({
            "version": acc["version"],
            "reason_code": "patient_request",
            "override_reason": "Patient phoned: admitted elsewhere",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{cancelled}");
    assert_eq!(cancelled["status"], "cancelled");
    let (st, visit) = call(
        &state,
        "GET",
        &format!("/api/v1/visits/{visit_id}"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(visit["status"], "cancelled", "{visit}");

    // History keeps every step.
    let (st, hist) = call(
        &state,
        "GET",
        &format!("/api/v1/appointments/{aid}/history"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{hist}");
    assert!(hist["items"].as_array().unwrap().len() >= 2);
}

#[tokio::test]
async fn concurrent_acceptance_of_the_same_capacity_books_once() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let p1 = register_patient(&state, &facility).await;
    let p2 = register_patient(&state, &facility).await;
    let r1 = submit_request(&state, &facility, &p1, &service).await;
    let r2 = submit_request(&state, &facility, &p2, &service).await;
    let (st, m1) = call(
        &state,
        "POST",
        &format!("/api/v1/access-requests/{}/match", s(&r1["id"])),
        REG,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{m1}");
    let (st, m2) = call(
        &state,
        "POST",
        &format!("/api/v1/access-requests/{}/match", s(&r2["id"])),
        REG,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{m2}");
    // Find an identical slot on the same primary resource offered to both.
    let o2s = m2["offers"].as_array().unwrap();
    let pair = m1["offers"].as_array().unwrap().iter().find_map(|a| {
        o2s.iter()
            .find(|b| b["starts_at"] == a["starts_at"] && b["resources"] == a["resources"])
            .map(|b| (a.clone(), b.clone()))
    });
    let (a, b) = pair.expect("both patients are offered the same slot");
    let pa = format!("/api/v1/offers/{}/accept", s(&a["id"]));
    let pb = format!("/api/v1/offers/{}/accept", s(&b["id"]));
    let (ra, rb) = tokio::join!(
        call(
            &state,
            "POST",
            &pa,
            REG,
            Some(json!({ "version": a["version"] })),
        ),
        call(
            &state,
            "POST",
            &pb,
            REG,
            Some(json!({ "version": b["version"] })),
        ),
    );
    let outcomes = [ra, rb];
    let ok = outcomes
        .iter()
        .filter(|(st, _)| *st == StatusCode::CREATED)
        .count();
    let lost = outcomes
        .iter()
        .filter(|(st, v)| *st == StatusCode::CONFLICT && code(v) == "slot_taken")
        .count();
    assert_eq!((ok, lost), (1, 1), "{outcomes:?}");
}

#[tokio::test]
async fn red_flag_text_routes_to_triage_and_blocks_matching() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let patient = register_patient(&state, &facility).await;
    let (st, req) = call(
        &state,
        "POST",
        "/api/v1/access-requests",
        REG,
        Some(json!({
            "patient_id": patient,
            "facility_id": facility,
            "free_text": "Crushing chest pain since this morning, short of breath",
            "constraints": { "service_code": service },
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{req}");
    assert_eq!(req["status"], "needs_clinical_triage", "{req}");
    let rid = s(&req["id"]);
    let (st, m) = call(
        &state,
        "POST",
        &format!("/api/v1/access-requests/{rid}/match"),
        REG,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{m}");
    // Registration cannot clear triage; a nurse can and must set urgency.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/access-requests/{rid}/clear-triage"),
        REG,
        Some(json!({ "reason": "urgency:priority; spoke to patient" })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/access-requests/{rid}/clear-triage"),
        NURSE,
        Some(json!({ "reason": "urgency:priority; nurse assessment: stable, needs early review" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "submitted");
    assert_eq!(v["urgency"], "priority");
    assert_eq!(v["urgency_source"], "human");
    let (st, m) = call(
        &state,
        "POST",
        &format!("/api/v1/access-requests/{rid}/match"),
        REG,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{m}");
    assert!(!m["offers"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn access_request_isolation_and_role_boundaries() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let patient = register_patient(&state, &facility).await;
    let req = submit_request(&state, &facility, &patient, &service).await;
    let rid = s(&req["id"]);
    // Another tenant sees nothing.
    for path in [
        format!("/api/v1/access-requests/{rid}"),
        format!("/api/v1/access-requests/{rid}/history"),
        format!("/api/v1/access-requests/{rid}/offers"),
    ] {
        let (st, _) = call(&state, "GET", &path, OTHER_TENANT, None).await;
        assert_eq!(st, StatusCode::NOT_FOUND, "{path}");
    }
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/access-requests/{rid}/match"),
        OTHER_TENANT,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    // A physician may read but not manage scheduling.
    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/access-requests/{rid}"),
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/access-requests/{rid}/match"),
        GARCIA,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    // Stale versions are rejected.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/access-requests/{rid}/withdraw"),
        REG,
        Some(json!({ "version": 999 })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/access-requests/{rid}/withdraw"),
        REG,
        Some(json!({ "version": req["version"], "reason": "patient changed mind" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "withdrawn");
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/access-requests/{rid}/submit"),
        REG,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(code(&v), "invalid_transition");
}

#[tokio::test]
async fn unknown_specialty_added_at_runtime_becomes_schedulable() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let specialty = uniq("spec").replace('-', "_");
    let service = uniq("svc").replace('-', "_");
    for (kind, code_, config) in [
        ("specialty", specialty.as_str(), json!({})),
        (
            "clinical_service",
            service.as_str(),
            json!({ "duration_minutes": 30, "modality_codes": ["in_person"], "required_resource_types": ["professional"], "preparation_en": "Bring previous reports", "preparation_es": "Traiga informes previos" }),
        ),
    ] {
        let (st, v) = call(
            &state,
            "POST",
            "/api/v1/catalog",
            ADMIN,
            Some(json!({
                "kind": kind,
                "code": code_,
                "name_en": format!("Runtime {kind}"),
                "name_es": format!("{kind} en tiempo de ejecución"),
                "config": config,
            })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
    }
    let (st, res) = call(
        &state,
        "POST",
        "/api/v1/scheduling/resources",
        ADMIN,
        Some(json!({
            "facility_id": facility,
            "resource_type_code": "professional",
            "name": uniq("Runtime specialist"),
            "specialty_codes": [specialty],
            "services": [{ "service_code": service, "modality_codes": ["in_person"] }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{res}");
    let id = s(&res["id"]);
    let rules: Vec<Value> = (1..=7)
        .map(|d| json!({ "weekday": d, "start_local": "09:00:00", "end_local": "13:00:00" }))
        .collect();
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/scheduling/resources/{id}/availability"),
        ADMIN,
        Some(json!({ "version": res["version"], "rules": rules })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let patient = register_patient(&state, &facility).await;
    let req = submit_request(&state, &facility, &patient, &service).await;
    let (st, m) = call(
        &state,
        "POST",
        &format!("/api/v1/access-requests/{}/match", s(&req["id"])),
        REG,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{m}");
    let offers = m["offers"].as_array().unwrap();
    assert!(!offers.is_empty());
    assert!(offers.iter().all(|o| o["service_code"] == service));
    let oid = s(&offers[0]["id"]);
    let (st, acc) = call(
        &state,
        "POST",
        &format!("/api/v1/offers/{oid}/accept"),
        REG,
        Some(json!({ "version": offers[0]["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{acc}");
    let (st, detail) = call(
        &state,
        "GET",
        &format!("/api/v1/appointments/{}", s(&acc["id"])),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{detail}");
    assert_eq!(
        detail["service"]["preparation_en"],
        "Bring previous reports"
    );
}

/// Book the first offer of a fresh request for `patient` and return the
/// confirmed appointment together with the offer it came from.
async fn book_first_offer(
    state: &AppState,
    facility: &str,
    patient: &str,
    service: &str,
) -> (Value, Value) {
    let req = submit_request(state, facility, patient, service).await;
    let (st, m) = call(
        state,
        "POST",
        &format!("/api/v1/access-requests/{}/match", s(&req["id"])),
        REG,
        Some(json!({ "version": req["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{m}");
    let offer = m["offers"][0].clone();
    assert!(offer.is_object(), "{m}");
    let (st, acc) = call(
        state,
        "POST",
        &format!("/api/v1/offers/{}/accept", s(&offer["id"])),
        REG,
        Some(json!({ "version": offer["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{acc}");
    (acc, offer)
}

async fn visit_status(state: &AppState, visit_id: &str) -> String {
    let (st, v) = call(
        state,
        "GET",
        &format!("/api/v1/visits/{visit_id}"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    s(&v["status"])
}

#[tokio::test]
async fn reschedule_keeps_history_and_moves_the_linked_visit() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let patient = register_patient(&state, &facility).await;
    let (first, _) = book_first_offer(&state, &facility, &patient, &service).await;
    let first_id = s(&first["id"]);
    let visit_id = s(&first["visit_id"]);
    let first_start = s(&first["starts_at"]);

    // Staff open reschedule options with a reason; the prior appointment is
    // untouched until an option is accepted.
    let (st, opts) = call(
        &state,
        "POST",
        &format!("/api/v1/appointments/{first_id}/reschedule-options"),
        REG,
        Some(json!({ "version": first["version"], "reason": "Clinic asked to move the slot" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{opts}");
    assert_eq!(opts["request"]["status"], "options_ready");
    assert_eq!(
        s(&opts["request"]["constraints"]["reschedule_of"]),
        first_id
    );
    let (st, still) = call(
        &state,
        "GET",
        &format!("/api/v1/appointments/{first_id}"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(still["status"], "confirmed", "{still}");

    // Pick an option at a different time and accept it as the replacement.
    let offer = opts["offers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| s(&o["starts_at"]) != first_start)
        .cloned()
        .expect("an alternative slot");
    let (st, second) = call(
        &state,
        "POST",
        &format!("/api/v1/offers/{}/accept", s(&offer["id"])),
        REG,
        Some(json!({
            "version": offer["version"],
            "reschedule_of": first_id,
            "reschedule_reason": "Clinic asked to move the slot",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{second}");
    assert_eq!(second["status"], "confirmed");
    assert_eq!(s(&second["rescheduled_from"]), first_id);
    assert_ne!(s(&second["starts_at"]), first_start);

    // The prior appointment is history (not overwritten), the single visit
    // moved to the new time and now belongs to the new appointment.
    let (st, prior) = call(
        &state,
        "GET",
        &format!("/api/v1/appointments/{first_id}"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(prior["status"], "rescheduled", "{prior}");
    assert_eq!(s(&prior["rescheduled_to"]), s(&second["id"]));
    assert_eq!(
        s(&prior["starts_at"]),
        first_start,
        "prior time is preserved"
    );
    assert_eq!(
        s(&second["visit_id"]),
        visit_id,
        "the visit is moved, not duplicated"
    );
    let (st, visit) = call(
        &state,
        "GET",
        &format!("/api/v1/visits/{visit_id}"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(visit["status"], "scheduled");
    assert_eq!(visit["scheduled_at"], second["starts_at"], "{visit}");
    let patient_uuid: Uuid = patient.parse().unwrap();
    let visits: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM visits WHERE patient_id = $1")
        .bind(patient_uuid)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(visits, 1);
    // The freed slot is released at the database level.
    let active_prior: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM resource_bookings WHERE appointment_id = $1 AND status = 'active'",
    )
    .bind(first_id.parse::<Uuid>().unwrap())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(active_prior, 0);
    let (st, hist) = call(
        &state,
        "GET",
        &format!("/api/v1/appointments/{first_id}/history"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{hist}");
    let items = hist["items"].as_array().unwrap();
    assert!(
        items.iter().any(|h| h["to_status"] == "rescheduled"),
        "{hist}"
    );
    // A rescheduled appointment cannot be rescheduled or cancelled again.
    let (st, again) = call(
        &state,
        "POST",
        &format!("/api/v1/appointments/{first_id}/cancel"),
        REG,
        Some(json!({ "version": prior["version"], "reason_code": "patient_request", "override_reason": "x" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{again}");
    assert_eq!(code(&again), "invalid_transition");
}

#[tokio::test]
async fn visit_closures_move_the_appointment_in_the_same_transaction() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;

    // No-show recorded on the visit board → appointment no_show.
    let p1 = register_patient(&state, &facility).await;
    let (a1, _) = book_first_offer(&state, &facility, &p1, &service).await;
    let v1 = s(&a1["visit_id"]);
    let (st, visit) = call(&state, "GET", &format!("/api/v1/visits/{v1}"), REG, None).await;
    assert_eq!(st, StatusCode::OK);
    let (st, ns) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{v1}/no-show"),
        REG,
        Some(json!({ "version": visit["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ns}");
    let (st, appt) = call(
        &state,
        "GET",
        &format!("/api/v1/appointments/{}", s(&a1["id"])),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(appt["status"], "no_show", "{appt}");
    assert!(appt["no_show_at"].is_string());

    // Arrival keeps the appointment confirmed; the visit becomes operational.
    let p2 = register_patient(&state, &facility).await;
    let (a2, _) = book_first_offer(&state, &facility, &p2, &service).await;
    let v2 = s(&a2["visit_id"]);
    let (_, visit) = call(&state, "GET", &format!("/api/v1/visits/{v2}"), REG, None).await;
    let (st, arrived) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{v2}/arrive"),
        REG,
        Some(json!({ "version": visit["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{arrived}");
    assert_eq!(arrived["status"], "arrived");
    let (st, board) = call(&state, "GET", &format!("/api/v1/visits/{v2}"), REG, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(s(&board["appointment_id"]), s(&a2["id"]), "{board}");
    let (_, appt) = call(
        &state,
        "GET",
        &format!("/api/v1/appointments/{}", s(&a2["id"])),
        REG,
        None,
    )
    .await;
    assert_eq!(appt["status"], "confirmed", "{appt}");

    // Appointment no-show recorded from the scheduling side closes the visit.
    // Before the slot has started it is an override and needs a reason.
    let p3 = register_patient(&state, &facility).await;
    let (a3, _) = book_first_offer(&state, &facility, &p3, &service).await;
    let (st, early) = call(
        &state,
        "POST",
        &format!("/api/v1/appointments/{}/no-show", s(&a3["id"])),
        REG,
        Some(json!({ "version": a3["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{early}");
    assert_eq!(code(&early), "override_required");
    assert_eq!(visit_status(&state, &s(&a3["visit_id"])).await, "scheduled");
    let (st, ns) = call(
        &state,
        "POST",
        &format!("/api/v1/appointments/{}/no-show", s(&a3["id"])),
        REG,
        Some(json!({
            "version": a3["version"],
            "override_reason": "patient phoned to say they will not attend",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ns}");
    assert_eq!(ns["status"], "no_show");
    assert_eq!(visit_status(&state, &s(&a3["visit_id"])).await, "no_show");
}

#[tokio::test]
async fn required_room_is_never_double_booked_across_professionals() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/scheduling/services/{service}/requirements"),
        ADMIN,
        Some(json!({ "requirements": [
            { "resource_type_code": "professional", "quantity": 1 },
            { "resource_type_code": "room", "quantity": 1 },
        ]})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let pro_a = create_professional(&state, &facility, &service).await;
    let pro_b = create_professional(&state, &facility, &service).await;
    // One generic room (no service list), open for exactly one 30-minute
    // slot per day.
    let (st, room) = call(
        &state,
        "POST",
        "/api/v1/scheduling/resources",
        ADMIN,
        Some(json!({
            "facility_id": facility,
            "resource_type_code": "room",
            "name": uniq("Room"),
            "capacity": 1,
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{room}");
    let room_id = s(&room["id"]);
    let rules: Vec<Value> = (1..=7)
        .map(|d| json!({ "weekday": d, "start_local": "09:00:00", "end_local": "09:30:00" }))
        .collect();
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/scheduling/resources/{room_id}/availability"),
        ADMIN,
        Some(json!({ "version": room["version"], "rules": rules })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");

    let p1 = register_patient(&state, &facility).await;
    let (first, offer) = book_first_offer(&state, &facility, &p1, &service).await;
    let used: Vec<String> = offer["resources"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| s(&r["resource_id"]))
        .collect();
    assert!(used.contains(&room_id), "room is part of the plan: {offer}");
    assert!(
        used.contains(&pro_a) || used.contains(&pro_b),
        "a professional is part of the plan: {offer}"
    );
    let (st, detail) = call(
        &state,
        "GET",
        &format!("/api/v1/appointments/{}", s(&first["id"])),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(detail["resources"].as_array().unwrap().len(), 2, "{detail}");
    let bookings: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM resource_bookings WHERE appointment_id = $1 AND status = 'active'",
    )
    .bind(s(&first["id"]).parse::<Uuid>().unwrap())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        bookings, 2,
        "one booking row per resource in the combination"
    );

    // The second patient has a free professional at that time but no room:
    // the taken slot must not be offered at all, with either professional.
    let p2 = register_patient(&state, &facility).await;
    let req = submit_request(&state, &facility, &p2, &service).await;
    let (st, m) = call(
        &state,
        "POST",
        &format!("/api/v1/access-requests/{}/match", s(&req["id"])),
        REG,
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{m}");
    let offers = m["offers"].as_array().unwrap();
    assert!(!offers.is_empty(), "{m}");
    assert!(
        offers.iter().all(|o| o["starts_at"] != first["starts_at"]),
        "{m}"
    );

    // Defence in depth: an overlapping active booking on the room is refused
    // by the exclusion constraint even when written directly.
    let tenant: Uuid = sqlx::query_scalar("SELECT tenant_id FROM appointments WHERE id = $1")
        .bind(s(&first["id"]).parse::<Uuid>().unwrap())
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let staff: Uuid = sqlx::query_scalar("SELECT booked_by FROM appointments WHERE id = $1")
        .bind(s(&first["id"]).parse::<Uuid>().unwrap())
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let err = sqlx::query(
        "INSERT INTO resource_bookings (id, tenant_id, resource_id, slot_index, starts_at, ends_at, kind, created_by)
         VALUES ($1, $2, $3, 0, $4::timestamptz + interval '10 minutes', $5::timestamptz + interval '10 minutes', 'appointment', $6)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(room_id.parse::<Uuid>().unwrap())
    .bind(s(&first["starts_at"]))
    .bind(s(&first["ends_at"]))
    .bind(staff)
    .execute(&state.pool)
    .await
    .expect_err("overlap must be refused");
    let db = err.as_database_error().expect("database error");
    assert_eq!(db.code().as_deref(), Some("23P01"), "{db}");
}

#[tokio::test]
async fn matching_stays_deterministic_when_dmind_is_disabled_or_degraded() {
    let base = test_state().await;
    let facility = main_facility(&base).await;
    let service = create_service(&base).await;
    create_professional(&base, &facility, &service).await;

    // Disabled provider: options, hold and confirmation all work; the run
    // says so explicitly and no artifact exists.
    let disabled = AppState::new(
        base.pool.clone(),
        Arc::new(dmind_gateway::DisabledGateway::disabled(
            "DMIND_MODEL_PROVIDER=disabled",
        )),
    );
    let patient = register_patient(&disabled, &facility).await;
    let req = submit_request(&disabled, &facility, &patient, &service).await;
    let (st, m) = call(
        &disabled,
        "POST",
        &format!("/api/v1/access-requests/{}/match", s(&req["id"])),
        REG,
        Some(json!({ "version": req["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{m}");
    assert_eq!(m["ranking"]["mode"], "deterministic", "{m}");
    assert_eq!(m["ranking"]["reason"], "disabled", "{m}");
    assert!(m["ranking"]["artifact_id"].is_null());
    let offers = m["offers"].as_array().unwrap();
    assert!(offers.len() >= 2, "{m}");
    assert!(offers.iter().all(|o| o["explanation"].is_null()), "{m}");
    let (st, run) = call(
        &disabled,
        "GET",
        &format!("/api/v1/matcher-runs/{}", s(&m["matcher_run_id"])),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(run["ranking_mode"], "deterministic", "{run}");
    let (st, acc) = call(
        &disabled,
        "POST",
        &format!("/api/v1/offers/{}/accept", s(&offers[0]["id"])),
        REG,
        Some(json!({ "version": offers[0]["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{acc}");

    // Degraded provider (configured but failing): same deterministic
    // outcome, reported as unavailable, nothing fabricated.
    let fake = Arc::new(dmind_gateway::fake::FakeProvider::new());
    fake.set_unavailable(true);
    let degraded = AppState::new(base.pool.clone(), fake.clone());
    let patient = register_patient(&degraded, &facility).await;
    let req = submit_request(&degraded, &facility, &patient, &service).await;
    let (st, m) = call(
        &degraded,
        "POST",
        &format!("/api/v1/access-requests/{}/match", s(&req["id"])),
        REG,
        Some(json!({ "version": req["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{m}");
    assert_eq!(m["ranking"]["mode"], "deterministic", "{m}");
    assert_eq!(m["ranking"]["reason"], "unavailable", "{m}");
    assert!(m["ranking"]["artifact_id"].is_null());
    assert!(!m["offers"].as_array().unwrap().is_empty());

    // Ready provider: one bounded ranking call over the same deterministic
    // candidates; the artifact is synthetic, cites candidate ids and the
    // offer set is unchanged (a permutation, never an invention).
    fake.set_unavailable(false);
    let patient = register_patient(&degraded, &facility).await;
    let req = submit_request(&degraded, &facility, &patient, &service).await;
    let (st, m) = call(
        &degraded,
        "POST",
        &format!("/api/v1/access-requests/{}/match", s(&req["id"])),
        REG,
        Some(json!({ "version": req["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{m}");
    assert_eq!(m["ranking"]["mode"], "dmind", "{m}");
    assert_eq!(m["ranking"]["synthetic"], true, "{m}");
    let artifact: Uuid = s(&m["ranking"]["artifact_id"]).parse().unwrap();
    let (atype, synthetic, run_ref, patient_ref): (String, bool, Option<Uuid>, Option<Uuid>) =
        sqlx::query_as(
            "SELECT artifact_type, synthetic, matcher_run_id, patient_id FROM ai_artifacts WHERE id = $1",
        )
        .bind(artifact)
        .fetch_one(&base.pool)
        .await
        .unwrap();
    assert_eq!(atype, "appointment_ranking");
    assert!(synthetic);
    assert_eq!(run_ref, Some(s(&m["matcher_run_id"]).parse().unwrap()));
    assert_eq!(patient_ref, Some(patient.parse().unwrap()));
    let offers = m["offers"].as_array().unwrap();
    let candidates: i32 =
        sqlx::query_scalar("SELECT jsonb_array_length(candidates) FROM matcher_runs WHERE id = $1")
            .bind(s(&m["matcher_run_id"]).parse::<Uuid>().unwrap())
            .fetch_one(&base.pool)
            .await
            .unwrap();
    assert_eq!(
        offers.len(),
        usize::try_from(candidates).unwrap(),
        "ranking never adds or drops options"
    );
}

/// The legacy-visit migration statements of 0015, exactly as shipped.
fn legacy_visit_migration_sql() -> &'static str {
    let sql = include_str!("../migrations/0015_dmind_access.sql");
    let start = sql
        .find("WITH candidates AS (")
        .expect("legacy migration block");
    let end = sql[start..]
        .find("-- Rollback (manual")
        .expect("end of legacy migration block");
    &sql[start..start + end]
}

#[tokio::test]
async fn legacy_scheduled_visits_migrate_once_into_appointments() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let patient = register_patient(&state, &facility).await;
    let facility_uuid: Uuid = facility.parse().unwrap();
    let patient_uuid: Uuid = patient.parse().unwrap();
    let (tenant, staff): (Uuid, Uuid) = sqlx::query_as(
        "SELECT p.tenant_id, (SELECT id FROM users WHERE tenant_id = p.tenant_id AND NOT is_service ORDER BY created_at LIMIT 1)
         FROM patients p WHERE p.id = $1",
    )
    .bind(patient_uuid)
    .fetch_one(&state.pool)
    .await
    .unwrap();

    // Visits as the pre-0015 access board created them: no appointment link.
    let mut legacy = Vec::new();
    for (status, closed_reason, offset_h) in [
        ("scheduled", None, 48i64),
        ("cancelled", Some("cancelled"), 72),
        ("no_show", Some("no_show"), -24),
        ("completed", None, -48),
    ] {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO visits (id, tenant_id, facility_id, patient_id, status, arrival_kind, service, reason,
                                 scheduled_at, closed_at, closed_reason, completed_at, created_by)
             VALUES ($1, $2, $3, $4, $5, 'scheduled', 'general_medicine', 'legacy follow-up',
                     now() + make_interval(hours => $6), CASE WHEN $7::text IS NOT NULL THEN now() END, $7,
                     CASE WHEN $5 = 'completed' THEN now() END, $8)",
        )
        .bind(id)
        .bind(tenant)
        .bind(facility_uuid)
        .bind(patient_uuid)
        .bind(status)
        .bind(offset_h as i32)
        .bind(closed_reason)
        .bind(staff)
        .execute(&state.pool)
        .await
        .unwrap();
        legacy.push((id, status));
    }

    // Run the shipped migration block twice: once to upgrade, once to prove
    // idempotency.
    for _ in 0..2 {
        let mut tx = state.pool.begin().await.unwrap();
        sqlx::raw_sql(legacy_visit_migration_sql())
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();
    }

    let expected = [
        ("scheduled", "confirmed"),
        ("cancelled", "cancelled"),
        ("no_show", "no_show"),
        ("completed", "fulfilled"),
    ];
    for (visit_id, visit_status) in &legacy {
        let rows: Vec<(Uuid, String, String, Option<Uuid>)> = sqlx::query_as(
            "SELECT a.id, a.status, a.booked_via, v.appointment_id
             FROM appointments a JOIN visits v ON v.id = a.visit_id WHERE a.visit_id = $1",
        )
        .bind(visit_id)
        .fetch_all(&state.pool)
        .await
        .unwrap();
        assert_eq!(rows.len(), 1, "exactly one appointment per legacy visit");
        let (appt_id, status, via, link) = &rows[0];
        let want = expected
            .iter()
            .find(|(v, _)| v == visit_status)
            .map(|(_, a)| *a)
            .unwrap();
        assert_eq!(status, want);
        assert_eq!(via, "migration");
        assert_eq!(link.as_ref(), Some(appt_id), "visit links back");
        let history: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM appointment_history WHERE appointment_id = $1 AND reason_code = 'migrated_from_scheduled_visit'",
        )
        .bind(appt_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(history, 1);
    }

    // The migrated confirmed appointment is a first-class record: it is
    // listed, has history, and cancelling it closes the legacy visit.
    let (visit_id, _) = legacy[0];
    let (appt_id,): (Uuid,) = sqlx::query_as("SELECT id FROM appointments WHERE visit_id = $1")
        .bind(visit_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let (st, appt) = call(
        &state,
        "GET",
        &format!("/api/v1/appointments/{appt_id}"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{appt}");
    assert_eq!(appt["status"], "confirmed");
    assert_eq!(appt["service_code"], "general_medicine");
    let (st, cancelled) = call(
        &state,
        "POST",
        &format!("/api/v1/appointments/{appt_id}/cancel"),
        REG,
        Some(json!({
            "version": appt["version"],
            "reason_code": "patient_request",
            "override_reason": "legacy visit cancelled by phone",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{cancelled}");
    assert_eq!(
        visit_status(&state, &visit_id.to_string()).await,
        "cancelled"
    );
}
