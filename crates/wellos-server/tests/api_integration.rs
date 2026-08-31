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

/// Create a fresh emergency clinician (physician + break_glass_authorized)
/// so per-user break-glass rate limits never leak between tests or runs.
async fn create_emergency_user(state: &AppState) -> String {
    let username = uniq("dr.em");
    let (tenant_id, facility_id): (uuid::Uuid, uuid::Uuid) = sqlx::query_as(
        "SELECT u.tenant_id, f.id FROM users u\n         JOIN facilities f ON f.tenant_id = u.tenant_id\n         WHERE u.username = 'dr.garcia' ORDER BY f.name LIMIT 1",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let uid = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, tenant_id, username, display_name) VALUES ($1,$2,$3,'Emergency Test Clinician')",
    )
    .bind(uid)
    .bind(tenant_id)
    .bind(&username)
    .execute(&state.pool)
    .await
    .unwrap();
    for role in ["physician", "break_glass_authorized"] {
        sqlx::query(
            "INSERT INTO role_assignments (id, tenant_id, user_id, role, facility_id) VALUES ($1,$2,$3,$4,$5)",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(tenant_id)
        .bind(uid)
        .bind(role)
        .bind(facility_id)
        .execute(&state.pool)
        .await
        .unwrap();
    }
    username
}

