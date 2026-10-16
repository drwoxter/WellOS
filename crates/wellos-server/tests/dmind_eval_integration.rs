//! dMind evaluation API: centrally authorized (clinical administrator manages,
//! security auditor reads), synthetic tenant only, offline/dry-run only over
//! HTTP, append-only reviews, and — the property that matters most — an
//! evaluation run writes nothing outside the `dmind_eval_*` tables.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use sqlx::{PgPool, Row};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;
use wellos_server::state::AppState;

const ADMIN: &str = "dev-admin.silva";
const AUDITOR: &str = "dev-audit.stone";
const DR: &str = "dev-dr.garcia";
const BASE: &str = "/api/v1/admin/dmind/evaluations";

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wellos:wellos_dev@localhost:5432/wellos".to_string())
}

async fn test_state() -> AppState {
    let pool = wellos_server::connect_pool(&database_url()).await.unwrap();
    wellos_server::run_migrations(&pool).await.unwrap();
    let (n,): (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    if n == 0 {
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

async fn send(
    state: &AppState,
    method: &str,
    token: &str,
    path: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    send_for(state, method, token, path, body, "operations").await
}

async fn send_for(
    state: &AppState,
    method: &str,
    token: &str,
    path: &str,
    body: Option<Value>,
    purpose: &str,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("x-purpose-of-use", purpose)
        .body(Body::from(
            body.map(|v| v.to_string().into_bytes()).unwrap_or_default(),
        ))
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

/// Row counts and content hashes of every non-evaluation table. Authorization
/// decisions are audited (`audit_events` + their `outbox_events` fan-out) and
/// rate-limit windows tick on every request, so those three infrastructure
/// tables are the only ones excluded; every clinical table must be identical.
async fn clinical_footprint(pool: &PgPool) -> Vec<(String, i64)> {
    let tables: Vec<String> = sqlx::query(
        "SELECT tablename FROM pg_tables WHERE schemaname = 'public'
           AND tablename NOT LIKE 'dmind_eval_%'
           AND tablename NOT IN ('audit_events', 'outbox_events', 'rate_limit_windows',
                                 '_sqlx_migrations', 'wellos_migrations')
         ORDER BY tablename",
    )
    .fetch_all(pool)
    .await
    .unwrap()
    .iter()
    .map(|r| r.get::<String, _>("tablename"))
    .collect();
    let mut out = Vec::new();
    for t in tables {
        let row = sqlx::query(&format!(
            "SELECT COUNT(*)::bigint AS n, COALESCE(md5(string_agg(x::text, '' ORDER BY x::text)), '') AS h FROM {t} x"
        ))
        .fetch_one(pool)
        .await
        .unwrap();
        out.push((
            format!("{t}:{}", row.get::<String, _>("h")),
            row.get::<i64, _>("n"),
        ));
    }
    out
}

#[tokio::test]
async fn offline_run_is_admin_only_auditor_readable_and_side_effect_free() {
    let state = test_state().await;
    let before = clinical_footprint(&state.pool).await;

    // Physicians have no evaluation permission at all.
    let (s, _) = send(&state, "GET", DR, &format!("{BASE}/suites"), None).await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Auditor: read-only.
    let (s, suites) = send(&state, "GET", AUDITOR, &format!("{BASE}/suites"), None).await;
    assert_eq!(s, StatusCode::OK, "{suites}");
    let corpus = wellos_server::eval::corpus::Corpus::load().unwrap();
    assert!(
        corpus.cases.len() >= 55,
        "corpus shrank: {}",
        corpus.cases.len()
    );
    assert_eq!(suites["compiled"]["case_count"], corpus.cases.len());
    assert_eq!(
        suites["compiled"]["operations"].as_array().unwrap().len(),
        wellos_server::eval::registry().len()
    );
    assert_eq!(suites["can_manage"], false);
    assert_eq!(suites["live_mode"], "cli_only");
    let (s, body) = send(
        &state,
        "POST",
        AUDITOR,
        &format!("{BASE}/runs"),
        Some(json!({ "mode": "offline" })),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN, "{body}");

    // Live mode is never reachable over HTTP.
    let (s, body) = send(
        &state,
        "POST",
        ADMIN,
        &format!("{BASE}/runs"),
        Some(json!({ "mode": "live" })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");

    // Administrator: offline run on two operations against the fixture provider.
    let (s, created) = send(
        &state,
        "POST",
        ADMIN,
        &format!("{BASE}/runs"),
        Some(json!({
            "mode": "offline",
            "operations": ["result_summary", "triage_proposal"],
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    let run_id = created["run_id"].as_str().unwrap().to_string();
    assert_eq!(created["totals"]["cases"], 12, "{created}");
    assert_eq!(created["totals"]["failed"], 0, "{created}");
    assert_eq!(created["safety_failures"].as_array().unwrap().len(), 0);

    // Listed and readable by both roles; summary persisted.
    for token in [ADMIN, AUDITOR] {
        let (s, list) = send(&state, "GET", token, &format!("{BASE}/runs"), None).await;
        assert_eq!(s, StatusCode::OK);
        let run = list["runs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == run_id)
            .expect("run listed");
        assert_eq!(run["status"], "completed");
        assert_eq!(run["mode"], "offline");
        assert_eq!(run["requested_via"], "api");
        assert_eq!(run["synthetic_provider"], true);
        assert_eq!(run["summary"]["totals"]["cases"], 12);
    }
    let (s, detail) = send(
        &state,
        "GET",
        AUDITOR,
        &format!("{BASE}/runs/{run_id}"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK);
    let executions = detail["executions"].as_array().unwrap();
    assert_eq!(executions.len(), 12);
    assert!(executions
        .iter()
        .all(|e| !e["metrics"].as_array().unwrap().is_empty()));
    assert!(executions
        .iter()
        .all(|e| e["provider_output"].is_object() && e["baseline_output"].is_object()));
    let execution_id = executions[0]["id"].as_str().unwrap().to_string();

    // Reviews: auditor refused, admin appends versioned rows; flags need a comment.
    let review_path = format!("{BASE}/executions/{execution_id}/reviews");
    let (s, _) = send(
        &state,
        "POST",
        AUDITOR,
        &review_path,
        Some(json!({ "decision": "accept_result" })),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);
    let (s, body) = send(
        &state,
        "POST",
        ADMIN,
        &review_path,
        Some(json!({ "decision": "flag_quality", "comment": "" })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    let (s, body) = send(
        &state,
        "POST",
        ADMIN,
        &review_path,
        Some(json!({ "decision": "approve" })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST, "{body}");
    let (s, r1) = send(
        &state,
        "POST",
        ADMIN,
        &review_path,
        Some(json!({ "decision": "accept_result" })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{r1}");
    assert_eq!(r1["version"], 1);
    let (s, r2) = send(
        &state,
        "POST",
        ADMIN,
        &review_path,
        Some(json!({ "decision": "flag_safety", "comment": "synthetic reviewer note" })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{r2}");
    assert_eq!(r2["version"], 2);
    let (_, detail) = send(&state, "GET", ADMIN, &format!("{BASE}/runs/{run_id}"), None).await;
    let reviewed = detail["executions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == execution_id)
        .unwrap();
    assert_eq!(reviewed["reviews"].as_array().unwrap().len(), 2);
    assert_eq!(reviewed["reviews"][1]["decision"], "flag_safety");
    assert_eq!(detail["can_manage"], true);

    // Reviews are append-only at the database level.
    let err =
        sqlx::query("UPDATE dmind_eval_reviews SET comment = 'edited' WHERE execution_id = $1")
            .bind(uuid::Uuid::parse_str(&execution_id).unwrap())
            .execute(&state.pool)
            .await
            .unwrap_err();
    assert!(err.to_string().contains("append-only"), "{err}");

    // Other tenants never see the run.
    let (s, _) = send(
        &state,
        "GET",
        "dev-dr.sur",
        &format!("{BASE}/runs/{run_id}"),
        None,
    )
    .await;
    assert_ne!(s, StatusCode::OK);

    // No clinical table changed (row counts and content hashes identical).
    let after = clinical_footprint(&state.pool).await;
    assert_eq!(
        before, after,
        "an evaluation run must not modify clinical data"
    );
}

#[tokio::test]
async fn dry_run_records_a_plan_without_provider_calls() {
    let state = test_state().await;
    let (s, created) = send(
        &state,
        "POST",
        ADMIN,
        &format!("{BASE}/runs"),
        Some(json!({ "mode": "dry-run", "operations": ["risk_summary"] })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    assert_eq!(created["status"], "aborted");
    assert_eq!(created["totals"]["provider_calls"], 0);
    let run_id = created["run_id"].as_str().unwrap();
    let (_, detail) = send(&state, "GET", ADMIN, &format!("{BASE}/runs/{run_id}"), None).await;
    assert_eq!(detail["run"]["abort_reason"], "dry_run");
    assert!(detail["executions"]
        .as_array()
        .unwrap()
        .iter()
        .all(|e| e["execution_kind"] == "skipped" && e["provider_output"].is_null()));
}

#[tokio::test]
async fn unknown_operation_and_bad_mode_are_rejected() {
    let state = test_state().await;
    for body in [
        json!({ "mode": "offline", "operations": ["prescribe"] }),
        json!({ "mode": "turbo" }),
    ] {
        let (s, v) = send(&state, "POST", ADMIN, &format!("{BASE}/runs"), Some(body)).await;
        assert_eq!(s, StatusCode::BAD_REQUEST, "{v}");
    }
    let (s, _) = send(
        &state,
        "GET",
        ADMIN,
        &format!("{BASE}/runs/{}", uuid::Uuid::nil()),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// Allow audits recorded under the `quality` purpose: only the audit section
/// below sends it, so concurrent tests (all `operations`) cannot skew counts.
async fn allow_audits(pool: &PgPool, action: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT COUNT(*) FROM audit_events
         WHERE action = $1 AND decision = 'allow' AND purpose_of_use = 'quality'",
    )
    .bind(action)
    .fetch_one(pool)
    .await
    .unwrap()
}

/// Duplicate a row of `table` with the JSON overrides applied, bypassing no
/// constraint: the copy goes through the same INSERT path as production.
async fn clone_row(pool: &PgPool, table: &str, id: Uuid, overrides: Value) -> Uuid {
    let new_id = Uuid::now_v7();
    let mut overrides = overrides;
    overrides["id"] = json!(new_id);
    sqlx::query(&format!(
        "INSERT INTO {table} SELECT (jsonb_populate_record(NULL::{table}, to_jsonb(t) || $2)).* \
         FROM {table} t WHERE t.id = $1"
    ))
    .bind(id)
    .bind(overrides)
    .execute(pool)
    .await
    .unwrap();
    new_id
}

#[tokio::test]
async fn review_rubric_follows_the_run_audit_follows_the_mutation_and_failures_are_not_cached() {
    let state = test_state().await;
    let (s, created) = send(
        &state,
        "POST",
        ADMIN,
        &format!("{BASE}/runs"),
        Some(json!({ "mode": "offline", "operations": ["risk_summary"] })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{created}");
    let run_id: Uuid = created["run_id"].as_str().unwrap().parse().unwrap();
    let (execution_id, suite_id): (Uuid, Uuid) = sqlx::query_as(
        "SELECT e.id, r.suite_id FROM dmind_eval_executions e
         JOIN dmind_eval_runs r ON r.id = e.run_id
         WHERE e.run_id = $1 AND e.provider_output IS NOT NULL LIMIT 1",
    )
    .bind(run_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();

    // A run scored by an older rubric: reviews must carry that rubric, not
    // the one the server compiles today.
    let legacy_suite = clone_row(
        &state.pool,
        "dmind_eval_suites",
        suite_id,
        json!({ "suite_version": format!("0.0.1-legacy-{}", Uuid::now_v7()), "rubric_version": "dmind-eval-rubric.v0-legacy" }),
    )
    .await;
    let legacy_run = clone_row(
        &state.pool,
        "dmind_eval_runs",
        run_id,
        json!({ "suite_id": legacy_suite }),
    )
    .await;
    let legacy_execution = clone_row(
        &state.pool,
        "dmind_eval_executions",
        execution_id,
        json!({ "run_id": legacy_run }),
    )
    .await;
    let (s, review) = send(
        &state,
        "POST",
        ADMIN,
        &format!("{BASE}/executions/{legacy_execution}/reviews"),
        Some(json!({ "decision": "accept_result", "comment": "" })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{review}");
    assert_eq!(review["rubric_version"], "dmind-eval-rubric.v0-legacy");
    let (stored,): (String,) =
        sqlx::query_as("SELECT rubric_version FROM dmind_eval_reviews WHERE execution_id = $1")
            .bind(legacy_execution)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(stored, "dmind-eval-rubric.v0-legacy");

    // A rejected mutation leaves no "allow" audit row for the manage action.
    let before = allow_audits(&state.pool, "dmind_eval.manage").await;
    let (s, _) = send_for(
        &state,
        "POST",
        ADMIN,
        &format!("{BASE}/executions/{execution_id}/reviews"),
        Some(json!({ "decision": "approve_everything", "comment": "" })),
        "quality",
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    let (s, _) = send_for(
        &state,
        "POST",
        ADMIN,
        &format!("{BASE}/executions/{}/reviews", Uuid::now_v7()),
        Some(json!({ "decision": "accept_result", "comment": "" })),
        "quality",
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
    let (s, _) = send_for(
        &state,
        "POST",
        ADMIN,
        &format!("{BASE}/runs"),
        Some(json!({ "mode": "offline", "operations": ["prescribe"] })),
        "quality",
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
    assert_eq!(allow_audits(&state.pool, "dmind_eval.manage").await, before);
    let (s, _) = send_for(
        &state,
        "POST",
        ADMIN,
        &format!("{BASE}/executions/{execution_id}/reviews"),
        Some(json!({ "decision": "accept_result", "comment": "" })),
        "quality",
    )
    .await;
    assert_eq!(s, StatusCode::CREATED);
    assert_eq!(
        allow_audits(&state.pool, "dmind_eval.manage").await,
        before + 1
    );

    // A failed provider output is never replayed into a later run.
    let (fingerprint_hash,): (String,) = sqlx::query_as(
        "SELECT f.config_hash FROM dmind_eval_runs r
         JOIN dmind_eval_provider_fingerprints f ON f.id = r.fingerprint_id WHERE r.id = $1",
    )
    .bind(run_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let (tenant_id,): (Uuid,) =
        sqlx::query_as("SELECT tenant_id FROM dmind_eval_runs WHERE id = $1")
            .bind(run_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let (input_hash,): (String,) =
        sqlx::query_as("SELECT input_hash FROM dmind_eval_executions WHERE id = $1")
            .bind(execution_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let failed_run = clone_row(&state.pool, "dmind_eval_runs", run_id, json!({})).await;
    clone_row(
        &state.pool,
        "dmind_eval_executions",
        execution_id,
        json!({ "run_id": failed_run, "status": "failed", "provider_output": null, "failure_code": "schema_invalid" }),
    )
    .await;
    let cache =
        wellos_server::eval::store::cached_executions(&state.pool, tenant_id, &fingerprint_hash)
            .await
            .unwrap();
    let same_input: Vec<_> = cache
        .iter()
        .filter(|c| c.input_hash == input_hash)
        .collect();
    assert_eq!(same_input.len(), 1, "failed duplicate must not be cached");
    assert!(same_input[0].provider_output.is_some());
    assert!(cache.iter().all(|c| c.provider_output.is_some()));
}

#[tokio::test]
async fn run_listing_pages_by_cursor_in_stable_order() {
    let state = test_state().await;
    let mut created_ids = Vec::new();
    for _ in 0..3 {
        let (s, created) = send(
            &state,
            "POST",
            ADMIN,
            &format!("{BASE}/runs"),
            Some(json!({ "mode": "dry-run", "operations": ["note_draft"] })),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{created}");
        created_ids.push(created["run_id"].as_str().unwrap().to_string());
    }
    let mut seen = Vec::new();
    let mut cursor: Option<String> = None;
    let mut pages = 0;
    loop {
        let path = match &cursor {
            Some(c) => format!("{BASE}/runs?limit=2&cursor={}", urlencoding(c)),
            None => format!("{BASE}/runs?limit=2"),
        };
        let (s, page) = send(&state, "GET", ADMIN, &path, None).await;
        assert_eq!(s, StatusCode::OK, "{page}");
        let runs = page["runs"].as_array().unwrap();
        assert!(runs.len() <= 2);
        seen.extend(runs.iter().map(|r| r["id"].as_str().unwrap().to_string()));
        pages += 1;
        match page["next_cursor"].as_str() {
            Some(c) => cursor = Some(c.to_string()),
            None => break,
        }
        assert!(pages < 1000);
    }
    assert!(pages >= 2);
    let unique: std::collections::BTreeSet<&String> = seen.iter().collect();
    assert_eq!(unique.len(), seen.len(), "no run repeats across pages");
    for id in &created_ids {
        assert!(seen.contains(id), "run {id} unreachable through pagination");
    }
    let (s, _) = send(
        &state,
        "GET",
        ADMIN,
        &format!("{BASE}/runs?cursor=not-a-cursor"),
        None,
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

fn urlencoding(s: &str) -> String {
    s.replace('+', "%2B")
        .replace('|', "%7C")
        .replace(':', "%3A")
}
