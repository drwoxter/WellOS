//! Integration tests for the governed AI runtime: honest capability
//! reporting, artifact reuse for identical requests (no provider call, no
//! quota spend), hourly quotas, disabled providers surfacing as typed errors
//! instead of fabricated output, external-processing gates, and the
//! development-only discovery of synthetic identities.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use dmind_gateway::{DisabledGateway, Operation};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;
use wellos_server::runtime::{AiQuotas, RuntimeConfig};
use wellos_server::state::{AppState, AuthConfig};
use wellos_server::{aigov, seeddata};

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wellos:wellos_dev@localhost:5432/wellos".to_string())
}

async fn pool() -> sqlx::PgPool {
    let pool = wellos_server::connect_pool(&database_url()).await.unwrap();
    wellos_server::run_migrations(&pool).await.unwrap();
    let (users,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    if users == 0 {
        seeddata::seed(&pool, &RuntimeConfig::test_fixtures())
            .await
            .unwrap();
    }
    pool
}

async fn fake_state() -> (AppState, Arc<dmind_gateway::fake::FakeProvider>) {
    let gateway = Arc::new(dmind_gateway::fake::FakeProvider::new());
    (AppState::new(pool().await, gateway.clone()), gateway)
}

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
) -> (StatusCode, Value, Option<String>) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("Content-Type", "application/json");
    if let Some(token) = token {
        req = req.header("Authorization", format!("Bearer {token}"));
    }
    let req = req
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
    let retry_after = res
        .headers()
        .get("retry-after")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value, retry_after)
}

const GARCIA: &str = "dev-dr.garcia";

async fn patient_by_identifier(state: &AppState, identifier: &str) -> (Uuid, Uuid) {
    sqlx::query_as("SELECT id, tenant_id FROM patients WHERE identifier = $1")
        .bind(identifier)
        .fetch_one(&state.pool)
        .await
        .unwrap()
}

async fn executions(state: &AppState, tenant_id: Uuid, artifact_type: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM ai_executions WHERE tenant_id = $1 AND artifact_type = $2",
    )
    .bind(tenant_id)
    .bind(artifact_type)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    n
}

#[tokio::test]
async fn readiness_reports_each_capability_honestly() {
    let (state, fake) = fake_state().await;
    let (st, body, _) = call(&state, "GET", "/ready", None, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["environment"], "test");
    let caps = &body["ai_capabilities"];
    for name in ["model", "transcription", "structured_note"] {
        assert_eq!(caps[name]["state"], "ready", "{name}: {caps}");
        assert_eq!(caps[name]["synthetic"], true, "{name}: {caps}");
        assert_eq!(caps[name]["external"], false, "{name}: {caps}");
    }
    assert!(caps["model"]["reason"].is_string(), "{caps}");
    assert!(caps["transcription"]["reason"].is_string(), "{caps}");
    assert_eq!(caps["model"]["provider"], "fake");
    assert_eq!(caps["transcription_languages"], json!(["en", "es"]));

    // A failing provider is reported as degraded, and the end-to-end scribe
    // path inherits the weaker of its two providers.
    fake.set_unavailable(true);
    let (_, body, _) = call(&state, "GET", "/ready", None, None).await;
    let caps = &body["ai_capabilities"];
    assert_eq!(caps["model"]["state"], "degraded", "{caps}");
    assert_eq!(caps["transcription"]["state"], "ready", "{caps}");
    assert_eq!(caps["structured_note"]["state"], "degraded", "{caps}");

    // A disabled provider is never presented as available or synthetic-ready.
    let disabled = AppState::new(
        state.pool.clone(),
        Arc::new(DisabledGateway::disabled("DMIND_MODEL_PROVIDER=disabled")),
    );
    let (_, body, _) = call(&disabled, "GET", "/ready", None, None).await;
    let caps = &body["ai_capabilities"];
    assert_eq!(caps["model"]["state"], "disabled", "{caps}");
    assert_eq!(caps["model"]["provider"], "disabled", "{caps}");
    assert_eq!(caps["structured_note"]["state"], "disabled", "{caps}");
    assert_eq!(caps["transcription"]["state"], "ready", "{caps}");
}

