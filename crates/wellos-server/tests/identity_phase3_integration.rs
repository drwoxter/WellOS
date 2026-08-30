//! Identity Phase 3A integration tests: browser OIDC Authorization Code +
//! PKCE (mocked provider), central facility-level authorization, and the
//! shared PostgreSQL-backed rate limiter. All identities, keys, and clinical
//! data are synthetic.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::{get, post};
use axum::Router;
use base64::Engine;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use ed25519_dalek::SigningKey;
use http_body_util::BodyExt;
use jsonwebtoken::{encode, EncodingKey, Header};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tower::ServiceExt;
use wellos_server::oidc::{JwksKeys, RemoteJwks};
use wellos_server::state::{AppState, AuthConfig, OidcConfig, OidcLoginConfig};

const AUDIENCE: &str = "wellos-api";

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wellos:wellos_dev@localhost:5432/wellos".to_string())
}

async fn pool_state(auth: AuthConfig) -> AppState {
    let pool = wellos_server::connect_pool(&database_url()).await.unwrap();
    wellos_server::run_migrations(&pool).await.unwrap();
    let seeded: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(&pool)
        .await
        .unwrap();
    if seeded.0 == 0 {
        wellos_server::seeddata::seed(&pool).await.unwrap();
    }
    let gateway = Arc::new(dmind_gateway::fake::FakeProvider::new());
    AppState::with_auth(pool, gateway, auth)
}

async fn dev_state() -> AppState {
    pool_state(AuthConfig::development()).await
}

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
    extra_headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let (status, _headers, value) =
        call_full(state, method, path, Some(token), body, extra_headers).await;
    (status, value)
}

/// Like `call` but optionally anonymous and returning response headers.
async fn call_full(
    state: &AppState,
    method: &str,
    path: &str,
    token: Option<&str>,
    body: Option<Value>,
    extra_headers: &[(&str, &str)],
) -> (StatusCode, axum::http::HeaderMap, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("Content-Type", "application/json");
    if let Some(token) = token {
        builder = builder.header("Authorization", format!("Bearer {token}"));
    }
    for (k, v) in extra_headers {
        builder = builder.header(*k, *v);
    }
    let req = builder
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
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, headers, value)
}

fn uniq(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::now_v7().simple())
}

/// Seeded fixtures used by the facility tests, resolved from the database.
struct Facilities {
    facility_a: uuid::Uuid,
    facility_a2: uuid::Uuid,
    patient_a: uuid::Uuid,
    patient_a2: uuid::Uuid,
}