/// Issue a service credential for the seeded lab-adapter machine principal.
async fn issue_service_credential(
    state: &AppState,
    scopes: &[&str],
    expires_interval: Option<&str>,
) -> String {
    let token = wellos_server::seeddata::generate_service_secret();
    let (uid, tid): (uuid::Uuid, uuid::Uuid) =
        sqlx::query_as("SELECT id, tenant_id FROM users WHERE username = 'svc.lab-adapter'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO service_credentials (id, tenant_id, user_id, name, token_hash, scopes, expires_at)\n         VALUES ($1,$2,$3,'integration test',$4,$5,\n                 CASE WHEN $6::text IS NULL THEN NULL ELSE now() + $6::interval END)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(tid)
    .bind(uid)
    .bind(wellos_server::auth::hash_service_secret(&token))
    .bind(scopes.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    .bind(expires_interval)
    .execute(&state.pool)
    .await
    .unwrap();
    token
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
    assert_eq!(st, StatusCode::NOT_FOUND);

    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient_id}"),
        "dev-dr.sur",
        None,
        &[
            ("X-Break-Glass-Reason", "attempted cross-tenant access"),
            ("X-Purpose-Of-Use", "emergency"),
        ],
    )
    .await;
    // Cross-tenant probes are indistinguishable from nonexistent resources.
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Nonexistent resources return the same shape, so IDs are not probeable.
    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{}", uuid::Uuid::now_v7()),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
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

    // dr.lopez has no care relationship and is not break-glass authorized:
    // denied with or without an asserted emergency.
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
        &[
            ("X-Break-Glass-Reason", "emergency department coverage"),
            ("X-Purpose-Of-Use", "emergency"),
        ],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // An authorized emergency clinician must also assert the emergency
    // purpose and give a substantive reason.
    let emergency = create_emergency_user(&state).await;
    let em_token = format!("dev-{emergency}");
    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient_id}"),
        &em_token,
        None,
        &[("X-Break-Glass-Reason", "emergency department coverage")],
    )
    .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "treatment purpose must not grant break-glass"
    );

    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient_id}"),
        &em_token,
        None,
        &[
            ("X-Break-Glass-Reason", "er"),
            ("X-Purpose-Of-Use", "emergency"),
        ],
    )
    .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "too-short reason must be rejected"
    );

    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient_id}"),
        &em_token,
        None,
        &[
            ("X-Break-Glass-Reason", "emergency department coverage"),
            ("X-Purpose-Of-Use", "emergency"),
        ],
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, audit) = call(
        &state,
        "GET",
        "/api/v1/audit?limit=50",
        "dev-privacy.wolf",
        None,
        &[("X-Purpose-Of-Use", "operations")],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(!audit["events"].as_array().unwrap().is_empty());
    // Query the audit table directly: concurrent tests can push these events
    // past the endpoint's fixed page size.
    let (allowed,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM audit_events
         WHERE actor = $1 AND break_glass AND resource_id = $2",
    )
    .bind(format!("user:{emergency}"))
    .bind(&patient_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(allowed > 0);
    // Denied attempts are audited too.
    let (denied,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM audit_events
         WHERE actor = 'user:dr.lopez' AND decision = 'deny'
           AND reason = 'emergency_requires_break_glass_role'",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(denied > 0);
}

#[tokio::test]
async fn break_glass_is_rate_limited_and_reviewable() {
    let (state, _) = test_state().await;
    let mut limited = state.clone();
    limited.auth = Arc::new(wellos_server::state::AuthConfig {
        break_glass_hourly_limit: 1,
        ..wellos_server::state::AuthConfig::development()
    });
    let lp = run_to_received(&limited, 4.0).await;
    let (_, detail) = call(
        &limited,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    let patient_id = detail["service_request"]["patient"]["id"].as_str().unwrap();
    let emergency = create_emergency_user(&limited).await;
    let em_token = format!("dev-{emergency}");
    let headers = [
        ("X-Break-Glass-Reason", "emergency department coverage"),
        ("X-Purpose-Of-Use", "emergency"),
    ];
    let (st, _) = call(
        &limited,
        "GET",
        &format!("/api/v1/patients/{patient_id}"),
        &em_token,
        None,
        &headers,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = call(
        &limited,
        "GET",
        &format!("/api/v1/patients/{patient_id}"),
        &em_token,
        None,
        &headers,
    )
    .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "per-user hourly limit must apply"
    );

    // Privacy officer reviews the pending event exactly once.
    let (st, list) = call(
        &limited,
        "GET",
        "/api/v1/break-glass",
        "dev-privacy.wolf",
        None,
        &[("X-Purpose-Of-Use", "operations")],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{list}");
    let event = list["events"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["actor"] == Value::String(emergency.clone()))
        .expect("break-glass event listed")
        .clone();
    assert_eq!(event["review_status"], "pending");
    let event_id = event["id"].as_str().unwrap();

    // Physicians cannot review break-glass events.
    let (st, _) = call(
        &limited,
        "POST",
        &format!("/api/v1/break-glass/{event_id}/review"),
        "dev-dr.garcia",
        Some(json!({ "note": "self review attempt" })),
        &[("X-Purpose-Of-Use", "operations")],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    let (st, reviewed) = call(
        &limited,
        "POST",
        &format!("/api/v1/break-glass/{event_id}/review"),
        "dev-privacy.wolf",
        Some(json!({ "note": "validated with ED charge nurse" })),
        &[("X-Purpose-Of-Use", "operations")],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{reviewed}");
    assert_eq!(reviewed["review_status"], "reviewed");

    // Review is once-only: the event itself stays immutable.
    let (st, _) = call(
        &limited,
        "POST",
        &format!("/api/v1/break-glass/{event_id}/review"),
        "dev-privacy.wolf",
        Some(json!({ "note": "second review" })),
        &[("X-Purpose-Of-Use", "operations")],
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
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
async fn amendment_retires_stale_critical_alerts_and_tasks() {
    let (state, _) = test_state().await;
    let lp = run_to_received(&state, 7.1).await;

    let detail = |state: &AppState| {
        let state = state.clone();
        let sr = lp.service_request_id.clone();
        async move {
            let (_, d) = call(
                &state,
                "GET",
                &format!("/api/v1/service-requests/{sr}"),
                "dev-dr.garcia",
                None,
                &[],
            )
            .await;
            d
        }
    };
    let open_alerts = |d: &Value| {
        d["alerts"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|a| a["status"] == "open")
            .count()
    };
    let open_tasks = |d: &Value| {
        d["follow_up_tasks"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|t| t["status"] == "open" || t["status"] == "overdue")
            .count()
    };

    let d = detail(&state).await;
    assert_eq!(open_alerts(&d), 1, "critical result raises an alert: {d}");
    assert_eq!(open_tasks(&d), 1, "critical result raises a task: {d}");

    // Critical-to-normal correction: stale alert/task retired, none created.
    let (st, corrected) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        "dev-lab.chen",
        Some(json!({
            "service_request_id": lp.service_request_id,
            "code_loinc": "2823-3",
            "value": 4.1,
            "unit": "mmol/L",
            "source_system": "fake-lab",
            "idempotency_key": uniq("amend-normal"),
            "effective_at": chrono::Utc::now(),
            "amends_observation_id": lp.observation_id,
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{corrected}");
    assert_eq!(corrected["critical"], false);
    let d = detail(&state).await;
    assert_eq!(open_alerts(&d), 0, "normal correction retires alerts: {d}");
    assert_eq!(open_tasks(&d), 0, "normal correction retires tasks: {d}");

    // Critical-to-critical correction: fresh alert/task replace the stale ones.
    let normal_obs_id = corrected["observation_id"].as_str().unwrap().to_string();
    let (st, recrit) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        "dev-lab.chen",
        Some(json!({
            "service_request_id": lp.service_request_id,
            "code_loinc": "2823-3",
            "value": 7.9,
            "unit": "mmol/L",
            "source_system": "fake-lab",
            "idempotency_key": uniq("amend-critical"),
            "effective_at": chrono::Utc::now(),
            "amends_observation_id": normal_obs_id,
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{recrit}");
    assert_eq!(recrit["critical"], true);
    let d = detail(&state).await;
    assert_eq!(
        open_alerts(&d),
        1,
        "critical correction raises one alert: {d}"
    );
    assert_eq!(
        open_tasks(&d),
        1,
        "critical correction raises one task: {d}"
    );
}

#[tokio::test]
async fn amendment_keeps_observation_rows_append_only() {
    let (state, _) = test_state().await;
    let lp = run_to_received(&state, 7.1).await;

    let (st, corrected) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        "dev-lab.chen",
        Some(json!({
            "service_request_id": lp.service_request_id,
            "code_loinc": "2823-3",
            "value": 4.1,
            "unit": "mmol/L",
            "source_system": "fake-lab",
            "idempotency_key": uniq("append-only"),
            "effective_at": chrono::Utc::now(),
            "amends_observation_id": lp.observation_id,
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{corrected}");

    // The historical row is never mutated: original status and value remain.
    let (status, value): (String, String) =
        sqlx::query_as("SELECT status, value_num::text FROM observations WHERE id = $1::uuid")
            .bind(&lp.observation_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(status, "final");
    assert_eq!(value, "7.1");

    // Supersession is derived from the amends relationship in presentation.
    let (_, detail) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    let obs = detail["observations"].as_array().unwrap();
    let old = obs
        .iter()
        .find(|o| o["id"] == json!(lp.observation_id))
        .unwrap();
    assert_eq!(old["status"], "amended-superseded", "{detail}");
    let new_obs = obs
        .iter()
        .find(|o| o["amends"] == json!(lp.observation_id))
        .unwrap();
    assert_eq!(new_obs["status"], "corrected", "{detail}");

    // The patient chart also derives supersession from the amends link.
    let patient_id = detail["service_request"]["patient"]["id"].as_str().unwrap();
    let (st, chart) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient_id}"),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{chart}");
    let chart_obs = chart["observations"].as_array().unwrap();
    let old = chart_obs
        .iter()
        .find(|o| o["id"] == json!(lp.observation_id))
        .unwrap();
    assert_eq!(old["status"], "amended-superseded", "{chart}");
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
async fn tenant_meta_requires_authorization() {
    let (state, _) = test_state().await;
    let (st, _) = call(
        &state,
        "GET",
        "/api/v1/meta/tenant",
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // Roles without tenant metadata access (e.g. research) are denied.
    let (st, _) = call(
        &state,
        "GET",
        "/api/v1/meta/tenant",
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
    let ops = [("X-Purpose-Of-Use", "operations")];
    let (st, _) = call(&state, "GET", "/api/v1/audit", "dev-dr.garcia", None, &ops).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(
        &state,
        "GET",
        "/api/v1/audit",
        "dev-audit.stone",
        None,
        &ops,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

#[tokio::test]
async fn purpose_of_use_is_enforced_per_action() {
    let (state, _) = test_state().await;
    // Audit reads are operations/quality context, never treatment.
    let (st, _) = call(&state, "GET", "/api/v1/audit", "dev-audit.stone", None, &[]).await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "treatment purpose must not read audit"
    );
    let (st, _) = call(
        &state,
        "GET",
        "/api/v1/audit",
        "dev-audit.stone",
        None,
        &[("X-Purpose-Of-Use", "quality")],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    // Clinical writes require treatment context: an asserted emergency or
    // operations purpose never widens access to them.
    let sr_id = create_order(&state).await;
    for purpose in ["emergency", "operations", "quality"] {
        let (st, _) = call(
            &state,
            "POST",
            &format!("/api/v1/service-requests/{sr_id}/review"),
            "dev-dr.garcia",
            Some(json!({ "version": 1, "note": "purpose test" })),
            &[("X-Purpose-Of-Use", purpose)],
        )
        .await;
        assert_eq!(
            st,
            StatusCode::FORBIDDEN,
            "purpose {purpose} must not review"
        );
    }
    // Worklist reads are valid in treatment, operations and quality context.
    for purpose in ["treatment", "quality"] {
        let (st, _) = call(
            &state,
            "GET",
            "/api/v1/worklist",
            "dev-dr.garcia",
            None,
            &[("X-Purpose-Of-Use", purpose)],
        )
        .await;
        assert_eq!(st, StatusCode::OK, "purpose {purpose} must read worklist");
    }
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
async fn nurse_without_care_relationship_cannot_notify() {
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
        "dev-nurse.kim",
        Some(json!({ "version": lp.version + 1, "note": "call attempted" })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
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
async fn break_glass_cannot_mutate_clinical_state() {
    let (state, _) = test_state().await;
    let lp = run_to_received(&state, 7.2).await;
    let emergency = create_emergency_user(&state).await;
    let em_token = format!("dev-{emergency}");
    // Even a fully authorized emergency user cannot use break-glass for
    // consequential transitions: review, notify, close.
    for path in ["review", "notify", "close"] {
        let (st, _) = call(
            &state,
            "POST",
            &format!("/api/v1/service-requests/{}/{path}", lp.service_request_id),
            &em_token,
            Some(json!({ "version": lp.version, "note": "attempted via break-glass", "method": "phone", "disposition": "repeat_test" })),
            &[
                ("x-break-glass-reason", "emergency department coverage"),
                ("x-purpose-of-use", "emergency"),
            ],
        )
        .await;
        assert_eq!(
            st,
            StatusCode::FORBIDDEN,
            "break-glass must not allow {path}"
        );
    }
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

fn ingest_payload(sr_id: &str) -> Value {
    json!({
        "service_request_id": sr_id,
        "code_loinc": "2823-3",
        "value": 4.1,
        "unit": "mmol/L",
        "source_system": "fake-lab",
        "idempotency_key": uniq("svc-cred"),
        "effective_at": chrono::Utc::now(),
    })
}

#[tokio::test]
async fn service_credentials_authenticate_machine_identities() {
    let (state, _) = test_state().await;
    let sr_id = create_order(&state).await;
    let token = issue_service_credential(&state, &["result.ingest"], Some("1 hour")).await;
    let (st, body) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        &token,
        Some(ingest_payload(&sr_id)),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");

    // Usage metadata is recorded.
    let (last_used,): (Option<chrono::DateTime<chrono::Utc>>,) =
        sqlx::query_as("SELECT last_used_at FROM service_credentials WHERE token_hash = $1")
            .bind(wellos_server::auth::hash_service_secret(&token))
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(last_used.is_some());

    // The legacy predictable svc-<username> mechanism no longer authenticates.
    let (st, _) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        "svc-svc.lab-adapter",
        Some(ingest_payload(&sr_id)),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn service_credentials_enforce_scope_expiry_revocation_and_shape() {
    let (state, _) = test_state().await;
    let sr_id = create_order(&state).await;

    // Wrong scope: authenticated but denied by policy.
    let wrong_scope = issue_service_credential(&state, &["worklist.read"], None).await;
    let (st, _) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        &wrong_scope,
        Some(ingest_payload(&sr_id)),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Expired.
    let expired = issue_service_credential(&state, &["result.ingest"], Some("-1 hour")).await;
    let (st, _) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        &expired,
        Some(ingest_payload(&sr_id)),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // Revoked.
    let revoked = issue_service_credential(&state, &["result.ingest"], None).await;
    sqlx::query("UPDATE service_credentials SET revoked_at = now() WHERE token_hash = $1")
        .bind(wellos_server::auth::hash_service_secret(&revoked))
        .execute(&state.pool)
        .await
        .unwrap();
    let (st, _) = call(
        &state,
        "POST",
        "/api/v1/lab/results",
        &revoked,
        Some(ingest_payload(&sr_id)),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);

    // Malformed / unknown secrets.
    for bad in ["wsk_", "wsk_notahexsecret", "wsk_0000000000000000"] {
        let (st, _) = call(&state, "GET", "/api/v1/worklist", bad, None, &[]).await;
        assert_eq!(st, StatusCode::UNAUTHORIZED, "token {bad}");
    }

    // Service credentials never authenticate human users and human dev
    // tokens never authenticate machine principals.
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
async fn patient_search_query_length_is_bounded() {
    let (state, _) = test_state().await;
    let (st, body) = call(
        &state,
        "GET",
        "/api/v1/patients?query=a",
        "dev-reg.rivera",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "validation_failed");
}

#[tokio::test]
async fn worklist_filters_apply_before_row_cap() {
    let (state, _) = test_state().await;
    // Copy tenant/facility context from a service request already visible to
    // dr.garcia so every inserted row falls inside the caller's scope.
    let (st, base) = call(
        &state,
        "GET",
        "/api/v1/worklist",
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{base}");
    let seed_sr: uuid::Uuid = base["items"][0]["id"].as_str().unwrap().parse().unwrap();
    let (tenant_id, requester_id, template_patient): (uuid::Uuid, uuid::Uuid, uuid::Uuid) =
        sqlx::query_as(
            "SELECT tenant_id, requester_id, patient_id FROM service_requests WHERE id = $1",
        )
        .bind(seed_sr)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let (facility_id,): (uuid::Uuid,) =
        sqlx::query_as("SELECT facility_id FROM patients WHERE id = $1")
            .bind(template_patient)
            .fetch_one(&state.pool)
            .await
            .unwrap();

    // A dedicated patient with one old open result that a bounded worklist
    // would otherwise hide behind newer rows.
    let family = uniq("Backlog");
    let patient_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO patients (id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier)
         VALUES ($1,$2,$3,$4,'Cap','1970-01-01','female',$5)",
    )
    .bind(patient_id)
    .bind(tenant_id)
    .bind(facility_id)
    .bind(&family)
    .bind(uniq("SYN-CAP"))
    .execute(&state.pool)
    .await
    .unwrap();
    let encounter_id = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO encounters (id, tenant_id, facility_id, patient_id, practitioner_id, status, started_at)
         VALUES ($1,$2,$3,$4,$5,'in_progress', now() - interval '30 days')",
    )
    .bind(encounter_id)
    .bind(tenant_id)
    .bind(facility_id)
    .bind(patient_id)
    .bind(requester_id)
    .execute(&state.pool)
    .await
    .unwrap();
    let old_sr = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO service_requests (id, tenant_id, encounter_id, patient_id, requester_id,
                                       code_loinc, display, loop_state, version, created_at)
         VALUES ($1,$2,$3,$4,$5,'2345-7','Glucose [Mass/volume] in Serum','received',2,
                 now() - interval '30 days')",
    )
    .bind(old_sr)
    .bind(tenant_id)
    .bind(encounter_id)
    .bind(patient_id)
    .bind(requester_id)
    .execute(&state.pool)
    .await
    .unwrap();
    // More routine, newer rows than the API's row cap.
    for _ in 0..210 {
        sqlx::query(
            "INSERT INTO service_requests (id, tenant_id, encounter_id, patient_id, requester_id,
                                           code_loinc, display, loop_state, version, created_at)
             VALUES ($1,$2,$3,$4,$5,'2823-3','Potassium [Moles/volume] in Serum','ordered',1, now())",
        )
        .bind(uuid::Uuid::now_v7())
        .bind(tenant_id)
        .bind(encounter_id)
        .bind(template_patient)
        .bind(requester_id)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    // Unfiltered, the old routine row is buried behind the cap.
    let (st, unfiltered) = call(
        &state,
        "GET",
        "/api/v1/worklist",
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(unfiltered["items"].as_array().unwrap().len(), 200);
    assert_eq!(unfiltered["has_more"], json!(true));
    assert!(
        !unfiltered["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"] == json!(old_sr)),
        "old row should be beyond the first page"
    );
    // Cursor paging reaches the old row even without filters, with no row
    // skipped or repeated across pages.
    let mut cursor = unfiltered["next_cursor"].as_str().unwrap().to_string();
    let mut seen: std::collections::HashSet<String> = unfiltered["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect();
    let mut found_via_paging = false;
    loop {
        let (st, page) = call(
            &state,
            "GET",
            &format!("/api/v1/worklist?cursor={cursor}"),
            "dev-dr.garcia",
            None,
            &[],
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{page}");
        for item in page["items"].as_array().unwrap() {
            let id = item["id"].as_str().unwrap().to_string();
            assert!(seen.insert(id), "cursor paging repeated a row: {item}");
            if item["id"] == json!(old_sr) {
                found_via_paging = true;
            }
        }
        // A row closing between page fetches must not shift later pages:
        // close one first-page row after fetching each page.
        if let Some(open_id) = seen.iter().next().cloned() {
            sqlx::query(
                "UPDATE service_requests SET loop_state = 'closed'
                 WHERE id = $1 AND loop_state <> 'closed'
                   AND id <> $2",
            )
            .bind(open_id.parse::<uuid::Uuid>().unwrap())
            .bind(old_sr)
            .execute(&state.pool)
            .await
            .unwrap();
        }
        if found_via_paging || page["has_more"] != json!(true) {
            break;
        }
        cursor = page["next_cursor"].as_str().unwrap().to_string();
    }
    assert!(
        found_via_paging,
        "cursor paging must reach every open result even when rows close between fetches"
    );
    let (st, _) = call(
        &state,
        "GET",
        "/api/v1/worklist?cursor=not-a-cursor",
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // A patient query filter is applied in SQL, so the old row is reachable.
    let (st, by_query) = call(
        &state,
        "GET",
        &format!("/api/v1/worklist?query={family}"),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{by_query}");
    assert!(
        by_query["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"] == json!(old_sr)),
        "{by_query}"
    );

    // The displayed order ("Given Family", as shown in the UI) matches too.
    let (st, by_display_order) = call(
        &state,
        "GET",
        &format!("/api/v1/worklist?query=Cap%20{family}"),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{by_display_order}");
    assert!(
        by_display_order["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|i| i["id"] == json!(old_sr)),
        "{by_display_order}"
    );

    // A workflow-state filter also reaches it.
    let (st, by_state) = call(
        &state,
        "GET",
        "/api/v1/worklist?state=received",
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    for item in by_state["items"].as_array().unwrap() {
        assert_eq!(item["loop_state"], "received");
    }

    // Criticality filter returns only rows with open alerts.
    let (st, by_critical) = call(
        &state,
        "GET",
        "/api/v1/worklist?critical=true",
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    for item in by_critical["items"].as_array().unwrap() {
        assert_eq!(item["has_open_alert"], json!(true));
    }

    // Unknown states are rejected, and `closed` is not a worklist state.
    for bad in ["closed", "bogus"] {
        let (st, _) = call(
            &state,
            "GET",
            &format!("/api/v1/worklist?state={bad}"),
            "dev-dr.garcia",
            None,
            &[],
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "state {bad}");
    }
}

#[tokio::test]
async fn tenant_meta_reports_facility_specific_capabilities() {
    let (state, _) = test_state().await;
    // Registration staff: can_register only in explicitly assigned facilities,
    // never clinical capability.
    let (st, meta) = call(
        &state,
        "GET",
        "/api/v1/meta/tenant",
        "dev-reg.rivera",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{meta}");
    let facilities = meta["facilities"].as_array().unwrap();
    let (rid, tid): (uuid::Uuid, uuid::Uuid) =
        sqlx::query_as("SELECT id, tenant_id FROM users WHERE username = 'reg.rivera'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let assigned: Vec<(Option<uuid::Uuid>,)> = sqlx::query_as(
        "SELECT facility_id FROM role_assignments
         WHERE tenant_id = $1 AND user_id = $2 AND role = 'registration_staff'",
    )
    .bind(tid)
    .bind(rid)
    .fetch_all(&state.pool)
    .await
    .unwrap();
    let assigned: Vec<uuid::Uuid> = assigned.into_iter().filter_map(|(f,)| f).collect();
    for f in facilities {
        let id: uuid::Uuid = f["id"].as_str().unwrap().parse().unwrap();
        assert_eq!(
            f["can_register"],
            json!(assigned.contains(&id)),
            "registration capability must be facility-specific: {f}"
        );
        assert_eq!(f["can_act_clinically"], json!(false), "{f}");
    }

    // Physician: clinical capability only in assigned facilities, no
    // registration capability anywhere.
    let (st, meta) = call(
        &state,
        "GET",
        "/api/v1/meta/tenant",
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let facilities = meta["facilities"].as_array().unwrap();
    assert!(facilities
        .iter()
        .any(|f| f["can_act_clinically"] == json!(true)));
    assert!(facilities.iter().all(|f| f["can_register"] == json!(false)));
    // A physician assigned to a single facility (dr.annex, North Annex only)
    // must not report clinical capability in the others.
    let (st, meta) = call(
        &state,
        "GET",
        "/api/v1/meta/tenant",
        "dev-dr.annex",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{meta}");
    let facilities = meta["facilities"].as_array().unwrap();
    assert!(facilities.len() > 1, "{meta}");
    assert!(
        facilities
            .iter()
            .any(|f| f["can_act_clinically"] == json!(true)),
        "{meta}"
    );
    assert!(
        facilities
            .iter()
            .any(|f| f["can_act_clinically"] == json!(false)),
        "clinical capability must not leak to unassigned facilities: {meta}"
    );

    // An ordinary clinical role with a NULL facility assignment gains no
    // facility capability (NULL is tenant-wide only for allowlisted roles).
    let username = uniq("dr.null");
    let uid = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, tenant_id, username, display_name) VALUES ($1,$2,$3,'Null Facility Physician')",
    )
    .bind(uid)
    .bind(tid)
    .bind(&username)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO role_assignments (id, tenant_id, user_id, role, facility_id) VALUES ($1,$2,$3,'physician',NULL)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(tid)
    .bind(uid)
    .execute(&state.pool)
    .await
    .unwrap();
    let (st, meta) = call(
        &state,
        "GET",
        "/api/v1/meta/tenant",
        &format!("dev-{username}"),
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{meta}");
    for f in meta["facilities"].as_array().unwrap() {
        assert_eq!(f["can_act_clinically"], json!(false), "{f}");
        assert_eq!(f["accessible"], json!(false), "{f}");
    }
}

#[tokio::test]
async fn seeded_ai_artifacts_use_governed_autonomy_level() {
    let (state, _) = test_state().await;
    let rows: Vec<(String,)> = sqlx::query_as(
        "SELECT DISTINCT autonomy_level FROM ai_artifacts WHERE artifact_type = 'result_summary'",
    )
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert!(!rows.is_empty());
    for (level,) in rows {
        assert_eq!(level, "A2", "result summaries are governed at A2");
    }
}

#[tokio::test]
async fn physician_can_open_chart_after_starting_encounter_from_search() {
    let (state, _) = test_state().await;
    // Register a brand-new patient (no prior encounters) as registration staff.
    let identifier = uniq("SYN-ENC");
    let (facility_id,): (uuid::Uuid,) = sqlx::query_as(
        "SELECT ra.facility_id FROM role_assignments ra
         JOIN users u ON u.id = ra.user_id
         WHERE u.username = 'reg.rivera' AND ra.role = 'registration_staff'
           AND ra.facility_id IS NOT NULL LIMIT 1",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let (st, created) = call(
        &state,
        "POST",
        "/api/v1/patients",
        "dev-reg.rivera",
        Some(json!({
            "facility_id": facility_id,
            "family_name": "Fresh",
            "given_name": "Encounterless",
            "birth_date": "1982-03-04",
            "sex": "male",
            "identifier": identifier,
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{created}");
    let patient_id = created["id"].as_str().unwrap().to_string();

    // A physician without a care relationship cannot open the chart yet.
    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient_id}"),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Search reflects that: the display-only capability is false for the
    // physician, but true for registration staff (facility-scoped reads).
    let (st, hits) = call(
        &state,
        "GET",
        &format!("/api/v1/patients?query={identifier}"),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{hits}");
    let hit = hits["patients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == json!(patient_id))
        .expect("registered patient in physician search results");
    assert_eq!(hit["can_open_chart"], json!(false), "{hit}");
    let (st, hits) = call(
        &state,
        "GET",
        &format!("/api/v1/patients?query={identifier}"),
        "dev-reg.rivera",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{hits}");
    let hit = hits["patients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == json!(patient_id))
        .expect("registered patient in registration search results");
    assert_eq!(hit["can_open_chart"], json!(true), "{hit}");

    // Starting an encounter (the search-result action) establishes it.
    let (st, enc) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{enc}");
    let (st, chart) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient_id}"),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{chart}");

    // With the relationship established the search capability flips to true.
    let (st, hits) = call(
        &state,
        "GET",
        &format!("/api/v1/patients?query={identifier}"),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{hits}");
    let hit = hits["patients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == json!(patient_id))
        .expect("patient in physician search results after encounter");
    assert_eq!(hit["can_open_chart"], json!(true), "{hit}");
}

#[tokio::test]
async fn service_request_requires_own_active_encounter() {
    let (state, _) = test_state().await;
    let (st, meta) = call(
        &state,
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
        &state,
        "POST",
        "/api/v1/patients",
        "dev-reg.rivera",
        Some(json!({
            "facility_id": facility,
            "family_name": "Ordering",
            "given_name": "Guard",
            "birth_date": "1970-01-15",
            "sex": "female",
            "identifier": uniq("MRN-ORD"),
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{patient}");
    let patient_id = patient["id"].as_str().unwrap().to_string();

    // A completed encounter is not a valid ordering context.
    let (st, enc) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{enc}");
    let completed_enc = enc["id"].as_str().unwrap().to_string();
    sqlx::query("UPDATE encounters SET status = 'completed' WHERE id = $1::uuid")
        .bind(&completed_enc)
        .execute(&state.pool)
        .await
        .unwrap();
    let (st, body) = call(
        &state,
        "POST",
        "/api/v1/service-requests",
        "dev-dr.garcia",
        Some(json!({
            "encounter_id": completed_enc,
            "code_loinc": "2823-3",
            "display": "Potassium",
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        body["error"]["code"],
        json!("encounter_not_active"),
        "{body}"
    );

    // Another practitioner's active encounter is not a valid ordering
    // context either, even for a physician assigned to the same facility.
    let (st, enc) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": patient_id })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{enc}");
    let garcia_enc = enc["id"].as_str().unwrap().to_string();
    let other = create_same_facility_physician(&state, &facility).await;
    let (st, body) = call(
        &state,
        "POST",
        "/api/v1/service-requests",
        &format!("dev-{other}"),
        Some(json!({
            "encounter_id": garcia_enc,
            "code_loinc": "2823-3",
            "display": "Potassium",
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");

    // The encounter's own practitioner can still order on it.
    let (st, sr) = call(
        &state,
        "POST",
        "/api/v1/service-requests",
        "dev-dr.garcia",
        Some(json!({
            "encounter_id": garcia_enc,
            "code_loinc": "2823-3",
            "display": "Potassium",
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sr}");
}

#[tokio::test]
async fn search_encounter_capability_is_facility_specific() {
    let (state, _) = test_state().await;
    let (st, meta) = call(
        &state,
        "GET",
        "/api/v1/meta/tenant",
        "dev-reg.rivera",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let facility_a = meta["facilities"][0]["id"].as_str().unwrap().to_string();
    let (tenant_id,): (uuid::Uuid,) =
        sqlx::query_as("SELECT tenant_id FROM users WHERE username = 'dr.garcia'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let (facility_b,): (uuid::Uuid,) =
        sqlx::query_as("SELECT id FROM facilities WHERE tenant_id = $1 AND id <> $2::uuid LIMIT 1")
            .bind(tenant_id)
            .bind(&facility_a)
            .fetch_one(&state.pool)
            .await
            .unwrap();

    // A mixed-role user: clinical rights at facility A, search-only
    // (registration) rights at facility B.
    let username = create_same_facility_physician(&state, &facility_a).await;
    let (uid,): (uuid::Uuid,) = sqlx::query_as("SELECT id FROM users WHERE username = $1")
        .bind(&username)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO role_assignments (id, tenant_id, user_id, role, facility_id)
         VALUES ($1,$2,$3,'registration_staff',$4)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(tenant_id)
    .bind(uid)
    .bind(facility_b)
    .execute(&state.pool)
    .await
    .unwrap();

    let id_a = uniq("MRN-FACA");
    let id_b = uniq("MRN-FACB");
    let (st, pat_a) = call(
        &state,
        "POST",
        "/api/v1/patients",
        &format!("dev-{username}"),
        Some(json!({
            "facility_id": facility_a,
            "family_name": "Mixed",
            "given_name": "AtHome",
            "birth_date": "1980-05-05",
            "sex": "female",
            "identifier": id_a,
        })),
        &[],
    )
    .await;
    // Registration at facility A may be denied for this user (they only
    // register at B); fall back to reg.rivera whose facility is A.
    let pat_a = if st == StatusCode::OK {
        pat_a
    } else {
        let (st, p) = call(
            &state,
            "POST",
            "/api/v1/patients",
            "dev-reg.rivera",
            Some(json!({
                "facility_id": facility_a,
                "family_name": "Mixed",
                "given_name": "AtHome",
                "birth_date": "1980-05-05",
                "sex": "female",
                "identifier": id_a,
            })),
            &[],
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{p}");
        p
    };
    let (st, pat_b) = call(
        &state,
        "POST",
        "/api/v1/patients",
        &format!("dev-{username}"),
        Some(json!({
            "facility_id": facility_b,
            "family_name": "Mixed",
            "given_name": "Elsewhere",
            "birth_date": "1981-06-06",
            "sex": "male",
            "identifier": id_b,
        })),
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{pat_b}");

    // Facility A patient: encounter capability true; facility B: false.
    for (identifier, patient, expected) in [(&id_a, &pat_a, true), (&id_b, &pat_b, false)] {
        let (st, hits) = call(
            &state,
            "GET",
            &format!("/api/v1/patients?query={identifier}"),
            &format!("dev-{username}"),
            None,
            &[],
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{hits}");
        let hit = hits["patients"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == patient["id"])
            .expect("patient visible in mixed-role search");
        assert_eq!(hit["can_start_encounter"], json!(expected), "{hit}");
    }

    // The display hint matches the backend: starting an encounter at the
    // search-only facility is denied.
    let (st, body) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        &format!("dev-{username}"),
        Some(json!({ "patient_id": pat_b["id"] })),
        &[],
    )
    .await;
    assert_ne!(st, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn worklist_detail_capabilities_match_backend_policy() {
    let (state, _) = test_state().await;
    let lp = run_to_received(&state, 7.1).await;

    let find_item = |body: &serde_json::Value| -> Option<serde_json::Value> {
        body["items"]
            .as_array()
            .unwrap()
            .iter()
            .find(|i| i["id"] == json!(lp.service_request_id))
            .cloned()
    };

    // The ordering physician has a care relationship: detail is reachable
    // and the transition capability is granted.
    let (st, body) = call(
        &state,
        "GET",
        "/api/v1/worklist",
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let item = find_item(&body).expect("ordering physician sees the row");
    assert_eq!(item["can_open_detail"], json!(true), "{item}");
    let (st, detail) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{detail}");
    assert_eq!(detail["capabilities"]["review"], json!(true), "{detail}");

    // A laboratory professional reads the worklist but cannot open the
    // patient-detail view: the hint is false and the endpoint denies.
    let (st, body) = call(&state, "GET", "/api/v1/worklist", "dev-lab.chen", None, &[]).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let item = find_item(&body).expect("laboratory professional sees the row");
    assert_eq!(item["can_open_detail"], json!(false), "{item}");
    let (st, body) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{}", lp.service_request_id),
        "dev-lab.chen",
        None,
        &[],
    )
    .await;
    assert_ne!(st, StatusCode::OK, "{body}");
}

/// Create a fresh physician assigned to the given facility.
async fn create_same_facility_physician(state: &AppState, facility_id: &str) -> String {
    let username = uniq("dr.other");
    let (tenant_id,): (uuid::Uuid,) =
        sqlx::query_as("SELECT tenant_id FROM users WHERE username = 'dr.garcia'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let uid = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, tenant_id, username, display_name) VALUES ($1,$2,$3,'Other Test Physician')",
    )
    .bind(uid)
    .bind(tenant_id)
    .bind(&username)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO role_assignments (id, tenant_id, user_id, role, facility_id) VALUES ($1,$2,$3,'physician',$4::uuid)",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(tenant_id)
    .bind(uid)
    .bind(facility_id)
    .execute(&state.pool)
    .await
    .unwrap();
    username
}
