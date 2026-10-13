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
    // Other suites book hundreds of cases at this facility; list the fixture
    // dental case through its episode so the bounded list stays deterministic.
    let (dental_episode,): (Uuid,) = sqlx::query_as(
        "SELECT episode_id FROM perioperative_cases
         WHERE tenant_id = $1 AND facility_id = $2
           AND procedure_code = 'dental_extraction_sedation'
           AND status IN ('requested','ready_for_scheduling')
         ORDER BY created_at LIMIT 1",
    )
    .bind(fx.tenant)
    .bind(fx.facility)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let (s, cases) = call(
        &state,
        "GET",
        &format!(
            "/api/v1/operations/theatre/cases?facility_id={}&episode_id={dental_episode}",
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
    let fx = fixtures(&state).await;
    let (s, v) = call(&state, "GET", "/api/v1/me/care", PATIENT, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let pid: Uuid = v["patient_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{v}"))
        .parse()
        .unwrap();
    // The seeded representative's patient has only a completed episode
    // (history fixture); a rehabilitation episode left by an interrupted run
    // is cancelled so the test owns the patient's active care.
    for e in v["episodes"].as_array().unwrap() {
        assert_ne!(e["status"], "completed", "history is not active care: {v}");
        if e["episode_type_code"] == "rehabilitation" {
            let stale = uuid(&e["episode_id"]);
            let row = {
                let mut conn = state.pool.acquire().await.unwrap();
                wellos_server::care_ops::load_episode(&mut conn, fx.tenant, stale, false)
                    .await
                    .unwrap()
            };
            let (s, v) = call(
                &state,
                "POST",
                &format!("/api/v1/operations/episodes/{stale}/transition"),
                BEDS,
                Some(json!({ "transition": "cancel", "expected_version": row.version, "reason_code": "test_cleanup" })),
            )
            .await;
            assert_eq!(s, StatusCode::OK, "{v}");
        }
    }

    // Open an episode for that patient with staff-only and patient-visible
    // work, then read it back as the representative.
    let (s, ep) = call(
        &state,
        "POST",
        "/api/v1/operations/episodes",
        BEDS,
        Some(json!({
            "facility_id": fx.facility,
            "patient_id": pid,
            "episode_type_code": "rehabilitation",
            "responsible_service_code": "physiotherapy",
            "priority": "routine",
            "activate": true
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{ep}");
    let episode_id = uuid(&ep["id"]);
    let (nurse,): (Uuid,) = sqlx::query_as("SELECT id FROM users WHERE username = 'nurse.kim'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    for (title, visible) in [
        ("Staff-only internal_note review", false),
        ("Bring walking aid", true),
    ] {
        let (s, t) = call(
            &state,
            "POST",
            "/api/v1/operations/tasks",
            BEDS,
            Some(json!({
                "episode_id": episode_id,
                "title_en": title,
                "title_es": title,
                "kind": "coordination",
                "accountable_user_id": nurse,
                "patient_visible": visible,
                "patient_text_en": visible.then_some("Please bring your walking aid"),
                "patient_text_es": visible.then_some("Traiga su andador"),
            })),
        )
        .await;
        assert_eq!(s, StatusCode::CREATED, "{t}");
    }
    let (s, v) = call(&state, "GET", "/api/v1/me/care", PATIENT, None).await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let mine = v["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| uuid(&e["episode_id"]) == episode_id)
        .unwrap_or_else(|| panic!("active episode visible to its representative: {v}"));
    let tasks = mine["pending_tasks"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    assert_eq!(tasks.len(), 1, "only patient-visible work: {v}");
    assert_eq!(tasks[0]["text_en"], "Please bring your walking aid");
    for e in v["episodes"].as_array().unwrap() {
        let (owner,): (Uuid,) =
            sqlx::query_as("SELECT patient_id FROM care_episodes WHERE id = $1")
                .bind(uuid(&e["episode_id"]))
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(owner, pid);
        assert!(e.get("patient_id").is_none() && e.get("responsible_user_id").is_none());
        for t in e["pending_tasks"].as_array().unwrap_or(&vec![]) {
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

    // Leave the shared representative fixture as seeded.
    let ep = {
        let mut conn = state.pool.acquire().await.unwrap();
        wellos_server::care_ops::load_episode(&mut conn, fx.tenant, episode_id, false)
            .await
            .unwrap()
    };
    let (s, v) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/episodes/{episode_id}/transition"),
        BEDS,
        Some(json!({
            "transition": "cancel",
            "expected_version": ep.version,
            "reason_code": "test_cleanup"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
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

#[tokio::test]
async fn fhir_care_operations_subset_is_authorized_and_linked() {
    let state = test_state().await;
    let fx = fixtures(&state).await;
    let ep = episode(&state, &fx, "inpatient").await;
    let episode_id = uuid(&ep["id"]);
    let (nurse,): (Uuid,) = sqlx::query_as("SELECT id FROM users WHERE username = 'nurse.kim'")
        .fetch_one(&state.pool)
        .await
        .unwrap();

    // A clinical encounter linked to the episode.
    let encounter_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO encounters (id, tenant_id, facility_id, patient_id, practitioner_id, status)
         VALUES ($1, $2, $3, $4, $5, 'in_progress')",
    )
    .bind(encounter_id)
    .bind(fx.tenant)
    .bind(fx.facility)
    .bind(fx.patient)
    .bind(nurse)
    .execute(&state.pool)
    .await
    .unwrap();
    let (s, v) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/episodes/{episode_id}/links"),
        BEDS,
        Some(json!({ "link_type": "encounter", "link_id": encounter_id })),
    )
    .await;
    assert!(s == StatusCode::CREATED || s == StatusCode::OK, "{s} {v}");

    // A coordination task accountable to a named professional.
    let (s, task) = call(
        &state,
        "POST",
        "/api/v1/operations/tasks",
        BEDS,
        Some(json!({
            "episode_id": episode_id,
            "title_en": "Arrange transport home",
            "title_es": "Organizar transporte a domicilio",
            "kind": "transport",
            "accountable_user_id": nurse,
            "due_at": (Utc::now() + Duration::hours(6)).to_rfc3339(),
            "barrier_category_code": "transport"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{task}");
    let task_id = uuid(&task["id"]);

    let (bed,): (Uuid,) = sqlx::query_as(
        "SELECT id FROM operational_locations WHERE tenant_id = $1 AND level = 'position' AND facility_id = $2 ORDER BY code LIMIT 1",
    )
    .bind(fx.tenant)
    .bind(fx.facility)
    .fetch_one(&state.pool)
    .await
    .unwrap();

    // EpisodeOfCare: operational status mapped, patient referenced, encounter
    // and task carried as extensions, readiness is operational only.
    let (s, eoc) = call(
        &state,
        "GET",
        &format!("/fhir/r4/EpisodeOfCare/{episode_id}"),
        OPS,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{eoc}");
    assert_eq!(eoc["resourceType"], "EpisodeOfCare");
    assert_eq!(eoc["status"], "active");
    assert_eq!(
        eoc["patient"]["reference"],
        format!("Patient/{}", fx.patient)
    );
    assert_eq!(eoc["type"][0]["coding"][0]["code"], "inpatient");
    let ext = eoc["extension"].as_array().unwrap();
    assert!(ext.iter().any(|e| {
        e["url"] == "urn:wellos:linked-encounter"
            && e["valueReference"]["reference"] == format!("Encounter/{encounter_id}")
    }));
    assert!(ext.iter().any(|e| {
        e["url"] == "urn:wellos:coordination-task"
            && e["valueReference"]["reference"] == format!("Task/{task_id}")
    }));
    assert!(ext.iter().any(|e| {
        e["url"] == "urn:wellos:operational-discharge-readiness" && e["valueCode"] == "active"
    }));

    // Encounter links back to the episode.
    let (s, enc) = call(
        &state,
        "GET",
        &format!("/fhir/r4/Encounter/{encounter_id}"),
        OPS,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{enc}");
    assert_eq!(enc["status"], "in-progress");
    assert_eq!(
        enc["episodeOfCare"][0]["reference"],
        format!("EpisodeOfCare/{episode_id}")
    );

    // Task: FHIR status/intent, patient, episode, owner, due time.
    let (s, t) = call(
        &state,
        "GET",
        &format!("/fhir/r4/Task/{task_id}"),
        OPS,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{t}");
    assert_eq!(t["status"], "requested");
    assert_eq!(t["intent"], "order");
    assert_eq!(t["code"]["coding"][0]["code"], "transport");
    assert_eq!(t["for"]["reference"], format!("Patient/{}", fx.patient));
    assert_eq!(
        t["basedOn"][0]["reference"],
        format!("EpisodeOfCare/{episode_id}")
    );
    assert_eq!(t["owner"]["reference"], format!("Practitioner/{nurse}"));
    assert!(t["restriction"]["period"]["end"].is_string());

    // Location: hierarchy level, parent and v2-0116 bed status.
    let (s, loc) = call(
        &state,
        "GET",
        &format!("/fhir/r4/Location/{bed}"),
        OPS,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{loc}");
    assert_eq!(loc["physicalType"]["coding"][0]["code"], "position");
    assert!(loc["partOf"]["reference"]
        .as_str()
        .unwrap()
        .starts_with("Location/"));
    assert_eq!(
        loc["operationalStatus"]["system"],
        "http://terminology.hl7.org/CodeSystem/v2-0116"
    );
    assert!(loc["operationalStatus"]["code"].is_string());

    // Patients/representatives and clinicians without care_ops.read are
    // refused; unknown ids are not found.
    for path in [
        format!("/fhir/r4/EpisodeOfCare/{episode_id}"),
        format!("/fhir/r4/Encounter/{encounter_id}"),
        format!("/fhir/r4/Task/{task_id}"),
        format!("/fhir/r4/Location/{bed}"),
    ] {
        let (s, _) = call(&state, "GET", &path, PATIENT, None).await;
        assert_eq!(s, StatusCode::FORBIDDEN, "{path} for a representative");
    }
    let (s, _) = call(
        &state,
        "GET",
        &format!("/fhir/r4/EpisodeOfCare/{}", Uuid::now_v7()),
        OPS,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::NOT_FOUND);
}

/// Review regression: a hold that expired must be released (position back to
/// available, request back to candidates_ready) by the very confirmation that
/// refuses it, not only by a later expiry tick.
#[tokio::test]
async fn expired_hold_is_released_by_the_refusing_confirmation() {
    let state = test_state().await;
    let fx = fixtures(&state).await;
    let ep = episode(&state, &fx, "inpatient").await;
    let req = placement_request(&state, uuid(&ep["id"]), requirements(&[])).await;
    let req_id = uuid(&req["id"]);
    let c = candidates(&state, req_id).await;
    let bed = c["match"]["feasible"][0]["location_id"].clone();
    assert!(bed.is_string(), "no feasible bed: {}", c["match"]);
    let (s, h) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/placement-requests/{req_id}/hold"),
        BEDS,
        Some(json!({"location_id": bed, "expected_version": c["request"]["version"], "facts_hash": c["match"]["facts_hash"], "hold_minutes": 30})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{h}");
    let placement_id = uuid(&h["placement"]["id"]);
    sqlx::query("UPDATE placements SET expires_at = now() - interval '1 minute' WHERE id = $1")
        .bind(placement_id)
        .execute(&state.pool)
        .await
        .unwrap();
    let (s, v) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/placement-requests/{req_id}/confirm"),
        BEDS,
        Some(json!({"expected_version": h["request"]["version"], "location_id": bed, "facts_hash": c["match"]["facts_hash"]})),
    )
    .await;
    assert_eq!(s, StatusCode::CONFLICT, "{v}");
    assert_eq!(v["error"]["code"], "hold_expired", "{v}");
    let (p_status,): (String,) = sqlx::query_as("SELECT status FROM placements WHERE id = $1")
        .bind(placement_id)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    assert_eq!(p_status, "expired", "release must survive the 409");
    let (pos_state,): (String,) =
        sqlx::query_as("SELECT position_state FROM operational_locations WHERE id = $1")
            .bind(uuid(&bed))
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(pos_state, "available");
    let (r_status,): (String,) =
        sqlx::query_as("SELECT status FROM placement_requests WHERE id = $1")
            .bind(req_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(r_status, "candidates_ready");
    // The bed is immediately holdable again.
    let c2 = candidates(&state, req_id).await;
    assert!(
        c2["match"]["feasible"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["location_id"] == bed),
        "{}",
        c2["match"]
    );
    let (ep_version,): (i64,) = sqlx::query_as("SELECT version FROM care_episodes WHERE id = $1")
        .bind(uuid(&ep["id"]))
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let (s, v) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/episodes/{}/transition", uuid(&ep["id"])),
        BEDS,
        Some(json!({"transition": "cancel", "expected_version": ep_version, "reason_code": "test_cleanup"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

/// Review regression: closing an episode cancels its open coordination tasks
/// (with history) so they leave queues and overdue counts.
#[tokio::test]
async fn terminal_episode_cancels_open_coordination_tasks() {
    let state = test_state().await;
    let fx = fixtures(&state).await;
    let ep = episode(&state, &fx, "inpatient").await;
    let (nurse,): (Uuid,) = sqlx::query_as("SELECT id FROM users WHERE username = 'nurse.kim'")
        .bind(fx.tenant)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let (s, task) = call(
        &state,
        "POST",
        "/api/v1/operations/tasks",
        BEDS,
        Some(json!({
            "episode_id": ep["id"],
            "title_en": "Arrange transport",
            "title_es": "Organizar transporte",
            "kind": "coordination",
            "accountable_user_id": nurse,
            "patient_visible": false
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{task}");
    let task_id = uuid(&task["id"]);
    let (s, done) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/tasks/{task_id}/transition"),
        BEDS,
        Some(json!({"transition": "start", "expected_version": task["version"]})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{done}");
    let (s, v) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/episodes/{}/transition", uuid(&ep["id"])),
        BEDS,
        Some(json!({"transition": "cancel", "expected_version": ep["version"], "reason_code": "patient_left"})),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    assert_eq!(v["status"], "cancelled", "{v}");
    let (t_status, t_version): (String, i64) =
        sqlx::query_as("SELECT status, version FROM care_coordination_tasks WHERE id = $1")
            .bind(task_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(t_status, "cancelled");
    assert_eq!(t_version, done["version"].as_i64().unwrap() + 1);
    let (from, kind, reason): (Option<String>, String, Option<String>) = sqlx::query_as(
        "SELECT from_status, change_kind, reason_code FROM care_coordination_task_history WHERE task_id = $1 AND to_status = 'cancelled'",
    )
    .bind(task_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(from.as_deref(), Some("in_progress"));
    assert_eq!(kind, "episode_closed");
    assert_eq!(reason.as_deref(), Some("episode_cancelled"));
    let (s, open) = call(
        &state,
        "GET",
        &format!(
            "/api/v1/operations/tasks?facility_id={}&open_only=true",
            fx.facility
        ),
        BEDS,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{open}");
    let listed = open
        .as_array()
        .cloned()
        .or_else(|| open["tasks"].as_array().cloned())
        .unwrap_or_default();
    assert!(
        !listed
            .iter()
            .any(|t| t["id"] == task["id"] || t["task"]["id"] == task["id"]),
        "cancelled task still queued: {open}"
    );
}

/// Review regression: an already scheduled case is *rescheduled* (not refused
/// as an invalid transition); the previous theatre booking is released and the
/// new interval booked.
#[tokio::test]
async fn scheduled_theatre_case_can_be_rescheduled() {
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
    let case = items
        .iter()
        .map(|c| c["case"].clone())
        .find(|c| c["status"] == "scheduled")
        .unwrap_or_else(|| panic!("fixture scheduled case missing: {cases}"));
    let theatre = uuid(&case["theatre_location_id"]);
    let (theatre_version,): (i64,) =
        sqlx::query_as("SELECT version FROM operational_locations WHERE id = $1")
            .bind(theatre)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let old_start: chrono::DateTime<Utc> =
        case["scheduled_start"].as_str().unwrap().parse().unwrap();
    let old_booking = case["booking_id"]
        .as_str()
        .map(|b| b.parse::<Uuid>().unwrap());
    let new_start = old_start + Duration::minutes(45);
    let (s, v) = call(
        &state,
        "POST",
        &format!(
            "/api/v1/operations/theatre/cases/{}/schedule",
            uuid(&case["id"])
        ),
        THEATRE,
        Some(json!({
            "expected_version": case["version"],
            "theatre_location_id": theatre,
            "expected_theatre_version": theatre_version,
            "scheduled_start": new_start,
            "team_user_ids": case["team_user_ids"],
            "override_reason": "surgeon availability changed"
        })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
    let out = &v["case"];
    assert_eq!(out["status"], "scheduled", "{v}");
    let got: chrono::DateTime<Utc> = out["scheduled_start"].as_str().unwrap().parse().unwrap();
    assert_eq!(got, new_start);
    assert_eq!(out["version"], json!(case["version"].as_i64().unwrap() + 1));
    if let Some(old) = old_booking {
        let (old_status,): (String,) =
            sqlx::query_as("SELECT status FROM resource_bookings WHERE id = $1")
                .bind(old)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(old_status, "released");
        let new_booking = uuid(&out["booking_id"]);
        assert_ne!(new_booking, old);
        let (new_status, starts_at): (String, chrono::DateTime<Utc>) =
            sqlx::query_as("SELECT status, starts_at FROM resource_bookings WHERE id = $1")
                .bind(new_booking)
                .fetch_one(&state.pool)
                .await
                .unwrap();
        assert_eq!(new_status, "active");
        assert_eq!(starts_at, new_start);
    }
    let (kind,): (String,) = sqlx::query_as(
        "SELECT change_kind FROM perioperative_case_history WHERE case_id = $1 ORDER BY recorded_at DESC, version DESC LIMIT 1",
    )
    .bind(uuid(&case["id"]))
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(kind, "rescheduled");
}

/// Review regression: reassigning a shift assignment to a shift at a facility
/// outside the coordinator's scope is refused and leaves the assignment intact.
#[tokio::test]
async fn workforce_reassignment_to_out_of_scope_facility_is_refused() {
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
    let shift = shifts
        .as_array()
        .cloned()
        .or_else(|| shifts["shifts"].as_array().cloned())
        .unwrap()
        .first()
        .map(|s| s["shift"].clone())
        .unwrap_or_else(|| panic!("fixture shift missing: {shifts}"));
    let (nurse,): (Uuid,) = sqlx::query_as("SELECT id FROM users WHERE username = 'nurse.kim'")
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let (s, a) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/workforce/shifts/{}/assignments", uuid(&shift["id"])),
        WORKFORCE,
        Some(json!({"user_id": nurse, "profession_code": "nurse", "capability_codes": ["acute_care"], "specialty_codes": [], "role_in_shift": "float"})),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{a}");
    let assignment_id = uuid(&a["id"]);
    // A shift at a facility the workforce coordinator is not assigned to.
    let (other_facility,): (Uuid,) = sqlx::query_as(
        "SELECT id FROM facilities WHERE tenant_id = $1 AND id <> $2 ORDER BY name LIMIT 1",
    )
    .bind(fx.tenant)
    .bind(fx.facility)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let target = Uuid::now_v7();
    let starts = Utc::now() + Duration::hours(1);
    sqlx::query(
        "INSERT INTO workforce_shifts (id, tenant_id, facility_id, label, local_date, starts_at, ends_at, created_by)
         VALUES ($1,$2,$3,'Out of scope','2030-01-01',$4,$5,$6)",
    )
    .bind(target)
    .bind(fx.tenant)
    .bind(other_facility)
    .bind(starts)
    .bind(starts + Duration::hours(8))
    .bind(nurse)
    .execute(&state.pool)
    .await
    .unwrap();
    let (s, v) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/workforce/assignments/{assignment_id}"),
        WORKFORCE,
        Some(json!({"expected_version": a["version"], "status": "reassigned", "target_shift_id": target, "reason": "cover"})),
    )
    .await;
    assert!(
        s == StatusCode::FORBIDDEN || s == StatusCode::NOT_FOUND,
        "cross-facility reassignment accepted: {s} {v}"
    );
    let (status, shift_id, version): (String, Uuid, i64) = sqlx::query_as(
        "SELECT status, shift_id, version FROM workforce_shift_assignments WHERE id = $1",
    )
    .bind(assignment_id)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(status, a["status"].as_str().unwrap());
    assert_eq!(shift_id, uuid(&shift["id"]));
    assert_eq!(version, a["version"].as_i64().unwrap());
    let (moved,): (i64,) =
        sqlx::query_as("SELECT count(*) FROM workforce_shift_assignments WHERE shift_id = $1")
            .bind(target)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert_eq!(moved, 0);
    sqlx::query("DELETE FROM workforce_shift_assignments WHERE id = $1")
        .bind(assignment_id)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query("DELETE FROM workforce_shifts WHERE id = $1")
        .bind(target)
        .execute(&state.pool)
        .await
        .unwrap();
}

/// Review regression: the patient care view lists only notifications that
/// were actually delivered; scheduled/in-flight/failed ones stay invisible.
#[tokio::test]
async fn patient_care_view_hides_undelivered_notifications() {
    let state = test_state().await;
    let (s, me) = call(&state, "GET", "/api/v1/me/care", PATIENT, None).await;
    assert_eq!(s, StatusCode::OK, "{me}");
    let pid: Uuid = me["patient_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{me}"))
        .parse()
        .unwrap();
    let (tenant,): (Uuid,) = sqlx::query_as("SELECT tenant_id FROM patients WHERE id = $1")
        .bind(pid)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let mut ids = Vec::new();
    for status in ["scheduled", "delivering", "failed", "delivered"] {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO notifications (id, tenant_id, patient_id, kind, dedupe_key, scheduled_for, status, delivered_at)
             VALUES ($1,$2,$3,'reminder',$4, now() - interval '1 minute', $5, CASE WHEN $5 = 'delivered' THEN now() END)",
        )
        .bind(id)
        .bind(tenant)
        .bind(pid)
        .bind(format!("review-test-{id}"))
        .bind(status)
        .execute(&state.pool)
        .await
        .unwrap();
        ids.push((status, id));
    }
    let (s, care) = call(&state, "GET", "/api/v1/me/care", PATIENT, None).await;
    assert_eq!(s, StatusCode::OK, "{care}");
    let listed: Vec<Uuid> = care["notifications"]
        .as_array()
        .unwrap()
        .iter()
        .map(|n| uuid(&n["id"]))
        .collect();
    for (status, id) in &ids {
        if *status == "delivered" {
            assert!(
                listed.contains(id),
                "delivered notification missing: {care}"
            );
        } else {
            assert!(
                !listed.contains(id),
                "{status} notification exposed: {care}"
            );
        }
    }
    assert!(
        care["notifications"]
            .as_array()
            .unwrap()
            .iter()
            .all(|n| n["status"] == "delivered" || n["status"] == "partially_delivered"),
        "{care}"
    );
    for (_, id) in ids {
        sqlx::query("DELETE FROM notifications WHERE id = $1")
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
    }
}

/// Review regression: a self-service grant is authorised within the
/// facilities the caller's assignments cover; an episode of the same patient
/// at another facility of the tenant is not exposed by `/me/care`.
#[tokio::test]
async fn patient_care_view_is_limited_to_the_grant_facility_scope() {
    let state = test_state().await;
    let fx = fixtures(&state).await;
    let (s, me) = call(&state, "GET", "/api/v1/me/care", PATIENT, None).await;
    assert_eq!(s, StatusCode::OK, "{me}");
    let pid: Uuid = me["patient_id"]
        .as_str()
        .unwrap_or_else(|| panic!("{me}"))
        .parse()
        .unwrap();
    let (other_facility,): (Uuid,) = sqlx::query_as(
        "SELECT id FROM facilities WHERE tenant_id = $1 AND id <> $2 ORDER BY name LIMIT 1",
    )
    .bind(fx.tenant)
    .bind(fx.facility)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let (creator,): (Uuid,) =
        sqlx::query_as("SELECT id FROM users WHERE tenant_id = $1 AND username = 'ops.lopez'")
            .bind(fx.tenant)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let foreign = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO care_episodes (id, tenant_id, facility_id, patient_id, episode_type_code, status,
                                    responsible_service_code, started_at, priority, created_by)
         VALUES ($1,$2,$3,$4,'inpatient','active','internal_medicine',now(),'routine',$5)",
    )
    .bind(foreign)
    .bind(fx.tenant)
    .bind(other_facility)
    .bind(pid)
    .bind(creator)
    .execute(&state.pool)
    .await
    .unwrap();
    let (s, v) = call(&state, "GET", "/api/v1/me/care", PATIENT, None).await;
    sqlx::query("DELETE FROM care_episodes WHERE id = $1")
        .bind(foreign)
        .execute(&state.pool)
        .await
        .unwrap();
    assert_eq!(s, StatusCode::OK, "{v}");
    let episodes = v["episodes"].as_array().unwrap();
    assert!(
        episodes.iter().all(|e| uuid(&e["episode_id"]) != foreign),
        "episode at another facility must not be exposed: {v}"
    );
    assert!(
        episodes
            .iter()
            .all(|e| uuid(&e["facility_id"]) == fx.facility),
        "every exposed episode is within the grant facility scope: {v}"
    );
}

/// Review flag: `pending_tasks` in `/me/care` must list only work that is
/// still open; completed tasks never crowd open ones out of the 20-row limit.
#[tokio::test]
async fn patient_care_view_lists_only_open_tasks() {
    let state = test_state().await;
    let fx = fixtures(&state).await;
    let (s, me) = call(&state, "GET", "/api/v1/me/care", PATIENT, None).await;
    assert_eq!(s, StatusCode::OK, "{me}");
    let pid = uuid(&me["patient_id"]);
    // An observation episode left by an interrupted run is cancelled so this
    // test owns the patient's observation care (one open episode per type).
    for e in me["episodes"].as_array().unwrap() {
        if e["episode_type_code"] == "observation" {
            let stale = uuid(&e["episode_id"]);
            let (version,): (i64,) =
                sqlx::query_as("SELECT version FROM care_episodes WHERE id = $1")
                    .bind(stale)
                    .fetch_one(&state.pool)
                    .await
                    .unwrap();
            let (s, v) = call(
                &state,
                "POST",
                &format!("/api/v1/operations/episodes/{stale}/transition"),
                BEDS,
                Some(json!({ "transition": "cancel", "expected_version": version, "reason_code": "test_cleanup" })),
            )
            .await;
            assert_eq!(s, StatusCode::OK, "{v}");
        }
    }
    let (s, ep) = call(
        &state,
        "POST",
        "/api/v1/operations/episodes",
        BEDS,
        Some(json!({
            "facility_id": fx.facility,
            "patient_id": pid,
            "episode_type_code": "observation",
            "responsible_service_code": "internal_medicine",
            "priority": "routine",
            "activate": true
        })),
    )
    .await;
    assert_eq!(s, StatusCode::CREATED, "{ep}");
    let episode = uuid(&ep["id"]);
    let (tenant, facility) = (fx.tenant, fx.facility);
    let (creator,): (Uuid,) =
        sqlx::query_as("SELECT id FROM users WHERE tenant_id = $1 AND username = 'bed.ortiz'")
            .bind(tenant)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let mut ids = Vec::new();
    for status in ["completed", "open", "in_progress", "cancelled"] {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO care_coordination_tasks
               (id, tenant_id, facility_id, episode_id, patient_id, title_en, title_es, status,
                patient_visible, patient_text_en, patient_text_es, created_by,
                accountable_user_id, completed_at, completed_by)
             VALUES ($1,$2,$3,$4,$5,$6,$6,$7,true,$6,$6,$8,$8,
                     CASE WHEN $7 = 'completed' THEN now() END,
                     CASE WHEN $7 = 'completed' THEN $8 END)",
        )
        .bind(id)
        .bind(tenant)
        .bind(facility)
        .bind(episode)
        .bind(pid)
        .bind(format!("review-task-{status}"))
        .bind(status)
        .bind(creator)
        .execute(&state.pool)
        .await
        .unwrap();
        ids.push((status, id));
    }
    let (s, care) = call(&state, "GET", "/api/v1/me/care", PATIENT, None).await;
    assert_eq!(s, StatusCode::OK, "{care}");
    let ep = care["episodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| uuid(&e["episode_id"]) == episode)
        .unwrap_or_else(|| panic!("episode missing from /me/care: {care}"));
    let listed: Vec<Uuid> = ep["pending_tasks"]
        .as_array()
        .unwrap()
        .iter()
        .map(|t| uuid(&t["id"]))
        .collect();
    for (status, id) in &ids {
        let open = matches!(*status, "open" | "in_progress");
        assert_eq!(
            listed.contains(id),
            open,
            "{status} task visibility wrong: {care}"
        );
    }
    assert!(
        ep["pending_tasks"]
            .as_array()
            .unwrap()
            .iter()
            .all(|t| t["status"] != "completed" && t["status"] != "cancelled"),
        "{care}"
    );
    for (_, id) in ids {
        sqlx::query("DELETE FROM care_coordination_tasks WHERE id = $1")
            .bind(id)
            .execute(&state.pool)
            .await
            .unwrap();
    }
    let (ep_version,): (i64,) = sqlx::query_as("SELECT version FROM care_episodes WHERE id = $1")
        .bind(episode)
        .fetch_one(&state.pool)
        .await
        .unwrap();
    let (s, v) = call(
        &state,
        "POST",
        &format!("/api/v1/operations/episodes/{episode}/transition"),
        BEDS,
        Some(json!({ "transition": "cancel", "expected_version": ep_version, "reason_code": "test_cleanup" })),
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{v}");
}

/// Review flag: the snapshot's 24 h theatre totals are computed over every
/// case in the window, not over the first 200 open cases of the facility.
#[tokio::test]
async fn snapshot_theatre_totals_are_not_capped_by_a_row_limit() {
    let state = test_state().await;
    let fx = fixtures(&state).await;
    let ep = episode(&state, &fx, "inpatient").await;
    let (theatre,): (Uuid,) = sqlx::query_as(
        "SELECT id FROM operational_locations
         WHERE tenant_id = $1 AND facility_id = $2 AND type_code = 'theatre' AND active LIMIT 1",
    )
    .bind(fx.tenant)
    .bind(fx.facility)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let (creator,): (Uuid,) =
        sqlx::query_as("SELECT id FROM users WHERE tenant_id = $1 AND username = 'theatre.ruiz'")
            .bind(fx.tenant)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let (s, before) = call(
        &state,
        "GET",
        &format!("/api/v1/operations/snapshot?facility_id={}", fx.facility),
        OPS,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{before}");
    let baseline = before["theatre"]["cases_next_24h"].as_i64().unwrap();
    let extra = 201i64;
    let base_start = Utc::now() + Duration::hours(2);
    let mut ids = Vec::with_capacity(extra as usize);
    for i in 0..extra {
        let id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO perioperative_cases
               (id, tenant_id, facility_id, episode_id, patient_id, procedure_code,
                responsible_service_code, status, expected_duration_minutes,
                theatre_location_id, scheduled_start, scheduled_end, created_by)
             VALUES ($1,$2,$3,$4,$5,'review-volume','surgery','scheduled',30,$6,$7,$8,$9)",
        )
        .bind(id)
        .bind(fx.tenant)
        .bind(fx.facility)
        .bind(uuid(&ep["id"]))
        .bind(fx.patient)
        .bind(theatre)
        .bind(base_start + Duration::minutes(i))
        .bind(base_start + Duration::minutes(i + 30))
        .bind(creator)
        .execute(&state.pool)
        .await
        .unwrap();
        ids.push(id);
    }
    let (s, after) = call(
        &state,
        "GET",
        &format!("/api/v1/operations/snapshot?facility_id={}", fx.facility),
        OPS,
        None,
    )
    .await;
    assert_eq!(s, StatusCode::OK, "{after}");
    let counted = after["theatre"]["cases_next_24h"].as_i64().unwrap();
    assert!(
        counted >= baseline + extra,
        "snapshot counted {counted} cases, expected at least {}",
        baseline + extra
    );
    sqlx::query("DELETE FROM perioperative_cases WHERE id = ANY($1)")
        .bind(&ids)
        .execute(&state.pool)
        .await
        .unwrap();
}
