//! End-to-end API integration tests for the closed-loop diagnostic result
//! slice. They run against a live PostgreSQL instance (DATABASE_URL) with
//! migrations applied and synthetic seed data loaded, and exercise the full
//! HTTP surface through the axum router without binding a socket.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use wellos_server::state::AppState;

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wellos:wellos_dev@localhost:5432/wellos".to_string())
}

async fn test_state() -> (AppState, Arc<dmind_gateway::fake::FakeProvider>) {
    let pool = wellos_server::connect_pool(&database_url()).await.unwrap();
    wellos_server::run_migrations(&pool).await.unwrap();
    let seeded: Option<(i64,)> = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_optional(&pool)
        .await
        .unwrap();
    if seeded.map(|(n,)| n).unwrap_or(0) == 0 {
        wellos_server::seeddata::seed(&pool).await.unwrap();
    }
    let gateway = Arc::new(dmind_gateway::fake::FakeProvider::new());
    (AppState::new(pool, gateway.clone()), gateway)
}

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
    extra_headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json");
    for (k, v) in extra_headers {
        builder = builder.header(*k, *v);
    }
    let req = builder
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
    format!("{prefix}-{}", uuid::Uuid::now_v7().simple())
}

struct Loop {
    service_request_id: String,
    observation_id: String,
    version: i64,
}

