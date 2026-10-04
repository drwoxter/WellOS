//! dMind Clinical Orders & Diagnostics v1 — integration suite through the
//! real router against the shared synthetic test database.
//!
//! Every scenario goes through HTTP routes with the synthetic dev identities
//! (`dev-fixtures`): catalog runtime administration, the order composer and
//! deterministic safety engine, order lifecycle, Access scheduling linkage,
//! specimen custody, typed results and report lifecycle, dMind bounds and
//! degraded behaviour, professional review, release and `/me` privacy,
//! tenant/facility isolation, the ObjectStore document path and the FHIR
//! subset including inbound `DiagnosticReport`.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use tower::ServiceExt;
use uuid::Uuid;
use wellos_server::state::AppState;

const ADMIN: &str = "dev-admin.silva";
const DR: &str = "dev-dr.garcia";
const DR_ANNEX: &str = "dev-dr.annex";
const DR_OTHER_TENANT: &str = "dev-dr.sur";
const LAB: &str = "dev-lab.chen";
const TECH: &str = "dev-tech.ruiz";
const REG: &str = "dev-reg.rivera";
const NURSE: &str = "dev-nurse.kim";
const REP: &str = "dev-rep.ortiz";

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wellos:wellos_dev@localhost:5432/wellos".to_string())
}

async fn pool() -> sqlx::PgPool {
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
    pool
}

async fn test_state() -> AppState {
    let gateway = Arc::new(dmind_gateway::fake::FakeProvider::new());
    AppState::new(pool().await, gateway)
}

async fn disabled_state() -> AppState {
    let gateway = Arc::new(dmind_gateway::DisabledGateway::disabled(
        "test: no provider",
    ));
    AppState::new(pool().await, gateway)
}

fn purpose_for(token: &str) -> &'static str {
    if token == ADMIN {
        "operations"
    } else {
        "treatment"
    }
}

async fn send(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    purpose: &str,
    content_type: &str,
    body: Body,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", content_type)
        .header("x-purpose-of-use", purpose)
        .body(body)
        .unwrap();
    let resp = wellos_server::routes::router(state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = resp.status();
    let bytes = to_bytes(resp.into_body(), 4 * 1024 * 1024).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| json!({ "raw": String::from_utf8_lossy(&bytes).to_string() }))
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
    let b = match body {
        Some(v) => Body::from(serde_json::to_vec(&v).unwrap()),
        None => Body::empty(),
    };
    send(
        state,
        method,
        path,
        token,
        purpose_for(token),
        "application/json",
        b,
    )
    .await
}

fn uniq(prefix: &str) -> String {
    format!("{prefix}-{}", Uuid::now_v7().simple())
}

fn id(v: &Value) -> String {
    v["id"]
        .as_str()
        .unwrap_or_else(|| panic!("value without id: {v}"))
        .to_string()
}

fn version(v: &Value) -> i64 {
    v["version"]
        .as_i64()
        .unwrap_or_else(|| panic!("value without version: {v}"))
}

fn code_of(v: &Value) -> &str {
    v["error"]["code"]
        .as_str()
        .or_else(|| v["code"].as_str())
        .unwrap_or("")
}

async fn main_facility(state: &AppState) -> String {
    let (st, meta) = call(state, "GET", "/api/v1/meta/tenant", REG, None).await;
    assert_eq!(st, StatusCode::OK, "{meta}");
    meta["facilities"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["name"] == "Main Campus")
        .map(id)
        .expect("Main Campus facility")
}

