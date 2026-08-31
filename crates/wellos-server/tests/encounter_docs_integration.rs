//! Integration tests for the encounter documentation workspace: note
//! lifecycle (draft → signed → addenda), optimistic concurrency, vital-sign
//! validation with server-side BMI, diagnoses, the governed dMind draft, and
//! facility/role authorization boundaries.

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

/// Register a fresh patient and start an in-progress encounter owned by
/// dr.garcia. Returns (patient_id, encounter_id).
async fn start_encounter(state: &AppState) -> (String, String) {
    let (st, meta) = call(state, "GET", "/api/v1/meta/tenant", "dev-reg.rivera", None).await;
    assert_eq!(st, StatusCode::OK);
    let facility = meta["facilities"][0]["id"].as_str().unwrap().to_string();
    let (st, patient) = call(
        state,
        "POST",
        "/api/v1/patients",
        "dev-reg.rivera",
        Some(json!({
            "facility_id": facility,
            "family_name": "Documentation",
            "given_name": "Test",
            "birth_date": "1980-02-02",
            "sex": "female",
            "identifier": uniq("MRN-DOC"),
        })),
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
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{enc}");
    (patient_id, enc["id"].as_str().unwrap().to_string())
}

#[tokio::test]
async fn note_draft_save_and_version_conflict() {
    let state = test_state().await;
    let (_, enc) = start_encounter(&state).await;

    // First save creates the draft at version 1.
    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "reason_for_encounter": "Chest pain", "assessment": "Likely benign" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");
    assert_eq!(note["status"], json!("draft"));
    assert_eq!(note["version"], json!(1));

    // Updating with the current version succeeds and bumps the version.
    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({
            "version": 1,
            "reason_for_encounter": "Chest pain",
            "assessment": "Musculoskeletal pain",
            "plan": "NSAIDs and rest",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");
    assert_eq!(note["version"], json!(2));

    // A concurrent editor with a stale version is rejected, not overwritten.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "version": 1, "assessment": "stale write" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("version_conflict"));

    // Omitting the version on an existing note is also rejected.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "assessment": "no version" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("version_required"));

    // The workspace reflects the saved draft.
    let (st, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ws}");
    assert_eq!(ws["note"]["status"], json!("draft"));
    assert_eq!(ws["note"]["plan"], json!("NSAIDs and rest"));
    assert_eq!(ws["capabilities"]["can_document"], json!(true));
    assert_eq!(ws["capabilities"]["can_sign"], json!(true));
}

#[tokio::test]
async fn vitals_validation_and_bmi() {
    let state = test_state().await;
    let (_, enc) = start_encounter(&state).await;
    let path = format!("/api/v1/encounters/{enc}/vitals");

    // Impossible values are rejected outright, even with confirmation.
    let (st, err) = call(
        &state,
        "POST",
        &path,
        "dev-dr.garcia",
        Some(json!({ "heart_rate_bpm": 900, "confirm_unusual": true })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{err}");
    assert_eq!(err["error"]["code"], json!("value_out_of_range"));

    // Unusual-but-possible values require explicit confirmation.
    let (st, err) = call(
        &state,
        "POST",
        &path,
        "dev-dr.garcia",
        Some(json!({ "heart_rate_bpm": 190 })),
    )
    .await;
    assert_eq!(st, StatusCode::UNPROCESSABLE_ENTITY, "{err}");
    assert_eq!(err["error"]["code"], json!("unusual_values"));
    let (st, ok) = call(
        &state,
        "POST",
        &path,
        "dev-dr.garcia",
        Some(json!({ "heart_rate_bpm": 190, "confirm_unusual": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ok}");

    // An empty set is rejected.
    let (st, err) = call(&state, "POST", &path, "dev-dr.garcia", Some(json!({}))).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{err}");

    // BMI is server-calculated from weight and height (80 / 1.75² = 26.1).
    let (st, ok) = call(
        &state,
        "POST",
        &path,
        "dev-dr.garcia",
        Some(json!({ "weight_kg": 80, "height_cm": 175 })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ok}");
    assert_eq!(ok["bmi"], json!("26.1"));

    // The workspace lists this encounter's vitals, newest first.
    let (st, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let vitals = ws["vitals"].as_array().unwrap();
    assert_eq!(vitals.len(), 2, "{ws}");
    assert_eq!(vitals[0]["bmi"], json!("26.1"));
}

#[tokio::test]
async fn sign_lifecycle_immutability_and_addenda() {
    let state = test_state().await;
    let (patient, enc) = start_encounter(&state).await;

    // Signing without a note is rejected.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/sign"),
        "dev-dr.garcia",
        Some(json!({ "version": 1 })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("note_missing"));

    // A note without a reason cannot be signed.
    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "assessment": "Assessment only" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/sign"),
        "dev-dr.garcia",
        Some(json!({ "version": 1 })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{err}");
    assert_eq!(err["error"]["code"], json!("sign_requires_reason"));

    // Complete the required minimum, then sign with a stale version first.
    let (st, note) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({
            "version": 1,
            "reason_for_encounter": "Annual review",
            "assessment": "Stable",
            "plan": "Continue current management",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{note}");
    let version = note["version"].as_i64().unwrap();
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/sign"),
        "dev-dr.garcia",
        Some(json!({ "version": version - 1 })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("version_conflict"));

    let (st, signed) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/sign"),
        "dev-dr.garcia",
        Some(json!({ "version": version })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{signed}");
    assert_eq!(signed["status"], json!("signed"));
    assert_eq!(signed["encounter_status"], json!("completed"));

    // The signed note is immutable: further edits and re-signing fail.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "version": version + 1, "assessment": "tamper" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/sign"),
        "dev-dr.garcia",
        Some(json!({ "version": version + 1 })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");

    // A signed encounter cannot be cancelled.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/cancel"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");

    // Corrections are dated addenda by the signing practitioner.
    let (st, add) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/addenda"),
        "dev-dr.garcia",
        Some(json!({ "body": "Correction: symptom onset was two weeks ago." })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{add}");

    // Another physician cannot add addenda to someone else's note.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/addenda"),
        "dev-dr.lopez",
        Some(json!({ "body": "not my note" })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{err}");

    // The workspace shows the signed note, its addendum, and read-only
    // capabilities except addenda for the author.
    let (st, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ws}");
    assert_eq!(ws["note"]["status"], json!("signed"));
    assert_eq!(ws["encounter"]["status"], json!("completed"));
    assert_eq!(ws["addenda"].as_array().unwrap().len(), 1);
    assert_eq!(ws["capabilities"]["can_document"], json!(false));
    assert_eq!(ws["capabilities"]["can_sign"], json!(false));
    assert_eq!(ws["capabilities"]["can_add_addendum"], json!(true));

    // The patient chart timeline shows the completed encounter with its
    // note status and addenda count.
    let (st, chart) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{chart}");
    let e = chart["encounters"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"] == json!(enc))
        .expect("completed encounter in chart");
    assert_eq!(e["status"], json!("completed"));
    assert_eq!(e["note_status"], json!("signed"));
    assert_eq!(e["addenda_count"], json!(1));
}

#[tokio::test]
async fn diagnosis_added_to_encounter_and_chart() {
    let state = test_state().await;
    let (patient, enc) = start_encounter(&state).await;

    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/diagnoses"),
        "dev-dr.garcia",
        Some(json!({ "display": "Hypertension", "status": "bogus" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{err}");

    let (st, dx) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/diagnoses"),
        "dev-dr.garcia",
        Some(
            json!({ "display": "Essential hypertension", "code": "I10", "status": "provisional" }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{dx}");

    let (st, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let found = ws["diagnoses"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["id"] == dx["id"])
        .expect("diagnosis in workspace");
    assert_eq!(found["status"], json!("provisional"));
    assert_eq!(found["this_encounter"], json!(true));

    let (st, chart) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    let cond = chart["conditions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["display"] == json!("Essential hypertension"))
        .expect("diagnosis in patient chart");
    assert_eq!(cond["status"], json!("provisional"));
}

#[tokio::test]
async fn documentation_requires_own_active_encounter_and_facility() {
    let state = test_state().await;
    let (_, enc) = start_encounter(&state).await;

    // Another same-facility physician cannot document someone else's
    // encounter.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.lopez",
        Some(json!({ "assessment": "not mine" })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{err}");

    // A physician assigned only to another facility gets the same
    // non-enumerating response used for out-of-scope resources.
    let (st, _) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.annex",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND);

    // Registration staff cannot document at all.
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-reg.rivera",
        Some(json!({ "assessment": "clerical" })),
    )
    .await;
    assert_ne!(st, StatusCode::OK);

    // Cancelled encounters no longer accept documentation.
    let (st, cancelled) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/cancel"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{cancelled}");
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(json!({ "assessment": "too late" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("encounter_not_active"));
}

#[tokio::test]
async fn ai_draft_generation_acceptance_and_rejection() {
    let state = test_state().await;
    let (_, enc) = start_encounter(&state).await;

    // With nothing documented there is nothing to summarize.
    let (st, err) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/ai-draft"),
        "dev-dr.garcia",
        Some(json!({ "language": "en" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{err}");
    assert_eq!(err["error"]["code"], json!("nothing_to_summarize"));

    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/note"),
        "dev-dr.garcia",
        Some(
            json!({ "reason_for_encounter": "Fatigue", "assessment": "Iron deficiency suspected" }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::OK);

    let (st, draft) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/ai-draft"),
        "dev-dr.garcia",
        Some(json!({ "language": "en" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{draft}");
    assert_eq!(draft["status"], json!("awaiting_review"));
    assert_eq!(draft["model"], json!("dmind-fake"));
    // Undocumented sections surface as limitations.
    assert!(
        draft["limitations"]
            .as_array()
            .unwrap()
            .iter()
            .any(|l| l.as_str().unwrap().contains("Incomplete documentation")),
        "{draft}"
    );
    let first = draft["id"].as_str().unwrap().to_string();

    // Regenerating supersedes the unreviewed draft.
    let (st, draft2) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/ai-draft"),
        "dev-dr.garcia",
        Some(json!({ "language": "es" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{draft2}");
    let second = draft2["id"].as_str().unwrap().to_string();
    assert_ne!(first, second);

    // Rejection is recorded; a rejected draft cannot be re-reviewed.
    let (st, rev) = call(
        &state,
        "POST",
        &format!("/api/v1/ai-artifacts/{second}/review"),
        "dev-dr.garcia",
        Some(json!({ "decision": "rejected", "note": "not useful" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{rev}");
    let (st, _) = call(
        &state,
        "POST",
        &format!("/api/v1/ai-artifacts/{second}/review"),
        "dev-dr.garcia",
        Some(json!({ "decision": "approved" })),
    )
    .await;
    assert_ne!(st, StatusCode::OK);

    // A fresh draft can be explicitly accepted; the decision is visible in
    // the workspace payload.
    let (st, draft3) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{enc}/ai-draft"),
        "dev-dr.garcia",
        Some(json!({})),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{draft3}");
    let third = draft3["id"].as_str().unwrap().to_string();
    let (st, rev) = call(
        &state,
        "POST",
        &format!("/api/v1/ai-artifacts/{third}/review"),
        "dev-dr.garcia",
        Some(json!({ "decision": "approved" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{rev}");
    let (st, ws) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{enc}"),
        "dev-dr.garcia",
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(ws["ai_draft"]["id"], json!(third), "{ws}");
    assert_eq!(ws["ai_draft"]["review_decision"], json!("approved"));

    // The assistant never modified the note or the encounter state.
    assert_eq!(ws["note"]["status"], json!("draft"));
    assert_eq!(ws["encounter"]["status"], json!("in_progress"));
}
