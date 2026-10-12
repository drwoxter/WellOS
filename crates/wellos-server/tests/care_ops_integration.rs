//! Care Operations v1 integration gate: tenant/facility isolation,
//! authorization, placement hard constraints + stale 409, lifecycle to
//! cleaning/available, escalation tick, theatre conflicts, workforce gaps,
//! deterministic forecast and patient privacy. Runs against the synthetic
//! fixture database (dev-fixtures only).

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::{Duration, Utc};
use http_body_util::BodyExt;
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use uuid::Uuid;
use wellos_server::state::AppState;

const OPS: &str = "dev-ops.lopez";
const BEDS: &str = "dev-bed.ortiz";
const THEATRE: &str = "dev-theatre.ruiz";
const WORKFORCE: &str = "dev-wf.diaz";
const NURSE: &str = "dev-nurse.kim";
const REG: &str = "dev-reg.rivera";
const ADMIN: &str = "dev-admin.silva";
const PATIENT: &str = "dev-rep.alba";

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wellos:wellos_dev@localhost:5432/wellos".to_string())
}

async fn test_state() -> AppState {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .try_init();
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

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let purpose = if token == PATIENT || token == NURSE {
        "treatment"
    } else {
        "operations"
    };
    let req = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("x-purpose-of-use", purpose)
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

fn uuid(v: &Value) -> Uuid {
    v.as_str().unwrap().parse().unwrap()
}

struct Fx {
    tenant: Uuid,
    facility: Uuid,
    other_tenant_facility: Uuid,
    patient: Uuid,
}

async fn fixtures(state: &AppState) -> Fx {
    let (tenant,): (Uuid,) =
        sqlx::query_as("SELECT tenant_id FROM users WHERE username = 'ops.lopez'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let (facility,): (Uuid,) = sqlx::query_as(
        "SELECT facility_id FROM operational_locations WHERE tenant_id = $1 AND code = 'MED-3'",
    )
    .bind(tenant)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let (other,): (Uuid,) =
        sqlx::query_as("SELECT id FROM facilities WHERE tenant_id <> $1 LIMIT 1")
            .bind(tenant)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    // Each test gets its own synthetic patient so concurrently running suites
    // never compete for the single open episode a patient may hold per type.
    let patient = Uuid::now_v7();
    let tag = patient.simple().to_string();
    sqlx::query(
        "INSERT INTO patients (id, tenant_id, facility_id, family_name, given_name, birth_date, sex, identifier)
         VALUES ($1,$2,$3,'Testpatient',$4,'1979-02-03','other',$5)",
    )
    .bind(patient)
    .bind(tenant)
    .bind(facility)
    .bind(format!("Synthetic {}", &tag[tag.len() - 6..]))
    .bind(format!("SYN-T-{}", &tag[tag.len() - 10..]))
    .execute(&state.pool)
    .await
    .unwrap();
    Fx {
        tenant,
        facility,
        other_tenant_facility: other,
        patient,
    }
}

fn loc(
    facility: Uuid,
    parent: Option<Uuid>,
    level: &str,
    type_code: &str,
    code: &str,
    extra: Value,
) -> Value {
    let mut v = json!({
        "facility_id": facility,
        "parent_id": parent,
        "level": level,
        "type_code": type_code,
        "code": code,
        "name_en": code,
        "name_es": code,
        "service_line_codes": ["internal_medicine"],
        "age_group_codes": ["adult"],
        "metadata": {"synthetic": true}
    });
    for (k, val) in extra.as_object().unwrap() {
        v[k] = val.clone();
    }
    v
}

async fn create_loc(state: &AppState, body: Value) -> Value {
    let (s, v) = call(
        state,
        "POST",
        "/api/v1/operations/locations",
        OPS,
        Some(body),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    v
}

async fn episode(state: &AppState, fx: &Fx, kind: &str) -> Value {
    let (s, v) = call(
        state,
        "POST",
        "/api/v1/operations/episodes",
        BEDS,
        Some(json!({
            "facility_id": fx.facility,
            "patient_id": fx.patient,
            "episode_type_code": kind,
            "responsible_service_code": "internal_medicine",
            "priority": "urgent",
            "activate": true
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    v
}

async fn placement_request(state: &AppState, episode_id: Uuid, requirements: Value) -> Value {
    let (s, v) = call(
        state,
        "POST",
        "/api/v1/operations/placement-requests",
        BEDS,
        Some(json!({
            "episode_id": episode_id,
            "kind": "admission",
            "requirements": requirements
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    v
}

async fn candidates(state: &AppState, request_id: Uuid) -> Value {
    let (s, v) = call(
        state,
        "POST",
        &format!("/api/v1/operations/placement-requests/{request_id}/candidates"),
        BEDS,
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    v
}

fn requirements(isolation: &[&str]) -> Value {
    json!({
        "service_code": "internal_medicine",
        "specialty_codes": [],
        "age_group_code": "adult",
        "isolation_codes": isolation,
        "accessibility_codes": [],
        "equipment_codes": [],
        "preferred_unit_ids": [],
        "required_staffed_professions": []
    })
}

#[tokio::test]
async fn care_ops_is_tenant_facility_and_role_scoped() {
    let state = test_state().await;
    let fx = fixtures(&state).await;

    // Snapshot for another tenant's facility is refused (anti-probing: 404).
    let (s, _) = call(
        &state,
        "GET",
        &format!(
            "/api/v1/operations/snapshot?facility_id={}",
            fx.other_tenant_facility
        ),
        OPS,
        None,
    )
    .await;
    assert!(
        s == StatusCode::NOT_FOUND || s == StatusCode::FORBIDDEN,
        "{s}"
    );

    // Registration staff can read the board but cannot configure locations
    // (role, not profession, decides).
    let (s, v) = call(
        &state,
        "GET",
        &format!("/api/v1/operations/snapshot?facility_id={}", fx.facility),
        NURSE,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    for key in [
        "emergency",
        "placement_queue",
        "capacity",
        "discharge",
        "theatre",
        "workforce",
        "forecasts",
        "generated_at",
    ] {
        assert!(v.get(key).is_some(), "snapshot missing {key}: {v}");
    }
    let (s, _) = call(
        &state,
        "POST",
        "/api/v1/operations/locations",
        REG,
        Some(loc(
            fx.facility,
            None,
            "department",
            "department",
            "X",
            json!({}),
        )),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Patients cannot read the operations surface at all.
    let (s, _) = call(
        &state,
        "GET",
        &format!("/api/v1/operations/snapshot?facility_id={}", fx.facility),
        PATIENT,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Bed coordinators cannot manage theatre or workforce.
    let (s, _) = call(
        &state,
        "POST",
        "/api/v1/operations/workforce/absences",
        BEDS,
        Some(json!({"user_id": Uuid::now_v7(), "starts_at": Utc::now(), "ends_at": Utc::now() + Duration::hours(1), "reason_code": "other"})),
    )
    .await;
    assert_eq!(s, StatusCode::FORBIDDEN);

    // Snapshot aggregates are real counts from the fixture database.
    let (_, v) = call(
        &state,
        "GET",
        &format!("/api/v1/operations/snapshot?facility_id={}", fx.facility),
        OPS,
        None,
    )
    .await;
    assert!(v["capacity"]["total"].as_i64().unwrap_or(0) > 0, "{v}");
    assert!(
        v["placement_queue"]["pending"].as_i64().unwrap_or(0) >= 1,
        "{v}"
    );
    let _ = fx.tenant;
}

#[tokio::test]
async fn placement_hard_constraints_hold_confirm_stale_release_cleaning() {
    let state = test_state().await;
    let fx = fixtures(&state).await;
    let started = Utc::now();
    let full = Uuid::now_v7().simple().to_string();
    let tag = full[full.len() - 8..].to_string();

    let dept = create_loc(
        &state,
        loc(
            fx.facility,
            None,
            "department",
            "department",
            &format!("D-{tag}"),
            json!({}),
        ),
    )
    .await;
    let unit = create_loc(
        &state,
        loc(
            fx.facility,
            Some(uuid(&dept["id"])),
            "unit",
            "ward",
            &format!("U-{tag}"),
            json!({"capacity": 2}),
        ),
    )
    .await;
    let room_plain = create_loc(
        &state,
        loc(
            fx.facility,
            Some(uuid(&unit["id"])),
            "room",
            "room",
            &format!("R1-{tag}"),
            json!({"room_policy": "mixed", "capacity": 1}),
        ),
    )
    .await;
    let room_iso = create_loc(&state, loc(fx.facility, Some(uuid(&unit["id"])), "room", "room", &format!("R2-{tag}"), json!({"room_policy": "single", "capacity": 1, "isolation_codes": ["negative_pressure"]}))).await;
    let bed_plain = create_loc(
        &state,
        loc(
            fx.facility,
            Some(uuid(&room_plain["id"])),
            "position",
            "bed",
            &format!("B1-{tag}"),
            json!({}),
        ),
    )
    .await;
    let bed_iso = create_loc(
        &state,
        loc(
            fx.facility,
            Some(uuid(&room_iso["id"])),
            "position",
            "bed",
            &format!("B2-{tag}"),
            json!({"isolation_codes": ["negative_pressure"]}),
        ),
    )
    .await;
    assert_eq!(bed_plain["position_state"], "available");

    // A position cannot be created top-level and a child cannot cross facilities.
    let (s, _) = call(
        &state,
        "POST",
        "/api/v1/operations/locations",
        OPS,
        Some(loc(
            fx.facility,
            None,
            "position",
            "bed",
            &format!("BX-{tag}"),
            json!({}),
        )),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    let ep = episode(&state, &fx, "emergency").await;
    let req = placement_request(
        &state,
        uuid(&ep["id"]),
        requirements(&["negative_pressure"]),
    )
    .await;
    let req_id = uuid(&req["id"]);
    let c = candidates(&state, req_id).await;
    let m = &c["match"];
    assert_eq!(m["matcher_version"], "care-placement.v1");
    let feasible: Vec<Uuid> = m["feasible"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| uuid(&x["location_id"]))
        .collect();
    assert!(feasible.contains(&uuid(&bed_iso["id"])), "{m}");
    assert!(!feasible.contains(&uuid(&bed_plain["id"])), "{m}");
    let plain_rej = m["rejected"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| uuid(&x["location_id"]) == uuid(&bed_plain["id"]))
        .expect("plain bed rejected");
    assert!(
        plain_rej["rejection_codes"]
            .as_array()
            .unwrap()
            .iter()
            .any(|c| c.as_str().unwrap().starts_with("isolation")),
        "{plain_rej}"
    );
    let iso = m["feasible"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| uuid(&x["location_id"]) == uuid(&bed_iso["id"]))
        .unwrap();
    assert!(!iso["factors"].as_array().unwrap().is_empty());
    let facts_hash = m["facts_hash"].as_str().unwrap().to_string();
    assert!(facts_hash.starts_with("sha256:"));

    // Stale version on hold → 409.
    let (s, v) = call(&state, "POST", &format!("/api/v1/operations/placement-requests/{req_id}/hold"), BEDS, Some(json!({"location_id": bed_iso["id"], "expected_version": 999, "facts_hash": facts_hash}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let version = c["request"]["version"].as_i64().unwrap();
    let (s, h) = call(&state, "POST", &format!("/api/v1/operations/placement-requests/{req_id}/hold"), BEDS, Some(json!({"location_id": bed_iso["id"], "expected_version": version, "facts_hash": facts_hash, "hold_minutes": 30}))).await;
    assert_eq!(s, StatusCode::OK, "{h}");
    assert_eq!(h["placement"]["status"], "held");
    let (_, l) = call(
        &state,
        "GET",
        &format!(
            "/api/v1/operations/locations?facility_id={}&parent_id={}",
            fx.facility,
            uuid(&room_iso["id"])
        ),
        OPS,
        None,
    )
    .await;
    let held = l
        .as_array()
        .map(|a| a.iter().any(|x| x["position_state"] == "held"))
        .unwrap_or(false)
        || l["locations"]
            .as_array()
            .map(|a| a.iter().any(|x| x["position_state"] == "held"))
            .unwrap_or(false);
    assert!(held, "{l}");

    // Second patient cannot hold or confirm the same bed while it is held.
    let ep2 = episode(&state, &fx, "inpatient").await;
    let req2 = placement_request(
        &state,
        uuid(&ep2["id"]),
        requirements(&["negative_pressure"]),
    )
    .await;
    let c2 = candidates(&state, uuid(&req2["id"])).await;
    let feasible2: Vec<Uuid> = c2["match"]["feasible"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| uuid(&x["location_id"]))
        .collect();
    assert!(
        !feasible2.contains(&uuid(&bed_iso["id"])),
        "held bed must not be feasible: {}",
        c2["match"]
    );
    let (s, v) = call(&state, "POST", &format!("/api/v1/operations/placement-requests/{}/confirm", uuid(&req2["id"])), BEDS, Some(json!({"expected_version": c2["request"]["version"], "location_id": bed_iso["id"], "facts_hash": c2["match"]["facts_hash"]}))).await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");

    // Confirm with stale version → 409; correct → occupied.
    let (s, _) = call(&state, "POST", &format!("/api/v1/operations/placement-requests/{req_id}/confirm"), BEDS, Some(json!({"expected_version": version, "location_id": bed_iso["id"], "facts_hash": facts_hash}))).await;
    assert_eq!(s, StatusCode::CONFLICT);
    let (s, conf) = call(&state, "POST", &format!("/api/v1/operations/placement-requests/{req_id}/confirm"), BEDS, Some(json!({"expected_version": h["request"]["version"], "location_id": bed_iso["id"], "facts_hash": facts_hash}))).await;
    assert_eq!(s, StatusCode::OK, "{conf}");
    assert_eq!(conf["placement"]["status"], "occupied");
    assert_eq!(conf["request"]["status"], "placed");
    assert_eq!(
        conf["episode"]["current_placement_id"],
        conf["placement"]["id"]
    );

    // Release → cleaning → available, each visible in history.
    let pid = uuid(&conf["placement"]["id"]);
    let (s, rel) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/placements/{pid}/release"),
        BEDS,
        Some(
            json!({"expected_version": conf["placement"]["version"], "reason_code": "discharged"}),
        ),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{rel}");
    assert_eq!(rel["placement"]["status"], "released");
    let (state_now,): (String,) =
        sqlx::query_as("SELECT position_state FROM operational_locations WHERE id = $1")
            .bind(uuid(&bed_iso["id"]))
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(state_now, "cleaning");
    let (ver,): (i64,) = sqlx::query_as("SELECT version FROM operational_locations WHERE id = $1")
        .bind(uuid(&bed_iso["id"]))
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let (s, v) = call(
        &state,
        "POST",
        &format!(
            "/api/v1/operations/locations/{}/state",
            uuid(&bed_iso["id"])
        ),
        BEDS,
        Some(json!({"expected_version": ver, "state": "available", "reason_code": "cleaned"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (n,): (i64,) =
        sqlx::query_as("SELECT COUNT(*) FROM placement_history WHERE placement_id = $1")
            .bind(pid)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(n >= 2, "placement history rows: {n}");
    let (audits,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM audit_events WHERE action = 'care_ops.manage_flow'
           AND decision = 'allow' AND recorded_at >= $1",
    )
    .bind(started)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(audits >= 2, "audit rows: {audits}");
    // Operational history is append-only.
    let r = sqlx::query("DELETE FROM placement_history WHERE placement_id = $1")
        .bind(pid)
        .execute(&state.pool)
        .await;
    assert!(r.is_err(), "placement_history must be append-only");
}

#[tokio::test]
async fn escalation_tick_fires_overdue_boarding() {
    let state = test_state().await;
    let fx = fixtures(&state).await;
    let (s, v) = call(
        &state,
        "POST",
        "/api/v1/operations/jobs/tick",
        ADMIN,
        Some(json!({})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let (s, list) = call(
        &state,
        "GET",
        &format!("/api/v1/operations/escalations?facility_id={}", fx.facility),
        OPS,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{list}");
    let items = list
        .as_array()
        .cloned()
        .or_else(|| list["escalations"].as_array().cloned())
        .unwrap();
    assert!(
        items
            .iter()
            .any(|e| e["kind"] == "boarding" && e["status"] == "escalated"),
        "{list}"
    );
}

#[tokio::test]
async fn theatre_equipment_conflict_needs_explicit_override() {
    let state = test_state().await;
    let fx = fixtures(&state).await;
    let (s, cases) = call(
        &state,
        "GET",
        &format!(
            "/api/v1/operations/theatre/cases?facility_id={}",
            fx.facility
        ),
        THEATRE,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{cases}");
    let items = cases
        .as_array()
        .cloned()
        .or_else(|| cases["cases"].as_array().cloned())
        .unwrap();
    let dental = items
        .iter()
        .map(|c| c["case"].clone())
        .find(|c| {
            c["procedure_code"] == "dental_extraction_sedation"
                && (c["status"] == "requested" || c["status"] == "ready_for_scheduling")
        })
        .unwrap_or_else(|| panic!("fixture dental case missing: {cases}"));
    let (or2,): (Uuid,) = sqlx::query_as(
        "SELECT id FROM operational_locations WHERE tenant_id = $1 AND code = 'OR-2'",
    )
    .bind(fx.tenant)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let (or_ver,): (i64,) =
        sqlx::query_as("SELECT version FROM operational_locations WHERE id = $1")
            .bind(or2)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let dental = if dental["status"] == "requested" {
        let (s, v) = call(
            &state,
            "POST",
            &format!(
                "/api/v1/operations/theatre/cases/{}/transition",
                uuid(&dental["id"])
            ),
            THEATRE,
            Some(json!({"transition": "mark_ready", "expected_version": dental["version"]})),
        )
        .await;
        assert_eq!(s, StatusCode::OK, "{v}");
        v
    } else {
        dental
    };
    assert_eq!(dental["status"], "ready_for_scheduling", "{dental}");
    let start = Utc::now() + Duration::hours(2);
    let body = json!({"expected_version": dental["version"], "theatre_location_id": or2, "expected_theatre_version": or_ver, "scheduled_start": start, "team_user_ids": []});
    let (s, v) = call(
        &state,
        "POST",
        &format!(
            "/api/v1/operations/theatre/cases/{}/schedule",
            uuid(&dental["id"])
        ),
        THEATRE,
        Some(body.clone()),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let conflicts = v["error"]["details"]["conflicts"]
        .as_array()
        .or_else(|| v["conflicts"].as_array())
        .cloned()
        .unwrap_or_default();
    assert!(
        conflicts
            .iter()
            .any(|c| c["code"].as_str().unwrap_or("").contains("equipment")),
        "{v}"
    );
    // Bed coordinator cannot schedule theatre (anti-probing: the case is
    // not even visible without care_ops.manage_theatre).
    let (s, _) = call(
        &state,
        "POST",
        &format!(
            "/api/v1/operations/theatre/cases/{}/schedule",
            uuid(&dental["id"])
        ),
        BEDS,
        Some(body),
    )
    .await;
    assert!(
        s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
        "{s}"
    );
}

#[tokio::test]
async fn workforce_gap_is_capability_specific_and_forecast_is_honest() {
    let state = test_state().await;
    let fx = fixtures(&state).await;
    let (s, shifts) = call(
        &state,
        "GET",
        &format!(
            "/api/v1/operations/workforce/shifts?facility_id={}&from={}",
            fx.facility,
            (Utc::now() - Duration::hours(36)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        ),
        WORKFORCE,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{shifts}");
    let items = shifts
        .as_array()
        .cloned()
        .or_else(|| shifts["shifts"].as_array().cloned())
        .unwrap();
    let day = items
        .iter()
        .map(|s| s["shift"].clone())
        .find(|s| s["label"] == "Medical ward 3 — day")
        .unwrap_or_else(|| panic!("fixture shift missing: {shifts}"));
    let (s, d) = call(
        &state,
        "GET",
        &format!("/api/v1/operations/workforce/shifts/{}", uuid(&day["id"])),
        WORKFORCE,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{d}");
    let gaps = d["coverage"]["gaps"].as_array().unwrap();
    assert!(
        gaps.iter()
            .any(|g| g["profession_code"] == "physiotherapist" && g["shortfall"] == 1),
        "{d}"
    );
    assert!(
        gaps.iter().any(|g| g["profession_code"] == "nurse"
            && g["capability_codes"]
                .as_array()
                .unwrap()
                .contains(&json!("acls"))),
        "{d}"
    );
    assert_eq!(d["coverage"]["engine_version"], "workforce-coverage.v1");

    let (s, f) = call(
        &state,
        "POST",
        "/api/v1/operations/forecasts",
        OPS,
        Some(json!({"facility_id": fx.facility, "horizon": "7d"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{f}");
    assert_eq!(f["forecast_version"], "care-operations-forecast.v1");
    assert_eq!(f["status"], "insufficient_history", "{f}");
    assert!(f["facts_hash"].as_str().unwrap().starts_with("sha256:"));
    assert!(
        f.get("source_facts").is_none(),
        "facts only for auditors: {f}"
    );
    let (s, _) = call(
        &state,
        "POST",
        "/api/v1/operations/forecasts",
        OPS,
        Some(json!({"facility_id": fx.facility, "horizon": "3d"})),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn patient_care_view_exposes_only_released_own_data() {
    let state = test_state().await;
    let (s, v) = call(&state, "GET", "/api/v1/me/care", PATIENT, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let pid: Uuid = v["patient_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{v}"))
        .parse()
        .unwrap();
    assert!(!v["episodes"].as_array().unwrap().is_empty(), "{v}");
    for e in v["episodes"].as_array().unwrap() {
        let (owner,): (Uuid,) =
            sqlx::query_as("SELECT patient_id FROM care_episodes WHERE id = $1")
                .bind(uuid(&e["episode_id"]))
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(owner, pid);
        assert!(e.get("patient_id").is_none() && e.get("responsible_user_id").is_none());
        for t in e["tasks"].as_array().unwrap_or(&vec![]) {
            assert!(
                t.get("text_en").is_some() && t.get("accountable_user_id").is_none(),
                "{t}"
            );
        }
    }
    let text = v.to_string();
    for forbidden in [
        "workforce",
        "forecast",
        "gaps",
        "staff_only",
        "internal_note",
        "facts_hash",
    ] {
        assert!(
            !text.contains(forbidden),
            "patient view leaks {forbidden}: {text}"
        );
    }
    let (s, _) = call(&state, "GET", "/api/v1/me/care", NURSE, None).await;
    assert_ne!(s, StatusCode::OK);
}

#[tokio::test]
async fn dmind_care_ops_artifacts_are_advisory_and_human_reviewed() {
    let state = test_state().await;
    let fx = fixtures(&state).await;
    let full = Uuid::now_v7().simple().to_string();
    let tag = full[full.len() - 8..].to_string();

    // Care-flow brief: a persisted snapshot + an artifact awaiting review.
    let (s, v) = call(
        &state,
        "POST",
        "/api/v1/operations/snapshot/brief",
        OPS,
        Some(json!({ "facility_id": fx.facility, "language": "es" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["brief"]["mode"], "dmind", "{v}");
    assert_eq!(v["brief"]["status"], "awaiting_review");
    let brief_id = uuid(&v["brief"]["artifact_id"]);
    let snapshot_id = uuid(&v["snapshot_id"]);
    let out = &v["brief"]["output"];
    assert!(out["summary"].as_str().is_some_and(|h| !h.is_empty()));
    assert!(out["pressures"].is_array(), "{out}");
    assert!(out["cited_sources"].as_array().unwrap().iter().all(|c| {
        let c = c.as_str().unwrap();
        c == "snapshot:summary" || c.starts_with("area:") || c.contains(':')
    }));
    let row: (Option<Uuid>, Option<Uuid>, String) = sqlx::query_as(
        "SELECT patient_id, care_operations_snapshot_id, status FROM ai_artifacts WHERE id = $1",
    )
    .bind(brief_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(row, (None, Some(snapshot_id), "awaiting_review".into()));

    // Other-tenant facility: denied as not-found (anti-probing).
    let (s, _) = call(
        &state,
        "POST",
        "/api/v1/operations/snapshot/brief",
        OPS,
        Some(json!({ "facility_id": fx.other_tenant_facility })),
    )
    .await;
    assert!(
        s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
        "{s}"
    );

    // Review requires care_ops.review_ai; registration staff cannot.
    let (s, _) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/artifacts/{brief_id}/review"),
        REG,
        Some(json!({ "decision": "approved" })),
    )
    .await;
    assert!(
        s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
        "{s}"
    );
    let (s, v) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/artifacts/{brief_id}/review"),
        OPS,
        Some(json!({ "decision": "approved", "note": "ok" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "approved");
    let (s, v) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/artifacts/{brief_id}/review"),
        OPS,
        Some(json!({ "decision": "rejected" })),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    let (reviewed,): (i64,) = sqlx::query_as(
        "SELECT count(*) FROM outbox_events WHERE event_type = 'ai.artifact.reviewed'
           AND resource_refs->>'artifact_id' = $1",
    )
    .bind(brief_id.to_string())
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(reviewed, 1);

    // Placement ranking over deterministic candidates only.
    let dept = create_loc(
        &state,
        loc(
            fx.facility,
            None,
            "department",
            "department",
            &format!("AD-{tag}"),
            json!({}),
        ),
    )
    .await;
    let unit = create_loc(
        &state,
        loc(
            fx.facility,
            Some(uuid(&dept["id"])),
            "unit",
            "ward",
            &format!("AU-{tag}"),
            json!({"capacity": 2}),
        ),
    )
    .await;
    let room = create_loc(
        &state,
        loc(
            fx.facility,
            Some(uuid(&unit["id"])),
            "room",
            "room",
            &format!("AR-{tag}"),
            json!({"room_policy": "mixed", "capacity": 2}),
        ),
    )
    .await;
    for i in 1..=2 {
        create_loc(
            &state,
            loc(
                fx.facility,
                Some(uuid(&room["id"])),
                "position",
                "bed",
                &format!("AB{i}-{tag}"),
                json!({}),
            ),
        )
        .await;
    }
    let ep = episode(&state, &fx, "inpatient").await;
    let ep_id = uuid(&ep["id"]);
    // A second open episode of the same type is a typed conflict, not a 500.
    let (s, v) = call(
        &state,
        "POST",
        "/api/v1/operations/episodes",
        BEDS,
        Some(json!({
            "facility_id": fx.facility,
            "patient_id": fx.patient,
            "episode_type_code": "inpatient",
            "activate": true
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "episode_already_open");
    let req = placement_request(&state, ep_id, requirements(&[])).await;
    let req_id = uuid(&req["id"]);
    let c = candidates(&state, req_id).await;
    let feasible = c["match"]["feasible"].as_array().map_or(0, Vec::len);
    assert!(feasible >= 2, "{c}");
    let (s, v) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/placement-requests/{req_id}/ranking"),
        BEDS,
        Some(json!({ "language": "en" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["ranking"]["mode"], "dmind", "{v}");
    let ranked = v["ranking"]["output"]["ranked"]
        .as_array()
        .unwrap_or_else(|| panic!("{v}"));
    assert_eq!(ranked.len(), feasible.min(20), "{v}");
    let (bound,): (Option<Uuid>,) =
        sqlx::query_as("SELECT ranking_artifact_id FROM placement_requests WHERE id = $1")
            .bind(req_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(bound, Some(uuid(&v["ranking"]["artifact_id"])));
    // The request itself is untouched: no hold, no placement, status unchanged.
    let (_, after) = call(
        &state,
        "GET",
        &format!("/api/v1/operations/placement-requests/{req_id}"),
        BEDS,
        None,
    )
    .await;
    assert_eq!(after["request"]["status"], "candidates_ready", "{after}");

    // Discharge coordination is operational, never a clinical clearance.
    let (s, v) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/episodes/{ep_id}/discharge-coordination"),
        BEDS,
        Some(json!({ "language": "es" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["coordination"]["mode"], "dmind", "{v}");
    assert_eq!(v["coordination"]["output"]["operational_only"], true, "{v}");
    let text = v["coordination"]["output"].to_string().to_lowercase();
    assert!(!text.contains("alta médica autorizada") && !text.contains("medically cleared"));

    // Handoff draft: stored as a dMind draft, becomes a handoff only when accepted.
    let (s, v) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/episodes/{ep_id}/handoffs/draft"),
        BEDS,
        Some(json!({ "shift_label": "Night", "language": "es" })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{v}");
    assert_eq!(v["handoff"]["status"], "draft", "{v}");
    assert_eq!(v["handoff"]["origin"], "dmind_draft");
    assert_eq!(v["handoff"]["artifact_id"], v["draft"]["artifact_id"]);
    let handoff_id = uuid(&v["handoff"]["id"]);
    let (s, v) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/handoffs/{handoff_id}/decision"),
        NURSE,
        Some(json!({ "expected_version": v["handoff"]["version"], "decision": "accept" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "accepted");
    let (artifact_status,): (String,) =
        sqlx::query_as("SELECT status FROM ai_artifacts WHERE id = $1")
            .bind(uuid(&v["artifact_id"]))
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(artifact_status, "approved");
    let (s, _) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/episodes/{ep_id}/handoffs/draft"),
        BEDS,
        Some(json!({ "language": "es" })),
    )
    .await;
    assert_eq!(s, StatusCode::BAD_REQUEST);

    // Forecast explanation on an insufficient-history forecast keeps the
    // uncertainty explicit instead of inventing precision.
    let (s, f) = call(
        &state,
        "POST",
        "/api/v1/operations/forecasts",
        OPS,
        Some(json!({ "facility_id": fx.facility, "horizon": "24h" })),
    )
    .await;
    assert!(s == StatusCode::OK || s == StatusCode::CREATED, "{f}");
    let fid = uuid(&f["id"]);
    let (s, v) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/forecasts/{fid}/explanation"),
        OPS,
        Some(json!({ "language": "en" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["explanation"]["mode"], "dmind", "{v}");
    if f["status"] == "insufficient_history" {
        assert!(
            !v["explanation"]["output"]["missing_information"]
                .as_array()
                .unwrap()
                .is_empty()
                || v["explanation"]["output"]["summary"]
                    .as_str()
                    .unwrap()
                    .to_lowercase()
                    .contains("history"),
            "{v}"
        );
    }
    let (bound,): (Option<Uuid>,) = sqlx::query_as(
        "SELECT explanation_artifact_id FROM care_operations_forecasts WHERE id = $1",
    )
    .bind(fid)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(bound, Some(uuid(&v["explanation"]["artifact_id"])));

    // Patient/representative cannot request or review any of these.
    let (s, _) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/placement-requests/{req_id}/ranking"),
        PATIENT,
        Some(json!({})),
    )
    .await;
    assert!(
        s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
        "{s}"
    );
}
