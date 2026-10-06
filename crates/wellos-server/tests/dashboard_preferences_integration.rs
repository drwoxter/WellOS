//! Per-user cockpit layout: tenant/user isolation, strict validation of the
//! layout document, versioned writes and role gating.

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
    token: &str,
    body: Option<Value>,
) -> (StatusCode, Value) {
    let req = Request::builder()
        .method(method)
        .uri("/api/v1/me/dashboard-preferences")
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .header("x-purpose-of-use", "treatment")
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
        serde_json::from_slice(&bytes)
            .unwrap_or(Value::String(String::from_utf8_lossy(&bytes).into_owned()))
    };
    (status, value)
}

fn layout(first: &str) -> Value {
    let mut order: Vec<&str> = wellos_server::routes::dashboard::WIDGETS.to_vec();
    order.retain(|w| *w != first);
    order.insert(0, first);
    json!({
        "order": order,
        "hidden": ["triage", "triage"],
        "sizes": { "results": "full" },
        "density": "compact",
    })
}

async fn reset(state: &AppState, usernames: &[&str]) {
    sqlx::query(
        "DELETE FROM dashboard_preferences
         WHERE user_id IN (SELECT id FROM users WHERE username = ANY($1))",
    )
    .bind(usernames)
    .execute(&state.pool)
    .await
    .unwrap();
}

