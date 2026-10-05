//! Integration tests for patient self-service: explicit patient-access
//! grants, grant-derived `/api/v1/me/...` identity, representatives with
//! several dependants, preferences, scheduling consents and privacy-
//! preserving personal-calendar import/sync/disconnect.

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
    path: &str,
    token: &str,
    content_type: &str,
    body: Vec<u8>,
) -> (StatusCode, Value) {
    let purpose = if token == ADMIN {
        "operations"
    } else {
        "treatment"
    };
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", content_type)
        .header("x-purpose-of-use", purpose)
        .body(Body::from(body))
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
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
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
    send(
        state,
        method,
        path,
        token,
        "application/json",
        body.map(|v| v.to_string().into_bytes()).unwrap_or_default(),
    )
    .await
}

fn uniq(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7().simple())
}

fn code(v: &Value) -> &str {
    v["error"]["code"].as_str().unwrap_or("")
}

fn s(v: &Value) -> String {
    v.as_str().unwrap_or_default().to_string()
}

const REG: &str = "dev-reg.rivera";
const ADMIN: &str = "dev-admin.silva";
const OTHER_TENANT: &str = "dev-dr.sur";

async fn main_facility(state: &AppState) -> String {
    let (st, meta) = call(state, "GET", "/api/v1/meta/tenant", REG, None).await;
    assert_eq!(st, StatusCode::OK);
    s(&meta["facilities"][0]["id"])
}