/// Register a patient, start an encounter and order potassium. Returns the
/// new service request id.
async fn create_order(state: &AppState) -> String {
    let (st, meta) = call(
        state,
        "GET",
        "/api/v1/meta/tenant",
        "dev-reg.rivera",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let facility = meta["facilities"][0]["id"].as_str().unwrap().to_string();

    let (st, patient) = call(
        state,
        "POST",
        "/api/v1/patients",
        "dev-reg.rivera",
        Some(json!({
            "facility_id": facility,
            "family_name": "Integration",
            "given_name": "Test",
            "birth_date": "1975-05-05",
            "sex": "male",
            "identifier": uniq("MRN-IT"),
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{patient}");
    let patient_id = patient["id"].as_str().unwrap().to_string();

    let (st, enc) = call(
        state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{enc}");
    let encounter_id = enc["id"].as_str().unwrap().to_string();

    let (st, sr) = call(
        state,
        "POST",
        "/api/v1/service-requests",
        "dev-dr.garcia",
        Some(json!({
            "encounter_id": encounter_id,
            "code_loinc": "2823-3",
            "display": "Potassium",
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sr}");
    sr["id"].as_str().unwrap().to_string()
}

/// Create an order and ingest a result with the given value.
async fn run_to_received(state: &AppState, value: f64) -> Loop {
    let sr_id = create_order(state).await;
    let (st, res) = call(
        state,
        "POST",
        "/api/v1/lab/results",
        "dev-lab.chen",
        Some(json!({
            "service_request_id": sr_id,
            "code_loinc": "2823-3",
            "value": value,
            "unit": "mmol/L",
            "source_system": "fake-lab",
            "idempotency_key": uniq("it-key"),
            "effective_at": chrono::Utc::now(),
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{res}");
    Loop {
        service_request_id: sr_id,
        observation_id: res["observation_id"].as_str().unwrap().to_string(),
        version: 2,
    }
}

#[tokio::test]
async fn happy_path_critical_result_closes_loop() {
    let (state, _) = test_state().await;
    let lp = run_to_received(&state, 7.1).await;

    let (st, detail) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(detail["service_request"]["loop_state"], "received");
    assert_eq!(detail["alerts"][0]["status"], "open");
    assert_eq!(detail["ai_artifacts"][0]["status"], "awaiting_review");
    assert_eq!(detail["ai_artifacts"][0]["autonomy_level"], "A2");
    assert!(detail["ai_artifacts"][0]["output"]["summary"]
        .as_str()
        .unwrap()
        .contains("requires review"));

    for (step, expected) in [
        ("review", "reviewed"),
        ("notify", "notified"),
        ("close", "closed"),
    ] {
        let (_, current) = call(
            &state,
            "GET",
            &format!("/api/v1/service-requests/{}", lp.service_request_id),
            "dev-dr.garcia",
            None,
            &[],
        )
        .await;
        let version = current["service_request"]["version"].as_i64().unwrap();
        let (st, body) = call(
            &state,
            "POST",
            &format!("/api/v1/service-requests/{}/{step}", lp.service_request_id),
            "dev-dr.garcia",
            Some(json!({ "version": version, "note": "integration test" })),
            &[],
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert_eq!(body["loop_state"], expected);
    }

    let (_, closed) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(closed["alerts"][0]["status"], "resolved");
    assert_eq!(closed["follow_up_tasks"][0]["status"], "completed");
    let _ = lp.observation_id;
}

#[tokio::test]
async fn normal_result_creates_no_alert() {
    let (state, _) = test_state().await;
    let lp = run_to_received(&state, 4.2).await;
    let (_, detail) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert!(detail["alerts"].as_array().unwrap().is_empty());
    assert!(detail["follow_up_tasks"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn duplicate_idempotency_key_creates_no_duplicates() {
    let (state, _) = test_state().await;
    let sr_id = create_order(&state).await;
    let key = uniq("dup-key");
    let payload = json!({
        "service_request_id": sr_id,
        "code_loinc": "2823-3",
        "value": 7.0,
        "unit": "mmol/L",
        "source_system": "fake-lab",
        "idempotency_key": key,
        "effective_at": chrono::Utc::now(),
    });
    let (st, first) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        "dev-lab.chen",
        Some(payload.clone()),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(first["duplicate"], false);
    let (st, second) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        "dev-lab.chen",
        Some(payload),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(second["duplicate"], true);
    assert_eq!(second["observation_id"], first["observation_id"]);
}

#[tokio::test]
async fn ai_unavailable_does_not_block_workflow() {
    let (state, gateway) = test_state().await;
    gateway.set_unavailable(true);
    let lp = run_to_received(&state, 7.3).await;
    gateway.set_unavailable(false);
    let (_, detail) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(detail["service_request"]["loop_state"], "received");
    assert_eq!(detail["alerts"][0]["status"], "open");
    assert_eq!(detail["ai_artifacts"][0]["status"], "unavailable");
}

#[tokio::test]
async fn cross_tenant_access_is_denied_even_with_break_glass() {
    let (state, _) = test_state().await;
    let lp = run_to_received(&state, 4.0).await;
    let (_, detail) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    let patient_id = detail["service_request"]["patient"]["id"].as_str().unwrap();

    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient_id}"),
        "dev-dr.sur",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient_id}"),
        "dev-dr.sur",
        None,
        &[("X-Break-Glass-Reason", "attempted cross-tenant")],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn break_glass_same_tenant_requires_reason_and_is_audited() {
    let (state, _) = test_state().await;
    let lp = run_to_received(&state, 4.0).await;
    let (_, detail) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    let patient_id = detail["service_request"]["patient"]["id"]
        .as_str()
        .unwrap()
        .to_string();

    // dr.lopez has no care relationship with this patient: denied without
    // break-glass, permitted with a reason.
    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient_id}"),
        "dev-dr.lopez",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient_id}"),
        "dev-dr.lopez",
        None,
        &[("X-Break-Glass-Reason", "emergency coverage")],
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, audit) = call(
        &state,
        "GET",
        "/api/v1/audit?limit=50",
        "dev-privacy.wolf",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let events = audit["events"].as_array().unwrap();
    assert!(events.iter().any(|e| {
        e["actor"] == "user:dr.lopez"
            && e["break_glass"] == true
            && e["resource_id"] == Value::String(patient_id.clone())
    }));
}

#[tokio::test]
async fn nurse_cannot_close_loop_but_can_view() {
    let (state, _) = test_state().await;
    let lp = run_to_received(&state, 7.2).await;

    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-nurse.kim",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/service-requests/{}/close", lp.service_request_id),
        "dev-nurse.kim",
        Some(json!({ "version": lp.version })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn unit_mismatch_records_data_quality_issue_without_alert() {
    let (state, _) = test_state().await;
    let sr_id = create_order(&state).await;
    let (st, res) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        "dev-lab.chen",
        Some(json!({
            "service_request_id": sr_id,
            "code_loinc": "2823-3",
            "value": 7.7,
            "unit": "furlongs",
            "source_system": "fake-lab",
            "idempotency_key": uniq("um-key"),
            "effective_at": chrono::Utc::now(),
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(res["unit_mismatch"], true);
    assert_eq!(res["critical"], false);

    let (_, detail) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{sr_id}"),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert!(!detail["data_quality_issues"].as_array().unwrap().is_empty());
    assert!(detail["alerts"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn amended_result_preserves_history_and_reopens_review() {
    let (state, _) = test_state().await;
    let lp = run_to_received(&state, 7.1).await;

    // Review the original result first.
    let (_, current) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    let version = current["service_request"]["version"].as_i64().unwrap();
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/service-requests/{}/review", lp.service_request_id),
        "dev-dr.garcia",
        Some(json!({ "version": version })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    // Amend the observation: history preserved, review reopened.
    let (st, amended) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        "dev-lab.chen",
        Some(json!({
            "service_request_id": lp.service_request_id,
            "code_loinc": "2823-3",
            "value": 5.1,
            "unit": "mmol/L",
            "source_system": "fake-lab",
            "idempotency_key": uniq("amend-key"),
            "effective_at": chrono::Utc::now(),
            "amends_observation_id": lp.observation_id,
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{amended}");
    assert_eq!(amended["loop_state"], "received");

    let (_, detail) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    let observations = detail["observations"].as_array().unwrap();
    assert!(observations.len() >= 2);
    assert!(observations
        .iter()
        .any(|o| o["amends"] == Value::String(lp.observation_id.clone())));
    assert_eq!(detail["service_request"]["loop_state"], "received");
}

#[tokio::test]
async fn research_user_has_no_clinical_access() {
    let (state, _) = test_state().await;
    let (st, _) = call(
        &state,
        "GET",
        "/api/v1/worklist",
        "dev-research.diaz",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn audit_read_restricted_to_privacy_and_security_roles() {
    let (state, _) = test_state().await;
    let (st, _) = call(&state, "GET", "/api/v1/audit", "dev-dr.garcia", None, &[]).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(&state, "GET", "/api/v1/audit", "dev-audit.stone", None, &[]).await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn fhir_endpoints_return_minimal_r4_resources() {
    let (state, _) = test_state().await;
    let lp = run_to_received(&state, 4.4).await;
    let (_, detail) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    let patient_id = detail["service_request"]["patient"]["id"].as_str().unwrap();

    let (st, patient) = call(
        &state,
        "GET",
        &format!("/fhir/r4/Patient/{patient_id}"),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(patient["resourceType"], "Patient");

    let (st, obs) = call(
        &state,
        "GET",
        &format!("/fhir/r4/Observation/{}", lp.observation_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(obs["resourceType"], "Observation");
    assert_eq!(obs["code"]["coding"][0]["system"], "http://loinc.org");

    let (st, sr) = call(
        &state,
        "GET",
        &format!("/fhir/r4/ServiceRequest/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(sr["resourceType"], "ServiceRequest");
}

#[tokio::test]
async fn stale_version_is_rejected() {
    let (state, _) = test_state().await;
    let lp = run_to_received(&state, 7.1).await;
    let (_, current) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    let version = current["service_request"]["version"].as_i64().unwrap();
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/service-requests/{}/review", lp.service_request_id),
        "dev-dr.garcia",
        Some(json!({ "version": version - 1 })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
}