async fn facilities(state: &AppState) -> Facilities {
    let (facility_a,): (uuid::Uuid,) =
        sqlx::query_as("SELECT id FROM facilities WHERE name = 'Main Campus'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let (facility_a2,): (uuid::Uuid,) =
        sqlx::query_as("SELECT id FROM facilities WHERE name = 'North Annex'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let (patient_a,): (uuid::Uuid,) =
        sqlx::query_as("SELECT id FROM patients WHERE identifier = 'SYN-0001'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    let (patient_a2,): (uuid::Uuid,) =
        sqlx::query_as("SELECT id FROM patients WHERE identifier = 'SYN-0002'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    Facilities {
        facility_a,
        facility_a2,
        patient_a,
        patient_a2,
    }
}

// ---------------------------------------------------------------------------
// Facility-level authorization
// ---------------------------------------------------------------------------

#[tokio::test]
async fn facility_scoped_clinician_cannot_read_other_facility_patient() {
    let state = dev_state().await;
    let f = facilities(&state).await;

    // dr.annex is assigned only to North Annex: Main Campus patients are
    // invisible, with the same non-enumerating 404 as cross-tenant probes.
    let (status, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{}", f.patient_a),
        "dev-dr.annex",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Their own facility's patient is readable once a care relationship
    // (encounter) exists.
    let (status, enc) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.annex",
        Some(json!({ "patient_id": f.patient_a2 })),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{enc}");
    let (status, body) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{}", f.patient_a2),
        "dev-dr.annex",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn multi_facility_clinician_reads_both_facilities() {
    let state = dev_state().await;
    let f = facilities(&state).await;
    for patient in [f.patient_a, f.patient_a2] {
        // Establish a care relationship in each facility, then read.
        let (status, enc) = call(
            &state,
            "POST",
            "/api/v1/encounters",
            "dev-dr.garcia",
            Some(json!({ "patient_id": patient })),
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{enc}");
        let (status, body) = call(
            &state,
            "GET",
            &format!("/api/v1/patients/{patient}"),
            "dev-dr.garcia",
            None,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
}

#[tokio::test]
async fn tenant_wide_administrator_reads_all_facilities() {
    let state = dev_state().await;
    let f = facilities(&state).await;
    // admin.silva holds an explicit tenant-wide (NULL facility) assignment.
    for patient in [f.patient_a, f.patient_a2] {
        let (status, body) = call(
            &state,
            "GET",
            &format!("/api/v1/patients/{patient}"),
            "dev-admin.silva",
            None,
            &[],
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{body}");
    }
}

#[tokio::test]
async fn patient_search_is_limited_to_accessible_facilities() {
    let state = dev_state().await;

    // dr.annex sees only the North Annex patient.
    let (status, body) = call(
        &state,
        "GET",
        "/api/v1/patients?query=Demopatient",
        "dev-dr.annex",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let identifiers: Vec<&str> = body["patients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["identifier"].as_str().unwrap())
        .collect();
    assert!(identifiers.contains(&"SYN-0002"), "{identifiers:?}");
    assert!(!identifiers.contains(&"SYN-0001"), "{identifiers:?}");

    // dr.garcia (both facilities) sees both.
    let (status, body) = call(
        &state,
        "GET",
        "/api/v1/patients?query=Demopatient",
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let identifiers: Vec<String> = body["patients"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["identifier"].as_str().unwrap().to_string())
        .collect();
    assert!(
        identifiers.contains(&"SYN-0001".to_string()),
        "{identifiers:?}"
    );
    assert!(
        identifiers.contains(&"SYN-0002".to_string()),
        "{identifiers:?}"
    );
}

#[tokio::test]
async fn registration_requires_facility_assignment() {
    let state = dev_state().await;
    let f = facilities(&state).await;

    // reg.rivera is assigned to Main Campus only: registering into the
    // North Annex is denied without enumerating the facility.
    let (status, _) = call(
        &state,
        "POST",
        "/api/v1/patients",
        "dev-reg.rivera",
        Some(json!({
            "facility_id": f.facility_a2,
            "family_name": "Denied",
            "given_name": "Registration",
            "birth_date": "1980-01-01",
            "sex": "female",
            "identifier": uniq("MRN-P3"),
        })),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Their own facility works.
    let (status, body) = call(
        &state,
        "POST",
        "/api/v1/patients",
        "dev-reg.rivera",
        Some(json!({
            "facility_id": f.facility_a,
            "family_name": "Allowed",
            "given_name": "Registration",
            "birth_date": "1980-01-01",
            "sex": "female",
            "identifier": uniq("MRN-P3"),
        })),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn encounter_creation_rejects_unauthorized_facility() {
    let state = dev_state().await;
    let f = facilities(&state).await;
    // dr.annex cannot open an encounter on a Main Campus patient.
    let (status, _) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.annex",
        Some(json!({ "patient_id": f.patient_a })),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn service_request_detail_is_facility_scoped() {
    let state = dev_state().await;
    let f = facilities(&state).await;

    // Build a Main Campus order via dr.garcia.
    let (status, enc) = call(
        &state,
        "POST",
        "/api/v1/encounters",
        "dev-dr.garcia",
        Some(json!({ "patient_id": f.patient_a })),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{enc}");
    let (status, sr) = call(
        &state,
        "POST",
        "/api/v1/service-requests",
        "dev-dr.garcia",
        Some(json!({
            "encounter_id": enc["id"].as_str().unwrap(),
            "code_loinc": "2823-3",
            "display": "Potassium",
        })),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{sr}");
    let sr_id = sr["id"].as_str().unwrap();

    // The annex-only physician cannot see it.
    let (status, _) = call(
        &state,
        "GET",
        &format!("/api/v1/service-requests/{sr_id}"),
        "dev-dr.annex",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn worklist_is_facility_scoped() {
    let state = dev_state().await;
    // dr.annex sees a worklist (possibly empty) but never Main Campus rows.
    let (status, body) = call(&state, "GET", "/api/v1/worklist", "dev-dr.annex", None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let f = facilities(&state).await;
    for item in body["items"].as_array().unwrap_or(&Vec::new()) {
        assert_ne!(
            item["patient_id"].as_str().unwrap_or_default(),
            f.patient_a.to_string(),
            "worklist leaked a Main Campus patient"
        );
    }
}

#[tokio::test]
async fn break_glass_does_not_bypass_facility_scope() {
    let state = dev_state().await;
    let f = facilities(&state).await;

    // dr.emergency's break-glass assignment covers Main Campus only: an
    // emergency read of the North Annex patient stays denied.
    let (status, _) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{}", f.patient_a2),
        "dev-dr.emergency",
        None,
        &[
            ("x-purpose-of-use", "emergency"),
            ("x-break-glass-reason", "synthetic emergency drill"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND);

    // Within the covered facility, break-glass still works.
    let (status, body) = call(
        &state,
        "GET",
        &format!("/api/v1/patients/{}", f.patient_a),
        "dev-dr.emergency",
        None,
        &[
            ("x-purpose-of-use", "emergency"),
            ("x-break-glass-reason", "synthetic emergency drill"),
        ],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

// ---------------------------------------------------------------------------
// Shared PostgreSQL rate limiting
// ---------------------------------------------------------------------------

fn low_limit_config(search_per_min: i64, login_per_min: i64) -> AuthConfig {
    let mut cfg = AuthConfig::development();
    cfg.rate.search_per_min = search_per_min;
    cfg.rate.login_per_min = login_per_min;
    cfg
}

#[tokio::test]
async fn patient_search_rate_limit_returns_429_with_retry_after() {
    let state = pool_state(low_limit_config(3, 1_000)).await;
    let mut last = (StatusCode::OK, axum::http::HeaderMap::new(), Value::Null);
    for _ in 0..10 {
        last = call_full(
            &state,
            "GET",
            "/api/v1/patients?query=Demopatient",
            Some("dev-dr.garcia"),
            None,
            &[],
        )
        .await;
        if last.0 == StatusCode::TOO_MANY_REQUESTS {
            break;
        }
    }
    assert_eq!(last.0, StatusCode::TOO_MANY_REQUESTS);
    let retry_after: u64 = last
        .1
        .get("retry-after")
        .expect("Retry-After header present")
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&retry_after), "{retry_after}");
    assert_eq!(last.2["error"]["code"], "rate_limited");

    // Tenant separation: a tenant-B principal is not affected by tenant A's
    // exhausted window.
    let (status, body) = call(
        &state,
        "GET",
        "/api/v1/patients?query=Demopaciente",
        "dev-dr.sur",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");
}

#[tokio::test]
async fn concurrent_requests_cannot_bypass_the_limit() {
    let state = pool_state(low_limit_config(5, 1_000)).await;
    let mut handles = Vec::new();
    for _ in 0..20 {
        let state = state.clone();
        handles.push(tokio::spawn(async move {
            let (status, _) = call(
                &state,
                "GET",
                "/api/v1/patients?query=Demopatient",
                "dev-nurse.kim",
                None,
                &[],
            )
            .await;
            status
        }));
    }
    let mut ok = 0;
    let mut limited = 0;
    for h in handles {
        match h.await.unwrap() {
            StatusCode::OK => ok += 1,
            StatusCode::TOO_MANY_REQUESTS => limited += 1,
            other => panic!("unexpected status {other}"),
        }
    }
    assert!(ok <= 5, "at most 5 requests may pass, got {ok}");
    assert_eq!(ok + limited, 20);
}

#[tokio::test]
async fn anonymous_login_endpoints_are_rate_limited() {
    // Browser login is unconfigured here: the limiter still runs first, so
    // exhausting the per-address window flips 503 (not configured) to 429.
    let state = pool_state(low_limit_config(1_000, 2)).await;
    let mut saw_429 = false;
    for _ in 0..5 {
        let (status, _, _) =
            call_full(&state, "POST", "/api/v1/auth/oidc/login", None, None, &[]).await;
        if status == StatusCode::TOO_MANY_REQUESTS {
            saw_429 = true;
            break;
        }
        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    }
    assert!(saw_429, "anonymous login initiation must be rate-limited");
}

#[tokio::test]
async fn rate_limit_store_outage_fails_closed() {
    // A pool pointing at a closed port: every limiter check errors, and the
    // request is denied rather than served without a successful check.
    let pool = sqlx::postgres::PgPoolOptions::new()
        .acquire_timeout(std::time::Duration::from_secs(1))
        .connect_lazy("postgres://wellos:wellos_dev@127.0.0.1:1/wellos")
        .unwrap();
    let gateway = Arc::new(dmind_gateway::fake::FakeProvider::new());
    let state = AppState::with_auth(pool, gateway, AuthConfig::development());
    let (status, _, _) = call_full(
        &state,
        "GET",
        "/api/v1/patients?query=Demopatient",
        Some("dev-dr.garcia"),
        None,
        &[],
    )
    .await;
    assert!(
        !status.is_success(),
        "an unavailable store must fail closed, got {status}"
    );
}

// ---------------------------------------------------------------------------
// Browser OIDC Authorization Code + PKCE (mocked provider)
// ---------------------------------------------------------------------------

struct TestIdp {
    encoding_key: EncodingKey,
    header: Header,
    jwks_json: String,
}

fn test_idp(seed: u8, kid: &str) -> TestIdp {
    let signing = SigningKey::from_bytes(&[seed; 32]);
    let der = signing.to_pkcs8_der().unwrap();
    let encoding_key = EncodingKey::from_ed_der(der.as_bytes());
    let x =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(signing.verifying_key().to_bytes());
    let jwks_json = json!({
        "keys": [{ "kty": "OKP", "crv": "Ed25519", "x": x, "kid": kid, "alg": "EdDSA", "use": "sig" }]
    })
    .to_string();
    let mut header = Header::new(jsonwebtoken::Algorithm::EdDSA);
    header.kid = Some(kid.to_string());
    TestIdp {
        encoding_key,
        header,
        jwks_json,
    }
}

/// A synthetic OIDC provider with discovery, JWKS, and a token endpoint.
/// The token endpoint returns the preloaded response and records the
/// submitted form parameters for PKCE assertions.
struct FakeProvider {
    issuer: String,
    token_response: Arc<std::sync::Mutex<(StatusCode, String)>>,
    token_request_form: Arc<std::sync::Mutex<Option<String>>>,
}

async fn start_fake_provider(jwks_json: &str) -> FakeProvider {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let issuer = format!("http://{addr}");

    let token_response = Arc::new(std::sync::Mutex::new((StatusCode::OK, "{}".to_string())));
    let token_request_form: Arc<std::sync::Mutex<Option<String>>> =
        Arc::new(std::sync::Mutex::new(None));

    let jwks = jwks_json.to_string();
    let issuer_c = issuer.clone();
    let response_c = token_response.clone();
    let form_c = token_request_form.clone();
    let app = Router::new()
        .route(
            "/.well-known/openid-configuration",
            get(move || {
                let issuer = issuer_c.clone();
                async move {
                    axum::Json(json!({
                        "issuer": issuer,
                        "jwks_uri": format!("{issuer}/jwks"),
                        "authorization_endpoint": format!("{issuer}/authorize"),
                        "token_endpoint": format!("{issuer}/token"),
                        "response_types_supported": ["code"],
                        "code_challenge_methods_supported": ["S256"],
                    }))
                }
            }),
        )
        .route(
            "/jwks",
            get(move || {
                let jwks = jwks.clone();
                async move { ([("content-type", "application/json")], jwks) }
            }),
        )
        .route(
            "/token",
            post(move |body: String| {
                let response = response_c.clone();
                let form = form_c.clone();
                async move {
                    *form.lock().unwrap() = Some(body);
                    let (status, body) = response.lock().unwrap().clone();
                    (status, [("content-type", "application/json")], body)
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    FakeProvider {
        issuer,
        token_response,
        token_request_form,
    }
}

async fn login_state(provider: &FakeProvider) -> AppState {
    let remote = RemoteJwks::new(provider.issuer.clone(), true, 3600, 0).unwrap();
    remote.initialize().await.unwrap();
    pool_state(AuthConfig {
        dev_auth_enabled: false,
        oidc: Some(OidcConfig {
            issuer: provider.issuer.clone(),
            audience: AUDIENCE.to_string(),
            keys: JwksKeys::Remote(Arc::new(remote)),
            leeway_secs: 60,
            require_mfa: false,
            accepted_amr: vec!["mfa".into()],
            accepted_acr: vec![],
            login: Some(OidcLoginConfig {
                client_id: "wellos-web".to_string(),
                client_secret: None,
                redirect_uri: "http://localhost:3000/api/auth/oidc/callback".to_string(),
                login_txn_secs: 300,
            }),
        }),
        ..AuthConfig::development()
    })
    .await
}

/// Begin a login and return the state/nonce/code_challenge the provider
/// would see (parsed from the authorization URL, as the browser round-trip
/// does).
async fn begin_login(state: &AppState) -> (String, String, String) {
    let (status, _, body) =
        call_full(state, "POST", "/api/v1/auth/oidc/login", None, None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let url = url::Url::parse(body["authorize_url"].as_str().unwrap()).unwrap();
    let get = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.to_string())
            .unwrap()
    };
    assert_eq!(get("response_type"), "code");
    assert_eq!(get("code_challenge_method"), "S256");
    (get("state"), get("nonce"), get("code_challenge"))
}

fn id_token(idp: &TestIdp, issuer: &str, aud: &str, nonce: &str, exp_offset: i64) -> String {
    let now = chrono::Utc::now().timestamp();
    encode(
        &idp.header,
        &json!({
            "sub": "synthetic|dr.garcia",
            "iss": issuer,
            "aud": aud,
            "exp": now + exp_offset,
            "iat": now,
            "nbf": now - 10,
            "nonce": nonce,
        }),
        &idp.encoding_key,
    )
    .unwrap()
}

async fn callback(state: &AppState, body: Value) -> (StatusCode, Value) {
    let (status, _, value) = call_full(
        state,
        "POST",
        "/api/v1/auth/oidc/callback",
        None,
        Some(body),
        &[],
    )
    .await;
    (status, value)
}

#[tokio::test]
async fn oidc_login_success_issues_opaque_session_only() {
    let idp = test_idp(31, "p3-kid");
    let provider = start_fake_provider(&idp.jwks_json).await;
    let state = login_state(&provider).await;

    let (oauth_state, nonce, challenge) = begin_login(&state).await;
    let token = id_token(&idp, &provider.issuer, AUDIENCE, &nonce, 600);
    *provider.token_response.lock().unwrap() = (
        StatusCode::OK,
        json!({ "id_token": token, "access_token": "synthetic-access", "token_type": "Bearer" })
            .to_string(),
    );

    let (status, body) = callback(
        &state,
        json!({ "code": "synthetic-code", "state": oauth_state }),
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{body}");

    // Only opaque WellOS values come back; provider tokens never appear.
    let session = body["session_token"].as_str().unwrap();
    let csrf = body["csrf_token"].as_str().unwrap();
    assert!(session.starts_with("wss_"), "opaque session identifier");
    assert!(csrf.starts_with("wsc_"), "opaque CSRF secret");
    let raw = body.to_string();
    assert!(!raw.contains(&token), "id_token must not leak");
    assert!(
        !raw.contains("synthetic-access"),
        "access token must not leak"
    );

    // The server-side exchange sent PKCE: the verifier matches the challenge.
    let form = provider.token_request_form.lock().unwrap().clone().unwrap();
    let verifier = form
        .split('&')
        .find_map(|kv| kv.strip_prefix("code_verifier="))
        .expect("code_verifier submitted")
        .to_string();
    let computed = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(Sha256::digest(verifier.as_bytes()));
    assert_eq!(computed, challenge, "S256(verifier) must equal challenge");
    assert!(form.contains("grant_type=authorization_code"));

    // The opaque session works against the session endpoint.
    let (status, _, _) = call_full(
        &state,
        "GET",
        "/api/v1/auth/session",
        Some(session),
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn oidc_callback_rejects_state_mismatch_and_replay() {
    let idp = test_idp(32, "p3-kid");
    let provider = start_fake_provider(&idp.jwks_json).await;
    let state = login_state(&provider).await;

    // Unknown state.
    let (status, _) = callback(
        &state,
        json!({ "code": "synthetic-code", "state": "wst_forged" }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Successful login, then replay of the same state.
    let (oauth_state, nonce, _) = begin_login(&state).await;
    let token = id_token(&idp, &provider.issuer, AUDIENCE, &nonce, 600);
    *provider.token_response.lock().unwrap() =
        (StatusCode::OK, json!({ "id_token": token }).to_string());
    let (status, _) = callback(
        &state,
        json!({ "code": "synthetic-code", "state": oauth_state }),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = callback(
        &state,
        json!({ "code": "synthetic-code", "state": oauth_state }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "replay must fail");
}

#[tokio::test]
async fn oidc_callback_rejects_nonce_mismatch() {
    let idp = test_idp(33, "p3-kid");
    let provider = start_fake_provider(&idp.jwks_json).await;
    let state = login_state(&provider).await;

    let (oauth_state, _nonce, _) = begin_login(&state).await;
    // The provider returns a token bound to a different nonce.
    let token = id_token(&idp, &provider.issuer, AUDIENCE, "wsn_other", 600);
    *provider.token_response.lock().unwrap() =
        (StatusCode::OK, json!({ "id_token": token }).to_string());
    let (status, _) = callback(
        &state,
        json!({ "code": "synthetic-code", "state": oauth_state }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn oidc_callback_rejects_expired_transaction() {
    let idp = test_idp(34, "p3-kid");
    let provider = start_fake_provider(&idp.jwks_json).await;
    let state = login_state(&provider).await;

    let (oauth_state, nonce, _) = begin_login(&state).await;
    sqlx::query("UPDATE login_transactions SET expires_at = now() - interval '1 second' WHERE state_hash = $1")
        .bind(wellos_server::auth::hash_service_secret(&oauth_state))
        .execute(&state.pool)
        .await
        .unwrap();
    let token = id_token(&idp, &provider.issuer, AUDIENCE, &nonce, 600);
    *provider.token_response.lock().unwrap() =
        (StatusCode::OK, json!({ "id_token": token }).to_string());
    let (status, _) = callback(
        &state,
        json!({ "code": "synthetic-code", "state": oauth_state }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn oidc_callback_rejects_provider_error_and_exchange_failure() {
    let idp = test_idp(35, "p3-kid");
    let provider = start_fake_provider(&idp.jwks_json).await;
    let state = login_state(&provider).await;

    // Provider error response (no code).
    let (oauth_state, _, _) = begin_login(&state).await;
    let (status, body) = callback(
        &state,
        json!({ "error": "access_denied", "state": oauth_state }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(
        !body.to_string().contains("access_denied"),
        "provider error codes are not echoed"
    );

    // Token exchange failure.
    let (oauth_state, _, _) = begin_login(&state).await;
    *provider.token_response.lock().unwrap() = (
        StatusCode::BAD_REQUEST,
        json!({ "error": "invalid_grant" }).to_string(),
    );
    let (status, _) = callback(
        &state,
        json!({ "code": "synthetic-code", "state": oauth_state }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn oidc_callback_rejects_invalid_issuer_audience_and_signature() {
    let idp = test_idp(36, "p3-kid");
    let provider = start_fake_provider(&idp.jwks_json).await;
    let state = login_state(&provider).await;

    // Wrong issuer.
    let (oauth_state, nonce, _) = begin_login(&state).await;
    let token = id_token(&idp, "https://evil.example.test/", AUDIENCE, &nonce, 600);
    *provider.token_response.lock().unwrap() =
        (StatusCode::OK, json!({ "id_token": token }).to_string());
    let (status, _) = callback(
        &state,
        json!({ "code": "synthetic-code", "state": oauth_state }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Wrong audience.
    let (oauth_state, nonce, _) = begin_login(&state).await;
    let token = id_token(&idp, &provider.issuer, "other-api", &nonce, 600);
    *provider.token_response.lock().unwrap() =
        (StatusCode::OK, json!({ "id_token": token }).to_string());
    let (status, _) = callback(
        &state,
        json!({ "code": "synthetic-code", "state": oauth_state }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Signed by a rogue key never published in the JWKS.
    let rogue = test_idp(37, "p3-kid");
    let (oauth_state, nonce, _) = begin_login(&state).await;
    let token = id_token(&rogue, &provider.issuer, AUDIENCE, &nonce, 600);
    *provider.token_response.lock().unwrap() =
        (StatusCode::OK, json!({ "id_token": token }).to_string());
    let (status, _) = callback(
        &state,
        json!({ "code": "synthetic-code", "state": oauth_state }),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn oidc_login_start_never_returns_raw_verifier_or_nonce_hash() {
    let idp = test_idp(38, "p3-kid");
    let provider = start_fake_provider(&idp.jwks_json).await;
    let state = login_state(&provider).await;

    let (status, _, body) =
        call_full(&state, "POST", "/api/v1/auth/oidc/login", None, None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    // The JSON response carries only the provider authorization URL.
    let obj = body.as_object().unwrap();
    assert_eq!(obj.len(), 1, "{body}");
    assert!(obj.contains_key("authorize_url"));

    // The stored verifier stays server-side and does not round-trip in the
    // URL (only its S256 challenge does).
    let url = body["authorize_url"].as_str().unwrap();
    let (verifier,): (String,) = sqlx::query_as(
        "SELECT code_verifier FROM login_transactions ORDER BY created_at DESC LIMIT 1",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert!(!url.contains(&verifier), "raw verifier must not leak");
}