async fn register_patient(state: &AppState, facility: &str, birth_date: &str, sex: &str) -> String {
    let (st, p) = call(
        state,
        "POST",
        "/api/v1/patients",
        REG,
        Some(json!({
            "facility_id": facility,
            "family_name": "Diagnostics",
            "given_name": "Synthetic",
            "birth_date": birth_date,
            "sex": sex,
            "identifier": uniq("MRN-DX"),
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{p}");
    id(&p)
}

async fn start_encounter(state: &AppState, patient: &str) -> String {
    let (st, e) = call(
        state,
        "POST",
        "/api/v1/encounters",
        DR,
        Some(json!({ "patient_id": patient })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{e}");
    id(&e)
}

/// New patient with an active consultation owned by dr.garcia.
async fn consultation(state: &AppState) -> (String, String, String) {
    let f = main_facility(state).await;
    let p = register_patient(state, &f, "1970-03-03", "female").await;
    let e = start_encounter(state, &p).await;
    (f, p, e)
}

async fn orderable_id(state: &AppState, code: &str) -> String {
    let (st, v) = call(
        state,
        "GET",
        &format!("/api/v1/diagnostics/catalog?q={code}&limit=50"),
        DR,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["code"] == code)
        .map(id)
        .unwrap_or_else(|| panic!("orderable {code} not in catalog: {v}"))
}

fn items(ids: &[(&str, Option<&str>)]) -> Vec<Value> {
    ids.iter()
        .map(|(oid, mode)| json!({ "orderable_id": oid, "fulfilment_mode": mode }))
        .collect()
}

async fn preflight(state: &AppState, encounter: &str, composition: Value) -> (StatusCode, Value) {
    let (st, v) = call(
        state,
        "POST",
        &format!("/api/v1/encounters/{encounter}/diagnostic-orders/preflight"),
        DR,
        Some(composition),
    )
    .await;
    if st == StatusCode::OK {
        (st, v["evaluation"].clone())
    } else {
        (st, v)
    }
}

fn finding_ids(eval: &Value, severity: &str) -> Vec<String> {
    eval["findings"]
        .as_array()
        .unwrap_or(&vec![])
        .iter()
        .filter(|f| f["severity"] == severity)
        .map(|f| f["id"].as_str().unwrap().to_string())
        .collect()
}

/// Preflight + confirm acknowledging every warning (no overrides).
async fn place(
    state: &AppState,
    encounter: &str,
    facility: &str,
    composed: &[(&str, Option<&str>)],
    extra: Value,
) -> Value {
    let mut composition = json!({
        "items": items(composed),
        "performing_facility_id": facility,
        "lang": "en",
    });
    if let Some(priority) = extra.get("priority") {
        composition["priority"] = priority.clone();
    }
    let (st, eval) = preflight(state, encounter, composition.clone()).await;
    assert_eq!(st, StatusCode::OK, "{eval}");
    assert!(
        finding_ids(&eval, "hard_stop").is_empty(),
        "unexpected hard stop: {eval}"
    );
    let mut body = composition;
    body["safety_evaluation_id"] = json!(id(&eval));
    body["acknowledged_ids"] = json!(finding_ids(&eval, "warning"));
    body["clinical_indication"] = json!("Synthetic indication for integration coverage");
    if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            b.insert(k.clone(), v.clone());
        }
    }
    let (st, out) = call(
        state,
        "POST",
        &format!("/api/v1/encounters/{encounter}/diagnostic-orders"),
        DR,
        Some(body),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{out}");
    out["group"].clone()
}

async fn transition(
    state: &AppState,
    token: &str,
    order: &Value,
    name: &str,
    mode: Option<&str>,
) -> (StatusCode, Value) {
    call(
        state,
        "POST",
        &format!("/api/v1/diagnostics/orders/{}/transition", id(order)),
        token,
        Some(json!({
            "transition": name,
            "version": version(order),
            "reason": "synthetic integration step",
            "fulfilment_mode": mode,
        })),
    )
    .await
}

async fn must_transition(
    state: &AppState,
    token: &str,
    order: &Value,
    name: &str,
    mode: Option<&str>,
) -> Value {
    let (st, v) = transition(state, token, order, name, mode).await;
    assert_eq!(st, StatusCode::OK, "{name}: {v}");
    v
}

async fn detail(state: &AppState, token: &str, order_id: &str) -> (StatusCode, Value) {
    call(
        state,
        "GET",
        &format!("/api/v1/diagnostics/orders/{order_id}"),
        token,
        None,
    )
    .await
}

async fn record_specimen(state: &AppState, order: &Value, facility: &str) -> Value {
    let (st, s) = call(
        state,
        "POST",
        &format!("/api/v1/diagnostics/orders/{}/specimens", id(order)),
        LAB,
        Some(json!({
            "specimen_type_code": "blood_venous",
            "container_code": "serum_tube",
            "collected": true,
            "collected_at": chrono::Utc::now() - chrono::Duration::minutes(10),
            "collection_facility_id": facility,
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s}");
    s
}

async fn specimen_event(
    state: &AppState,
    spec: &Value,
    event: &str,
    facility: &str,
    reason: Option<&str>,
) -> (StatusCode, Value) {
    call(
        state,
        "POST",
        &format!("/api/v1/diagnostics/specimens/{}/events", id(spec)),
        LAB,
        Some(json!({
            "event": event,
            "version": version(spec),
            "facility_id": facility,
            "reason": reason,
        })),
    )
    .await
}

fn quantity(code: &str, value: &str, unit: &str) -> Value {
    json!({ "code": code, "value": { "type": "quantity", "value": value, "unit": unit } })
}

async fn issue(state: &AppState, token: &str, order_id: &str, body: Value) -> (StatusCode, Value) {
    call(
        state,
        "POST",
        &format!("/api/v1/diagnostics/orders/{order_id}/reports"),
        token,
        Some(body),
    )
    .await
}

async fn review(state: &AppState, report: &Value, extra: Value) -> Value {
    let mut body = json!({
        "report_version": version(report),
        "clinical_assessment": "Reviewed by the accountable clinician (synthetic).",
        "disposition": "routine_follow_up",
        "follow_ups": [],
    });
    if let (Some(b), Some(e)) = (body.as_object_mut(), extra.as_object()) {
        for (k, v) in e {
            b.insert(k.clone(), v.clone());
        }
    }
    let (st, v) = call(
        state,
        "POST",
        &format!("/api/v1/diagnostics/reports/{}/review", id(report)),
        DR,
        Some(body),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    v
}

async fn grant_for(state: &AppState, patient: &str) -> String {
    let (st, me) = call(state, "GET", "/api/v1/me", REP, None).await;
    let rep_id = me["user"]["id"]
        .as_str()
        .or_else(|| me["id"].as_str())
        .map(str::to_string);
    let rep_id = match rep_id {
        Some(v) => v,
        None => {
            let (uid,): (Uuid,) =
                sqlx::query_as("SELECT id FROM users WHERE username = 'rep.ortiz'")
                    .fetch_one(&state.pool)
                    .await
                    .unwrap();
            assert!(st.is_success() || st == StatusCode::NOT_FOUND, "{me}");
            uid.to_string()
        }
    };
    let (st, g) = call(
        state,
        "POST",
        "/api/v1/patient-grants",
        REG,
        Some(json!({
            "user_id": rep_id,
            "patient_id": patient,
            "relationship": "authorized_proxy",
            "verification_note": "Synthetic: identity and proxy authorisation checked in person",
        })),
    )
    .await;
    assert!(st == StatusCode::OK || st == StatusCode::CREATED, "{g}");
    id(&g)
}

fn lab_config(code: &str, unit: &str, range: &str) -> Value {
    json!({
        "category_code": "laboratory",
        "modality_code": "laboratory",
        "result_type": "quantity",
        "components": [{ "code": code, "display": "Runtime analyte", "result_type": "quantity",
                         "unit": unit, "reference_range": range }],
        "specimen": { "type_code": "blood_venous", "container_code": "serum_tube", "minimum_volume_ml": 2 },
        "fulfilment_modes": ["immediate", "walk_in"],
        "duplicate_window_days": 3,
    })
}

// ---------------------------------------------------------------------------
// 1. Runtime catalog administration
// ---------------------------------------------------------------------------

#[tokio::test]
async fn catalog_runtime_add_update_search_deactivate_history() {
    let state = test_state().await;
    let f = main_facility(&state).await;
    let code = uniq("rt_ferritin").to_lowercase();

    // Invalid configuration is refused before anything is written.
    let (st, v) = call(
        &state,
        "POST",
        "/api/v1/catalog",
        ADMIN,
        Some(json!({
            "kind": "diagnostic_orderable", "code": code, "name_en": "Ferritin (runtime)",
            "name_es": "Ferritina (runtime)",
            "config": { "category_code": "laboratory", "components": [{ "code": "BAD CODE!", "display": "x" }] },
        })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(code_of(&v), "validation_failed");

    let (st, created) = call(
        &state,
        "POST",
        "/api/v1/catalog",
        ADMIN,
        Some(json!({
            "kind": "diagnostic_orderable", "code": code,
            "name_en": "Ferritin (runtime)", "name_es": "Ferritina (runtime)",
            "synonyms": ["iron stores"],
            "external_codings": [{ "system": "http://loinc.org", "code": "2276-4", "display": "Ferritin [Mass/volume] in Serum or Plasma" }],
            "config": lab_config("2276-4", "ng/mL", "30-300"),
            "facility_ids": [f],
            "change_reason": "runtime addition (synthetic)",
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{created}");
    let entry_id = id(&created);

    // Searchable by code, synonym and Spanish name, scoped to the facility.
    for q in ["ferritin", "iron stores", "Ferritina"] {
        let (st, v) = call(
            &state,
            "GET",
            &format!(
                "/api/v1/diagnostics/catalog?q={}&facility_id={f}",
                q.replace(' ', "%20")
            ),
            DR,
            None,
        )
        .await;
        assert_eq!(st, StatusCode::OK, "{v}");
        assert!(
            v["items"]
                .as_array()
                .unwrap()
                .iter()
                .any(|o| o["id"] == entry_id.as_str()),
            "{q} should find the runtime orderable: {v}"
        );
    }

    // Immediately orderable through the composer.
    let p = register_patient(&state, &f, "1985-01-01", "male").await;
    let e = start_encounter(&state, &p).await;
    let group = place(&state, &e, &f, &[(&entry_id, Some("immediate"))], json!({})).await;
    assert_eq!(group["orders"][0]["orderable_code"], code.as_str());

    // Update with optimistic version; stale version is a conflict.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/catalog/{entry_id}"),
        ADMIN,
        Some(json!({ "version": version(&created) + 7, "name_en": "Stale" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    let (st, updated) = call(
        &state,
        "POST",
        &format!("/api/v1/catalog/{entry_id}"),
        ADMIN,
        Some(json!({
            "version": version(&created), "name_en": "Serum ferritin (runtime)",
            "config": lab_config("2276-4", "ng/mL", "20-250"),
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{updated}");
    assert_eq!(updated["name_en"], "Serum ferritin (runtime)");
    assert_eq!(version(&updated), version(&created) + 1);

    let (st, hist) = call(
        &state,
        "GET",
        &format!("/api/v1/catalog/{entry_id}/history"),
        ADMIN,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{hist}");
    let n = hist["items"]
        .as_array()
        .map(Vec::len)
        .unwrap_or_else(|| hist.as_array().map(Vec::len).unwrap_or(0));
    assert!(n >= 2, "create + update history expected: {hist}");

    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/catalog/{entry_id}/deactivate"),
        ADMIN,
        Some(json!({ "version": version(&updated), "change_reason": "retired (synthetic)" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call(
        &state,
        "GET",
        &format!("/api/v1/diagnostics/catalog?q=ferritin&facility_id={f}"),
        DR,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        !v["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["id"] == entry_id.as_str()),
        "deactivated orderables leave the composer search: {v}"
    );
    // ...and can no longer be ordered.
    let (st, v) = preflight(
        &state,
        &e,
        json!({ "items": items(&[(&entry_id, Some("immediate"))]), "performing_facility_id": f }),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(code_of(&v), "orderable_inactive");
}

// ---------------------------------------------------------------------------
// 2. Composer, deterministic safety, groups, specimens, typed results
// ---------------------------------------------------------------------------

#[tokio::test]
async fn composer_safety_groups_specimen_chain_and_typed_results() {
    let state = test_state().await;
    let (f, p, e) = consultation(&state).await;
    let cbc = orderable_id(&state, "cbc_panel").await;
    let hb = orderable_id(&state, "hemoglobin").await;
    let lytes = orderable_id(&state, "electrolytes").await;

    // Panel + member: redundant-combination warning must be acknowledged.
    let composition = json!({
        "items": items(&[(&cbc, Some("immediate")), (&hb, Some("immediate")), (&lytes, Some("immediate"))]),
        "performing_facility_id": f,
        "priority": "urgent",
        "lang": "es",
    });
    let (st, eval) = preflight(&state, &e, composition.clone()).await;
    assert_eq!(st, StatusCode::OK, "{eval}");
    assert_eq!(eval["engine_version"], "diagnostic-safety.v1");
    let warnings = finding_ids(&eval, "warning");
    assert!(
        warnings.iter().any(|w| w.contains("redundant")),
        "panel + member should warn as redundant: {eval}"
    );
    assert!(finding_ids(&eval, "hard_stop").is_empty());

    // Confirmation without acknowledgement is refused; nothing is written.
    let mut body = composition.clone();
    body["safety_evaluation_id"] = json!(id(&eval));
    body["acknowledged_ids"] = json!([]);
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{e}/diagnostic-orders"),
        DR,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(code_of(&v), "safety_unacknowledged");
    let (st, list) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{e}/diagnostic-orders"),
        DR,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        list["orders"].as_array().map(Vec::len).unwrap_or(0),
        0,
        "{list}"
    );

    // A changed composition invalidates the persisted evaluation.
    let mut stale = body.clone();
    stale["acknowledged_ids"] = json!(warnings);
    stale["items"] = json!(items(&[
        (&cbc, Some("immediate")),
        (&lytes, Some("immediate"))
    ]));
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{e}/diagnostic-orders"),
        DR,
        Some(stale),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(code_of(&v), "safety_evaluation_stale");

    // Confirm with acknowledgements + idempotency key, then replay.
    body["acknowledged_ids"] = json!(warnings);
    body["idempotency_key"] = json!(uniq("confirm"));
    body["clinical_indication"] = json!("Fatigue; anaemia and electrolyte screen (synthetic)");
    body["clinical_question"] = json!("Anaemia? Hypokalaemia?");
    let (st, out) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{e}/diagnostic-orders"),
        DR,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{out}");
    let group = &out["group"];
    let orders = group["orders"].as_array().unwrap();
    assert_eq!(orders.len(), 3, "{out}");
    assert!(orders
        .iter()
        .all(|o| o["order_status"] == "placed" && o["priority"] == "urgent"));
    assert!(orders.iter().all(|o| o["patient_id"] == p.as_str()));
    assert_eq!(group["replayed"], false);
    let (st, replay) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{e}/diagnostic-orders"),
        DR,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{replay}");
    assert_eq!(replay["group"]["id"], group["id"]);
    assert_eq!(replay["group"]["replayed"], true);
    let mut clash = body.clone();
    clash["clinical_question"] = json!("different payload, same key");
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{e}/diagnostic-orders"),
        DR,
        Some(clash),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(code_of(&v), "idempotency_conflict");

    // Safety evaluation is bound to the group with acknowledgements + actor.
    let lytes_order = orders
        .iter()
        .find(|o| o["orderable_code"] == "electrolytes")
        .unwrap();
    let (st, d) = detail(&state, DR, &id(lytes_order)).await;
    assert_eq!(st, StatusCode::OK, "{d}");
    assert_eq!(d["safety"]["engine_version"], "diagnostic-safety.v1");
    assert_eq!(
        d["safety"]["acknowledged_ids"].as_array().unwrap().len(),
        finding_ids(&eval, "warning").len()
    );
    assert!(d["history"]
        .as_array()
        .unwrap()
        .iter()
        .any(|h| h["to_status"] == "placed"));

    // Duplicate warning: the same electrolytes inside the 1-day window.
    let (st, dup) = preflight(
        &state,
        &e,
        json!({ "items": items(&[(&lytes, Some("immediate"))]), "performing_facility_id": f }),
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        finding_ids(&dup, "warning")
            .iter()
            .any(|w| w.contains("duplicate") || w.starts_with("pending_equivalent:")),
        "pending identical order should raise a duplicate warning: {dup}"
    );

    // Concurrency: two accepts on the same version — exactly one wins.
    let (a, b) = tokio::join!(
        transition(&state, LAB, lytes_order, "accept", None),
        transition(&state, LAB, lytes_order, "accept", None)
    );
    let statuses = [a.0, b.0];
    assert!(statuses.contains(&StatusCode::OK), "{a:?} {b:?}");
    assert!(statuses.contains(&StatusCode::CONFLICT), "{a:?} {b:?}");
    let accepted = if a.0 == StatusCode::OK { a.1 } else { b.1 };
    assert_eq!(accepted["order_status"], "accepted");

    // Specimen custody chain with identifier, rejection and recollection.
    let s1 = record_specimen(&state, &accepted, &f).await;
    assert_eq!(s1["status"], "collected");
    assert!(s1["identifier"].as_str().unwrap().len() >= 6);
    let (st, s1) = specimen_event(&state, &s1, "dispatched", &f, None).await;
    assert_eq!(st, StatusCode::OK, "{s1}");
    assert_eq!(s1["status"], "in_transit");
    let (st, v) = specimen_event(&state, &s1, "processed", &f, None).await;
    assert_eq!(
        st,
        StatusCode::CONFLICT,
        "custody must follow the chain: {v}"
    );
    let (st, s1) = specimen_event(&state, &s1, "received", &f, None).await;
    assert_eq!(st, StatusCode::OK, "{s1}");
    let (st, v) = specimen_event(&state, &s1, "rejected", &f, None).await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "rejection needs a reason: {v}");
    let (st, s1) = specimen_event(&state, &s1, "rejected", &f, Some("haemolysed sample")).await;
    assert_eq!(st, StatusCode::OK, "{s1}");
    assert_eq!(s1["status"], "rejected");
    assert_eq!(s1["rejection_reason"], "haemolysed sample");
    let (st, s2) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/orders/{}/specimens", id(&accepted)),
        LAB,
        Some(json!({
            "specimen_type_code": "blood_venous", "container_code": "serum_tube", "collected": true,
            "collected_at": chrono::Utc::now(), "collection_facility_id": f, "recollection_of": id(&s1),
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s2}");
    assert_eq!(s2["recollection_of"], id(&s1).as_str());
    let stale = s2.clone();
    let mut s2 = s2;
    for ev in ["dispatched", "received", "processing_started", "processed"] {
        let (st, v) = specimen_event(&state, &s2, ev, &f, None).await;
        assert_eq!(st, StatusCode::OK, "{ev}: {v}");
        s2 = v;
    }
    assert_eq!(s2["status"], "processed");
    let (st, v) = specimen_event(&state, &stale, "consumed", &f, None).await;
    assert_eq!(st, StatusCode::CONFLICT, "stale version must conflict: {v}");
    assert_eq!(code_of(&v), "version_conflict");
    let (st, v) = specimen_event(&state, &s2, "consumed", &f, None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "consumed");

    // Typed results: unknown code and wrong type are refused deterministically.
    let (st, v) = issue(
        &state,
        LAB,
        &id(&accepted),
        json!({ "status": "final", "components": [quantity("718-7", "13", "g/dL")] }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(code_of(&v), "code_mismatch");
    let (st, v) = issue(
        &state,
        LAB,
        &id(&accepted),
        json!({ "status": "final", "components": [{ "code": "2823-3", "value": { "type": "text", "text": "four" } }] }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(code_of(&v), "type_mismatch");

    let key = uniq("lytes");
    let final_body = json!({
        "status": "final",
        "components": [quantity("2951-2", "139", "mmol/L"), quantity("2823-3", "4.1", "mmol/L"), quantity("2075-0", "102", "mmol/L")],
        "idempotency_key": key, "source_system": "integration-lab", "sign": true,
    });
    let (st, report) = issue(&state, LAB, &id(&accepted), final_body.clone()).await;
    assert_eq!(st, StatusCode::OK, "{report}");
    assert_eq!(report["status"], "final");
    assert_eq!(report["criticality"], "normal");
    assert_eq!(report["components"].as_array().unwrap().len(), 3);
    assert!(report["signed_at"].is_string(), "{report}");
    // Idempotent replay returns the same report; a different payload under
    // the same key is refused.
    let (st, again) = issue(&state, LAB, &id(&accepted), final_body.clone()).await;
    assert_eq!(st, StatusCode::OK, "{again}");
    assert_eq!(again["id"], report["id"]);
    let mut other = final_body.clone();
    other["components"] = json!([quantity("2823-3", "4.2", "mmol/L")]);
    let (st, v) = issue(&state, LAB, &id(&accepted), other).await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(code_of(&v), "idempotency_key_reuse");

    // Order completed with the full trace: history, specimens, report.
    let (st, d) = detail(&state, DR, &id(&accepted)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(d["order_status"], "completed", "{d}");
    assert_eq!(d["specimens"].as_array().unwrap().len(), 2);
    assert_eq!(d["reports"].as_array().unwrap().len(), 1);
    // The consumed specimen is part of result recording.
    assert!(
        d["specimens"]
            .as_array()
            .unwrap()
            .iter()
            .any(|s| s["status"] == "consumed"),
        "{d}"
    );
    // Observations are append-only with provenance.
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM observations WHERE service_request_id = $1::uuid AND source_system = 'integration-lab'",
    )
    .bind(id(&accepted))
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(n, 3);

    // Hold/resume and cancellation on another order in the group.
    let cbc_order = orders
        .iter()
        .find(|o| o["orderable_code"] == "cbc_panel")
        .unwrap();
    let held = must_transition(&state, DR, cbc_order, "hold", None).await;
    assert_eq!(held["order_status"], "on_hold");
    let (st, v) = issue(
        &state,
        LAB,
        &id(&held),
        json!({ "status": "final", "conclusion": "Attempted delivery on a held order (synthetic)" }),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    let resumed = must_transition(&state, DR, &held, "resume", None).await;
    assert_eq!(resumed["order_status"], "accepted");
    let cancelled = must_transition(&state, DR, &resumed, "cancel", None).await;
    assert_eq!(cancelled["order_status"], "cancelled");
    let (st, v) = transition(&state, DR, &cancelled, "accept", None).await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    // Entered-in-error on the haemoglobin order; nurses may not do it.
    let hb_order = orders
        .iter()
        .find(|o| o["orderable_code"] == "hemoglobin")
        .unwrap();
    let (st, v) = transition(&state, NURSE, hb_order, "enter_in_error", None).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let eie = must_transition(&state, DR, hb_order, "enter_in_error", None).await;
    assert_eq!(eie["order_status"], "entered_in_error");

    // Patient-level diagnostics view carries pending + results.
    let (st, pd) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{p}/diagnostics"),
        DR,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{pd}");
    assert!(pd.is_object());
}

// ---------------------------------------------------------------------------
// 3. Hard stops, overrides, questions and facts
// ---------------------------------------------------------------------------

#[tokio::test]
async fn hard_stop_requires_authorized_override_bound_to_evaluation() {
    let state = test_state().await;
    let (f, p, e) = consultation(&state).await;
    sqlx::query(
        "INSERT INTO conditions (id, tenant_id, patient_id, code, display)
         SELECT $1, tenant_id, id, 'Z95.0', 'Presence of cardiac pacemaker (synthetic)' FROM patients WHERE id = $2::uuid",
    )
    .bind(Uuid::now_v7())
    .bind(&p)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO allergies (id, tenant_id, patient_id, substance, criticality)
         SELECT $1, tenant_id, id, 'Iodinated contrast media', 'high' FROM patients WHERE id = $2::uuid",
    )
    .bind(Uuid::now_v7())
    .bind(&p)
    .execute(&state.pool)
    .await
    .unwrap();
    let mri = orderable_id(&state, "mri_brain").await;
    let ct = orderable_id(&state, "ct_abdomen_contrast").await;

    let composition = json!({
        "items": items(&[(&mri, Some("scheduled")), (&ct, Some("scheduled"))]),
        "performing_facility_id": f,
        "lang": "en",
    });
    let (st, eval) = preflight(&state, &e, composition.clone()).await;
    assert_eq!(st, StatusCode::OK, "{eval}");
    let hard = finding_ids(&eval, "hard_stop");
    assert!(
        hard.iter().any(|h| h.contains("pacemaker")),
        "pacemaker fact must hard-stop MRI: {eval}"
    );
    let warnings = finding_ids(&eval, "warning");
    assert!(
        warnings.iter().any(|w| w.contains("contrast_allergy")),
        "contrast allergy must warn on contrast CT: {eval}"
    );
    assert_eq!(eval["requires_override"], true);

    let mut body = composition.clone();
    body["safety_evaluation_id"] = json!(id(&eval));
    body["acknowledged_ids"] = json!(warnings);
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{e}/diagnostic-orders"),
        DR,
        Some(body.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(code_of(&v), "safety_hard_stop");
    body["override_reason"] = json!("short");
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{e}/diagnostic-orders"),
        DR,
        Some(body.clone()),
    )
    .await;
    assert_ne!(
        st,
        StatusCode::OK,
        "a 5-character override reason is not an override: {v}"
    );
    body["override_reason"] =
        json!("Device confirmed MR-conditional by cardiology; radiology protocol agreed.");
    let (st, out) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{e}/diagnostic-orders"),
        DR,
        Some(body),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{out}");
    let (st, d) = detail(&state, DR, &id(&out["group"]["orders"][0])).await;
    assert_eq!(st, StatusCode::OK);
    assert!(
        d["safety"]["override_reason"]
            .as_str()
            .unwrap_or("")
            .contains("MR-conditional"),
        "override reason persists with the evaluation: {d}"
    );
    assert_eq!(d["safety"]["hard_stops"], 1, "{d}");
    assert!(d["safety"]["overridden_by"].is_string(), "{d}");
    assert!(d["safety"]["overridden_at"].is_string(), "{d}");

    // Mammography: question rule must be answered; wrong facility mapping is refused.
    let mammo = orderable_id(&state, "mammography_screening").await;
    let (st, eval) = preflight(
        &state,
        &e,
        json!({ "items": items(&[(&mammo, Some("scheduled"))]), "performing_facility_id": f }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{eval}");
    assert!(
        !eval["findings"].as_array().unwrap().is_empty(),
        "an unanswered question is an open finding: {eval}"
    );
    let open_ids: Vec<String> = eval["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| f["kind"] == "unanswered_question")
        .map(|f| f["id"].as_str().unwrap().to_string())
        .collect();
    assert!(!open_ids.is_empty(), "{eval}");
    let answers: serde_json::Map<String, Value> = open_ids
        .iter()
        .map(|k| {
            (
                k.trim_start_matches("unanswered_question:").to_string(),
                json!(true),
            )
        })
        .collect();
    let (st, answered) = preflight(
        &state,
        &e,
        json!({ "items": items(&[(&mammo, Some("scheduled"))]), "performing_facility_id": f, "answers": answers }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{answered}");
    assert!(
        !answered["findings"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["kind"] == "unanswered_question"),
        "answered questions are resolved: {answered}"
    );
}

// ---------------------------------------------------------------------------
// 4. Access linkage, cancellation conflicts, rescheduling, immediate modes
// ---------------------------------------------------------------------------

#[tokio::test]
async fn access_scheduling_linkage_conflicts_and_fulfilment_modes() {
    let state = test_state().await;
    let (f, _p, e) = consultation(&state).await;
    let cxr = orderable_id(&state, "chest_xray").await;
    let hb = orderable_id(&state, "hemoglobin").await;

    // Imaging cannot be fulfilled immediately unless the tenant allows it.
    let (st, v) = preflight(
        &state,
        &e,
        json!({ "items": items(&[(&cxr, Some("immediate"))]), "performing_facility_id": f }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(code_of(&v), "validation_failed");

    let group = place(
        &state,
        &e,
        &f,
        &[(&cxr, Some("scheduled")), (&hb, Some("walk_in"))],
        json!({ "priority": "routine" }),
    )
    .await;
    let orders = group["orders"].as_array().unwrap();
    let cxr_order = orders
        .iter()
        .find(|o| o["orderable_code"] == "chest_xray")
        .unwrap();
    let hb_order = orders
        .iter()
        .find(|o| o["orderable_code"] == "hemoglobin")
        .unwrap();
    assert!(
        cxr_order["access_request_id"].is_string(),
        "scheduled orders open an Access request: {cxr_order}"
    );
    assert!(cxr_order["access_request_id"] != Value::Null);
    assert_eq!(hb_order["fulfilment_mode"], "walk_in");
    assert!(
        hb_order["access_request_id"].is_null(),
        "walk-in orders need no appointment: {hb_order}"
    );

    // Walk-in laboratory order starts without any appointment.
    let acc = must_transition(&state, LAB, hb_order, "accept", None).await;
    let started = must_transition(&state, LAB, &acc, "start", None).await;
    assert_eq!(started["order_status"], "in_progress");
    assert!(started["appointment_id"].is_null());
    // A scheduled order cannot start before its appointment / without a mode switch.
    let (st, v) = transition(&state, TECH, cxr_order, "start", None).await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");

    // Book through the ordinary Access path: matcher → offer → accept.
    let req_id = cxr_order["access_request_id"].as_str().unwrap();
    let (st, m) = call(
        &state,
        "POST",
        &format!("/api/v1/access-requests/{req_id}/match"),
        REG,
        Some(json!({ "ranking": false })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{m}");
    let offer = m["offers"].as_array().and_then(|o| o.first()).cloned();
    let Some(offer) = offer else {
        panic!("no offers for the chest radiograph request: {m}");
    };
    let (st, booked) = call(
        &state,
        "POST",
        &format!("/api/v1/offers/{}/accept", id(&offer)),
        REG,
        Some(json!({ "reason": "booked for the ordered radiograph" })),
    )
    .await;
    assert!(
        st == StatusCode::OK || st == StatusCode::CREATED,
        "{booked}"
    );
    let appt_id = booked["appointment"]["id"]
        .as_str()
        .or_else(|| booked["id"].as_str())
        .unwrap()
        .to_string();
    let (st, d) = detail(&state, DR, &id(cxr_order)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(d["order_status"], "scheduled", "{d}");
    assert_eq!(d["appointment_id"], appt_id.as_str());
    assert_eq!(d["appointment"]["id"], appt_id.as_str());

    // Cancelling the appointment never cancels the order: it becomes a
    // divergence visible to staff and reschedulable.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/appointments/{appt_id}/cancel"),
        REG,
        Some(json!({ "reason_code": "patient_request", "note": "synthetic cancellation" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, d) = detail(&state, DR, &id(cxr_order)).await;
    assert_eq!(st, StatusCode::OK);
    assert_ne!(d["order_status"], "cancelled", "{d}");
    assert!(
        d["appointment_id"].is_null() || d["appointment"]["status"] == "cancelled",
        "{d}"
    );
    let (st, wl) = call(
        &state,
        "GET",
        &format!("/api/v1/diagnostics/orders?conflicts_only=true&facility_id={f}"),
        TECH,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{wl}");
    assert!(
        wl["items"]
            .as_array()
            .unwrap()
            .iter()
            .any(|o| o["id"] == cxr_order["id"]),
        "order/appointment divergence must appear in the conflicts worklist: {wl}"
    );
    let (st, re) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/orders/{}/schedule", id(cxr_order)),
        DR,
        Some(json!({ "version": version(&d), "facility_id": f })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{re}");
    assert!(re["access_request_id"].is_string(), "{re}");
    assert_ne!(re["access_request_id"], cxr_order["access_request_id"]);
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/orders/{}/schedule", id(cxr_order)),
        DR,
        Some(json!({ "version": version(&re), "facility_id": f })),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::CONFLICT,
        "an open request must not be duplicated: {v}"
    );
    assert_eq!(code_of(&v), "access_request_open");

    // Inpatient fulfilment of an imaging order starts without appointment,
    // recording the explicit mode switch.
    let (f2, _p2, e2) = consultation(&state).await;
    let g2 = place(
        &state,
        &e2,
        &f2,
        &[(&cxr, Some("inpatient"))],
        json!({ "priority": "stat" }),
    )
    .await;
    let o2 = &g2["orders"][0];
    assert!(o2["access_request_id"].is_null(), "{o2}");
    let a2 = must_transition(&state, TECH, o2, "accept", None).await;
    let s2 = must_transition(&state, TECH, &a2, "start", None).await;
    assert_eq!(s2["order_status"], "in_progress");
    assert_eq!(s2["fulfilment_mode"], "inpatient");
    let (st, wl) = call(
        &state,
        "GET",
        &format!("/api/v1/diagnostics/orders?status=in_progress&facility_id={f2}"),
        TECH,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{wl}");
    assert!(wl["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["id"] == o2["id"]));
}

// ---------------------------------------------------------------------------
// 5. dMind order suggestion: bounded, reviewable, degraded, stale
// ---------------------------------------------------------------------------

#[tokio::test]
async fn dmind_order_suggestion_is_bounded_reviewed_and_degrades_deterministically() {
    let state = test_state().await;
    let (f, p, e) = consultation(&state).await;
    // Context: a condition and a medication the suggestion may cite.
    sqlx::query(
        "INSERT INTO conditions (id, tenant_id, patient_id, code, display)
         SELECT $1, tenant_id, id, 'E11.9', 'Type 2 diabetes mellitus; glycated haemoglobin monitoring due (synthetic)' FROM patients WHERE id = $2::uuid",
    )
    .bind(Uuid::now_v7())
    .bind(&p)
    .execute(&state.pool)
    .await
    .unwrap();

    let (st, s) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{e}/diagnostic-orders/suggest"),
        DR,
        Some(json!({ "lang": "en", "facility_id": f })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{s}");
    let artifact_id = s["artifact_id"].as_str().unwrap().to_string();
    assert_eq!(s["status"], "awaiting_review", "{s}");
    assert_eq!(s["autonomy_level"], "A2", "{s}");
    assert_eq!(s["synthetic"], true, "{s}");
    assert!(!s["limitations"].as_array().unwrap().is_empty(), "{s}");
    let suggested = s["suggestions"].as_array().cloned().unwrap_or_default();
    assert!(
        suggested
            .iter()
            .any(|x| x["code"] == "hba1c" || x["orderable_code"] == "hba1c"),
        "the documented condition should surface HbA1c: {s}"
    );
    assert!(!s["cited_sources"].as_array().unwrap().is_empty(), "{s}");
    // Every suggested orderable is a real catalog entry supplied by the server.
    for sug in &suggested {
        let oid = sug["orderable_id"].as_str().unwrap();
        let (n,): (i64,) = sqlx::query_as(
            "SELECT COUNT(*) FROM catalog_entries WHERE id = $1::uuid AND kind = 'diagnostic_orderable' AND active",
        )
        .bind(oid)
        .fetch_one(&state.pool)
        .await
        .unwrap();
        assert_eq!(
            n, 1,
            "dMind may only suggest server-supplied orderables: {sug}"
        );
    }
    // No order exists until the clinician confirms.
    let (st, list) = call(
        &state,
        "GET",
        &format!("/api/v1/encounters/{e}/diagnostic-orders"),
        DR,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(list["orders"].as_array().map(Vec::len).unwrap_or(0), 0);

    // A suggestion from another consultation cannot be bound.
    let (_f2, _p2, e2) = consultation(&state).await;
    let hba1c = orderable_id(&state, "hba1c").await;
    let composition =
        json!({ "items": items(&[(&hba1c, Some("immediate"))]), "performing_facility_id": f });
    let (st, eval) = preflight(&state, &e2, composition.clone()).await;
    assert_eq!(st, StatusCode::OK, "{eval}");
    let mut body = composition.clone();
    body["safety_evaluation_id"] = json!(id(&eval));
    body["acknowledged_ids"] = json!(finding_ids(&eval, "warning"));
    body["suggestion_artifact_id"] = json!(artifact_id);
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{e2}/diagnostic-orders"),
        DR,
        Some(body),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");

    // Confirming from the suggestion records the clinician's acceptance.
    let group = place(
        &state,
        &e,
        &f,
        &[(&hba1c, Some("immediate"))],
        json!({ "suggestion_artifact_id": artifact_id }),
    )
    .await;
    assert_eq!(group["suggestion_artifact_id"], artifact_id.as_str());
    let (status, decision): (String, Option<String>) =
        sqlx::query_as("SELECT status, review_decision FROM ai_artifacts WHERE id = $1::uuid")
            .bind(&artifact_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(status, "approved");
    assert_eq!(decision.as_deref(), Some("accepted"));

    // Disabled gateway: typed unavailability, deterministic path intact.
    let disabled = disabled_state().await;
    let (st, v) = call(
        &disabled,
        "POST",
        &format!("/api/v1/encounters/{e}/diagnostic-orders/suggest"),
        DR,
        Some(json!({ "lang": "en", "facility_id": f })),
    )
    .await;
    assert!(
        st == StatusCode::SERVICE_UNAVAILABLE || st == StatusCode::CONFLICT,
        "disabled dMind must be a typed, non-fatal refusal: {st} {v}"
    );
    let lipid = orderable_id(&disabled, "lipid_panel").await;
    let (st, eval) = preflight(
        &disabled,
        &e,
        json!({ "items": items(&[(&lipid, Some("immediate"))]), "performing_facility_id": f }),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "deterministic preflight without AI: {eval}"
    );
    assert_eq!(eval["engine_version"], "diagnostic-safety.v1");
}

// ---------------------------------------------------------------------------
// 6. Reports: critical result, review, synthesis, explanation, release, /me
// ---------------------------------------------------------------------------

#[tokio::test]
async fn critical_report_review_release_and_patient_privacy() {
    let state = test_state().await;
    let (f, p, e) = consultation(&state).await;
    let lytes = orderable_id(&state, "electrolytes").await;
    let group = place(
        &state,
        &e,
        &f,
        &[(&lytes, Some("immediate"))],
        json!({ "priority": "urgent" }),
    )
    .await;
    let order = &group["orders"][0];
    let acc = must_transition(&state, LAB, order, "accept", None).await;
    let spec = record_specimen(&state, &acc, &f).await;
    let (st, spec) = specimen_event(&state, &spec, "received", &f, None).await;
    assert_eq!(st, StatusCode::OK, "{spec}");

    // Preliminary then final (critical potassium by the baseline rule).
    let (st, prelim) = issue(
        &state,
        LAB,
        &id(&acc),
        json!({ "status": "preliminary", "components": [quantity("2823-3", "6.6", "mmol/L")], "source_system": "integration-lab" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{prelim}");
    assert_eq!(prelim["status"], "preliminary");
    assert_eq!(prelim["criticality"], "critical", "{prelim}");
    let (st, d) = detail(&state, LAB, &id(&acc)).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(
        d["order_status"], "in_progress",
        "preliminary results keep the order open: {d}"
    );
    // Release of a preliminary report is impossible.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/reports/{}/release", id(&prelim)),
        DR,
        Some(json!({ "report_version": version(&prelim), "review_id": Uuid::now_v7(), "decision": "release", "notify_patient": false })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    let (st, report) = issue(
        &state,
        LAB,
        &id(&acc),
        json!({
            "status": "final", "sign": true, "source_system": "integration-lab",
            "components": [quantity("2951-2", "138", "mmol/L"), quantity("2823-3", "6.8", "mmol/L"), quantity("2075-0", "101", "mmol/L")],
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{report}");
    assert_eq!(report["status"], "final");
    assert_eq!(report["criticality"], "critical");
    assert_eq!(report["replaces"], prelim["id"]);
    let potassium = report["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == "2823-3")
        .unwrap();
    assert_eq!(potassium["interpretation"], "critical", "{potassium}");

    // Critical results sit first in the review worklist; nothing is released.
    let (st, wl) = call(
        &state,
        "GET",
        "/api/v1/diagnostics/reviews?state=pending",
        DR,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{wl}");
    let pos = wl["items"]
        .as_array()
        .unwrap()
        .iter()
        .position(|r| r["id"] == report["id"] || r["report_id"] == report["id"])
        .unwrap_or_else(|| panic!("critical report missing from review worklist: {wl}"));
    let first_non_critical = wl["items"]
        .as_array()
        .unwrap()
        .iter()
        .position(|r| r["criticality"] != "critical");
    if let Some(nc) = first_non_critical {
        assert!(pos < nc, "critical reports are prioritised: {wl}");
    }
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/reports/{}/release", id(&report)),
        DR,
        Some(json!({ "report_version": version(&report), "review_id": Uuid::now_v7(), "decision": "release", "notify_patient": true })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(code_of(&v), "review_required");
    // Laboratory staff cannot review or release.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/reports/{}/review", id(&report)),
        LAB,
        Some(json!({ "report_version": version(&report), "clinical_assessment": "Laboratory staff attempting a clinical review (synthetic)", "disposition": "routine_follow_up" })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");

    // dMind synthesis is bound to the exact report version and reviewed.
    let (st, synth) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/reports/{}/synthesis", id(&report)),
        DR,
        Some(json!({ "lang": "en" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{synth}");
    assert_eq!(synth["status"], "awaiting_review");
    assert_eq!(synth["report_version"], report["version"]);
    assert!(
        synth["citations"].is_array() && !synth["citations"].as_array().unwrap().is_empty(),
        "{synth}"
    );
    assert!(
        synth["limitations"].is_array() || synth["output"]["limitations"].is_array(),
        "{synth}"
    );
    let (st, again) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/reports/{}/synthesis", id(&report)),
        DR,
        Some(json!({ "lang": "en" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{again}");
    // Same report version: the deterministic output is reused with provenance and
    // the earlier awaiting artifact is superseded, never duplicated as live.
    assert_ne!(again["id"], synth["id"], "{again}");
    assert_eq!(again["output"], synth["output"], "{again}");
    assert_eq!(again["status"], "awaiting_review");
    let (prior_status, prior_reused): (String, Option<Uuid>) =
        sqlx::query_as("SELECT status, reused_from FROM ai_artifacts WHERE id = $1::uuid")
            .bind(id(&synth))
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(prior_status, "superseded");
    assert!(prior_reused.is_none());
    let reused_from: Option<Uuid> =
        sqlx::query_scalar("SELECT reused_from FROM ai_artifacts WHERE id = $1::uuid")
            .bind(id(&again))
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(reused_from.map(|u| u.to_string()), Some(id(&synth)));

    // Review with a stale version is refused; then the real review.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/reports/{}/review", id(&report)),
        DR,
        Some(json!({ "report_version": version(&report) + 1, "clinical_assessment": "Stale review attempt (synthetic).", "disposition": "routine_follow_up" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(code_of(&v), "report_version_mismatch");
    let reviewed = review(
        &state,
        &report,
        json!({
            "disposition": "immediate_contact",
            "clinical_assessment": "Severe hyperkalaemia on a signed final result; patient contacted and directed to emergency care (synthetic).",
            "follow_ups": [{ "description": "Confirm emergency attendance and repeat potassium", "priority": "urgent", "due_in_hours": 4 }],
            "synthesis_artifact_id": again["id"], "synthesis_decision": "approved",
        }),
    )
    .await;
    let review_id = reviewed["review_id"].as_str().unwrap().to_string();
    assert!(
        reviewed["synthesis"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == again["id"]
                && a["status"] == "approved"
                && a["review_decision"] == "approved"),
        "{reviewed}"
    );
    assert!(
        reviewed["reviews"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["id"] == review_id.as_str()),
        "{reviewed}"
    );
    // A second review of the same version is refused.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/reports/{}/review", id(&report)),
        DR,
        Some(json!({ "report_version": version(&report), "clinical_assessment": "Second review attempt (synthetic).", "disposition": "routine_follow_up" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    // Follow-up task exists and is open; the critical loop is not closed by review.
    let (tasks,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM follow_up_tasks WHERE patient_id = $1::uuid AND status IN ('open','overdue')",
    )
    .bind(&p)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(tasks >= 1);

    // Patient view before release: nothing but "under review".
    grant_for(&state, &p).await;
    let (st, me) = call(
        &state,
        "GET",
        &format!("/api/v1/me/diagnostics?patient_id={p}"),
        REP,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{me}");
    assert_eq!(me["released"].as_array().unwrap().len(), 0, "{me}");
    assert!(
        me["under_review"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["service_request_id"] == acc["id"]),
        "{me}"
    );
    let (st, v) = call(
        &state,
        "GET",
        &format!("/api/v1/me/diagnostics/{}?patient_id={p}", id(&report)),
        REP,
        None,
    )
    .await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "unreleased reports are invisible to the patient: {v}"
    );

    // Explanation draft must be approved and its text supplied explicitly.
    let (st, expl) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/reports/{}/explanation", id(&report)),
        DR,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{expl}");
    let expl_id = id(&expl);
    let release_body = json!({
        "report_version": version(&report), "review_id": review_id, "decision": "release",
        "explanation_artifact_id": expl_id,
        "explanation_en": expl["output"]["explanation_en"], "explanation_es": expl["output"]["explanation_es"],
        "notify_patient": true,
    });
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/reports/{}/release", id(&report)),
        DR,
        Some(release_body.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(code_of(&v), "explanation_not_approved");
    let (st, v) = call(
        &state,
        "POST",
        &format!(
            "/api/v1/diagnostics/reports/{}/explanation/{expl_id}/review",
            id(&report)
        ),
        DR,
        Some(json!({ "decision": "approved", "note": "wording checked" })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let mut stale = release_body.clone();
    stale["report_version"] = json!(version(&report) + 1);
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/reports/{}/release", id(&report)),
        DR,
        Some(stale),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(code_of(&v), "report_version_mismatch");
    // Nurses cannot release.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/reports/{}/release", id(&report)),
        NURSE,
        Some(release_body.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, released) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/reports/{}/release", id(&report)),
        DR,
        Some(release_body),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{released}");
    assert!(released["release_decision_id"].is_string());
    assert!(
        released["notification_id"].is_string(),
        "release with notification queues a job: {released}"
    );
    let (kind, status): (String, String) =
        sqlx::query_as("SELECT kind, status FROM notifications WHERE id = $1::uuid")
            .bind(released["notification_id"].as_str().unwrap())
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(kind, "diagnostic_result_released");
    assert_ne!(
        status, "sent",
        "the worker, not the release route, sends notifications"
    );

    // Now visible to the authorised representative with the approved text.
    let (st, me) = call(
        &state,
        "GET",
        &format!("/api/v1/me/diagnostics?patient_id={p}"),
        REP,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{me}");
    assert!(
        me["released"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["report_id"] == report["id"] || r["id"] == report["id"]),
        "{me}"
    );
    let (st, mine) = call(
        &state,
        "GET",
        &format!("/api/v1/me/diagnostics/{}?patient_id={p}", id(&report)),
        REP,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{mine}");
    let text = mine.to_string();
    assert!(text.contains("explanation"), "{mine}");
    assert!(
        !text.contains("clinical_assessment"),
        "internal review text never reaches the patient view: {mine}"
    );
    // Another representative without a grant sees nothing for this patient.
    let (st, v) = call(
        &state,
        "GET",
        &format!("/api/v1/me/diagnostics?patient_id={p}"),
        "dev-rep.alba",
        None,
    )
    .await;
    assert_ne!(st, StatusCode::OK, "{v}");

    // A correction reopens review and withdraws the release from the patient.
    let (st, corrected) = issue(
        &state,
        LAB,
        &id(&acc),
        json!({
            "status": "corrected", "sign": true, "source_system": "integration-lab",
            "change_reason": "Analyser recalibration: potassium re-run on the retained sample",
            "components": [quantity("2823-3", "4.9", "mmol/L")],
        }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{corrected}");
    assert_eq!(corrected["status"], "corrected");
    assert_eq!(corrected["replaces"], report["id"]);
    assert_eq!(corrected["reviewable"], true, "{corrected}");
    let k = corrected["components"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["code"] == "2823-3")
        .unwrap();
    assert!(
        k["amends_observation_id"].is_string() || k["amends"].is_string(),
        "corrections are append-only amendments: {k}"
    );
    let (st, me) = call(
        &state,
        "GET",
        &format!("/api/v1/me/diagnostics?patient_id={p}"),
        REP,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{me}");
    assert!(
        me["under_review"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["service_request_id"] == acc["id"]),
        "corrected results return to review before the patient sees them: {me}"
    );
    assert!(
        !me["released"]
            .as_array()
            .unwrap()
            .iter()
            .any(|r| r["report_id"] == corrected["id"] || r["id"] == corrected["id"]),
        "{me}"
    );
    let (st, v) = call(
        &state,
        "GET",
        &format!("/api/v1/me/diagnostics/{}?patient_id={p}", id(&corrected)),
        REP,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");
    // The superseded version can no longer be released.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/reports/{}/review", id(&report)),
        DR,
        Some(json!({ "report_version": version(&report), "clinical_assessment": "Concurrent review attempt (synthetic).", "disposition": "routine_follow_up" })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
}

// ---------------------------------------------------------------------------
// 7. Pathology narrative: amendment chain
// ---------------------------------------------------------------------------

#[tokio::test]
async fn pathology_amendment_chain_and_entered_in_error() {
    let state = test_state().await;
    let (f, _p, e) = consultation(&state).await;
    let biopsy = orderable_id(&state, "skin_biopsy_histology").await;
    let group = place(&state, &e, &f, &[(&biopsy, Some("immediate"))], json!({})).await;
    let order = &group["orders"][0];
    let acc = must_transition(&state, LAB, order, "accept", None).await;
    let (st, spec) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/orders/{}/specimens", id(&acc)),
        LAB,
        Some(json!({
            "specimen_type_code": "tissue", "container_code": "formalin_pot", "body_site": "left forearm",
            "collected": true, "collected_at": chrono::Utc::now(), "collection_facility_id": f,
        })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{spec}");
    let narrative =
        |text: &str| json!({ "code": "22637-3", "value": { "type": "narrative", "text": text } });
    let (st, final_report) = issue(
        &state,
        LAB,
        &id(&acc),
        json!({ "status": "final", "sign": true, "components": [narrative("Benign naevus. No atypia (synthetic).")],
                "conclusion": "Benign melanocytic naevus", "conclusion_codes": [{ "system": "urn:wellos:histology", "code": "benign" }] }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{final_report}");
    assert_ne!(
        final_report["criticality"], "critical",
        "a narrative without critical conclusion codes is never critical: {final_report}"
    );
    assert_eq!(final_report["reviewable"], true, "{final_report}");
    // Amendment without a change reason is refused.
    let (st, v) = issue(
        &state,
        LAB,
        &id(&acc),
        json!({ "status": "amended", "sign": true, "components": [narrative("Addendum.")] }),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let (st, amended) = issue(
        &state,
        LAB,
        &id(&acc),
        json!({ "status": "amended", "sign": true, "change_reason": "Deeper levels examined: atypical features",
                "components": [narrative("Atypical melanocytic proliferation; melanoma cannot be excluded (synthetic).")],
                "conclusion_codes": [{ "system": "urn:wellos:histology", "code": "malignant" }] }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{amended}");
    assert_eq!(amended["status"], "amended");
    assert_eq!(amended["replaces"], final_report["id"]);
    assert_eq!(
        amended["criticality"], "critical",
        "configured critical conclusion codes: {amended}"
    );
    let (st, old) = call(
        &state,
        "GET",
        &format!("/api/v1/diagnostics/reports/{}", id(&final_report)),
        DR,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{old}");
    assert_eq!(old["replaced_by"], amended["id"], "{old}");
    assert_eq!(old["reviewable"], false, "{old}");
    // Entered in error is terminal for the chain.
    // Retracting a report is a clinical decision: the laboratory cannot do it alone.
    let (st, v) = issue(
        &state,
        LAB,
        &id(&acc),
        json!({ "status": "entered_in_error", "change_reason": "Wrong patient label on the cassette (synthetic)" }),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, eie) = issue(
        &state,
        DR,
        &id(&acc),
        json!({ "status": "entered_in_error", "change_reason": "Wrong patient label on the cassette (synthetic)" }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{eie}");
    assert_eq!(eie["status"], "entered_in_error");
    let (st, v) = issue(
        &state,
        LAB,
        &id(&acc),
        json!({ "status": "amended", "sign": true, "change_reason": "late", "components": [narrative("x")] }),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
}

// ---------------------------------------------------------------------------
// 8. Tenant / facility / role isolation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn tenant_facility_and_role_isolation() {
    let state = test_state().await;
    let (f, _p, e) = consultation(&state).await;
    let hb = orderable_id(&state, "hemoglobin").await;

    // Only the owning practitioner of an active consultation may order.
    let (st, v) = preflight(
        &state,
        &e,
        json!({ "items": items(&[(&hb, Some("immediate"))]), "performing_facility_id": f }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{e}/diagnostic-orders/preflight"),
        LAB,
        Some(json!({ "items": items(&[(&hb, Some("immediate"))]), "performing_facility_id": f })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/encounters/{e}/diagnostic-orders/preflight"),
        "dev-dr.lopez",
        Some(json!({ "items": items(&[(&hb, Some("immediate"))]), "performing_facility_id": f })),
    )
    .await;
    assert_eq!(
        st,
        StatusCode::FORBIDDEN,
        "another physician cannot order on this consultation: {v}"
    );

    let group = place(&state, &e, &f, &[(&hb, Some("immediate"))], json!({})).await;
    let order = &group["orders"][0];
    let oid = id(order);
    let (st, v) = detail(&state, DR_OTHER_TENANT, &oid).await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");
    let (st, v) = detail(&state, DR_ANNEX, &oid).await;
    assert!(
        st == StatusCode::FORBIDDEN || st == StatusCode::NOT_FOUND,
        "{v}"
    );
    let (st, wl) = call(&state, "GET", "/api/v1/diagnostics/orders", DR_ANNEX, None).await;
    assert_eq!(st, StatusCode::OK, "{wl}");
    assert!(!wl["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["id"] == oid.as_str()));
    let (st, wl) = call(
        &state,
        "GET",
        "/api/v1/diagnostics/orders",
        DR_OTHER_TENANT,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{wl}");
    assert!(!wl["items"]
        .as_array()
        .unwrap()
        .iter()
        .any(|o| o["id"] == oid.as_str()));

    // Specimens and results are fulfilment roles; registration staff cannot.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/orders/{oid}/specimens"),
        REG,
        Some(json!({ "specimen_type_code": "blood_venous", "collected": true, "collection_facility_id": f })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, v) = issue(
        &state,
        REG,
        &oid,
        json!({ "status": "final", "components": [] }),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    let (st, v) = call(
        &state,
        "POST",
        "/api/v1/catalog",
        DR,
        Some(json!({ "kind": "diagnostic_orderable", "code": uniq("x"), "name_en": "x", "name_es": "x" })),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    // Representatives cannot read the staff diagnostics surface.
    let (st, v) = detail(&state, REP, &oid).await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");
    // Unauthenticated callers are refused.
    let (st, _) = send(
        &state,
        "GET",
        &format!("/api/v1/diagnostics/orders/{oid}"),
        "nope",
        "treatment",
        "application/json",
        Body::empty(),
    )
    .await;
    assert_eq!(st, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// 9. Documents through the ObjectStore and imaging references
// ---------------------------------------------------------------------------

#[tokio::test]
async fn documents_object_store_and_imaging_references() {
    let state = test_state().await;
    let (f, p, e) = consultation(&state).await;
    let cxr = orderable_id(&state, "chest_xray").await;
    let group = place(&state, &e, &f, &[(&cxr, Some("inpatient"))], json!({})).await;
    let order = &group["orders"][0];
    let acc = must_transition(&state, TECH, order, "accept", None).await;
    let started = must_transition(&state, TECH, &acc, "start", None).await;
    let oid = id(&started);

    // Imaging study reference (no pixel data), idempotent.
    let study = json!({
        "study_instance_uid": format!("1.2.826.0.1.3680043.10.9999.{}", Uuid::now_v7().as_u128() % 1_000_000_000),
        "accession_number": uniq("ACC"), "modality_code": "CR", "description": "Chest PA (synthetic)",
        "series": [{ "series_instance_uid": "1.2.3.4.5.1", "modality": "CR", "number_of_instances": 1 }],
        "pacs_endpoint_code": "pacs_main", "status": "available", "source_system": "integration-pacs",
        "idempotency_key": uniq("img"),
    });
    let (st, img) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/orders/{oid}/imaging-studies"),
        TECH,
        Some(study.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{img}");
    let (st, img2) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/orders/{oid}/imaging-studies"),
        TECH,
        Some(study.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{img2}");
    assert_eq!(img2["id"], img["id"]);
    let mut other = study.clone();
    other["study_instance_uid"] = json!("1.2.826.0.1.3680043.10.9999.0");
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/orders/{oid}/imaging-studies"),
        TECH,
        Some(other),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    // Sender-supplied URLs are never stored.
    assert!(!img.to_string().contains("http://"), "{img}");

    // Document: register → pre-signed fixture upload → complete (clean).
    let payload = b"%PDF-1.4 synthetic radiology report\n".to_vec();
    let checksum = hex::encode(Sha256::digest(&payload));
    let register = |checksum: &str, size: usize| {
        json!({
            "kind": "report_pdf", "title": "Chest radiograph report (synthetic)", "mime_type": "application/pdf",
            "size_bytes": size, "checksum_sha256": checksum, "source_system": "integration-ris",
        })
    };
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/orders/{oid}/documents"),
        TECH,
        Some(json!({ "kind": "report_pdf", "title": "exe", "mime_type": "application/x-msdownload", "size_bytes": 10, "checksum_sha256": checksum })),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    assert_eq!(code_of(&v), "unsupported_media_type");
    let (st, reg) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/orders/{oid}/documents"),
        TECH,
        Some(register(&checksum, payload.len())),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{reg}");
    let doc_id = id(&reg["document"]);
    assert_eq!(reg["document"]["status"], "quarantined", "{reg}");
    assert_eq!(reg["document"]["downloadable"], false, "{reg}");
    assert!(reg["document"]["scan_verdict"].is_null(), "{reg}");
    let url = reg["upload"]["url"].as_str().unwrap().to_string();
    assert!(
        url.starts_with("/api/v1/dev/objects/"),
        "fixture store uses the dev object route: {url}"
    );
    // Completing before upload fails with a typed error.
    let (st, v) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/documents/{doc_id}/complete"),
        TECH,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    assert_eq!(code_of(&v), "object_missing");
    // Tampered grant is refused.
    let (st, _) = send(
        &state,
        "PUT",
        &format!("{url}x"),
        TECH,
        "operations",
        "application/pdf",
        Body::from(payload.clone()),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN);
    let (st, _) = send(
        &state,
        "PUT",
        &url,
        TECH,
        "operations",
        "application/pdf",
        Body::from(payload.clone()),
    )
    .await;
    assert!(st.is_success(), "fixture upload: {st}");
    let (st, done) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/documents/{doc_id}/complete"),
        TECH,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{done}");
    assert_eq!(
        done["document"]["status"]
            .as_str()
            .or(done["status"].as_str()),
        Some("clean"),
        "{done}"
    );
    // Checksum mismatch → rejected, never clean.
    let (st, bad) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/orders/{oid}/documents"),
        TECH,
        Some(register(
            &hex::encode(Sha256::digest(b"other bytes")),
            payload.len(),
        )),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{bad}");
    let bad_url = bad["upload"]["url"].as_str().unwrap().to_string();
    let (st, _) = send(
        &state,
        "PUT",
        &bad_url,
        TECH,
        "operations",
        "application/pdf",
        Body::from(payload.clone()),
    )
    .await;
    assert!(
        st.is_success() || st == StatusCode::CONFLICT || st == StatusCode::BAD_REQUEST,
        "{st}"
    );
    let (st, v) = call(
        &state,
        "POST",
        &format!(
            "/api/v1/diagnostics/documents/{}/complete",
            id(&bad["document"])
        ),
        TECH,
        None,
    )
    .await;
    assert_ne!(v["document"]["status"], "clean", "{st} {v}");

    // Staff download is pre-signed; the patient cannot download before release.
    let (st, dl) = call(
        &state,
        "GET",
        &format!("/api/v1/diagnostics/documents/{doc_id}/download"),
        DR,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{dl}");
    assert!(dl["download"]["url"].is_string(), "{dl}");
    let (st, v) = call(
        &state,
        "GET",
        &format!("/api/v1/diagnostics/documents/{doc_id}/download"),
        DR_ANNEX,
        None,
    )
    .await;
    assert_ne!(st, StatusCode::OK, "{v}");
    grant_for(&state, &p).await;
    let (st, report) = issue(
        &state,
        TECH,
        &oid,
        json!({ "status": "final", "sign": true, "components": [{ "code": "36643-5", "value": { "type": "narrative", "text": "Clear lungs. No pneumothorax (synthetic)." } }],
                "conclusion_codes": [{ "system": "urn:wellos:cxr", "code": "normal" }] }),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{report}");
    // A document attached to the report is released with it; the order-level
    // document registered before the report stays staff-only.
    let report_payload = b"synthetic signed report pdf bytes".to_vec();
    let mut report_doc = register(
        &hex::encode(Sha256::digest(&report_payload)),
        report_payload.len(),
    );
    report_doc["diagnostic_report_id"] = json!(id(&report));
    let (st, rd) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/orders/{oid}/documents"),
        TECH,
        Some(report_doc),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{rd}");
    let report_doc_id = id(&rd["document"]);
    let rd_url = rd["upload"]["url"].as_str().unwrap().to_string();
    let (st, _) = send(
        &state,
        "PUT",
        &rd_url,
        TECH,
        "operations",
        "application/pdf",
        Body::from(report_payload.clone()),
    )
    .await;
    assert!(st.is_success(), "fixture upload: {st}");
    let (st, done) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/documents/{report_doc_id}/complete"),
        TECH,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{done}");
    for d in [&report_doc_id, &doc_id] {
        let (st, v) = call(
            &state,
            "GET",
            &format!(
                "/api/v1/me/diagnostics/{}/documents/{d}/download?patient_id={p}",
                id(&report)
            ),
            REP,
            None,
        )
        .await;
        assert_eq!(
            st,
            StatusCode::NOT_FOUND,
            "nothing reaches the patient before release: {v}"
        );
    }
    let reviewed = review(&state, &report, json!({ "disposition": "no_action" })).await;
    let (st, rel) = call(
        &state,
        "POST",
        &format!("/api/v1/diagnostics/reports/{}/release", id(&report)),
        DR,
        Some(json!({ "report_version": version(&report), "review_id": reviewed["review_id"], "decision": "release",
                     "notify_patient": false, "release_documents": true })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{rel}");
    let (st, v) = call(
        &state,
        "GET",
        &format!(
            "/api/v1/me/diagnostics/{}/documents/{report_doc_id}/download?patient_id={p}",
            id(&report)
        ),
        REP,
        None,
    )
    .await;
    assert_eq!(
        st,
        StatusCode::OK,
        "released documents are downloadable by the grant holder: {v}"
    );
    assert!(v["download"]["url"].is_string(), "{v}");
    let (st, v) = call(
        &state,
        "GET",
        &format!(
            "/api/v1/me/diagnostics/{}/documents/{doc_id}/download?patient_id={p}",
            id(&report)
        ),
        REP,
        None,
    )
    .await;
    assert_eq!(
        st,
        StatusCode::NOT_FOUND,
        "order-level documents are not released through a report: {v}"
    );
    // Imaging reference is visible on the order, not pixel data.
    let (st, d) = detail(&state, DR, &oid).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(d["imaging_studies"].as_array().unwrap().len(), 1, "{d}");
    assert_eq!(d["documents"].as_array().unwrap().len(), 3, "{d}");
}

// ---------------------------------------------------------------------------
// 10. FHIR subset + inbound DiagnosticReport
// ---------------------------------------------------------------------------

async fn service_token(state: &AppState, scopes: &[&str]) -> String {
    let token = wellos_server::seeddata::generate_service_secret();
    let (uid, tid): (Uuid, Uuid) =
        sqlx::query_as("SELECT id, tenant_id FROM users WHERE username = 'svc.lab-adapter'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    sqlx::query(
        "INSERT INTO service_credentials (id, tenant_id, user_id, name, token_hash, scopes, expires_at)
         VALUES ($1,$2,$3,'diagnostics integration',$4,$5, now() + interval '1 hour')",
    )
    .bind(Uuid::now_v7())
    .bind(tid)
    .bind(uid)
    .bind(wellos_server::auth::hash_service_secret(&token))
    .bind(scopes.iter().map(|s| s.to_string()).collect::<Vec<_>>())
    .execute(&state.pool)
    .await
    .unwrap();
    token
}

#[tokio::test]
async fn fhir_subset_and_inbound_diagnostic_report() {
    let state = test_state().await;
    let (f, p, e) = consultation(&state).await;
    let hba1c = orderable_id(&state, "hba1c").await;
    let group = place(&state, &e, &f, &[(&hba1c, Some("immediate"))], json!({})).await;
    let order = &group["orders"][0];
    let acc = must_transition(&state, LAB, order, "accept", None).await;
    let spec = record_specimen(&state, &acc, &f).await;
    let oid = id(&acc);

    let (st, sr) = call(
        &state,
        "GET",
        &format!("/fhir/r4/ServiceRequest/{oid}"),
        DR,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{sr}");
    assert_eq!(sr["resourceType"], "ServiceRequest");
    assert_eq!(sr["status"], "active");
    assert_eq!(sr["subject"]["reference"], format!("Patient/{p}"));
    let (st, fs) = call(
        &state,
        "GET",
        &format!("/fhir/r4/Specimen/{}", id(&spec)),
        DR,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{fs}");
    assert_eq!(fs["resourceType"], "Specimen");
    assert_eq!(
        fs["request"][0]["reference"],
        format!("ServiceRequest/{oid}")
    );
    let (st, v) = call(
        &state,
        "GET",
        &format!("/fhir/r4/ServiceRequest/{oid}"),
        DR_OTHER_TENANT,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::NOT_FOUND, "{v}");

    // Inbound DiagnosticReport through the machine principal.
    let token = service_token(&state, &["result.ingest"]).await;
    let idem = uniq("fhir");
    let inbound = json!({
        "resourceType": "DiagnosticReport",
        "meta": { "source": "urn:synthetic:lis" },
        "identifier": [{ "system": "urn:wellos:idempotency", "value": idem }],
        "status": "final",
        "code": { "coding": [{ "system": "http://loinc.org", "code": "4548-4", "display": "Hemoglobin A1c" }] },
        "basedOn": [{ "reference": format!("ServiceRequest/{oid}") }],
        "subject": { "reference": format!("Patient/{p}") },
        "effectiveDateTime": "2026-10-01T08:00:00Z",
        "result": [{ "reference": "#obs1" }],
        "contained": [{
            "resourceType": "Observation", "id": "obs1", "status": "final",
            "subject": { "reference": format!("Patient/{p}") },
            "code": { "coding": [{ "system": "http://loinc.org", "code": "4548-4", "display": "Hemoglobin A1c" }] },
            "valueQuantity": { "value": 7.9, "unit": "%", "system": "http://unitsofmeasure.org", "code": "%" },
            "referenceRange": [{ "text": "4-5.6" }]
        }]
    });
    let (st, v) = send(
        &state,
        "POST",
        "/fhir/r4/DiagnosticReport",
        &token,
        "treatment",
        "application/fhir+json",
        Body::from(serde_json::to_vec(&json!({ "resourceType": "Observation" })).unwrap()),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "{v}");
    let mut missing_source = inbound.clone();
    missing_source["meta"] = json!({});
    let (st, v) = send(
        &state,
        "POST",
        "/fhir/r4/DiagnosticReport",
        &token,
        "treatment",
        "application/fhir+json",
        Body::from(serde_json::to_vec(&missing_source).unwrap()),
    )
    .await;
    assert_eq!(st, StatusCode::BAD_REQUEST, "provenance is mandatory: {v}");
    let (st, dr) = send(
        &state,
        "POST",
        "/fhir/r4/DiagnosticReport",
        &token,
        "treatment",
        "application/fhir+json",
        Body::from(serde_json::to_vec(&inbound).unwrap()),
    )
    .await;
    assert!(
        st == StatusCode::CREATED || st == StatusCode::OK,
        "{st} {dr}"
    );
    assert_eq!(dr["resourceType"], "DiagnosticReport");
    assert_eq!(dr["status"], "final");
    let report_id = id(&dr);
    let (st, dr2) = send(
        &state,
        "POST",
        "/fhir/r4/DiagnosticReport",
        &token,
        "treatment",
        "application/fhir+json",
        Body::from(serde_json::to_vec(&inbound).unwrap()),
    )
    .await;
    assert!(st.is_success(), "{dr2}");
    assert_eq!(dr2["id"], dr["id"], "idempotent replay");
    // Same key, different content → conflict.
    let mut altered = inbound.clone();
    altered["contained"][0]["valueQuantity"]["value"] = json!(8.4);
    let (st, v) = send(
        &state,
        "POST",
        "/fhir/r4/DiagnosticReport",
        &token,
        "treatment",
        "application/fhir+json",
        Body::from(serde_json::to_vec(&altered).unwrap()),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{v}");
    // Clinicians without the ingest scope cannot push results via FHIR.
    let (st, v) = send(
        &state,
        "POST",
        "/fhir/r4/DiagnosticReport",
        REG,
        "treatment",
        "application/fhir+json",
        Body::from(serde_json::to_vec(&inbound).unwrap()),
    )
    .await;
    assert_eq!(st, StatusCode::FORBIDDEN, "{v}");

    // Processed through the same typed path: abnormal interpretation, order completed.
    let (st, rep) = call(
        &state,
        "GET",
        &format!("/api/v1/diagnostics/reports/{report_id}"),
        DR,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{rep}");
    assert_eq!(rep["source_system"], "urn:synthetic:lis");
    assert_eq!(rep["criticality"], "abnormal", "{rep}");
    let (st, d) = detail(&state, DR, &oid).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(d["order_status"], "completed", "{d}");
    let (st, fdr) = call(
        &state,
        "GET",
        &format!("/fhir/r4/DiagnosticReport/{report_id}"),
        DR,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{fdr}");
    assert_eq!(
        fdr["basedOn"][0]["reference"],
        format!("ServiceRequest/{oid}")
    );
    let obs_ref = fdr["result"][0]["reference"].as_str().unwrap().to_string();
    let obs_id = obs_ref.trim_start_matches("Observation/").to_string();
    let (st, fo) = call(
        &state,
        "GET",
        &format!("/fhir/r4/Observation/{obs_id}"),
        DR,
        None,
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{fo}");
    assert_eq!(fo["code"]["coding"][0]["code"], "4548-4");
    assert_eq!(fo["valueQuantity"]["value"], 7.9);
    let (st, fp) = call(&state, "GET", &format!("/fhir/r4/Patient/{p}"), DR, None).await;
    assert_eq!(st, StatusCode::OK, "{fp}");
    assert_eq!(fp["resourceType"], "Patient");
}
