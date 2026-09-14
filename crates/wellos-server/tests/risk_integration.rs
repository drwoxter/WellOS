//! Integration tests for Patient 360, the deterministic risk engine
//! persistence, the risk worklist, dMind risk summaries, confirmation of AI
//! suggestions and the governed insurer projection: permissions, purposes,
//! audit events, safety floor and the synthetic demo scenarios.

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
        wellos_server::seeddata::seed(&pool).await.unwrap();
    }
    let gateway = Arc::new(dmind_gateway::fake::FakeProvider::new());
    AppState::new(pool, gateway)
}

async fn call_with(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
    headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut req = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json");
    for (k, v) in headers {
        req = req.header(*k, *v);
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
    call_with(state, method, path, token, body, &[]).await
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

const GARCIA: &str = "dev-dr.garcia";
const NURSE: &str = "dev-nurse.kim";
const LAB: &str = "dev-lab.chen";
const OTHER_TENANT: &str = "dev-dr.sur";
const ADMIN: &str = "dev-admin.silva";

async fn patient_by_identifier(state: &AppState, identifier: &str) -> Uuid {
    let (id,): (Uuid,) = sqlx::query_as("SELECT id FROM patients WHERE identifier = $1")
        .bind(identifier)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    id
}

async fn event_count(state: &AppState, event: &str, needle: &str) -> i64 {
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM outbox_events WHERE event_type = $1 AND resource_refs::text LIKE $2",
    )
    .bind(event)
    .bind(format!("%{needle}%"))
    .fetch_one(&state.pool)
    .await
    .unwrap();
    n
}

#[tokio::test]
async fn patient_360_and_risk_smoke() {
    let state = test_state().await;
    let pid = patient_by_identifier(&state, "SYN-0001").await;
    let (st, body) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{pid}/360"),
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert!(
        body["patient"].is_object() && body["brief"].is_object() && body["diagnostics"].is_object(),
        "{body}"
    );
    assert!(body["responsible_professional"].is_object(), "{body}");
    assert!(body["risk"]["current"].is_object() || body["risk"]["current"].is_null());

    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{pid}/risk/recalculate"),
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let assessment_id = body["current"]["id"].as_str().unwrap().to_string();
    assert_eq!(body["current"]["rules_version"], "risk-rules.v1");

    let (st, body) = call(&state, "GET", "/api/v1/risk/worklist", GARCIA, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert!(body["items"].is_array(), "{body}");

    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{pid}/risk/acknowledge"),
        GARCIA,
        Some(json!({ "assessment_id": assessment_id, "domain": "acute_safety" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");

    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{pid}/risk/summary"),
        GARCIA,
        Some(json!({ "language": "en" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let artifact_id = body["id"].as_str().unwrap().to_string();
    assert_eq!(body["summary"]["output"]["ai_generated"], true);
    assert_eq!(body["summary"]["status"], "awaiting_review");

    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{pid}/risk/summary/{artifact_id}/review"),
        GARCIA,
        Some(json!({ "decision": "approve" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["summary"]["status"], "approved", "{body}");
    let suggestions = body["summary"]["output"]["follow_up_suggestions"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    if !suggestions.is_empty() {
        let (st, body) = call(
            &state,
            "POST",
            &format!("/api/v1/patients/{pid}/risk/summary/{artifact_id}/confirm"),
            GARCIA,
            Some(json!({ "suggestion_index": 0 })),
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{body}");
        assert!(body["task_id"].is_string(), "{body}");
    }

    let (st, body) = call_with(
        &state,
        "GET",
        &format!("/api/v1/patients/{pid}/risk/projection"),
        ADMIN,
        None,
        &[("X-Purpose-Of-Use", "operations")],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert!(body.get("notes").is_none());
    assert!(event_count(&state, "risk.projection.accessed", &pid.to_string()).await >= 1);

    let (st, body) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{pid}/risk"),
        OTHER_TENANT,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{body} {}", code(&body));
    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{pid}/risk"),
        LAB,
        None,
    )
    .await;
    assert_ne!(st, StatusCode::OK);
    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{pid}/risk"),
        NURSE,
        None,
    )
    .await;
    assert!(st == StatusCode::OK || st == StatusCode::FORBIDDEN);
}

async fn user_id(state: &AppState, username: &str) -> Uuid {
    let (id,): (Uuid,) = sqlx::query_as("SELECT id FROM users WHERE username = $1")
        .bind(username)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    id
}

async fn risk(state: &AppState, pid: Uuid, token: &str) -> Value {
    let (st, body) = call(
        state,
        "GET",
        &format!("/api/v1/patients/{pid}/risk"),
        token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    body
}

fn domain<'a>(assessment: &'a Value, name: &str) -> &'a Value {
    assessment["domains"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["domain"] == name)
        .unwrap_or_else(|| panic!("domain {name} missing in {assessment}"))
}

#[tokio::test]
async fn synthetic_scenarios_have_expected_deterministic_levels() {
    let state = test_state().await;

    let lucia = patient_by_identifier(&state, "SYN-0101").await;
    let cur = &risk(&state, lucia, GARCIA).await["current"];
    assert_eq!(cur["overall_level"], "low", "{cur}");
    assert_eq!(cur["rules_version"], "risk-rules.v1");
    for d in cur["domains"].as_array().unwrap() {
        assert_eq!(d["level"], "low", "{d}");
    }

    let ramon = patient_by_identifier(&state, "SYN-0102").await;
    let body = risk(&state, ramon, GARCIA).await;
    let cur = &body["current"];
    assert_eq!(cur["overall_level"], "high", "{cur}");
    assert_eq!(cur["trend"], "worsening", "{cur}");
    let chronic = domain(cur, "chronic_complexity");
    assert_eq!(chronic["level"], "high");
    assert_eq!(chronic["trend"], "worsening", "{chronic}");
    assert!(
        chronic["factors"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["code"] == "multiple_chronic_conditions"
                && f["evidence"].as_array().is_some_and(|e| e.len() >= 3)),
        "{chronic}"
    );
    assert!(body["history"].as_array().unwrap().len() >= 2, "{body}");

    let teresa = patient_by_identifier(&state, "SYN-0103").await;
    let cur = &risk(&state, teresa, GARCIA).await["current"];
    assert_eq!(cur["overall_level"], "critical");
    assert_eq!(cur["safety_floor"], "critical");
    let diag = domain(cur, "diagnostic_result");
    assert_eq!(diag["level"], "critical");
    let f = &diag["factors"][0];
    assert_eq!(f["code"], "critical_result_unreviewed", "{diag}");
    assert_eq!(f["evidence"][0]["record_type"], "observation");
    assert!(f["evidence"][0]["record_id"].is_string());
    assert!(f["detected_at"].is_string() && diag["calculated_at"].is_string());
    assert_eq!(domain(cur, "acute_safety")["level"], "critical");

    let hugo = patient_by_identifier(&state, "SYN-0104").await;
    let cur = &risk(&state, hugo, GARCIA).await["current"];
    assert_eq!(cur["overall_level"], "critical");
    let meds = domain(cur, "medication_allergy_safety");
    assert_eq!(meds["level"], "critical");
    assert_eq!(meds["factors"][0]["code"], "medication_allergy_conflict");
    let kinds: Vec<&str> = meds["factors"][0]["evidence"]
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["record_type"].as_str().unwrap())
        .collect();
    assert!(
        kinds.contains(&"medication") && kinds.contains(&"allergy"),
        "{meds}"
    );
    assert_eq!(
        domain(cur, "diagnostic_result")["level"],
        "insufficient_data",
        "no laboratory results yet"
    );

    let nora = patient_by_identifier(&state, "SYN-0105").await;
    let cur = &risk(&state, nora, GARCIA).await["current"];
    assert_eq!(cur["overall_level"], "high");
    let prev = domain(cur, "preventive_care");
    assert_eq!(prev["level"], "high");
    let codes: Vec<&str> = prev["factors"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["code"].as_str().unwrap())
        .collect();
    assert!(codes.contains(&"hba1c_overdue"), "{codes:?}");
    assert!(!prev["stale_data"].as_array().unwrap().is_empty(), "{prev}");
    assert!(
        !prev["missing_data"].as_array().unwrap().is_empty(),
        "{prev}"
    );
    assert!(
        !domain(cur, "access_utilization")["stale_data"]
            .as_array()
            .unwrap()
            .is_empty(),
        "stale follow-up must be flagged"
    );

    let ivan = patient_by_identifier(&state, "SYN-0106").await;
    let cur = &risk(&state, ivan, GARCIA).await["current"];
    assert_eq!(cur["overall_level"], "insufficient_data");
    assert_eq!(cur["trend"], "unknown");
    for d in cur["domains"].as_array().unwrap() {
        assert_eq!(d["level"], "insufficient_data", "{d}");
        assert!(!d["missing_data"].as_array().unwrap().is_empty(), "{d}");
    }
}

#[tokio::test]
async fn ai_summary_never_lowers_deterministic_critical_floor() {
    let state = test_state().await;
    let teresa = patient_by_identifier(&state, "SYN-0103").await;
    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{teresa}/risk/summary"),
        GARCIA,
        Some(json!({ "language": "es" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let out = &body["summary"]["output"];
    assert_eq!(out["schema_version"], "risk-summary.v1");
    assert_eq!(out["ai_generated"], true);
    assert_eq!(out["overall_level"], "critical");
    assert_eq!(out["safety_floor"], "critical");
    assert_eq!(out["rules_version"], "risk-rules.v1");
    assert!(!out["cited_sources"].as_array().unwrap().is_empty());
    assert!(out["limitations"]
        .as_array()
        .unwrap()
        .iter()
        .any(|l| l.as_str().unwrap().contains("diagnóstico")));
    assert_eq!(body["summary"]["prompt_version"], "risk-summary-prompt.v1");
    assert!(body["summary"]["model"].is_string() && body["summary"]["generated_at"].is_string());
    let artifact_id = body["id"].as_str().unwrap().to_string();
    assert!(event_count(&state, "ai.artifact.generated", &artifact_id).await >= 1);

    // Acknowledging a critical domain records the review but does not lower it.
    let assessment_id = risk(&state, teresa, GARCIA).await["current"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{teresa}/risk/acknowledge"),
        GARCIA,
        Some(json!({ "assessment_id": assessment_id, "domain": "diagnostic_result" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let diag = domain(&body["current"], "diagnostic_result");
    assert_eq!(diag["level"], "critical");
    assert_eq!(diag["review"]["status"], "acknowledged", "{diag}");
    assert!(
        diag["review"]["reviewer"].is_string() || diag["review"]["reviewer_id"].is_string(),
        "{diag}"
    );
    assert!(event_count(&state, "risk.item.acknowledged", &teresa.to_string()).await >= 1);
}

#[tokio::test]
async fn confirmation_is_required_before_a_suggestion_becomes_work() {
    let state = test_state().await;
    let hugo = patient_by_identifier(&state, "SYN-0104").await;
    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{hugo}/risk/summary"),
        GARCIA,
        Some(json!({ "language": "en" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let artifact_id = body["id"].as_str().unwrap().to_string();
    let suggestions = body["summary"]["output"]["follow_up_suggestions"]
        .as_array()
        .cloned()
        .unwrap();
    assert!(!suggestions.is_empty(), "{body}");
    assert!(suggestions
        .iter()
        .all(|s| s["requires_confirmation"] == true));

    // No task may be created from an unreviewed proposal.
    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{hugo}/risk/summary/{artifact_id}/confirm"),
        GARCIA,
        Some(json!({ "suggestion_index": 0 })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
    assert_eq!(code(&body), "invalid_artifact_state");
    let (tasks_before,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM follow_up_tasks WHERE patient_id = $1 AND source = 'risk_suggestion'",
    )
    .bind(hugo)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(tasks_before, 0);

    // A nurse without the treatment relationship cannot review; the owner can.
    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{hugo}/risk/summary/{artifact_id}/review"),
        LAB,
        Some(json!({ "decision": "approve" })),
    )
    .await;
    assert_ne!(st, StatusCode::OK, "{body}");
    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{hugo}/risk/summary/{artifact_id}/review"),
        GARCIA,
        Some(json!({ "decision": "approve", "note": "Reviewed with the patient (synthetic)" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["summary"]["status"], "approved");
    assert!(event_count(&state, "ai.artifact.reviewed", &artifact_id).await >= 1);

    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{hugo}/risk/summary/{artifact_id}/confirm"),
        GARCIA,
        Some(json!({ "suggestion_index": 0, "priority": "urgent", "due_in_days": 2 })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let task_id = body["task_id"].as_str().unwrap().to_string();
    let (source, created_by, ai_artifact_id): (String, Option<Uuid>, Option<Uuid>) =
        sqlx::query_as(
            "SELECT source, created_by, ai_artifact_id FROM follow_up_tasks WHERE id = $1",
        )
        .bind(Uuid::parse_str(&task_id).unwrap())
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(source, "risk_suggestion");
    assert_eq!(created_by, Some(user_id(&state, "dr.garcia").await));
    assert_eq!(ai_artifact_id, Some(Uuid::parse_str(&artifact_id).unwrap()));
    assert!(event_count(&state, "risk.suggestion.confirmed", &task_id).await >= 1);

    // Confirming the same suggestion twice does not duplicate work.
    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{hugo}/risk/summary/{artifact_id}/confirm"),
        GARCIA,
        Some(json!({ "suggestion_index": 0 })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");
    assert_eq!(code(&body), "already_confirmed");

    // Source clinical records were not altered by the AI workflow.
    let (meds,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM medications WHERE patient_id = $1 AND status = 'active'",
    )
    .bind(hugo)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(meds, 1);

    // The confirmed task appears in Patient 360 pending work.
    let (st, body) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{hugo}/360"),
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert!(body.to_string().contains(&task_id), "{body}");
}

#[tokio::test]
async fn worklist_orders_critical_first_and_filters() {
    let state = test_state().await;
    let (st, body) = call(&state, "GET", "/api/v1/risk/worklist", GARCIA, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let items = body["items"].as_array().unwrap();
    assert!(items.len() >= 4, "{body}");
    let rank = |l: &str| match l {
        "critical" => 0,
        "high" => 1,
        "moderate" => 2,
        "insufficient_data" => 3,
        _ => 4,
    };
    let levels: Vec<&str> = items
        .iter()
        .map(|i| i["overall_level"].as_str().unwrap())
        .collect();
    assert!(
        levels.windows(2).all(|w| rank(w[0]) <= rank(w[1])),
        "not sorted: {levels:?}"
    );
    assert_eq!(levels[0], "critical");
    for item in items {
        assert!(item["explained"].is_array(), "{item}");
        assert!(item["patient"]["id"].is_string(), "{item}");
    }

    let (st, body) = call(
        &state,
        "GET",
        "/api/v1/risk/worklist?domain=medication_allergy_safety",
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let ids: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["patient"]["identifier"].as_str().unwrap())
        .collect();
    assert!(ids.contains(&"SYN-0104"), "{ids:?}");
    assert!(!ids.contains(&"SYN-0103"), "{ids:?}");

    let (st, body) = call(
        &state,
        "GET",
        "/api/v1/risk/worklist?trend=worsening",
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let ids: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["patient"]["identifier"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["SYN-0102"], "{body}");

    let (st, body) = call(
        &state,
        "GET",
        "/api/v1/risk/worklist?trend=sideways",
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");

    // Other tenant sees none of these patients.
    let (st, body) = call(&state, "GET", "/api/v1/risk/worklist", OTHER_TENANT, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert!(
        !body.to_string().contains("Riskdemo"),
        "tenant isolation broken: {body}"
    );
}

#[tokio::test]
async fn assign_and_review_are_audited_and_version_bound() {
    let state = test_state().await;
    let ramon = patient_by_identifier(&state, "SYN-0102").await;
    let cur = risk(&state, ramon, GARCIA).await;
    let assessment_id = cur["current"]["id"].as_str().unwrap().to_string();
    let nurse = user_id(&state, "nurse.kim").await;

    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{ramon}/risk/assign"),
        GARCIA,
        Some(json!({ "assignee_user_id": nurse })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(
        body["follow_up_owner"]["user_id"],
        nurse.to_string(),
        "{body}"
    );
    assert!(event_count(&state, "risk.item.assigned", &ramon.to_string()).await >= 1);

    // Assigning to a user of another tenant is refused.
    let stranger = user_id(&state, "dr.sur").await;
    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{ramon}/risk/assign"),
        GARCIA,
        Some(json!({ "assignee_user_id": stranger })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{body}");

    // A review must reference the assessment that was displayed.
    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{ramon}/risk/review"),
        GARCIA,
        Some(json!({ "assessment_id": Uuid::now_v7(), "domain": "chronic_complexity" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{body}");

    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{ramon}/risk/review"),
        GARCIA,
        Some(json!({
            "assessment_id": assessment_id,
            "domain": "chronic_complexity",
            "note": "Discussed at the multidisciplinary meeting (synthetic)"
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let d = domain(&body["current"], "chronic_complexity");
    assert_eq!(d["review"]["status"], "reviewed", "{d}");
    assert_eq!(d["level"], "high", "review never changes the level");
    assert!(event_count(&state, "risk.item.reviewed", &ramon.to_string()).await >= 1);

    // The worklist reflects the review and the assignee filter.
    let (st, body) = call(
        &state,
        "GET",
        &format!("/api/v1/risk/worklist?assignee={nurse}"),
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let ids: Vec<&str> = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["patient"]["identifier"].as_str().unwrap())
        .collect();
    assert_eq!(ids, vec!["SYN-0102"], "{body}");

    // A recalculation that changes nothing keeps the review; the worklist
    // still shows the patient at the top of the high band.
    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{ramon}/risk/recalculate"),
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["current"]["overall_level"], "high");
    assert_ne!(
        body["current"]["id"], assessment_id,
        "snapshots are append-only"
    );
    let (n,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM risk_assessments WHERE patient_id = $1")
            .bind(ramon)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(n >= 3, "history preserved: {n}");
}

#[tokio::test]
async fn permissions_and_purposes_are_enforced() {
    let state = test_state().await;
    let lucia = patient_by_identifier(&state, "SYN-0101").await;
    let path = format!("/api/v1/patients/{lucia}/risk/projection");

    // The projection is an operations-purpose read: treatment purpose is refused.
    let (st, body) = call_with(
        &state,
        "GET",
        &path,
        ADMIN,
        None,
        &[("X-Purpose-Of-Use", "treatment")],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    // Physicians and laboratory staff have no projection permission at all.
    let (st, body) = call_with(
        &state,
        "GET",
        &path,
        GARCIA,
        None,
        &[("X-Purpose-Of-Use", "operations")],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    let (st, body) = call_with(
        &state,
        "GET",
        &path,
        LAB,
        None,
        &[("X-Purpose-Of-Use", "operations")],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    // Another tenant cannot even learn that the patient exists.
    let (st, body) = call_with(
        &state,
        "GET",
        &path,
        OTHER_TENANT,
        None,
        &[("X-Purpose-Of-Use", "operations")],
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{body}");

    let before = event_count(&state, "risk.projection.accessed", &lucia.to_string()).await;
    let (st, body) = call_with(
        &state,
        "GET",
        &path,
        ADMIN,
        None,
        &[("X-Purpose-Of-Use", "operations")],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["schema"], "risk-projection.v1");
    assert_eq!(body["authorization"]["consent_status"], "active");
    assert_eq!(body["authorization"]["shared"], true);
    assert_eq!(body["risk"]["overall"]["level"], "low", "{body}");
    assert_eq!(body["risk"]["rules_version"], "risk-rules.v1");
    let text = body.to_string();
    for forbidden in [
        "assessment\":\"",
        "plan\":",
        "history_present_illness",
        "Riskdemo",
        "synthetic)",
    ] {
        assert!(
            !text.contains(forbidden),
            "projection leaked clinical text: {forbidden}"
        );
    }
    assert!(body["non_goals"]
        .as_array()
        .unwrap()
        .iter()
        .any(|n| n == "no_automated_denial"));
    assert_eq!(
        event_count(&state, "risk.projection.accessed", &lucia.to_string()).await,
        before + 1
    );

    // Without a sharing consent nothing is shared, but the attempt is audited.
    let ramon = patient_by_identifier(&state, "SYN-0102").await;
    let before = event_count(&state, "risk.projection.accessed", &ramon.to_string()).await;
    let (st, body) = call_with(
        &state,
        "GET",
        &format!("/api/v1/patients/{ramon}/risk/projection"),
        ADMIN,
        None,
        &[("X-Purpose-Of-Use", "operations")],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert_eq!(body["authorization"]["shared"], false);
    assert_eq!(body["authorization"]["consent_status"], "not_recorded");
    assert!(body["risk"].is_null());
    assert_eq!(
        event_count(&state, "risk.projection.accessed", &ramon.to_string()).await,
        before + 1
    );

    // Reviewing risk is a treatment action: the quality purpose may read but not review.
    let cur = risk(&state, ramon, GARCIA).await;
    let assessment_id = cur["current"]["id"].as_str().unwrap().to_string();
    let (st, body) = call_with(
        &state,
        "GET",
        &format!("/api/v1/patients/{ramon}/risk"),
        GARCIA,
        None,
        &[("X-Purpose-Of-Use", "quality")],
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    let (st, body) = call_with(
        &state,
        "POST",
        &format!("/api/v1/patients/{ramon}/risk/review"),
        GARCIA,
        Some(json!({ "assessment_id": assessment_id, "domain": "overall" })),
        &[("X-Purpose-Of-Use", "quality")],
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{body}");
    // Laboratory staff can neither read nor recalculate risk.
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{ramon}/risk/recalculate"),
        LAB,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(&state, "GET", "/api/v1/risk/worklist", LAB, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // Patient 360 requires the chart permission and a care relationship.
    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{ramon}/360"),
        LAB,
        None,
    )
    .await;
    assert_ne!(st, StatusCode::OK);
    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{ramon}/360"),
        OTHER_TENANT,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn confirmed_result_review_recalculates_risk_in_the_same_transaction() {
    let state = test_state().await;
    // SYN-0003 carries the foundation demo's critical potassium awaiting review.
    let pid = patient_by_identifier(&state, "SYN-0003").await;
    let (st, before) = call(
        &state,
        "POST",
        &format!("/api/v1/patients/{pid}/risk/recalculate"),
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{before}");
    let diag = domain(&before["current"], "diagnostic_result");
    assert_eq!(diag["level"], "critical", "{diag}");
    assert_eq!(diag["factors"][0]["code"], "critical_result_unreviewed");
    let calc_before = event_count(&state, "risk.assessment.calculated", &pid.to_string()).await;

    let (sr_id, version): (Uuid, i64) = sqlx::query_as(
        "SELECT id, version FROM service_requests
         WHERE patient_id = $1 AND loop_state = 'received' ORDER BY created_at DESC LIMIT 1",
    )
    .bind(pid)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let (st, body) = call(
        &state,
        "POST",
        &format!("/api/v1/service-requests/{sr_id}/review"),
        GARCIA,
        Some(json!({ "version": version, "note": "Reviewed critical potassium (synthetic)" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");

    let after = risk(&state, pid, GARCIA).await;
    assert_ne!(after["current"]["id"], before["current"]["id"]);
    let diag = domain(&after["current"], "diagnostic_result");
    assert_eq!(diag["level"], "high", "reviewed but still open: {diag}");
    assert_eq!(diag["factors"][0]["code"], "critical_result_open_loop");
    assert_eq!(diag["trend"], "improving", "{diag}");
    assert_eq!(
        event_count(&state, "risk.assessment.calculated", &pid.to_string()).await,
        calc_before + 1
    );
}
