//! Integration tests for patient access, arrival, triage, care-team routing,
//! internal alerts and the consultation handoff: the visit state machine,
//! optimistic versions, tenant/facility isolation, deterministic safety-rule
//! precedence over dMind and humans, explicit proposal review, alert
//! visibility, duplicate-encounter prevention and the golden path.

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
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
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

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

const REG: &str = "dev-reg.rivera";
const NURSE: &str = "dev-nurse.kim";
const GARCIA: &str = "dev-dr.garcia";
const LOPEZ: &str = "dev-dr.lopez";
const ANNEX: &str = "dev-dr.annex";
const OTHER_TENANT: &str = "dev-dr.sur";
const LAB: &str = "dev-lab.chen";

/// Register a fresh synthetic patient at the tenant's main facility.
async fn register_patient(state: &AppState) -> String {
    let (st, meta) = call(state, "GET", "/api/v1/meta/tenant", REG, None).await;
    assert_eq!(st, StatusCode::OK);
    let facility = meta["facilities"][0]["id"].as_str().unwrap().to_string();
    let (st, patient) = call(
        state,
        "POST",
        "/api/v1/patients",
        REG,
        Some(json!({
            "facility_id": facility,
            "family_name": "Access",
            "given_name": "Synthetic",
            "birth_date": "1975-06-15",
            "sex": "female",
            "identifier": uniq("MRN-ACC"),
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{patient}");
    patient["id"].as_str().unwrap().to_string()
}

/// Create a visit for a fresh patient; returns (patient_id, visit_id, version).
async fn create_visit(
    state: &AppState,
    arrival_kind: &str,
    service: &str,
) -> (String, String, i64) {
    let patient = register_patient(state).await;
    let mut body = json!({
        "patient_id": patient,
        "arrival_kind": arrival_kind,
        "service": service,
        "reason": "Synthetic reason for attendance",
    });
    if arrival_kind == "scheduled" {
        body["scheduled_at"] = json!(chrono::Utc::now() + chrono::Duration::hours(2));
    }
    let (st, visit) = call(state, "POST", "/api/v1/visits", REG, Some(body)).await;
    assert_eq!(st, StatusCode::OK, "{visit}");
    let id = visit["id"].as_str().unwrap().to_string();
    let version = visit["version"].as_i64().unwrap();
    (patient, id, version)
}

async fn detail(state: &AppState, visit: &str, token: &str) -> (StatusCode, Value) {
    call(
        state,
        "GET",
        &format!("/api/v1/visits/{visit}"),
        token,
        None,
    )
    .await
}

async fn version_of(state: &AppState, visit: &str) -> i64 {
    let (st, d) = detail(state, visit, NURSE).await;
    assert_eq!(st, StatusCode::OK, "{d}");
    d["version"].as_i64().unwrap()
}

/// Save a triage assessment at the current version; returns the response.
async fn save_triage(state: &AppState, visit: &str, extra: Value) -> (StatusCode, Value) {
    let version = version_of(state, visit).await;
    let mut body = json!({ "version": version, "reason": "Triage reason" });
    if let (Value::Object(b), Value::Object(e)) = (&mut body, extra) {
        for (k, v) in e {
            b.insert(k, v);
        }
    }
    call(
        state,
        "POST",
        &format!("/api/v1/visits/{visit}/triage"),
        NURSE,
        Some(body),
    )
    .await
}

async fn propose(state: &AppState, visit: &str) -> (StatusCode, Value) {
    call(
        state,
        "POST",
        &format!("/api/v1/visits/{visit}/triage/proposal"),
        NURSE,
        Some(json!({ "language": "en" })),
    )
    .await
}

async fn review(state: &AppState, visit: &str, artifact: &str, body: Value) -> (StatusCode, Value) {
    let version = version_of(state, visit).await;
    let mut body = body;
    body["version"] = json!(version);
    call(
        state,
        "POST",
        &format!("/api/v1/visits/{visit}/triage/proposal/{artifact}/review"),
        NURSE,
        Some(body),
    )
    .await
}

async fn complete_triage(state: &AppState, visit: &str, extra: Value) -> (StatusCode, Value) {
    let version = version_of(state, visit).await;
    let mut body = json!({
        "version": version,
        "priority": "standard",
        "requested_service": "general_medicine",
        "handoff_summary": "Synthetic handoff summary",
    });
    if let (Value::Object(b), Value::Object(e)) = (&mut body, extra) {
        for (k, v) in e {
            b.insert(k, v);
        }
    }
    call(
        state,
        "POST",
        &format!("/api/v1/visits/{visit}/triage/complete"),
        NURSE,
        Some(body),
    )
    .await
}

async fn start_consultation(state: &AppState, visit: &str, token: &str) -> (StatusCode, Value) {
    call(
        state,
        "POST",
        &format!("/api/v1/visits/{visit}/start-consultation"),
        token,
        Some(json!({})),
    )
    .await
}

async fn alerts_for(state: &AppState, token: &str, visit: &str) -> Vec<Value> {
    let (st, body) = call(state, "GET", "/api/v1/alerts", token, None).await;
    assert_eq!(st, StatusCode::OK, "{body}");
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["visit_id"] == json!(visit))
        .cloned()
        .collect()
}

async fn user_id(state: &AppState, username: &str) -> String {
    let id: uuid::Uuid = sqlx::query_scalar("SELECT id FROM users WHERE username = $1")
        .bind(username)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    id.to_string()
}

async fn list_ids(state: &AppState, token: &str, view: &str) -> Vec<String> {
    let (st, body) = call(
        state,
        "GET",
        &format!("/api/v1/visits?view={view}"),
        token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    body["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap().to_string())
        .collect()
}

// ---------------------------------------------------------------------------
// Golden path
// ---------------------------------------------------------------------------

#[tokio::test]
async fn golden_path_walk_in_to_completed_consultation() {
    let (state, _) = test_state().await;
    let (patient, visit, version) = create_visit(&state, "walk_in", "general_medicine").await;
    assert_eq!(version, 1);

    // Arrival routed the patient to the service queue; nothing is directed
    // at an individual and no alert is raised for an ordinary walk-in.
    let (st, d) = detail(&state, &visit, NURSE).await;
    assert_eq!(st, StatusCode::OK, "{d}");
    assert_eq!(d["status"], json!("arrived"));
    assert_eq!(d["assignment"]["kind"], json!("queue"));
    assert_eq!(d["assignment"]["code"], json!("general_medicine"));
    assert_eq!(d["capabilities"]["can_triage"], json!(true));
    assert!(alerts_for(&state, GARCIA, &visit).await.is_empty());
    assert!(list_ids(&state, NURSE, "triage").await.contains(&visit));

    // Nurse saves triage with vitals: SpO2 92 % sets an urgent floor.
    let (st, t) = save_triage(
        &state,
        &visit,
        json!({
            "concerns": ["cough", "shortness_of_breath"],
            "onset": "2 days",
            "vitals": { "spo2_percent": 92, "heart_rate_bpm": 98, "systolic_mmhg": 128, "diastolic_mmhg": 82 },
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["status"], json!("triage_in_progress"));
    assert_eq!(t["safety_floor"], json!("urgent"));
    assert_eq!(t["triage_version"], json!(1));
    assert!(t["vital_signs_id"].is_string());

    // dMind proposes (A2, awaiting review) and cannot sit below the floor.
    let (st, p) = propose(&state, &visit).await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(p["status"], json!("awaiting_review"));
    assert_eq!(p["triage_version"], json!(1));
    assert!(matches!(
        p["output"]["proposed_priority"].as_str(),
        Some("urgent") | Some("immediate")
    ));
    assert_eq!(p["output"]["safety_floor"], json!("urgent"));
    assert!(!p["citations"].as_array().unwrap().is_empty());
    assert!(!p["limitations"].as_array().unwrap().is_empty());
    let artifact = p["id"].as_str().unwrap().to_string();

    // Completing triage before deciding on the proposal is refused.
    let (st, err) = complete_triage(&state, &visit, json!({ "priority": "urgent" })).await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "proposal_awaiting_review");

    // Nurse accepts the proposal: an explicit human decision.
    let (st, r) = review(&state, &visit, &artifact, json!({ "decision": "accept" })).await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["status"], json!("approved"));
    let (_, d) = detail(&state, &visit, NURSE).await;
    assert_eq!(d["triage"]["ai_decision"], json!("accept"));
    assert_eq!(d["triage"]["ai_artifact_id"], json!(artifact));
    assert_eq!(d["proposal"]["review_decision"], json!("approved"));

    // Triage completes with an explicit professional: alert directed to him.
    let garcia = user_id(&state, "dr.garcia").await;
    let (st, c) = complete_triage(
        &state,
        &visit,
        json!({ "priority": "urgent", "assignee_user_id": garcia }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["status"], json!("ready_for_consultation"));
    assert!(c["alert_id"].is_string());

    let mine = alerts_for(&state, GARCIA, &visit).await;
    assert_eq!(mine.len(), 1, "{mine:?}");
    assert_eq!(mine[0]["kind"], json!("patient_ready"));
    assert_eq!(mine[0]["priority"], json!("urgent"));
    assert_eq!(mine[0]["target"]["kind"], json!("professional"));
    assert_eq!(
        mine[0]["visit"]["handoff_summary"],
        json!("Synthetic handoff summary")
    );
    assert!(alerts_for(&state, LOPEZ, &visit).await.is_empty());
    assert!(list_ids(&state, GARCIA, "ready").await.contains(&visit));
    let (_, d) = detail(&state, &visit, GARCIA).await;
    assert_eq!(d["assignment"]["kind"], json!("professional"));
    assert_eq!(d["capabilities"]["can_start_consultation"], json!(true));

    // Start consultation → encounter created, visit in consultation, alert
    // resolved.
    let (st, s) = start_consultation(&state, &visit, GARCIA).await;
    assert_eq!(st, StatusCode::OK, "{s}");
    assert_eq!(s["status"], json!("in_consultation"));
    assert_eq!(s["resumed"], json!(false));
    let encounter = s["encounter_id"].as_str().unwrap().to_string();
    assert!(alerts_for(&state, GARCIA, &visit).await.is_empty());

    // Resume from the same card: no duplicate encounter.
    let (st, s2) = start_consultation(&state, &visit, GARCIA).await;
    assert_eq!(st, StatusCode::OK, "{s2}");
    assert_eq!(s2["resumed"], json!(true));
    assert_eq!(s2["encounter_id"], json!(encounter));
    let encounters: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM encounters WHERE patient_id = $1::uuid AND status = 'in_progress'",
    )
    .bind(uuid::Uuid::parse_str(&patient).unwrap())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(encounters, 1);

    // The encounter workspace shows the handoff; signing completes the visit.
    let (st, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{encounter}"),
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ws}");
    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{encounter}/note"),
        GARCIA,
        Some(json!({ "reason_for_encounter": "Cough and breathlessness", "assessment": "Synthetic assessment" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");
    let (st, signed) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{encounter}/sign"),
        GARCIA,
        Some(json!({ "version": 1 })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{signed}");
    let (_, d) = detail(&state, &visit, GARCIA).await;
    assert_eq!(d["status"], json!("completed"));
    assert!(list_ids(&state, GARCIA, "closed").await.contains(&visit));

    // Every step left an audit trail with known events.
    let events: Vec<String> = sqlx::query_scalar(
        "SELECT event_type FROM outbox_events WHERE resource_refs->>'visit_id' = $1 ORDER BY occurred_at, id",
    )
    .bind(&visit)
    .fetch_all(&state.pool)
    .await
    .unwrap();
    for expected in [
        "visit.created",
        "visit.arrived",
        "visit.triage.saved",
        "ai.artifact.requested",
        "ai.artifact.reviewed",
        "visit.triage.completed",
        "visit.consultation.started",
        "visit.completed",
    ] {
        assert!(
            events.iter().any(|e| e == expected),
            "missing {expected} in {events:?}"
        );
    }
}

// ---------------------------------------------------------------------------
// State machine
// ---------------------------------------------------------------------------

#[tokio::test]
async fn scheduled_visit_lifecycle_and_invalid_transitions() {
    let (state, _) = test_state().await;
    let (_, visit, version) = create_visit(&state, "scheduled", "general_medicine").await;
    let path = |action: &str| format!("/api/v1/visits/{visit}/{action}");

    // Scheduled visits are visible on the access list, not the triage list.
    assert!(list_ids(&state, REG, "access").await.contains(&visit));
    assert!(!list_ids(&state, NURSE, "triage").await.contains(&visit));

    // Triage cannot start before arrival.
    let (st, err) = save_triage(&state, &visit, json!({})).await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "visit_not_triageable");

    // A stale version is rejected without changing anything.
    let (st, err) = call(
        &state,
        "POST",
        &path("arrive"),
        REG,
        Some(json!({ "version": version + 5 })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "stale_version");

    let (st, a) = call(
        &state,
        "POST",
        &path("arrive"),
        REG,
        Some(json!({ "version": version })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    assert_eq!(a["status"], json!("arrived"));
    let v2 = a["version"].as_i64().unwrap();
    assert!(v2 > version);

    // Arriving twice and marking an arrived patient as a no-show are invalid.
    let (st, err) = call(
        &state,
        "POST",
        &path("arrive"),
        REG,
        Some(json!({ "version": v2 })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "invalid_visit_transition");
    let (st, err) = call(
        &state,
        "POST",
        &path("no-show"),
        REG,
        Some(json!({ "version": v2 })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "invalid_visit_transition");

    // Consultation cannot start before triage is complete.
    let (st, err) = start_consultation(&state, &visit, GARCIA).await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "invalid_visit_transition");

    // Cancel with a reason; the visit closes and leaves the worklists.
    let (st, c) = call(
        &state,
        "POST",
        &path("cancel"),
        REG,
        Some(json!({ "version": v2, "reason": "Patient left" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["status"], json!("cancelled"));
    assert!(!list_ids(&state, REG, "access").await.contains(&visit));
    let (closed_reason, closed_at): (Option<String>, Option<chrono::DateTime<chrono::Utc>>) =
        sqlx::query_as("SELECT closed_reason, closed_at FROM visits WHERE id = $1::uuid")
            .bind(uuid::Uuid::parse_str(&visit).unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(closed_reason.as_deref(), Some("Patient left"));
    assert!(closed_at.is_some());

    // Terminal: no further transitions.
    let (st, err) = save_triage(&state, &visit, json!({})).await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "visit_not_triageable");
    let (st, err) = call(
        &state,
        "POST",
        &path("cancel"),
        REG,
        Some(json!({ "version": c["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "invalid_visit_transition");
}

#[tokio::test]
async fn no_show_and_one_open_visit_per_patient() {
    let (state, _) = test_state().await;
    let (patient, scheduled, version) = create_visit(&state, "scheduled", "general_medicine").await;
    let (st, n) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{scheduled}/no-show"),
        REG,
        Some(json!({ "version": version })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{n}");
    assert_eq!(n["status"], json!("no_show"));

    // The same patient later walks in: one open visit is allowed …
    let (st, walk) = call(
        &state,
        "POST",
        "/api/v1/visits",
        REG,
        Some(json!({ "patient_id": patient, "arrival_kind": "walk_in", "service": "general_medicine" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{walk}");
    assert_eq!(walk["status"], json!("arrived"));

    // … a second arrival is refused with a clear message …
    let (st, err) = call(
        &state,
        "POST",
        "/api/v1/visits",
        REG,
        Some(json!({ "patient_id": patient, "arrival_kind": "urgent", "service": "emergency" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "patient_already_present");

    // … and so is arriving a separate appointment while the walk-in is open.
    let (st, appt) = call(
        &state,
        "POST",
        "/api/v1/visits",
        REG,
        Some(json!({
            "patient_id": patient,
            "arrival_kind": "scheduled",
            "service": "general_medicine",
            "scheduled_at": chrono::Utc::now() + chrono::Duration::hours(1),
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{appt}");
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{}/arrive", appt["id"].as_str().unwrap()),
        REG,
        Some(json!({ "version": appt["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "patient_already_present");
}

#[tokio::test]
async fn concurrent_arrivals_for_one_patient_yield_one_open_visit() {
    let (state, _) = test_state().await;
    let patient = register_patient(&state).await;
    let (_, appt) = call(
        &state,
        "POST",
        "/api/v1/visits",
        REG,
        Some(json!({
            "patient_id": patient,
            "arrival_kind": "scheduled",
            "service": "general_medicine",
            "scheduled_at": chrono::Utc::now() + chrono::Duration::minutes(30),
        })),
    )
    .await;
    let arrive_path = format!("/api/v1/visits/{}/arrive", appt["id"].as_str().unwrap());
    // Two desks act at once: one arrives the appointment, one registers a
    // walk-in for the same patient. Exactly one open visit may result.
    let (a, b) = tokio::join!(
        call(
            &state,
            "POST",
            &arrive_path,
            REG,
            Some(json!({ "version": appt["version"] })),
        ),
        call(
            &state,
            "POST",
            "/api/v1/visits",
            REG,
            Some(
                json!({ "patient_id": patient, "arrival_kind": "walk_in", "service": "general_medicine" })
            ),
        ),
    );
    let outcomes = [a, b];
    let ok = outcomes
        .iter()
        .filter(|(st, _)| *st == StatusCode::OK)
        .count();
    let conflict = outcomes
        .iter()
        .filter(|(st, b)| *st == StatusCode::CONFLICT && code(b) == "patient_already_present")
        .count();
    assert_eq!((ok, conflict), (1, 1), "{outcomes:?}");
    let open: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM visits WHERE patient_id = $1::uuid
         AND status IN ('arrived','triage_in_progress','ready_for_consultation','in_consultation')",
    )
    .bind(uuid::Uuid::parse_str(&patient).unwrap())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(open, 1);
}

#[tokio::test]
async fn visit_creation_validation() {
    let (state, _) = test_state().await;
    let patient = register_patient(&state).await;
    let (st, err) = call(
        &state,
        "POST",
        "/api/v1/visits",
        REG,
        Some(json!({ "patient_id": patient, "arrival_kind": "scheduled", "service": "general_medicine" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{err}");
    let (st, err) = call(
        &state,
        "POST",
        "/api/v1/visits",
        REG,
        Some(json!({ "patient_id": patient, "arrival_kind": "teleport", "service": "general_medicine" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{err}");
    let (st, err) = call(
        &state,
        "POST",
        "/api/v1/visits",
        REG,
        Some(json!({ "patient_id": patient, "arrival_kind": "walk_in", "service": "cardiology" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{err}");
    // Unknown patients are indistinguishable from out-of-scope ones.
    let (st, _) = call(
        &state,
        "POST",
        "/api/v1/visits",
        REG,
        Some(json!({ "patient_id": uuid::Uuid::now_v7(), "arrival_kind": "walk_in", "service": "general_medicine" })),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
}

// ---------------------------------------------------------------------------
// Isolation and authorization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tenant_and_facility_isolation() {
    let (state, _) = test_state().await;
    let (_, visit, version) = create_visit(&state, "walk_in", "general_medicine").await;

    // Another tenant: indistinguishable from a nonexistent visit.
    let (st, _) = detail(&state, &visit, OTHER_TENANT).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{visit}/cancel"),
        OTHER_TENANT,
        Some(json!({ "version": version })),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert!(!list_ids(&state, OTHER_TENANT, "all").await.contains(&visit));

    // Same tenant, other facility only: same non-enumerating response.
    let (st, _) = detail(&state, &visit, ANNEX).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = start_consultation(&state, &visit, ANNEX).await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    assert!(!list_ids(&state, ANNEX, "all").await.contains(&visit));
    // Asking for a facility outside one's scope yields nothing, not an error.
    let (_, meta) = call(&state, "GET", "/api/v1/meta/tenant", REG, None).await;
    let main_facility = meta["facilities"][0]["id"].as_str().unwrap();
    let (st, body) = call(
        &state,
        "GET",
        &format!("/api/v1/visits?view=all&facility_id={main_facility}"),
        ANNEX,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{body}");
    assert!(body["items"].as_array().unwrap().is_empty());

    // Same facility: visible, with role-derived capabilities.
    let (st, d) = detail(&state, &visit, LOPEZ).await;
    assert_eq!(st, StatusCode::OK, "{d}");
    assert_eq!(d["capabilities"]["can_triage"], json!(true));
    assert_eq!(d["capabilities"]["can_cancel"], json!(false));
    let (st, d) = detail(&state, &visit, REG).await;
    assert_eq!(st, StatusCode::OK, "{d}");
    assert_eq!(d["capabilities"]["can_cancel"], json!(true));
    assert_eq!(d["capabilities"]["can_triage"], json!(false));
    assert_eq!(d["capabilities"]["can_start_consultation"], json!(false));
}

#[tokio::test]
async fn role_boundaries_are_enforced_server_side() {
    let (state, _) = test_state().await;
    let (_, visit, version) = create_visit(&state, "walk_in", "general_medicine").await;

    // Registration cannot triage or assign.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{visit}/triage"),
        REG,
        Some(json!({ "version": version, "reason": "clerical" })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{err}");
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{visit}/assign"),
        REG,
        Some(json!({ "version": version, "assignee_user_id": user_id(&state, "dr.garcia").await })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    // Registration cannot start consultations.
    let (st, _) = start_consultation(&state, &visit, REG).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Laboratory staff have no visit access at all.
    let (st, _) = detail(&state, &visit, LAB).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(&state, "GET", "/api/v1/visits", LAB, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = call(&state, "GET", "/api/v1/alerts", LAB, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Physicians cannot register arrivals or cancel visits.
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{visit}/cancel"),
        GARCIA,
        Some(json!({ "version": version })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);

    // Missing or unknown tokens are rejected before any lookup.
    let (st, _) = detail(&state, &visit, "dev-nobody").await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// Safety rules, dMind review
// ---------------------------------------------------------------------------

#[tokio::test]
async fn safety_floor_binds_ai_and_humans() {
    let (state, _) = test_state().await;
    let (_, visit, _) = create_visit(&state, "walk_in", "general_medicine").await;

    // Unknown red flags are rejected rather than trusted.
    let (st, err) = save_triage(&state, &visit, json!({ "red_flags": ["made_up_flag"] })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{err}");

    // Chest pain → deterministic urgent floor; a nurse cannot set standard.
    let (st, err) = save_triage(
        &state,
        &visit,
        json!({ "red_flags": ["chest_pain"], "priority": "standard" }),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{err}");
    assert_eq!(code(&err), "priority_below_safety_floor");
    let (st, t) = save_triage(&state, &visit, json!({ "red_flags": ["chest_pain"] })).await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["safety_floor"], json!("urgent"));
    assert_eq!(t["safety_rules"][0]["rule"], json!("red_flag:chest_pain"));

    // The proposal is clamped to the floor and records it.
    let (st, p) = propose(&state, &visit).await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert!(matches!(
        p["output"]["proposed_priority"].as_str(),
        Some("urgent") | Some("immediate")
    ));
    assert_eq!(p["output"]["safety_floor"], json!("urgent"));
    let artifact = p["id"].as_str().unwrap().to_string();

    // Overriding below the floor is refused; overriding at/above it applies.
    let (st, err) = review(
        &state,
        &visit,
        &artifact,
        json!({ "decision": "override", "priority": "standard" }),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{err}");
    assert_eq!(code(&err), "priority_below_safety_floor");
    let (st, err) = review(&state, &visit, &artifact, json!({ "decision": "override" })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{err}");
    let (st, r) = review(
        &state,
        &visit,
        &artifact,
        json!({ "decision": "override", "priority": "immediate", "requested_service": "emergency" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["applied_priority"], json!("immediate"));
    assert_eq!(r["applied_service"], json!("emergency"));
    let (_, d) = detail(&state, &visit, NURSE).await;
    assert_eq!(d["triage"]["ai_decision"], json!("override"));
    assert_eq!(d["triage"]["priority"], json!("immediate"));
    assert_eq!(
        d["proposal"]["review_detail"]["decision"],
        json!("override")
    );
    assert_eq!(
        d["proposal"]["review_detail"]["safety_floor"],
        json!("urgent")
    );

    // Completing triage below the floor is refused too.
    let (st, err) = complete_triage(&state, &visit, json!({ "priority": "non_urgent" })).await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{err}");
    assert_eq!(code(&err), "priority_below_safety_floor");
    let (st, c) = complete_triage(
        &state,
        &visit,
        json!({ "priority": "immediate", "requested_service": "emergency" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    assert_eq!(c["service"], json!("emergency"));
    // Re-routed to the emergency queue of the same facility.
    let (_, d) = detail(&state, &visit, NURSE).await;
    assert_eq!(d["assignment"]["code"], json!("emergency"));
}

#[tokio::test]
async fn vitals_drive_the_safety_floor() {
    let (state, _) = test_state().await;
    let (_, visit, _) = create_visit(&state, "walk_in", "general_medicine").await;
    let (st, t) = save_triage(&state, &visit, json!({ "vitals": { "systolic_mmhg": 84 } })).await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["safety_floor"], json!("immediate"));
    // Normal vitals on a later save recompute the floor from the new record.
    let (st, t) = save_triage(
        &state,
        &visit,
        json!({ "vitals": { "systolic_mmhg": 122, "spo2_percent": 98 } }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["safety_floor"], json!("non_urgent"));
    assert_eq!(t["safety_rules"], json!([]));
    assert_eq!(t["triage_version"], json!(2));
    // Out-of-range vitals are rejected as validation errors.
    let (st, err) = save_triage(&state, &visit, json!({ "vitals": { "spo2_percent": 140 } })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{err}");
}

#[tokio::test]
async fn urgent_arrival_raises_queue_alert_and_floor() {
    let (state, _) = test_state().await;
    let (_, visit, _) = create_visit(&state, "urgent", "general_medicine").await;
    // Urgent arrivals go to the emergency queue with an alert to that queue,
    // visible to consulting professionals of the facility only.
    let (_, d) = detail(&state, &visit, NURSE).await;
    assert_eq!(d["assignment"]["code"], json!("emergency"));
    let for_garcia = alerts_for(&state, GARCIA, &visit).await;
    assert_eq!(for_garcia.len(), 1, "{for_garcia:?}");
    assert_eq!(for_garcia[0]["kind"], json!("urgent_arrival"));
    assert_eq!(for_garcia[0]["target"]["kind"], json!("queue"));
    assert_eq!(alerts_for(&state, LOPEZ, &visit).await.len(), 1);
    assert!(alerts_for(&state, ANNEX, &visit).await.is_empty());
    assert!(alerts_for(&state, OTHER_TENANT, &visit).await.is_empty());

    let (st, t) = save_triage(&state, &visit, json!({})).await;
    assert_eq!(st, StatusCode::OK, "{t}");
    assert_eq!(t["safety_floor"], json!("urgent"));
    assert_eq!(t["safety_rules"][0]["rule"], json!("arrival:urgent"));

    // Completing triage supersedes the arrival alert with a ready alert.
    let (st, c) = complete_triage(
        &state,
        &visit,
        json!({ "priority": "urgent", "requested_service": "emergency" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    let now = alerts_for(&state, GARCIA, &visit).await;
    assert_eq!(now.len(), 1, "{now:?}");
    assert_eq!(now[0]["kind"], json!("patient_ready"));
}

#[tokio::test]
async fn proposal_review_is_explicit_bound_and_single_use() {
    let (state, gateway) = test_state().await;
    let (_, visit, _) = create_visit(&state, "walk_in", "general_medicine").await;

    // No proposal before triage facts exist.
    let (st, err) = propose(&state, &visit).await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "triage_not_started");
    let (st, _) = save_triage(&state, &visit, json!({ "concerns": ["headache"] })).await;
    assert_eq!(st, StatusCode::OK);

    // Provider outage is surfaced and does not block triage.
    gateway.set_unavailable(true);
    let (st, err) = propose(&state, &visit).await;
    assert_eq!(st, StatusCode::SERVICE_UNAVAILABLE, "{err}");
    assert_eq!(code(&err), "ai_unavailable");
    gateway.set_unavailable(false);
    let (st, c) = complete_triage(&state, &visit, json!({})).await;
    assert_eq!(st, StatusCode::OK, "{c}");

    // A proposal generated from version 1 …
    let (_, visit2, _) = create_visit(&state, "walk_in", "general_medicine").await;
    let (st, _) = save_triage(&state, &visit2, json!({ "concerns": ["headache"] })).await;
    assert_eq!(st, StatusCode::OK);
    let (st, p1) = propose(&state, &visit2).await;
    assert_eq!(st, StatusCode::OK, "{p1}");
    let a1 = p1["id"].as_str().unwrap().to_string();
    // … is superseded when the facts change, and can no longer be reviewed.
    let (st, _) = save_triage(
        &state,
        &visit2,
        json!({ "concerns": ["headache", "vomiting"] }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let (_, d) = detail(&state, &visit2, NURSE).await;
    assert_eq!(d["proposal"]["id"], json!(a1));
    assert_eq!(d["proposal"]["status"], json!("superseded"));
    let (st, err) = review(&state, &visit2, &a1, json!({ "decision": "accept" })).await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "invalid_artifact_state");
    // A superseded proposal no longer blocks completion.
    let (st, p2) = propose(&state, &visit2).await;
    assert_eq!(st, StatusCode::OK, "{p2}");
    let a2 = p2["id"].as_str().unwrap().to_string();
    assert_eq!(p2["triage_version"], json!(2));

    // The generic artifact review route never decides a triage proposal:
    // callers without a care relationship are denied, callers with one are
    // redirected to the triage review route. Either way it stays awaiting.
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/ai-artifacts/{a2}/review"),
        GARCIA,
        Some(json!({ "decision": "approved" })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/ai-artifacts/{a2}/review"),
        NURSE,
        Some(json!({ "decision": "approved" })),
    )
    .await;
    assert!(
        st == StatusCode::FORBIDDEN
            || (st == StatusCode::CONFLICT && code(&err) == "use_triage_review"),
        "{st} {err}"
    );
    let status: String = sqlx::query_scalar("SELECT status FROM ai_artifacts WHERE id = $1::uuid")
        .bind(uuid::Uuid::parse_str(&a2).unwrap())
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(status, "awaiting_review");

    // Invalid decisions and stale versions are rejected.
    let (st, _) = review(&state, &visit2, &a2, json!({ "decision": "maybe" })).await;
    assert_eq!(st, StatusCode::BAD_REQUEST);
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{visit2}/triage/proposal/{a2}/review"),
        NURSE,
        Some(json!({ "version": 999, "decision": "reject" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "stale_version");

    // Reject records the decision and applies nothing; a second review of
    // the same artifact is refused.
    let (st, r) = review(
        &state,
        &visit2,
        &a2,
        json!({ "decision": "reject", "note": "Not consistent" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{r}");
    assert_eq!(r["status"], json!("rejected"));
    assert!(r["applied_priority"].is_null());
    let (_, d) = detail(&state, &visit2, NURSE).await;
    assert_eq!(d["triage"]["ai_decision"], json!("reject"));
    assert!(d["triage"]["priority"].is_null());
    assert_eq!(d["proposal"]["review_decision"], json!("rejected"));
    let (st, err) = review(&state, &visit2, &a2, json!({ "decision": "accept" })).await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "invalid_artifact_state");

    // Artifacts from another visit cannot be reviewed against this one.
    let (_, visit3, _) = create_visit(&state, "walk_in", "general_medicine").await;
    let (st, _) = save_triage(&state, &visit3, json!({})).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = review(&state, &visit3, &a2, json!({ "decision": "accept" })).await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Provenance: the persisted artifact is bound to the triage version and
    // records provider metadata and the cited source fields.
    let row = sqlx::query(
        "SELECT autonomy_level, model, model_version, template, input_hash, triage_version, citations
         FROM ai_artifacts WHERE id = $1::uuid",
    )
    .bind(uuid::Uuid::parse_str(&a2).unwrap())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    use sqlx::Row;
    assert_eq!(row.get::<String, _>("autonomy_level"), "A2");
    assert_eq!(row.get::<Option<i64>, _>("triage_version"), Some(2));
    assert!(row.get::<Option<String>, _>("model").is_some());
    assert!(row.get::<Option<String>, _>("model_version").is_some());
    assert_eq!(
        row.get::<Option<String>, _>("template").as_deref(),
        Some("triage-proposal@1.0.0")
    );
    assert!(row.get::<Option<String>, _>("input_hash").is_some());
    let citations: Value = row.get("citations");
    assert!(citations
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c == "triage.version"));
}

// ---------------------------------------------------------------------------
// Routing, alerts, assignment
// ---------------------------------------------------------------------------

#[tokio::test]
async fn queue_routing_reassignment_and_alert_acknowledgement() {
    let (state, _) = test_state().await;
    let (_, visit, _) = create_visit(&state, "walk_in", "general_medicine").await;
    let (st, _) = save_triage(&state, &visit, json!({})).await;
    assert_eq!(st, StatusCode::OK);

    // Unassigned completion routes to the service queue; the ready alert is
    // visible to every consulting professional of the facility.
    let (st, c) = complete_triage(&state, &visit, json!({})).await;
    assert_eq!(st, StatusCode::OK, "{c}");
    let (_, d) = detail(&state, &visit, NURSE).await;
    assert_eq!(d["assignment"]["kind"], json!("queue"));
    assert_eq!(d["assignment"]["code"], json!("general_medicine"));
    let ga = alerts_for(&state, GARCIA, &visit).await;
    let la = alerts_for(&state, LOPEZ, &visit).await;
    assert_eq!(ga.len(), 1);
    assert_eq!(la.len(), 1);
    assert_eq!(ga[0]["id"], la[0]["id"]);
    assert!(alerts_for(&state, ANNEX, &visit).await.is_empty());
    let queue_alert = ga[0]["id"].as_str().unwrap().to_string();

    // Ineligible targets are refused: a physician from another facility, a
    // queue from another facility, both at once.
    let annex_id = user_id(&state, "dr.annex").await;
    let version = version_of(&state, &visit).await;
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{visit}/assign"),
        NURSE,
        Some(json!({ "version": version, "assignee_user_id": annex_id })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{err}");
    assert_eq!(code(&err), "assignee_not_eligible");
    let foreign_queue: uuid::Uuid = sqlx::query_scalar(
        "SELECT q.id FROM service_queues q JOIN facilities f ON f.id = q.facility_id
         WHERE f.name ILIKE '%annex%' LIMIT 1",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{visit}/assign"),
        NURSE,
        Some(json!({ "version": version, "queue_id": foreign_queue })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{err}");
    assert_eq!(code(&err), "queue_not_eligible");
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{visit}/assign"),
        NURSE,
        Some(json!({ "version": version })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST);

    // Reassign to Dr. López: the queue alert is resolved and a directed one
    // replaces it; Dr. García no longer sees it and cannot pick the patient.
    let lopez_id = user_id(&state, "dr.lopez").await;
    let (st, a) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{visit}/assign"),
        NURSE,
        Some(json!({ "version": version, "assignee_user_id": lopez_id })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{a}");
    assert!(alerts_for(&state, GARCIA, &visit).await.is_empty());
    let la = alerts_for(&state, LOPEZ, &visit).await;
    assert_eq!(la.len(), 1, "{la:?}");
    assert_ne!(la[0]["id"], json!(queue_alert));
    assert_eq!(la[0]["target"]["kind"], json!("professional"));
    let directed = la[0]["id"].as_str().unwrap().to_string();
    let (st, err) = start_consultation(&state, &visit, GARCIA).await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "assigned_to_other_professional");
    let (_, d) = detail(&state, &visit, GARCIA).await;
    assert_eq!(d["capabilities"]["can_start_consultation"], json!(false));
    let (_, d) = detail(&state, &visit, LOPEZ).await;
    assert_eq!(d["capabilities"]["can_start_consultation"], json!(true));

    // Only the target can acknowledge a directed alert; once.
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/alerts/{directed}/acknowledge"),
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/alerts/{directed}/acknowledge"),
        OTHER_TENANT,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);
    let (st, ack) = call(
        &state,
        "POST",
        &format!("/api/v1/alerts/{directed}/acknowledge"),
        LOPEZ,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ack}");
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/alerts/{directed}/acknowledge"),
        LOPEZ,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "alert_not_open");
    let la = alerts_for(&state, LOPEZ, &visit).await;
    assert_eq!(la[0]["status"], json!("acknowledged"));
    assert_eq!(la[0]["acknowledged_by_me"], json!(true));
    let (_, body) = call(&state, "GET", "/api/v1/alerts", LOPEZ, None).await;
    let open_for_visit = body["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|a| a["visit_id"] == json!(visit) && a["status"] == "open")
        .count();
    assert_eq!(open_for_visit, 0);

    // Care-team membership is separate from system roles: the assignment
    // gives Dr. López a patient-read relationship, not a role.
    let (st, d) = detail(&state, &visit, LOPEZ).await;
    assert_eq!(st, StatusCode::OK);
    let (st, s) = start_consultation(&state, &visit, LOPEZ).await;
    assert_eq!(st, StatusCode::OK, "{s}");
    assert_eq!(s["status"], json!("in_consultation"));
    assert!(alerts_for(&state, LOPEZ, &visit).await.is_empty());
    let _ = d;
}

#[tokio::test]
async fn care_team_assignments_are_recorded_with_provenance() {
    let (state, _) = test_state().await;
    let (patient, visit, _) = create_visit(&state, "walk_in", "nursing").await;
    let (st, _) = save_triage(&state, &visit, json!({})).await;
    assert_eq!(st, StatusCode::OK);
    let garcia = user_id(&state, "dr.garcia").await;
    let (st, _) = complete_triage(&state, &visit, json!({ "assignee_user_id": garcia })).await;
    assert_eq!(st, StatusCode::OK);
    use sqlx::Row;
    let rows = sqlx::query(
        "SELECT function, active, source, assignee_user_id, queue_id, assigned_by
         FROM care_team_assignments WHERE visit_id = $1::uuid ORDER BY created_at, id",
    )
    .bind(uuid::Uuid::parse_str(&visit).unwrap())
    .fetch_all(&state.pool)
    .await
    .unwrap();
    // Registration queue routing (nursing queue, now inactive), the nurse's
    // own membership, the treating professional; exactly one active target.
    let sources: Vec<String> = rows.iter().map(|r| r.get("source")).collect();
    assert!(sources.contains(&"registration".to_string()), "{sources:?}");
    assert!(sources.contains(&"triage".to_string()), "{sources:?}");
    let queue_rows: Vec<&sqlx::postgres::PgRow> = rows
        .iter()
        .filter(|r| r.get::<String, _>("function") == "destination_queue")
        .collect();
    assert_eq!(queue_rows.len(), 1);
    assert!(!queue_rows[0].get::<bool, _>("active"));
    assert!(rows
        .iter()
        .any(|r| r.get::<bool, _>("active") && r.get::<String, _>("function") == "triage_nurse"));
    let treating: Vec<&sqlx::postgres::PgRow> = rows
        .iter()
        .filter(|r| {
            r.get::<bool, _>("active") && r.get::<String, _>("function") == "treating_professional"
        })
        .collect();
    assert_eq!(treating.len(), 1);
    assert_eq!(
        treating[0].get::<Option<uuid::Uuid>, _>("assignee_user_id"),
        Some(uuid::Uuid::parse_str(&garcia).unwrap())
    );
    assert!(treating[0]
        .get::<Option<uuid::Uuid>, _>("queue_id")
        .is_none());
    assert!(rows
        .iter()
        .all(|r| r.get::<Option<uuid::Uuid>, _>("assigned_by").is_some()));
    // The patient's chart is now readable by the assigned physician through
    // the care-team relationship, not through a role grant.
    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient}"),
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Concurrency and handoff integrity
// ---------------------------------------------------------------------------

#[tokio::test]
async fn stale_triage_saves_are_rejected_and_versions_advance() {
    let (state, _) = test_state().await;
    let (_, visit, v1) = create_visit(&state, "walk_in", "general_medicine").await;
    let (st, t) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{visit}/triage"),
        NURSE,
        Some(json!({ "version": v1, "reason": "first" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{t}");
    // A second tab still holding v1 loses cleanly.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{visit}/triage"),
        NURSE,
        Some(json!({ "version": v1, "reason": "stale tab" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "stale_version");
    let (_, d) = detail(&state, &visit, NURSE).await;
    assert_eq!(d["triage"]["reason"], json!("first"));
    assert_eq!(d["version"], t["version"]);
    // Completing triage with the stale version is refused too.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/visits/{visit}/triage/complete"),
        NURSE,
        Some(json!({ "version": v1, "priority": "standard", "requested_service": "general_medicine" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(code(&err), "stale_version");
}

#[tokio::test]
async fn concurrent_start_from_queue_yields_one_consultation() {
    let (state, _) = test_state().await;
    let (patient, visit, _) = create_visit(&state, "walk_in", "general_medicine").await;
    let (st, _) = save_triage(&state, &visit, json!({})).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = complete_triage(&state, &visit, json!({})).await;
    assert_eq!(st, StatusCode::OK);

    // Two physicians of the queue click Start at the same time.
    let (a, b) = tokio::join!(
        start_consultation(&state, &visit, GARCIA),
        start_consultation(&state, &visit, LOPEZ),
    );
    let outcomes = [a, b];
    let ok = outcomes
        .iter()
        .filter(|(st, _)| *st == StatusCode::OK)
        .count();
    let conflict = outcomes
        .iter()
        .filter(|(st, b)| *st == StatusCode::CONFLICT && code(b) == "consultation_in_progress")
        .count();
    assert_eq!((ok, conflict), (1, 1), "{outcomes:?}");
    let encounters: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM encounters WHERE patient_id = $1::uuid AND status = 'in_progress'",
    )
    .bind(uuid::Uuid::parse_str(&patient).unwrap())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(encounters, 1);
    // Only the winner can resume; the other sees the consultation as taken.
    let (_, d) = detail(&state, &visit, LOPEZ).await;
    assert_eq!(d["status"], json!("in_consultation"));
    let (_, d2) = detail(&state, &visit, GARCIA).await;
    let resumable = [
        d["capabilities"]["can_resume_consultation"]
            .as_bool()
            .unwrap(),
        d2["capabilities"]["can_resume_consultation"]
            .as_bool()
            .unwrap(),
    ];
    assert_eq!(resumable.iter().filter(|r| **r).count(), 1, "{resumable:?}");
}

#[tokio::test]
async fn existing_draft_encounter_is_resumed_not_duplicated() {
    let (state, _) = test_state().await;
    let (patient, visit, _) = create_visit(&state, "walk_in", "general_medicine").await;
    // Dr. García already opened a consultation for this patient.
    let (st, enc) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        GARCIA,
        Some(json!({ "patient_id": patient })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{enc}");
    let existing = enc["id"].as_str().unwrap().to_string();
    let (st, _) = save_triage(&state, &visit, json!({})).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = complete_triage(&state, &visit, json!({})).await;
    assert_eq!(st, StatusCode::OK);
    let (st, s) = start_consultation(&state, &visit, GARCIA).await;
    assert_eq!(st, StatusCode::OK, "{s}");
    assert_eq!(s["resumed"], json!(true));
    assert_eq!(s["encounter_id"], json!(existing));
    let (_, d) = detail(&state, &visit, GARCIA).await;
    assert_eq!(d["encounter_id"], json!(existing));
    let (st, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{existing}"),
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ws}");
}

#[tokio::test]
async fn cancelling_the_encounter_releases_the_visit() {
    let (state, _) = test_state().await;
    let (_, visit, _) = create_visit(&state, "walk_in", "general_medicine").await;
    let (st, _) = save_triage(&state, &visit, json!({})).await;
    assert_eq!(st, StatusCode::OK);
    let (st, _) = complete_triage(&state, &visit, json!({ "priority": "urgent" })).await;
    assert_eq!(st, StatusCode::OK);
    let (st, s) = start_consultation(&state, &visit, GARCIA).await;
    assert_eq!(st, StatusCode::OK, "{s}");
    let encounter = s["encounter_id"].as_str().unwrap().to_string();
    assert!(alerts_for(&state, LOPEZ, &visit).await.is_empty());

    let (st, c) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{encounter}/cancel"),
        GARCIA,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{c}");
    // Back on the ready list under queue routing, with a fresh ready alert
    // for the queue and no encounter link.
    let (_, d) = detail(&state, &visit, GARCIA).await;
    assert_eq!(d["status"], json!("ready_for_consultation"));
    assert!(d["encounter_id"].is_null());
    assert_eq!(d["priority"], json!("urgent"));
    assert_eq!(d["assignment"]["kind"], json!("queue"));
    let la = alerts_for(&state, LOPEZ, &visit).await;
    assert_eq!(la.len(), 1, "{la:?}");
    assert_eq!(la[0]["kind"], json!("patient_ready"));
    assert_eq!(la[0]["status"], json!("open"));
    // Another professional can now start a new consultation.
    let (st, s2) = start_consultation(&state, &visit, LOPEZ).await;
    assert_eq!(st, StatusCode::OK, "{s2}");
    assert_eq!(s2["resumed"], json!(false));
    assert_ne!(s2["encounter_id"], json!(encounter));
}

#[tokio::test]
async fn seeded_demo_states_are_reachable_through_the_api() {
    let (state, _) = test_state().await;
    // Seeded visits cover every worklist the demo needs.
    let (st, access) = call(&state, "GET", "/api/v1/visits?view=all", NURSE, None).await;
    assert_eq!(st, StatusCode::OK, "{access}");
    let statuses: Vec<&str> = access["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|i| i["status"].as_str())
        .collect();
    for s in [
        "scheduled",
        "arrived",
        "triage_in_progress",
        "ready_for_consultation",
        "in_consultation",
    ] {
        assert!(statuses.contains(&s), "missing seeded {s} in {statuses:?}");
    }
    // The seeded triage-in-progress visit carries a proposal awaiting review
    // bound to its triage version.
    let seeded = access["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|i| i["status"] == "triage_in_progress" && i["patient"]["given_name"] == "Diego")
        .expect("seeded Diego visit");
    let (st, d) = detail(&state, seeded["id"].as_str().unwrap(), NURSE).await;
    assert_eq!(st, StatusCode::OK, "{d}");
    assert_eq!(d["proposal"]["status"], json!("awaiting_review"));
    assert_eq!(d["proposal"]["triage_version"], d["triage"]["version"]);
    assert_eq!(d["rules_version"], json!("triage-safety@1.0.0"));
    // The seeded ready visit is assigned to Dr. García with an open alert.
    let (_, body) = call(&state, "GET", "/api/v1/alerts", GARCIA, None).await;
    assert!(body["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|a| a["kind"] == "patient_ready" && a["target"]["kind"] == "professional"));
}