#[tokio::test]
async fn identical_requests_reuse_the_artifact_without_a_new_execution() {
    let (state, _) = fake_state().await;
    let (teresa, tenant) = patient_by_identifier(&state, "SYN-0103").await;
    let path = format!("/api/v1/patients/{teresa}/risk/summary");
    let lang = json!({ "language": "en" });

    let (st, first, _) = call(&state, "POST", &path, Some(GARCIA), Some(lang.clone())).await;
    assert_eq!(st, StatusCode::OK, "{first}");
    let first_id: Uuid = first["id"].as_str().unwrap().parse().unwrap();
    let before = executions(&state, tenant, "risk_summary").await;

    let (st, second, _) = call(&state, "POST", &path, Some(GARCIA), Some(lang)).await;
    assert_eq!(st, StatusCode::OK, "{second}");
    let second_id: Uuid = second["id"].as_str().unwrap().parse().unwrap();
    assert_ne!(
        first_id, second_id,
        "a new reviewable artifact is still recorded"
    );
    assert_eq!(second["summary"]["output"], first["summary"]["output"]);

    let row = sqlx::query_as::<_, (Option<Uuid>, String, String, bool, Option<String>)>(
        "SELECT reused_from, prompt_version, provider, synthetic, input_hash
         FROM ai_artifacts WHERE id = $1",
    )
    .bind(second_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let first_hash: (Option<String>,) =
        sqlx::query_as("SELECT input_hash FROM ai_artifacts WHERE id = $1")
            .bind(first_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(
        row.0,
        Some(first_id),
        "reuse provenance points at the prior artifact"
    );
    assert_eq!(row.1, "risk-summary-deterministic.v1");
    assert!(row.3, "fixture output is persisted as synthetic");
    assert_eq!(row.4, first_hash.0, "same input hash");
    assert_eq!(
        executions(&state, tenant, "risk_summary").await,
        before,
        "reuse must not spend an execution / quota slot"
    );

    // A different input (language) is a different request and executes.
    let (st, third, _) = call(
        &state,
        "POST",
        &path,
        Some(GARCIA),
        Some(json!({ "language": "es" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{third}");
    assert_eq!(executions(&state, tenant, "risk_summary").await, before + 1);
    let (reused,): (Option<Uuid>,) =
        sqlx::query_as("SELECT reused_from FROM ai_artifacts WHERE id = $1")
            .bind(third["id"].as_str().unwrap().parse::<Uuid>().unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(reused, None);
}

#[tokio::test]
async fn task_quota_is_enforced_per_hour_with_retry_after() {
    let gateway = Arc::new(dmind_gateway::fake::FakeProvider::new());
    let mut runtime = RuntimeConfig::test_fixtures();
    runtime.ai_quotas = AiQuotas {
        tenant_per_hour: 100_000,
        task_per_hour: 1,
    };
    let state = AppState::from_runtime(
        pool().await,
        gateway,
        Arc::new(dmind_gateway::scribe::FakeTranscription::new()),
        AuthConfig::development(),
        runtime,
    );
    let (patient, tenant) = patient_by_identifier(&state, "SYN-0103").await;
    // A task type unique to this test so no other suite's executions count.
    let task = format!("quota-test-{}", Uuid::now_v7().simple());

    let plan = aigov::plan(
        &state,
        tenant,
        patient,
        &task,
        Operation::RiskSummary,
        "hash-1",
        "risk-summary.v1",
    )
    .await
    .unwrap();
    assert!(matches!(plan, aigov::ExecutionPlan::Execute { .. }));
    assert_eq!(executions(&state, tenant, &task).await, 1);

    let err = aigov::plan(
        &state,
        tenant,
        patient,
        &task,
        Operation::RiskSummary,
        "hash-2",
        "risk-summary.v1",
    )
    .await
    .expect_err("second distinct request exceeds the task quota");
    assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(err.code, "ai_quota_exceeded");
    let retry = err.retry_after.expect("Retry-After is set");
    assert!(retry > 0 && retry <= 3600, "{retry}");
    assert_eq!(
        executions(&state, tenant, &task).await,
        1,
        "a refused request reserves nothing"
    );
}

#[tokio::test]
async fn disabled_provider_surfaces_a_typed_error_and_never_fabricates() {
    let state = AppState::new(
        pool().await,
        Arc::new(DisabledGateway::disabled("DMIND_MODEL_PROVIDER=disabled")),
    );
    let (teresa, tenant) = patient_by_identifier(&state, "SYN-0103").await;
    let count = || async {
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM ai_artifacts WHERE tenant_id = $1 AND patient_id = $2
             AND artifact_type = 'risk_summary'",
        )
        .bind(tenant)
        .bind(teresa)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        n
    };
    let before = count().await;
    let (st, body, _) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{teresa}/risk/summary"),
        Some(GARCIA),
        Some(json!({ "language": "en" })),
    )
    .await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{body}");
    assert_eq!(body["error"]["code"], "ai_disabled", "{body}");
    assert_eq!(
        count().await,
        before,
        "no artifact is written for a disabled provider"
    );

    // The deterministic assessment itself stays fully available.
    let (st, body, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{teresa}/risk"),
        Some(GARCIA),
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["current"]["overall_level"], "critical", "{body}");
}

#[tokio::test]
async fn external_processing_needs_deployment_opt_in_and_patient_consent() {
    let (mut state, _) = fake_state().await;
    let (patient, tenant) = patient_by_identifier(&state, "SYN-0103").await;

    let err = aigov::external_processing_allowed(&state, tenant, patient)
        .await
        .expect_err("deployment default forbids external AI");
    assert_eq!(err.status, StatusCode::SERVICE_UNAVAILABLE);
    assert_eq!(err.code, "ai_external_disallowed");

    state.allow_external_ai = true;
    let err = aigov::external_processing_allowed(&state, tenant, patient)
        .await
        .expect_err("no active ai_external_processing consent");
    assert_eq!(err.status, StatusCode::CONFLICT);
    assert_eq!(err.code, "ai_external_consent_required");

    sqlx::query(
        "INSERT INTO consents (id, tenant_id, patient_id, purpose, status, version)
         SELECT $1, $2, $3, 'ai_external_processing', 'active',
                COALESCE(MAX(version), 0) + 1
         FROM consents WHERE tenant_id = $2 AND patient_id = $3
           AND purpose = 'ai_external_processing'",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(patient)
    .execute(&state.pool)
    .await
    .unwrap();
    aigov::external_processing_allowed(&state, tenant, patient)
        .await
        .expect("opt-in + active consent permits external processing");
    sqlx::query(
        "INSERT INTO consents (id, tenant_id, patient_id, purpose, status, version)
         SELECT $1, $2, $3, 'ai_external_processing', 'revoked',
                COALESCE(MAX(version), 0) + 1
         FROM consents WHERE tenant_id = $2 AND patient_id = $3
           AND purpose = 'ai_external_processing'",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(patient)
    .execute(&state.pool)
    .await
    .unwrap();
    assert_eq!(
        aigov::external_processing_allowed(&state, tenant, patient)
            .await
            .err()
            .map(|e| e.code),
        Some("ai_external_consent_required")
    );
}

#[tokio::test]
async fn sign_in_discovery_is_server_controlled_and_synthetic_only() {
    let (state, _) = fake_state().await;
    let (st, body, _) = call(&state, "GET", "/api/v1/auth/providers", None, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["environment"], "test");
    assert_eq!(body["development"], true);
    assert_eq!(body["oidc"], false);

    let (st, body, _) = call(&state, "GET", "/api/v1/auth/dev/users", None, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["synthetic"], true);
    let users = body["users"].as_array().unwrap();
    assert!(users
        .iter()
        .any(|u| u["username"] == "dr.garcia" && u["tenant_name"].is_string()));
    assert!(
        users
            .iter()
            .all(|u| !u["roles"].as_array().unwrap().is_empty()),
        "{users:?}"
    );
    assert!(
        users
            .iter()
            .all(|u| !u["username"].as_str().unwrap().starts_with("svc.")),
        "machine principals are never offered as sign-in identities: {users:?}"
    );
    // No credential material is ever returned — only usernames.
    assert!(!body.to_string().contains("dev-dr.garcia"));

    let mut auth = AuthConfig::development();
    auth.dev_auth_enabled = false;
    let locked = AppState::with_auth(
        state.pool.clone(),
        Arc::new(dmind_gateway::fake::FakeProvider::new()),
        auth,
    );
    let (st, body, _) = call(&locked, "GET", "/api/v1/auth/providers", None, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(body["development"], false);
    let (st, _, _) = call(&locked, "GET", "/api/v1/auth/dev/users", None, None).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn seed_refuses_a_database_holding_non_synthetic_tenants() {
    let pool = pool().await;
    let tenant = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO tenants (id, cell, name, data_class)
         VALUES ($1, 'cell-dev-1', 'Real Hospital (test row)', 'production')",
    )
    .bind(tenant)
    .execute(&pool)
    .await
    .unwrap();
    // An already-seeded database is skipped idempotently unless a
    // non-synthetic tenant is present, which is a hard refusal.
    let err = match seeddata::seed(&pool, &RuntimeConfig::test_fixtures()).await {
        Ok(_) => panic!("seed must refuse to mix with production data"),
        Err(err) => err,
    };
    assert!(err.to_string().contains("non-synthetic"), "{err}");
    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(tenant)
        .execute(&pool)
        .await
        .unwrap();
}
