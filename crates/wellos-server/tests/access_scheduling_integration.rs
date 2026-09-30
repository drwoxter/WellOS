//! Integration tests for dMind Access v1: runtime catalogs, schedulable
//! resources, access requests, the deterministic matcher, offers and holds,
//! appointments and their linked visits, ICS export and the security
//! boundaries around them.

use axum::body::Body;
use axum::http::{Request, StatusCode};
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