async fn register_patient(state: &AppState, facility: &str) -> String {
    let (st, patient) = call(
        state,
        "POST",
        "/api/v1/patients",
        REG,
        Some(json!({
            "facility_id": facility,
            "family_name": "SelfService",
            "given_name": "Synthetic",
            "birth_date": "2015-06-01",
            "sex": "male",
            "identifier": uniq("MRN-ME"),
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{patient}");
    s(&patient["id"])
}

/// A synthetic person account of tenant A holding only
/// `patient_representative`. Self-service accounts are created by the
/// identity provider onboarding in production; the test inserts the local
/// identity record directly and reaches it through the synthetic dev token.
async fn create_representative(state: &AppState) -> (Uuid, String) {
    let username = uniq("rep").to_lowercase();
    let tenant: Uuid =
        sqlx::query_scalar("SELECT tenant_id FROM users WHERE username = 'reg.rivera'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let uid = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, tenant_id, username, display_name, is_service, oidc_subject)
         VALUES ($1,$2,$3,'Synthetic Representative',false,$4)",
    )
    .bind(uid)
    .bind(tenant)
    .bind(&username)
    .bind(format!("synthetic|{username}"))
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO role_assignments (id, tenant_id, user_id, role, facility_id)
         VALUES ($1,$2,$3,'patient_representative',NULL)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant)
    .bind(uid)
    .execute(&state.pool)
    .await
    .unwrap();
    (uid, format!("dev-{username}"))
}

async fn create_grant(
    state: &AppState,
    user_id: Uuid,
    patient: &str,
    relationship: &str,
    expires_at: Option<Value>,
) -> Value {
    let (st, g) = call(
        state,
        "POST",
        "/api/v1/patient-grants",
        REG,
        Some(json!({
            "user_id": user_id,
            "patient_id": patient,
            "relationship": relationship,
            "verification_note": "Synthetic: passport and custody document checked in person",
            "expires_at": expires_at,
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{g}");
    g
}

async fn create_service(state: &AppState) -> String {
    let service = uniq("svc").replace('-', "_");
    let (st, v) = call(
        state,
        "POST",
        "/api/v1/catalog",
        ADMIN,
        Some(json!({
            "kind": "clinical_service",
            "code": service,
            "name_en": "Self-service runtime service",
            "name_es": "Servicio de autoservicio",
            "config": { "duration_minutes": 30, "modality_codes": ["in_person"], "required_resource_types": ["professional"] },
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    service
}

async fn create_professional(state: &AppState, facility: &str, service: &str) -> String {
    let (st, res) = call(
        state,
        "POST",
        "/api/v1/scheduling/resources",
        ADMIN,
        Some(json!({
            "facility_id": facility,
            "resource_type_code": "professional",
            "name": uniq("Dr Self"),
            "languages": ["es", "en"],
            "services": [{ "service_code": service, "modality_codes": ["in_person"] }],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{res}");
    let id = s(&res["id"]);
    let version = res["version"].as_i64().unwrap();
    let rules: Vec<Value> = (1..=7)
        .map(|d| json!({ "weekday": d, "start_local": "08:00:00", "end_local": "18:00:00" }))
        .collect();
    let (st, res) = call(
        state,
        "POST",
        &format!("/api/v1/scheduling/resources/{id}/availability"),
        ADMIN,
        Some(json!({ "version": version, "rules": rules })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{res}");
    id
}

async fn set_consent(state: &AppState, token: &str, patient: &str, purpose: &str, status: &str) {
    let (st, v) = call(
        state,
        "POST",
        "/api/v1/me/consents",
        token,
        Some(json!({ "patient_id": patient, "purpose": purpose, "status": status })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
}

#[tokio::test]
async fn self_service_identity_comes_only_from_an_active_grant() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let patient = register_patient(&state, &facility).await;
    let stranger = register_patient(&state, &facility).await;
    let (uid, token) = create_representative(&state).await;

    // No grant: the account is known but may act for nobody.
    let (st, me) = call(&state, "GET", "/api/v1/me", &token, None).await;
    assert_eq!(st, StatusCode::OK, "{me}");
    assert_eq!(me["patients"].as_array().unwrap().len(), 0);
    let (st, v) = call(
        &state,
        "POST",
        "/api/v1/me/access-requests",
        &token,
        Some(json!({ "free_text": "checkup" })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "no_patient_grant");
    // Naming a patient does not help without a grant: not_found, never a hint.
    let (st, v) = call(
        &state,
        "GET",
        &format!("/api/v1/me/appointments?patient_id={patient}"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");

    // Grants require staff verification with a note; the grantee must be a
    // representative account of the same tenant.
    let (st, v) = call(
        &state,
        "POST",
        "/api/v1/patient-grants",
        REG,
        Some(json!({ "user_id": uid, "patient_id": patient, "relationship": "self" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, v) = call(
        &state,
        "POST",
        "/api/v1/patient-grants",
        &token,
        Some(
            json!({ "user_id": uid, "patient_id": patient, "relationship": "self",
                     "verification_note": "self-issued" }),
        ),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "a patient cannot grant themselves: {v}"
    );
    let (st, v) = call(
        &state,
        "POST",
        "/api/v1/patient-grants",
        OTHER_TENANT,
        Some(
            json!({ "user_id": uid, "patient_id": patient, "relationship": "self",
                     "verification_note": "cross tenant" }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");

    let grant = create_grant(&state, uid, &patient, "self", None).await;
    assert_eq!(grant["status"], "active");
    let gid = s(&grant["id"]);
    let (st, dup) = call(
        &state,
        "POST",
        "/api/v1/patient-grants",
        REG,
        Some(
            json!({ "user_id": uid, "patient_id": patient, "relationship": "self",
                     "verification_note": "duplicate" }),
        ),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{dup}");
    assert_eq!(code(&dup), "grant_exists");

    // With one grant the patient is implied; a foreign patient_id is 404.
    let (st, me) = call(&state, "GET", "/api/v1/me", &token, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(s(&me["patients"][0]["patient_id"]), patient);
    assert_eq!(me["patients"][0]["relationship"], "self");
    let (st, req) = call(
        &state,
        "POST",
        "/api/v1/me/access-requests",
        &token,
        Some(json!({ "patient_id": stranger, "free_text": "checkup", "submit": false })),
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{req}");
    let (st, req) = call(
        &state,
        "POST",
        "/api/v1/me/access-requests",
        &token,
        Some(json!({ "free_text": "checkup for my knee", "submit": false })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{req}");
    assert_eq!(s(&req["patient_id"]), patient);
    assert_eq!(req["channel"], "patient");
    assert_eq!(req["status"], "draft");
    let rid = s(&req["id"]);

    // Urgency is never patient-claimed: the field is not accepted.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/me/access-requests/{rid}/amend"),
        &token,
        Some(json!({ "version": req["version"], "urgency": "urgent" })),
    )
    .await;
    assert!(
        st == StatusCode::UNPROCESSABLE_ENTITY || st == StatusCode::BAD_REQUEST,
        "{st} {v}"
    );
    let (st, after) = call(
        &state,
        "GET",
        &format!("/api/v1/me/access-requests/{rid}"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{after}");
    assert_eq!(after["request"]["urgency"], "routine");

    // Staff see the request through the staff API; a stranger's request is
    // invisible on /me even though it exists in the tenant.
    let (st, other_req) = call(
        &state,
        "POST",
        "/api/v1/access-requests",
        REG,
        Some(json!({ "patient_id": stranger, "facility_id": facility, "free_text": "x", "submit": false })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{other_req}");
    let (st, v) = call(
        &state,
        "GET",
        &format!("/api/v1/me/access-requests/{}", s(&other_req["id"])),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");
    // And self-service never reaches the clinical chart.
    let (st, v) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{patient}"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");

    // Listing grants needs a patient or user filter and staff permission.
    let (st, list) = call(
        &state,
        "GET",
        &format!("/api/v1/patient-grants?patient_id={patient}"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 1);
    let (st, v) = call(
        &state,
        "GET",
        &format!("/api/v1/patient-grants?patient_id={patient}"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");

    // Revocation needs a reason and takes effect immediately.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/patient-grants/{gid}/revoke"),
        REG,
        Some(json!({ "version": grant["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, revoked) = call(
        &state,
        "POST",
        &format!("/api/v1/patient-grants/{gid}/revoke"),
        REG,
        Some(json!({ "version": grant["version"], "reason": "Synthetic: relationship ended" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{revoked}");
    assert_eq!(revoked["status"], "revoked");
    let (st, v) = call(
        &state,
        "GET",
        &format!("/api/v1/me/access-requests/{rid}"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "revoked grant: {v}");
    let (st, v) = call(&state, "GET", "/api/v1/me/access-requests", &token, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "no_patient_grant");

    // An expired grant is equally unusable, and reported as expired.
    let g2 = create_grant(
        &state,
        uid,
        &patient,
        "self",
        Some(json!("2099-01-01T00:00:00Z")),
    )
    .await;
    let (st, ok) = call(&state, "GET", "/api/v1/me/access-requests", &token, None).await;
    assert_eq!(st, StatusCode::OK, "{ok}");
    sqlx::query(
        "UPDATE patient_access_grants SET expires_at = now() - interval '1 minute' WHERE id = $1",
    )
    .bind(s(&g2["id"]).parse::<Uuid>().unwrap())
    .execute(&state.pool)
    .await
    .unwrap();
    let (st, v) = call(&state, "GET", "/api/v1/me/access-requests", &token, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "expired grant: {v}");
    let (st, list) = call(
        &state,
        "GET",
        &format!("/api/v1/patient-grants?patient_id={patient}&status=all"),
        REG,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{list}");
    let statuses: Vec<String> = list["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|g| s(&g["status"]))
        .collect();
    assert!(statuses.contains(&"revoked".to_string()), "{statuses:?}");
    assert!(statuses.contains(&"expired".to_string()), "{statuses:?}");

    // Grant lifecycle is audited.
    let audited: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM outbox_events WHERE event_type IN ('patient_grant.created','patient_grant.revoked')
           AND resource_refs->>'patient_id' = $1",
    )
    .bind(&patient)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(audited >= 3, "{audited}");
}

#[tokio::test]
async fn representative_books_for_one_of_several_dependants() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let child_a = register_patient(&state, &facility).await;
    let child_b = register_patient(&state, &facility).await;
    let (uid, token) = create_representative(&state).await;
    create_grant(&state, uid, &child_a, "parent_guardian", None).await;
    create_grant(&state, uid, &child_b, "parent_guardian", None).await;

    let (st, me) = call(&state, "GET", "/api/v1/me", &token, None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(me["patients"].as_array().unwrap().len(), 2);

    // Two dependants: the patient must be named explicitly.
    let (st, v) = call(
        &state,
        "POST",
        "/api/v1/me/access-requests",
        &token,
        Some(json!({ "free_text": "vaccination" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(code(&v), "patient_required");

    let (st, req) = call(
        &state,
        "POST",
        "/api/v1/me/access-requests",
        &token,
        Some(json!({
            "patient_id": child_a,
            "facility_id": facility,
            "free_text": "Routine review for my child, mornings",
            // Beyond the 24h online-cancellation window so the cancel below is in policy.
            "constraints": {
                "service_code": service,
                "facility_ids": [facility],
                "earliest": chrono::Utc::now() + chrono::Duration::days(2),
            },
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{req}");
    assert_eq!(s(&req["patient_id"]), child_a);
    assert_eq!(req["channel"], "representative");
    assert_eq!(req["status"], "submitted");
    let rid = s(&req["id"]);

    let (st, m) = call(
        &state,
        "POST",
        &format!("/api/v1/me/access-requests/{rid}/match"),
        &token,
        Some(json!({ "version": req["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{m}");
    assert_eq!(m["request"]["status"], "options_ready");
    let offers = m["offers"].as_array().unwrap();
    assert!(!offers.is_empty(), "{m}");
    // The self-service view explains how options were produced.
    let (st, view) = call(
        &state,
        "GET",
        &format!("/api/v1/me/access-requests/{rid}"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{view}");
    assert_eq!(view["matcher_run"]["matcher_version"], "access-matcher.v1");
    assert!(view["offers"][0]["score"]["factors"].is_array(), "{view}");

    let offer = &offers[0];
    let oid = s(&offer["id"]);
    let (st, held) = call(
        &state,
        "POST",
        &format!("/api/v1/me/offers/{oid}/hold"),
        &token,
        Some(json!({ "version": offer["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{held}");
    assert_eq!(held["status"], "held");
    // Staff override fields are not part of the patient contract.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/me/offers/{oid}/accept"),
        &token,
        Some(json!({ "version": held["version"], "override_reason": "because" })),
    )
    .await;
    assert!(
        st == StatusCode::UNPROCESSABLE_ENTITY || st == StatusCode::BAD_REQUEST,
        "{st} {v}"
    );
    let (st, acc) = call(
        &state,
        "POST",
        &format!("/api/v1/me/offers/{oid}/accept"),
        &token,
        Some(json!({ "version": held["version"], "idempotency_key": uniq("me-acc") })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{acc}");
    assert_eq!(acc["status"], "confirmed");
    assert_eq!(acc["booked_via"], "representative");
    assert_eq!(s(&acc["patient_id"]), child_a);
    let aid = s(&acc["id"]);
    assert!(acc["visit_id"].as_str().is_some(), "{acc}");

    // The appointment appears for child A only; child B's list is empty.
    let (st, list) = call(
        &state,
        "GET",
        &format!("/api/v1/me/appointments?patient_id={child_a}"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{list}");
    assert_eq!(s(&list["items"][0]["id"]), aid);
    let (st, list) = call(
        &state,
        "GET",
        &format!("/api/v1/me/appointments?patient_id={child_b}"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{list}");
    assert_eq!(list["items"].as_array().unwrap().len(), 0);

    // ICS export and history through /me.
    let (st, ics) = send(
        &state,
        "GET",
        &format!("/api/v1/me/appointments/{aid}/ics?lang=es"),
        &token,
        "application/json",
        Vec::new(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{ics}");
    let ics = s(&ics);
    assert!(ics.contains("BEGIN:VCALENDAR"), "{ics}");
    assert!(
        !ics.contains(&child_a),
        "no patient identifier in the ICS: {ics}"
    );
    let (st, hist) = call(
        &state,
        "GET",
        &format!("/api/v1/me/appointments/{aid}/history"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{hist}");
    assert!(!hist["items"].as_array().unwrap().is_empty());

    // Another representative (no grant for child A) sees nothing.
    let (_, other_token) = create_representative(&state).await;
    let (st, v) = call(
        &state,
        "GET",
        &format!("/api/v1/me/appointments/{aid}"),
        &other_token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");

    // Cancel within policy from /me; the linked visit follows.
    let (st, cancelled) = call(
        &state,
        "POST",
        &format!("/api/v1/me/appointments/{aid}/cancel"),
        &token,
        Some(json!({ "version": acc["version"], "reason_code": "patient_request" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{cancelled}");
    assert_eq!(cancelled["status"], "cancelled");
    let visit_status: String = sqlx::query_scalar("SELECT status FROM visits WHERE id = $1")
        .bind(s(&acc["visit_id"]).parse::<Uuid>().unwrap())
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(visit_status, "cancelled");

    // A cancelled future appointment leaves "upcoming" and is kept in history.
    let ids = |v: &Value| -> Vec<String> {
        v["items"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| s(&a["id"]))
            .collect()
    };
    let (st, up) = call(
        &state,
        "GET",
        &format!("/api/v1/me/appointments?patient_id={child_a}&range=upcoming"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{up}");
    assert!(!ids(&up).contains(&aid), "cancelled still upcoming: {up}");
    let (st, past) = call(
        &state,
        "GET",
        &format!("/api/v1/me/appointments?patient_id={child_a}&range=past"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{past}");
    assert!(
        ids(&past).contains(&aid),
        "cancelled missing from history: {past}"
    );
}

#[tokio::test]
async fn preferences_roundtrip_with_validation_and_versions() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let patient = register_patient(&state, &facility).await;
    let (uid, token) = create_representative(&state).await;
    create_grant(&state, uid, &patient, "self", None).await;

    let (st, p) = call(&state, "GET", "/api/v1/me/preferences", &token, None).await;
    assert_eq!(st, StatusCode::OK, "{p}");
    assert_eq!(p["channels"], json!(["in_app"]));
    assert_eq!(p["version"], 0, "no stored preferences yet: {p}");

    let body = json!({
        "available_windows": [{ "weekday": 1, "start": "09:00:00", "end": "12:00:00" }],
        "unavailable_windows": [{ "weekday": 5, "start": "14:00:00", "end": "18:00:00" }],
        "preferred_modalities": ["in_person"],
        "preferred_facility_ids": [facility],
        "language": "ES",
        "accessibility_needs": [],
        "time_zone": "Europe/Madrid",
        "channels": ["email"],
        "quiet_hours_start": "22:00:00",
        "quiet_hours_end": "07:00:00",
        "version": 0,
    });
    let (st, saved) = call(
        &state,
        "PUT",
        "/api/v1/me/preferences",
        &token,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{saved}");
    assert_eq!(saved["language"], "es");
    assert_eq!(saved["time_zone"], "Europe/Madrid");
    assert_eq!(
        saved["channels"],
        json!(["in_app", "email"]),
        "in_app is always kept"
    );
    assert_eq!(saved["available_windows"][0]["weekday"], 1);
    assert_eq!(saved["has_contact_email"], false);
    let v = saved["version"].as_i64().unwrap();
    assert!(v >= 1);

    // Stale version -> 409; wrong time zone / window / facility -> 400.
    let (st, stale) = call(
        &state,
        "PUT",
        "/api/v1/me/preferences",
        &token,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{stale}");
    let mut bad = body.clone();
    bad["version"] = json!(v);
    bad["time_zone"] = json!("Mars/Olympus");
    let (st, e) = call(&state, "PUT", "/api/v1/me/preferences", &token, Some(bad)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    let mut bad = body.clone();
    bad["version"] = json!(v);
    bad["available_windows"] = json!([{ "weekday": 8, "start": "09:00:00", "end": "12:00:00" }]);
    let (st, e) = call(&state, "PUT", "/api/v1/me/preferences", &token, Some(bad)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    let mut bad = body.clone();
    bad["version"] = json!(v);
    bad["preferred_facility_ids"] = json!([Uuid::now_v7()]);
    let (st, e) = call(&state, "PUT", "/api/v1/me/preferences", &token, Some(bad)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");
    let mut bad = body.clone();
    bad["version"] = json!(v);
    bad["channels"] = json!(["carrier_pigeon"]);
    let (st, e) = call(&state, "PUT", "/api/v1/me/preferences", &token, Some(bad)).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{e}");

    // Contact details are sealed at rest and never echoed.
    let mut with_email = body.clone();
    with_email["version"] = json!(v);
    with_email["contact_email"] = json!("synthetic.parent@example.org");
    let (st, saved) = call(
        &state,
        "PUT",
        "/api/v1/me/preferences",
        &token,
        Some(with_email),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{saved}");
    assert_eq!(saved["has_contact_email"], true);
    assert!(saved.get("contact_email").is_none());
    let stored: Option<Vec<u8>> = sqlx::query_scalar(
        "SELECT contact_email_enc FROM patient_scheduling_preferences WHERE patient_id = $1",
    )
    .bind(patient.parse::<Uuid>().unwrap())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let stored = stored.expect("sealed email stored");
    assert!(
        !String::from_utf8_lossy(&stored).contains("synthetic.parent"),
        "email must not be stored in clear"
    );

    // Another patient's preferences are unreachable; staff cannot use /me.
    let other = register_patient(&state, &facility).await;
    let (st, e) = call(
        &state,
        "GET",
        &format!("/api/v1/me/preferences?patient_id={other}"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{e}");
    let (st, e) = call(&state, "GET", "/api/v1/me/preferences", REG, None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{e}");
}

const ICS_WITH_PRIVATE_CONTENT: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:-//synthetic//EN\r\n\
BEGIN:VEVENT\r\nUID:evt-1@synthetic\r\nDTSTART:__START__\r\nDTEND:__END__\r\n\
SUMMARY:Therapy session with Dr Secret\r\nDESCRIPTION:Very private details https://meet.example.org/abc\r\n\
ATTENDEE:mailto:friend@example.org\r\nRRULE:FREQ=WEEKLY;COUNT=6\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

fn ics_sample() -> (String, String) {
    let start = (chrono::Utc::now() + chrono::Duration::days(3))
        .date_naive()
        .and_hms_opt(10, 0, 0)
        .unwrap()
        .and_utc();
    let end = start + chrono::Duration::hours(1);
    let fmt = |t: chrono::DateTime<chrono::Utc>| t.format("%Y%m%dT%H%M%SZ").to_string();
    (
        ICS_WITH_PRIVATE_CONTENT
            .replace("__START__", &fmt(start))
            .replace("__END__", &fmt(end)),
        fmt(start),
    )
}

#[tokio::test]
async fn calendar_import_is_consented_bounded_and_privacy_preserving() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let patient = register_patient(&state, &facility).await;
    let patient_uuid: Uuid = patient.parse().unwrap();
    let (uid, token) = create_representative(&state).await;
    create_grant(&state, uid, &patient, "self", None).await;
    let (ics, _) = ics_sample();

    // Consent first.
    let (st, v) = send(
        &state,
        "POST",
        "/api/v1/me/calendars/ics",
        &token,
        "text/calendar",
        ics.clone().into_bytes(),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "consent_required");
    let (st, consents) = call(&state, "GET", "/api/v1/me/consents", &token, None).await;
    assert_eq!(st, StatusCode::OK, "{consents}");
    assert!(consents["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["purpose"] == "scheduling_calendar" && c["status"] == "revoked"));
    // Only scheduling purposes are self-service.
    let (st, v) = call(
        &state,
        "POST",
        "/api/v1/me/consents",
        &token,
        Some(json!({ "purpose": "research", "status": "active" })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    set_consent(&state, &token, &patient, "scheduling_calendar", "active").await;

    // Import: 6 weekly occurrences within the horizon become 6 intervals.
    let (st, src) = send(
        &state,
        "POST",
        "/api/v1/me/calendars/ics?time_zone=Europe/Madrid",
        &token,
        "text/calendar",
        ics.clone().into_bytes(),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{src}");
    assert_eq!(src["source_type"], "ics_import");
    assert_eq!(src["status"], "connected");
    assert_eq!(src["interval_count"], 6);
    assert_eq!(src["import"]["events_seen"], 1);
    assert_eq!(src["integrity_hash"].as_str().unwrap().len(), 64);
    let sid: Uuid = s(&src["id"]).parse().unwrap();
    let out = src.to_string();
    for secret in [
        "Dr Secret",
        "Very private",
        "meet.example.org",
        "friend@example.org",
    ] {
        assert!(!out.contains(secret), "{secret} leaked in response: {out}");
    }

    // Nothing but intervals and metadata is persisted anywhere.
    let stored: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM patient_busy_intervals WHERE source_id = $1")
            .bind(sid)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(stored, 6);
    let leaked: i64 = sqlx::query_scalar(
        "SELECT COUNT(*) FROM (
            SELECT to_jsonb(s)::text AS t FROM patient_calendar_sources s WHERE s.id = $1
            UNION ALL SELECT to_jsonb(b)::text FROM patient_busy_intervals b WHERE b.source_id = $1
            UNION ALL SELECT resource_refs::text FROM outbox_events WHERE resource_refs->>'source_id' = $2
         ) x WHERE x.t ILIKE '%Dr Secret%' OR x.t ILIKE '%Very private%' OR x.t ILIKE '%friend@example%'",
    )
    .bind(sid)
    .bind(sid.to_string())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(leaked, 0);
    let audited: Vec<String> = sqlx::query_scalar(
        "SELECT event_type FROM outbox_events WHERE resource_refs->>'source_id' = $1 ORDER BY occurred_at, id",
    )
    .bind(sid.to_string())
    .fetch_all(&state.pool)
    .await
    .unwrap();
    assert_eq!(audited, vec!["patient_calendar.connected".to_string()]);

    // Re-sync replaces, never accumulates.
    let (st, again) = send(
        &state,
        "POST",
        &format!("/api/v1/me/calendars/ics?source_id={sid}"),
        &token,
        "text/calendar",
        ics.clone().into_bytes(),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{again}");
    assert_eq!(again["interval_count"], 6);
    let stored: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM patient_busy_intervals WHERE source_id = $1")
            .bind(sid)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(stored, 6);

    // Bounds and malformed input.
    let huge = vec![b'X'; 1_000_001];
    let (st, v) = send(
        &state,
        "POST",
        "/api/v1/me/calendars/ics",
        &token,
        "text/calendar",
        huge,
    )
    .await;
    assert_eq!(st, StatusCode::PAYLOAD_TOO_LARGE, "{v}");
    let (st, v) = send(
        &state,
        "POST",
        "/api/v1/me/calendars/ics",
        &token,
        "text/calendar",
        b"not a calendar at all".to_vec(),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(code(&v), "calendar_invalid");
    let bomb = format!(
        "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nUID:b\r\nDTSTART:{}\r\nDTEND:{}\r\nRRULE:FREQ=MINUTELY\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n",
        (chrono::Utc::now() + chrono::Duration::days(1)).format("%Y%m%dT%H%M%SZ"),
        (chrono::Utc::now() + chrono::Duration::days(1) + chrono::Duration::minutes(1))
            .format("%Y%m%dT%H%M%SZ")
    );
    let (st, v) = send(
        &state,
        "POST",
        "/api/v1/me/calendars/ics",
        &token,
        "text/calendar",
        bomb.into_bytes(),
    )
    .await;
    assert!(
        st == StatusCode::BAD_REQUEST || st == StatusCode::CREATED,
        "recurrence expansion must be bounded, not exhaust resources: {st} {v}"
    );
    if st == StatusCode::CREATED {
        assert!(v["interval_count"].as_i64().unwrap() <= 5_000, "{v}");
    }

    // Device free/busy sync.
    let now = chrono::Utc::now();
    let (st, dev) = call(
        &state,
        "POST",
        "/api/v1/me/calendars/device-sync",
        &token,
        Some(json!({
            "time_zone": "Europe/Madrid",
            "intervals": [
                { "starts_at": now + chrono::Duration::days(2), "ends_at": now + chrono::Duration::days(2) + chrono::Duration::hours(1) },
                { "starts_at": now + chrono::Duration::days(2) + chrono::Duration::minutes(30), "ends_at": now + chrono::Duration::days(2) + chrono::Duration::hours(2) },
                { "starts_at": now - chrono::Duration::days(30), "ends_at": now - chrono::Duration::days(29) },
            ],
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{dev}");
    assert_eq!(dev["source_type"], "device_sync");
    assert_eq!(
        dev["interval_count"], 1,
        "overlaps merge, past is skipped: {dev}"
    );
    assert_eq!(dev["import"]["intervals_skipped"], 1);
    let dev_id: Uuid = s(&dev["id"]).parse().unwrap();
    let (st, v) = call(
        &state,
        "POST",
        "/api/v1/me/calendars/device-sync",
        &token,
        Some(json!({ "time_zone": "Europe/Madrid",
                     "intervals": [{ "starts_at": now, "ends_at": now }] })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");

    let (st, list) = call(&state, "GET", "/api/v1/me/calendars", &token, None).await;
    assert_eq!(st, StatusCode::OK, "{list}");
    assert_eq!(list["calendar_consent"], true);
    assert_eq!(
        list["items"].as_array().unwrap().len(),
        2 + usize::from(bomb_connected(&list))
    );

    // Another representative cannot see or disconnect these sources.
    let (_, other_token) = create_representative(&state).await;
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/me/calendars/{sid}/disconnect"),
        &other_token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");

    // Disconnect deletes derived intervals; idempotent.
    let (st, disc) = call(
        &state,
        "POST",
        &format!("/api/v1/me/calendars/{sid}/disconnect"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{disc}");
    assert_eq!(disc["status"], "disconnected");
    assert_eq!(disc["interval_count"], 0);
    let stored: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM patient_busy_intervals WHERE source_id = $1")
            .bind(sid)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(stored, 0);
    let (st, disc2) = call(
        &state,
        "POST",
        &format!("/api/v1/me/calendars/{sid}/disconnect"),
        &token,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{disc2}");
    let (st, v) = send(
        &state,
        "POST",
        &format!("/api/v1/me/calendars/ics?source_id={sid}"),
        &token,
        "text/calendar",
        ics.into_bytes(),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::CONFLICT,
        "re-sync of a disconnected source: {v}"
    );

    // Withdrawing calendar consent disconnects everything that is left.
    let (st, withdrawn) = call(
        &state,
        "POST",
        "/api/v1/me/consents",
        &token,
        Some(json!({ "purpose": "scheduling_calendar", "status": "revoked" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{withdrawn}");
    assert!(
        withdrawn["calendars_disconnected"].as_u64().unwrap() >= 1,
        "{withdrawn}"
    );
    let remaining: i64 =
        sqlx::query_scalar("SELECT COUNT(*) FROM patient_busy_intervals WHERE patient_id = $1")
            .bind(patient_uuid)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(remaining, 0);
    let dev_status: String =
        sqlx::query_scalar("SELECT status FROM patient_calendar_sources WHERE id = $1")
            .bind(dev_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(dev_status, "disconnected");
    let (st, v) = call(
        &state,
        "POST",
        "/api/v1/me/calendars/device-sync",
        &token,
        Some(json!({ "time_zone": "UTC", "intervals": [] })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    assert_eq!(code(&v), "consent_required");
}

fn bomb_connected(list: &Value) -> bool {
    list["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|i| i["status"] == "connected")
        .count()
        > 2
}

#[tokio::test]
async fn imported_busy_time_excludes_matcher_candidates() {
    let state = test_state().await;
    let facility = main_facility(&state).await;
    let service = create_service(&state).await;
    create_professional(&state, &facility, &service).await;
    let patient = register_patient(&state, &facility).await;
    let (uid, token) = create_representative(&state).await;
    create_grant(&state, uid, &patient, "self", None).await;
    set_consent(&state, &token, &patient, "scheduling_calendar", "active").await;

    // The patient is busy every day 08:00-18:00 UTC for the whole horizon
    // except one day; the professional only works those hours. The free day
    // is the first weekday five or more days ahead, so the facility's
    // weekday opening hours never leave it without slots.
    let now = chrono::Utc::now();
    let free_day = (5..12)
        .map(|d| (now + chrono::Duration::days(d)).date_naive())
        .find(|d| chrono::Datelike::weekday(d).number_from_monday() <= 5)
        .unwrap();
    let mut intervals = Vec::new();
    for d in 0..=120 {
        let day = (now + chrono::Duration::days(d)).date_naive();
        if day == free_day {
            continue;
        }
        let start = day.and_hms_opt(0, 0, 0).unwrap().and_utc();
        intervals
            .push(json!({ "starts_at": start, "ends_at": start + chrono::Duration::hours(24) }));
    }
    let (st, dev) = call(
        &state,
        "POST",
        "/api/v1/me/calendars/device-sync",
        &token,
        Some(json!({ "time_zone": "UTC", "intervals": intervals })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{dev}");

    let (st, req) = call(
        &state,
        "POST",
        "/api/v1/me/access-requests",
        &token,
        Some(json!({
            "facility_id": facility,
            "free_text": "Follow-up",
            "constraints": { "service_code": service, "facility_ids": [facility] },
        })),
    )
    .await;
    assert_eq!(st, StatusCode::CREATED, "{req}");
    let rid = s(&req["id"]);
    let (st, m) = call(
        &state,
        "POST",
        &format!("/api/v1/me/access-requests/{rid}/match"),
        &token,
        Some(json!({ "version": req["version"] })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{m}");
    let offers = m["offers"].as_array().unwrap();
    assert!(!offers.is_empty(), "{m}");
    for o in offers {
        let starts: chrono::DateTime<chrono::Utc> = s(&o["starts_at"]).parse().unwrap();
        assert_eq!(
            starts.date_naive(),
            free_day,
            "every option must fall on the only free day: {o}"
        );
    }
    assert!(
        m["rejected_summary"].to_string().contains("busy")
            || m["rejected_summary"].to_string().contains("calendar"),
        "rejections explain the calendar conflicts: {}",
        m["rejected_summary"]
    );
}
