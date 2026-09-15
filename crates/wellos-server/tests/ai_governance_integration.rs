//! Integration tests for the governed AI runtime: honest capability
//! reporting, artifact reuse for identical requests (no provider call, no
//! quota spend), hourly quotas, disabled providers surfacing as typed errors
//! instead of fabricated output, external-processing gates, and the
//! development-only discovery of synthetic identities.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use dmind_gateway::DisabledGateway;
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
    // Hugo is not used by any other test in this binary: the tests run
    // concurrently and this one writes risk_summary artifacts.
    let (hugo, tenant) = patient_by_identifier(&state, "SYN-0104").await;
    let path = format!("/api/v1/patients/{hugo}/risk/summary");
    let lang = json!({ "language": "en" });
    // Withdrawn artifacts are not reusable, so on a reused database this
    // test starts from the same state as on a fresh one.
    sqlx::query(
        "UPDATE ai_artifacts SET status = 'withdrawn'
         WHERE tenant_id = $1 AND patient_id = $2 AND artifact_type = 'risk_summary'",
    )
    .bind(tenant)
    .bind(hugo)
    .execute(&state.pool)
    .await
    .unwrap();

    let baseline = executions(&state, tenant, "risk_summary").await;
    let (st, first, _) = call(&state, "POST", &path, Some(GARCIA), Some(lang.clone())).await;
    assert_eq!(st, StatusCode::OK, "{first}");
    let first_id: Uuid = first["id"].as_str().unwrap().parse().unwrap();
    let before = executions(&state, tenant, "risk_summary").await;
    assert_eq!(before, baseline + 1, "a withdrawn artifact is never reused");

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

    // A professional rejects the Spanish summary: identical requests must
    // execute again instead of resurfacing the rejected text.
    let third_id = third["id"].as_str().unwrap();
    let (st, review, _) = call(
        &state,
        "POST",
        &format!("{path}/{third_id}/review"),
        Some(GARCIA),
        Some(json!({ "decision": "reject", "note": "Not clinically useful." })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{review}");
    assert_eq!(review["summary"]["status"], "rejected", "{review}");
    let (st, fourth, _) = call(
        &state,
        "POST",
        &path,
        Some(GARCIA),
        Some(json!({ "language": "es" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{fourth}");
    assert_ne!(fourth["id"], third["id"]);
    assert_eq!(
        executions(&state, tenant, "risk_summary").await,
        before + 2,
        "a rejected artifact is never reused"
    );
    let (reused, status): (Option<Uuid>, String) =
        sqlx::query_as("SELECT reused_from, status FROM ai_artifacts WHERE id = $1")
            .bind(fourth["id"].as_str().unwrap().parse::<Uuid>().unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(reused, None);
    assert_eq!(status, "awaiting_review");
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
    // An isolated synthetic tenant so no other suite's executions count.
    let tenant = isolated_tenant(&state.pool, "Quota fixture").await;
    let patient = Uuid::now_v7();
    let task = "risk_summary";
    let scope = aigov::ReuseScope::RiskSummary {
        risk_assessment_id: Uuid::now_v7(),
    };

    let plan = aigov::plan(&state, tenant, patient, scope, "hash-1", "risk-summary.v1")
        .await
        .unwrap();
    assert!(matches!(plan, aigov::ExecutionPlan::Execute { .. }));
    assert_eq!(executions(&state, tenant, task).await, 1);

    let err = aigov::plan(&state, tenant, patient, scope, "hash-2", "risk-summary.v1")
        .await
        .expect_err("second distinct request exceeds the task quota");
    assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
    assert_eq!(err.code, "ai_quota_exceeded");
    let retry = err.retry_after.expect("Retry-After is set");
    assert!(retry > 0 && retry <= 3600, "{retry}");
    assert_eq!(
        executions(&state, tenant, task).await,
        1,
        "a refused request reserves nothing"
    );
}

async fn isolated_tenant(pool: &sqlx::PgPool, name: &str) -> Uuid {
    let tenant = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO tenants (id, cell, name, data_class)
         VALUES ($1, 'cell-test', $2, 'synthetic')",
    )
    .bind(tenant)
    .bind(name)
    .execute(pool)
    .await
    .unwrap();
    tenant
}

/// `Retry-After` follows the window that actually blocks the request: an
/// older execution of an unrelated task must not shorten a task-quota wait,
/// and when tenant and task windows are both exhausted the later one wins.
#[tokio::test]
async fn quota_retry_after_follows_the_blocking_window() {
    let gateway = Arc::new(dmind_gateway::fake::FakeProvider::new());
    let mut runtime = RuntimeConfig::test_fixtures();
    runtime.ai_quotas = AiQuotas {
        tenant_per_hour: 100_000,
        task_per_hour: 1,
    };
    let state = AppState::from_runtime(
        pool().await,
        gateway.clone(),
        Arc::new(dmind_gateway::scribe::FakeTranscription::new()),
        AuthConfig::development(),
        runtime,
    );
    // An isolated synthetic tenant so no other suite's executions count.
    let tenant = isolated_tenant(&state.pool, "Quota window fixture").await;
    let patient = Uuid::now_v7();
    // Task B is the risk summary; task A ("other") the triage proposal.
    let task_scope = aigov::ReuseScope::RiskSummary {
        risk_assessment_id: Uuid::now_v7(),
    };
    let other_scope = aigov::ReuseScope::TriageProposal {
        visit_id: Uuid::now_v7(),
    };
    let other = other_scope.artifact_type();
    let plan_task = |hash: &'static str| {
        aigov::plan(&state, tenant, patient, task_scope, hash, "risk-summary.v1")
    };

    // Task A ran 59 minutes ago ...
    sqlx::query(
        "INSERT INTO ai_executions (id, tenant_id, artifact_type, provider, model, external, executed_at)
         VALUES ($1, $2, $3, 'fake', 'dmind-fake', false, now() - interval '59 minutes')",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(other)
    .execute(&state.pool)
    .await
    .unwrap();
    // ... task B reaches its limit now.
    assert!(matches!(
        plan_task("hash-1").await.unwrap(),
        aigov::ExecutionPlan::Execute { .. }
    ));
    let err = plan_task("hash-2").await.expect_err("task quota exhausted");
    assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
    let retry = err.retry_after.expect("Retry-After is set");
    assert!(
        (3540..=3600).contains(&retry),
        "task wait follows task B's own execution, not task A's: {retry}"
    );

    // Both windows exhausted: tenant (limit 2) frees when its 2nd newest
    // execution (59 minutes old) ages out in ~1 minute, the task window only
    // in ~1 hour; the request cannot be admitted before both permit it.
    let mut runtime = RuntimeConfig::test_fixtures();
    runtime.ai_quotas = AiQuotas {
        tenant_per_hour: 2,
        task_per_hour: 1,
    };
    let both = AppState::from_runtime(
        state.pool.clone(),
        gateway,
        Arc::new(dmind_gateway::scribe::FakeTranscription::new()),
        AuthConfig::development(),
        runtime,
    );
    let err = aigov::plan(
        &both,
        tenant,
        patient,
        task_scope,
        "hash-3",
        "risk-summary.v1",
    )
    .await
    .expect_err("tenant and task quotas exhausted");
    assert_eq!(err.status, StatusCode::TOO_MANY_REQUESTS);
    assert!(
        err.message.contains("organization"),
        "tenant exhaustion is named: {}",
        err.message
    );
    let retry = err.retry_after.expect("Retry-After is set");
    assert!(
        (3540..=3600).contains(&retry),
        "the later (task) window governs: {retry}"
    );
    // For task A both windows hinge on the 59-minute-old execution, so the
    // wait is about a minute, not an hour.
    let err = aigov::plan(
        &both,
        tenant,
        patient,
        other_scope,
        "hash-4",
        wellos_domain::triage::TRIAGE_PROPOSAL_SCHEMA,
    )
    .await
    .expect_err("tenant quota exhausted");
    let retry = err.retry_after.expect("Retry-After is set");
    assert!(
        (1..=60).contains(&retry),
        "tenant window frees soon: {retry}"
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

/// Even with development authentication enabled, a `dev-<username>` token
/// only ever authenticates a synthetic human of a synthetic tenant: users of
/// a production-class tenant and humans linked to a real identity-provider
/// subject fail exactly like an unknown token.
#[tokio::test]
async fn dev_tokens_authenticate_only_synthetic_users_of_synthetic_tenants() {
    let (state, _) = fake_state().await;
    let suffix = Uuid::now_v7().simple().to_string();
    let production_tenant = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO tenants (id, cell, name, data_class)
         VALUES ($1, 'cell-dev-1', 'Real Hospital (dev-token fixture)', 'production')",
    )
    .bind(production_tenant)
    .execute(&state.pool)
    .await
    .unwrap();
    let (synthetic_tenant,): (Uuid,) =
        sqlx::query_as("SELECT tenant_id FROM users WHERE username = 'dr.garcia'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let production_user = format!("dr.real.{suffix}");
    let federated_user = format!("dr.idp.{suffix}");
    let fixture_user = format!("dr.fixture.{suffix}");
    for (tenant, username, subject) in [
        (production_tenant, &production_user, None),
        (
            synthetic_tenant,
            &federated_user,
            Some(format!("https://idp.example.org|{suffix}")),
        ),
        (
            synthetic_tenant,
            &fixture_user,
            Some(format!("synthetic|{fixture_user}")),
        ),
    ] {
        let user_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO users (id, tenant_id, username, display_name, oidc_subject)
             VALUES ($1, $2, $3, 'Dev-token boundary fixture', $4)",
        )
        .bind(user_id)
        .bind(tenant)
        .bind(username)
        .bind(subject)
        .execute(&state.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO role_assignments (id, tenant_id, user_id, role)
             VALUES ($1, $2, $3, 'physician')",
        )
        .bind(Uuid::now_v7())
        .bind(tenant)
        .bind(user_id)
        .execute(&state.pool)
        .await
        .unwrap();
    }

    let probe = |token: String| {
        let state = state.clone();
        async move { call(&state, "GET", "/api/v1/meta/tenant", Some(&token), None).await }
    };
    let (st, body, _) = probe(format!("dev-{production_user}")).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "production tenant: {body}");
    let (st, body, _) = probe(format!("dev-{federated_user}")).await;
    assert_eq!(st, StatusCode::UNAUTHORIZED, "real IdP subject: {body}");
    let (st, body, _) = probe(format!("dev-{fixture_user}")).await;
    assert_eq!(st, StatusCode::OK, "synthetic fixture user: {body}");
    let (st, body, _) = probe(GARCIA.to_string()).await;
    assert_eq!(st, StatusCode::OK, "seeded synthetic user: {body}");

    // Discovery never lists the production-class or federated users either.
    let (st, body, _) = call(&state, "GET", "/api/v1/auth/dev/users", None, None).await;
    assert_eq!(st, StatusCode::OK);
    let listed = body.to_string();
    assert!(!listed.contains(&production_user), "{listed}");
    assert!(!listed.contains(&federated_user), "{listed}");

    sqlx::query("DELETE FROM role_assignments WHERE user_id IN (SELECT id FROM users WHERE username = ANY($1))")
        .bind(vec![production_user.clone(), federated_user.clone(), fixture_user.clone()])
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM users WHERE username = ANY($1)")
        .bind(vec![production_user, federated_user, fixture_user])
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM tenants WHERE id = $1")
        .bind(production_tenant)
        .execute(&state.pool)
        .await
        .unwrap();
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
