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

#[tokio::test]
async fn mismatched_result_code_is_rejected() {
    let (state, _) = test_state().await;
    let sr_id = create_order(&state).await;
    let (st, body) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        "dev-lab.chen",
        Some(json!({
            "service_request_id": sr_id,
            "code_loinc": "2345-7",
            "value": 100,
            "unit": "mg/dL",
            "source_system": "fake-lab",
            "idempotency_key": uniq("mismatch-key"),
            "effective_at": chrono::Utc::now(),
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "code_mismatch");

    let (_, detail) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{sr_id}"),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(detail["service_request"]["loop_state"], "ordered");
    assert!(detail["observations"].as_array().unwrap().is_empty());
}

#[tokio::test]
async fn notify_and_close_require_documentation_note() {
    let (state, _) = test_state().await;
    let lp = run_to_received(&state, 5.0).await;

    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/service-requests/{}/review", lp.service_request_id),
        "dev-dr.garcia",
        Some(json!({ "version": lp.version })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/service-requests/{}/notify", lp.service_request_id),
        "dev-dr.garcia",
        Some(json!({ "version": lp.version + 1 })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "documentation_required");

    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/service-requests/{}/notify", lp.service_request_id),
        "dev-dr.garcia",
        Some(json!({ "version": lp.version + 1, "note": "patient called with results" })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/service-requests/{}/close", lp.service_request_id),
        "dev-dr.garcia",
        Some(json!({ "version": lp.version + 2 })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "documentation_required");

    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/service-requests/{}/close", lp.service_request_id),
        "dev-dr.garcia",
        Some(json!({ "version": lp.version + 2, "note": "no further follow-up needed" })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (_, detail) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    let notes = detail["notes"].as_array().unwrap();
    assert!(notes
        .iter()
        .any(|n| n["kind"] == "notification" && n["note"] == "patient called with results"));
    assert!(notes
        .iter()
        .any(|n| n["kind"] == "closure" && n["note"] == "no further follow-up needed"));
}

#[tokio::test]
async fn consent_changes_preserve_immutable_history() {
    let (state, _) = test_state().await;
    let (_, meta) = call(
        &state,
        "GET",
        "/api/v1/meta/tenant",
        "dev-reg.rivera",
        None,
        &[],
    )
    .await;
    let facility = meta["facilities"][0]["id"].as_str().unwrap().to_string();
    let (st, patient) = call(
        &state,
        "POST",
        "/api/v1/patients",
        "dev-reg.rivera",
        Some(json!({
            "facility_id": facility,
            "family_name": "Consent",
            "given_name": "History",
            "birth_date": "1980-01-01",
            "sex": "female",
            "identifier": uniq("MRN-CH"),
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let patient_id = patient["id"].as_str().unwrap().to_string();

    for status in ["active", "revoked"] {
        let (st, _) = call(
            &state,
            "POST",
            "/api/v1/consents",
            "dev-privacy.wolf",
            Some(json!({
                "patient_id": patient_id,
                "purpose": "ai_external_processing",
                "status": status,
            })),
            &[],
        )
        .await;
        assert_eq!(st, StatusCode::OK);
    }

    let rows: Vec<(String, i32)> = sqlx::query_as(
        "SELECT status, version FROM consents
         WHERE patient_id = $1::uuid AND purpose = 'ai_external_processing'
         ORDER BY version",
    )
    .bind(uuid::Uuid::parse_str(&patient_id).unwrap())
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[0], ("active".to_string(), 1));
    assert_eq!(rows[1], ("revoked".to_string(), 2));

    let (_, chart) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient_id}"),
        "dev-reg.rivera",
        None,
        &[],
    )
    .await;
    let consents = chart["consents"].as_array().unwrap();
    let current = consents
        .iter()
        .find(|c| c["purpose"] == "ai_external_processing")
        .unwrap();
    assert_eq!(current["status"], "revoked");
}

#[tokio::test]
async fn amendment_supersedes_unreviewed_ai_artifacts() {
    let (state, _) = test_state().await;
    let lp = run_to_received(&state, 7.1).await;

    let (st, _) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        "dev-lab.chen",
        Some(json!({
            "service_request_id": lp.service_request_id,
            "code_loinc": "2823-3",
            "value": 5.0,
            "unit": "mmol/L",
            "source_system": "fake-lab",
            "idempotency_key": uniq("supersede-key"),
            "effective_at": chrono::Utc::now(),
            "amends_observation_id": lp.observation_id,
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (_, detail) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    let artifacts = detail["ai_artifacts"].as_array().unwrap();
    let stale = artifacts
        .iter()
        .find(|a| a["observation_id"] == Value::String(lp.observation_id.clone()))
        .unwrap();
    assert_eq!(stale["status"], "superseded");

    let (st, _) = call(
        &state,
        "POST",
        &format!(
            "/api/v1/ai-artifacts/{}/review",
            stale["id"].as_str().unwrap()
        ),
        "dev-dr.garcia",
        Some(json!({ "decision": "approved" })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
}

#[tokio::test]
async fn service_principals_cannot_use_dev_tokens() {
    let (state, _) = test_state().await;
    let (st, _) = call(
        &state,
        "GET",
        "/api/v1/worklist",
        "dev-svc.lab-adapter",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn idempotency_key_reuse_with_different_payload_conflicts() {
    let (state, _) = test_state().await;
    let sr_id = create_order(&state).await;
    let key = uniq("reuse-key");
    let (st, _) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        "dev-lab.chen",
        Some(json!({
            "service_request_id": sr_id,
            "code_loinc": "2823-3",
            "value": 4.4,
            "unit": "mmol/L",
            "source_system": "fake-lab",
            "idempotency_key": key,
            "effective_at": chrono::Utc::now(),
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let other_sr = create_order(&state).await;
    let (st, body) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        "dev-lab.chen",
        Some(json!({
            "service_request_id": other_sr,
            "code_loinc": "2823-3",
            "value": 5.5,
            "unit": "mmol/L",
            "source_system": "fake-lab",
            "idempotency_key": key,
            "effective_at": chrono::Utc::now(),
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT);
    assert_eq!(body["error"]["code"], "idempotency_key_reuse");
}

#[tokio::test]
async fn break_glass_cannot_close_loops_without_relationship() {
    let (state, _) = test_state().await;
    let lp = run_to_received(&state, 7.2).await;
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/service-requests/{}/review", lp.service_request_id),
        "dev-dr.lopez",
        Some(json!({ "version": lp.version, "note": "attempted via break-glass" })),
        &[("x-break-glass-reason", "emergency coverage")],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn unknown_purpose_of_use_is_rejected() {
    let (state, _) = test_state().await;
    let (st, body) = call(
        &state,
        "GET",
        "/api/v1/worklist",
        "dev-dr.garcia",
        None,
        &[("x-purpose-of-use", "marketing")],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"]["code"], "invalid_purpose_of_use");
}