#[tokio::test]
async fn layouts_are_per_user_per_tenant_and_versioned() {
    let state = test_state().await;
    reset(&state, &["dr.garcia", "nurse.kim", "dr.sur"]).await;

    // Nothing stored yet: an honest empty answer, version 0.
    let (st, v) = call(&state, "GET", "dev-dr.garcia", None).await;
    assert_eq!(st, StatusCode::OK, "{v}");
    assert!(v["layout"].is_null(), "{v}");
    assert_eq!(v["version"], 0);

    let (st, saved) = call(
        &state,
        "PUT",
        "dev-dr.garcia",
        Some(json!({ "layout": layout("results"), "version": 0 })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{saved}");
    assert_eq!(saved["version"], 1);
    assert_eq!(saved["layout"]["order"][0], "results");
    // Duplicates are normalised away; sizes and density are kept.
    assert_eq!(saved["layout"]["hidden"], json!(["triage"]));
    assert_eq!(saved["layout"]["sizes"]["results"], "full");
    assert_eq!(saved["layout"]["density"], "compact");

    // A stale version cannot overwrite; the current layout is returned.
    let (st, conflict) = call(
        &state,
        "PUT",
        "dev-dr.garcia",
        Some(json!({ "layout": layout("tasks"), "version": 0 })),
    )
    .await;
    assert_eq!(st, StatusCode::CONFLICT, "{conflict}");
    assert_eq!(conflict["error"]["code"], "layout_conflict");
    let (_, still) = call(&state, "GET", "dev-dr.garcia", None).await;
    assert_eq!(still["layout"]["order"][0], "results", "{still}");

    let (st, saved2) = call(
        &state,
        "PUT",
        "dev-dr.garcia",
        Some(json!({ "layout": layout("tasks"), "version": 1 })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{saved2}");
    assert_eq!(saved2["version"], 2);

    // Another user in the same tenant and a user in another tenant see
    // nothing of it.
    for other in ["dev-nurse.kim", "dev-dr.sur"] {
        let (st, v) = call(&state, "GET", other, None).await;
        assert_eq!(st, StatusCode::OK, "{other}: {v}");
        assert!(
            v["layout"].is_null(),
            "{other} must not see dr.garcia's layout: {v}"
        );
        assert_eq!(v["version"], 0);
    }
    let (st, mine) = call(&state, "GET", "dev-dr.garcia", None).await;
    assert_eq!(st, StatusCode::OK);
    assert_eq!(mine["layout"]["order"][0], "tasks");
    assert_eq!(mine["version"], 2);
}

#[tokio::test]
async fn only_well_formed_layouts_are_stored() {
    let state = test_state().await;
    reset(&state, &["lab.chen"]).await;
    let mut cases: Vec<(&str, Value)> = vec![
        ("not an object", json!([1, 2])),
        ("unknown widget in order", {
            let mut l = layout("ready");
            l["order"][0] = json!("patients_phi");
            l
        }),
        ("missing widget", {
            let mut l = layout("ready");
            l["order"].as_array_mut().unwrap().pop();
            l
        }),
        ("duplicate widget", {
            let mut l = layout("ready");
            l["order"][1] = json!("ready");
            l
        }),
        ("unknown hidden", {
            let mut l = layout("ready");
            l["hidden"] = json!(["notes"]);
            l
        }),
        ("unknown size", {
            let mut l = layout("ready");
            l["sizes"] = json!({ "results": "giant" });
            l
        }),
        ("unknown density", {
            let mut l = layout("ready");
            l["density"] = json!("dense");
            l
        }),
        ("free text field", {
            let mut l = layout("ready");
            l["note"] = json!("patient Alba SYN-0001");
            l
        }),
    ];
    for (name, bad) in cases.drain(..) {
        let (st, v) = call(
            &state,
            "PUT",
            "dev-lab.chen",
            Some(json!({ "layout": bad, "version": 0 })),
        )
        .await;
        assert_eq!(st, StatusCode::BAD_REQUEST, "{name}: {v}");
        assert_eq!(v["error"]["code"], "invalid_layout", "{name}: {v}");
    }
    let (_, v) = call(&state, "GET", "dev-lab.chen", None).await;
    assert!(
        v["layout"].is_null(),
        "nothing may be stored after rejections: {v}"
    );
}

#[tokio::test]
async fn preferences_follow_cockpit_access() {
    let state = test_state().await;
    reset(&state, &["reg.rivera"]).await;
    // Registration staff have the access widgets (visit.read) but no worklist.
    let (st, v) = call(
        &state,
        "PUT",
        "dev-reg.rivera",
        Some(json!({ "layout": layout("access"), "version": 0 })),
    )
    .await;
    assert_eq!(st, StatusCode::OK, "{v}");
    // Patients and representatives have no cockpit at all.
    for token in ["dev-rep.alba", "dev-nobody"] {
        let (st, v) = call(&state, "GET", token, None).await;
        assert!(
            st == StatusCode::FORBIDDEN || st == StatusCode::UNAUTHORIZED,
            "{token}: {st} {v}"
        );
    }
}

#[tokio::test]
async fn concurrent_first_saves_keep_exactly_one_layout() {
    let state = test_state().await;
    reset(&state, &["dr.lopez"]).await;
    // Two tabs save for the first time with version 0 at once: there is no
    // row to lock yet, so the insert itself must settle the race — exactly
    // one save wins, the other gets layout_conflict and the stored version
    // is 1, never 2.
    let (a, b) = tokio::join!(
        call(
            &state,
            "PUT",
            "dev-dr.lopez",
            Some(json!({ "layout": layout("results"), "version": 0 })),
        ),
        call(
            &state,
            "PUT",
            "dev-dr.lopez",
            Some(json!({ "layout": layout("tasks"), "version": 0 })),
        ),
    );
    let mut statuses = [a.0, b.0];
    statuses.sort();
    assert_eq!(
        statuses,
        [StatusCode::OK, StatusCode::CONFLICT],
        "{} / {}",
        a.1,
        b.1
    );
    let winner = if a.0 == StatusCode::OK { &a.1 } else { &b.1 };
    assert_eq!(winner["version"], 1);
    let (st, stored) = call(&state, "GET", "dev-dr.lopez", None).await;
    assert_eq!(st, StatusCode::OK, "{stored}");
    assert_eq!(stored["version"], 1, "{stored}");
    assert_eq!(stored["layout"]["order"][0], winner["layout"]["order"][0]);
}
