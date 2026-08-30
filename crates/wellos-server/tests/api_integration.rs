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
        "SELECT u.tenant_id, f.id FROM users u\n         JOIN facilities f ON f.tenant_id = u.tenant_id\n         WHERE u.username = 'dr.garcia' LIMIT 1",
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
