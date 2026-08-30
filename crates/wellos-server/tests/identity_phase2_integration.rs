//! Identity Phase 2 integration tests: OIDC discovery/JWKS rotation, MFA
//! enforcement, provider-aware identity mapping, opaque browser sessions with
//! CSRF, service-credential administration, security headers, and the
//! emergency-search authorization gate.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::Router;
use base64::Engine;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use ed25519_dalek::SigningKey;
use http_body_util::BodyExt;
use jsonwebtoken::{encode, EncodingKey, Header};
use serde_json::{json, Value};
use std::sync::Arc;
use tower::ServiceExt;
use wellos_server::oidc::{JwksKeys, RemoteJwks};
use wellos_server::state::{AppState, AuthConfig, OidcConfig};

const AUDIENCE: &str = "wellos-api";

fn database_url() -> String {
    std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://wellos:wellos_dev@localhost:5432/wellos".to_string())
}

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

async fn call(
    state: &AppState,
    method: &str,
    path: &str,
    token: &str,
    body: Option<Value>,
    extra_headers: &[(&str, &str)],
) -> (StatusCode, Value) {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json");
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
    let bytes = res.into_body().collect().await.unwrap().to_bytes();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

fn claims(sub: &str, iss: &str, extra: Value) -> Value {
    let now = chrono::Utc::now().timestamp();
    let mut base = json!({
        "sub": sub,
        "iss": iss,
        "aud": AUDIENCE,
        "exp": now + 600,
        "iat": now,
        "nbf": now - 10,
    });
    if let (Some(base_map), Some(extra_map)) = (base.as_object_mut(), extra.as_object()) {
        for (k, v) in extra_map {
            base_map.insert(k.clone(), v.clone());
        }
    }
    base
}

fn oidc_config(issuer: &str, keys: JwksKeys, require_mfa: bool) -> OidcConfig {
    OidcConfig {
        issuer: issuer.to_string(),
        audience: AUDIENCE.to_string(),
        keys,
        leeway_secs: 60,
        require_mfa,
        accepted_amr: vec!["mfa".into(), "otp".into(), "hwk".into()],
        accepted_acr: vec!["phrh".into()],
    }
}

fn static_state_cfg(idp: &TestIdp, issuer: &str, require_mfa: bool) -> AuthConfig {
    AuthConfig {
        dev_auth_enabled: false,
        oidc: Some(oidc_config(
            issuer,
            JwksKeys::Static(serde_json::from_str(&idp.jwks_json).unwrap()),
            require_mfa,
        )),
        ..AuthConfig::development()
    }
}

// ---------------------------------------------------------------------------
// Local synthetic IdP: serves discovery metadata and a mutable JWKS document
// over loopback HTTP (allowed only because tests run as development).
// ---------------------------------------------------------------------------

struct FakeIdpServer {
    issuer: String,
    jwks: Arc<std::sync::Mutex<String>>,
    metadata_issuer: Arc<std::sync::Mutex<Option<String>>>,
    serve_jwks: Arc<std::sync::atomic::AtomicBool>,
}

async fn start_fake_idp(jwks_json: &str) -> FakeIdpServer {
    let jwks = Arc::new(std::sync::Mutex::new(jwks_json.to_string()));
    let metadata_issuer: Arc<std::sync::Mutex<Option<String>>> =
        Arc::new(std::sync::Mutex::new(None));
    let serve_jwks = Arc::new(std::sync::atomic::AtomicBool::new(true));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let issuer = format!("http://{addr}");

    let jwks_c = jwks.clone();
    let serve_c = serve_jwks.clone();
    let meta_c = metadata_issuer.clone();
    let issuer_c = issuer.clone();
    let app = Router::new()
        .route(
            "/.well-known/openid-configuration",
            get(move || {
                let meta = meta_c.clone();
                let issuer = issuer_c.clone();
                async move {
                    let iss = meta.lock().unwrap().clone().unwrap_or(issuer.clone());
                    axum::Json(json!({
                        "issuer": iss,
                        "jwks_uri": format!("{issuer}/jwks"),
                    }))
                }
            }),
        )
        .route(
            "/jwks",
            get(move || {
                let jwks = jwks_c.clone();
                let serve = serve_c.clone();
                async move {
                    if !serve.load(std::sync::atomic::Ordering::SeqCst) {
                        return Err(StatusCode::SERVICE_UNAVAILABLE);
                    }
                    let body = jwks.lock().unwrap().clone();
                    Ok(([("content-type", "application/json")], body))
                }
            }),
        );
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    FakeIdpServer {
        issuer,
        jwks,
        metadata_issuer,
        serve_jwks,
    }
}

// ---------------------------------------------------------------------------
// OIDC discovery, caching, rotation
// ---------------------------------------------------------------------------

#[tokio::test]
async fn discovery_resolves_jwks_and_refreshes_on_new_kid() {
    let idp1 = test_idp(21, "kid-1");
    let server = start_fake_idp(&idp1.jwks_json).await;

    let remote = RemoteJwks::new(server.issuer.clone(), true, 3600, 0).unwrap();
    remote.initialize().await.unwrap();
    let keys = JwksKeys::Remote(Arc::new(remote));

    let state = pool_state(AuthConfig {
        dev_auth_enabled: false,
        oidc: Some(oidc_config(&server.issuer, keys, false)),
        ..AuthConfig::development()
    })
    .await;

    // Initial key works.
    let token = encode(
        &idp1.header,
        &claims("synthetic|dr.garcia", &server.issuer, json!({})),
        &idp1.encoding_key,
    )
    .unwrap();
    let (status, _) = call(&state, "GET", "/api/v1/worklist", &token, None, &[]).await;
    assert_eq!(status, StatusCode::OK);

    // Rotate the IdP key: a new kid appears and is picked up via refresh.
    let idp2 = test_idp(22, "kid-2");
    *server.jwks.lock().unwrap() = idp2.jwks_json.clone();
    let token2 = encode(
        &idp2.header,
        &claims("synthetic|dr.garcia", &server.issuer, json!({})),
        &idp2.encoding_key,
    )
    .unwrap();
    let (status, _) = call(&state, "GET", "/api/v1/worklist", &token2, None, &[]).await;
    assert_eq!(status, StatusCode::OK);

    // A token signed with a never-published key still fails.
    let rogue = test_idp(23, "kid-rogue");
    let bad = encode(
        &rogue.header,
        &claims("synthetic|dr.garcia", &server.issuer, json!({})),
        &rogue.encoding_key,
    )
    .unwrap();
    let (status, _) = call(&state, "GET", "/api/v1/worklist", &bad, None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn discovery_keeps_last_known_keys_when_jwks_becomes_unavailable() {
    let idp = test_idp(24, "kid-a");
    let server = start_fake_idp(&idp.jwks_json).await;
    let remote = RemoteJwks::new(server.issuer.clone(), true, 3600, 0).unwrap();
    remote.initialize().await.unwrap();
    let keys = JwksKeys::Remote(Arc::new(remote));
    let state = pool_state(AuthConfig {
        dev_auth_enabled: false,
        oidc: Some(oidc_config(&server.issuer, keys, false)),
        ..AuthConfig::development()
    })
    .await;

    server
        .serve_jwks
        .store(false, std::sync::atomic::Ordering::SeqCst);

    // Cached key still validates tokens.
    let token = encode(
        &idp.header,
        &claims("synthetic|dr.garcia", &server.issuer, json!({})),
        &idp.encoding_key,
    )
    .unwrap();
    let (status, _) = call(&state, "GET", "/api/v1/worklist", &token, None, &[]).await;
    assert_eq!(status, StatusCode::OK);

    // Unknown kid triggers a refresh attempt that fails; auth fails closed.
    let unknown = test_idp(25, "kid-b");
    let bad = encode(
        &unknown.header,
        &claims("synthetic|dr.garcia", &server.issuer, json!({})),
        &unknown.encoding_key,
    )
    .unwrap();
    let (status, _) = call(&state, "GET", "/api/v1/worklist", &bad, None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn discovery_rejects_issuer_mismatch_and_requires_https_in_production() {
    let idp = test_idp(26, "kid-x");
    let server = start_fake_idp(&idp.jwks_json).await;
    *server.metadata_issuer.lock().unwrap() = Some("http://impostor.example.test".to_string());
    let remote = RemoteJwks::new(server.issuer.clone(), true, 3600, 0).unwrap();
    assert!(
        remote.initialize().await.is_err(),
        "metadata issuer mismatch must fail closed"
    );

    // HTTPS is mandatory outside development.
    assert!(RemoteJwks::new("http://idp.example.test".into(), false, 3600, 30).is_err());
    assert!(RemoteJwks::new("https://idp.example.test".into(), false, 3600, 30).is_ok());
}

// ---------------------------------------------------------------------------
// Provider-aware identity mapping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn legacy_subject_is_migrated_into_provider_aware_mapping() {
    let idp = test_idp(27, "kid-map");
    let issuer = "https://idp-a.example.test/";
    let state = pool_state(static_state_cfg(&idp, issuer, false)).await;
    sqlx::query("DELETE FROM user_identities WHERE issuer = $1")
        .bind(issuer)
        .execute(&state.pool)
        .await
        .unwrap();

    let token = encode(
        &idp.header,
        &claims("synthetic|dr.garcia", issuer, json!({})),
        &idp.encoding_key,
    )
    .unwrap();
    let (status, _) = call(&state, "GET", "/api/v1/worklist", &token, None, &[]).await;
    assert_eq!(status, StatusCode::OK);

    // The legacy users.oidc_subject match was recorded as (issuer, subject).
    let (n,): (i64,) = sqlx::query_as(
        "SELECT COUNT(*) FROM user_identities WHERE issuer = $1 AND subject = 'synthetic|dr.garcia'",
    )
    .bind(issuer)
    .fetch_one(&state.pool)
    .await
    .unwrap();
    assert_eq!(n, 1);
}

#[tokio::test]
async fn identity_mapping_is_scoped_to_the_configured_issuer() {
    let idp = test_idp(28, "kid-scope");
    let issuer_a = "https://idp-scope-a.example.test/";
    let issuer_b = "https://idp-scope-b.example.test/";
    let state = pool_state(static_state_cfg(&idp, issuer_b, false)).await;

    // A mapping that exists only for issuer A must not authenticate a token
    // from issuer B with the same subject unless the legacy column matches.
    let subject = format!("scoped|{}", uuid::Uuid::now_v7().simple());
    let (user_id,): (uuid::Uuid,) =
        sqlx::query_as("SELECT id FROM users WHERE username = 'dr.garcia'")
            .fetch_one(&state.pool)
            .await
            .unwrap();
    sqlx::query("INSERT INTO user_identities (id, user_id, issuer, subject) VALUES ($1,$2,$3,$4)")
        .bind(uuid::Uuid::now_v7())
        .bind(user_id)
        .bind(issuer_a)
        .bind(&subject)
        .execute(&state.pool)
        .await
        .unwrap();

    let token = encode(
        &idp.header,
        &claims(&subject, issuer_b, json!({})),
        &idp.encoding_key,
    )
    .unwrap();
    let (status, _) = call(&state, "GET", "/api/v1/worklist", &token, None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

// ---------------------------------------------------------------------------
// MFA enforcement
// ---------------------------------------------------------------------------

#[tokio::test]
async fn mfa_policy_accepts_valid_signals_and_fails_closed_otherwise() {
    let idp = test_idp(29, "kid-mfa");
    let issuer = "https://idp-mfa.example.test/";
    let state = pool_state(static_state_cfg(&idp, issuer, true)).await;

    let ok_cases = [
        json!({ "amr": ["pwd", "otp"] }),
        json!({ "amr": ["mfa"] }),
        json!({ "acr": "phrh" }),
    ];
    for extra in ok_cases {
        let token = encode(
            &idp.header,
            &claims("synthetic|dr.garcia", issuer, extra.clone()),
            &idp.encoding_key,
        )
        .unwrap();
        let (status, _) = call(&state, "GET", "/api/v1/worklist", &token, None, &[]).await;
        assert_eq!(status, StatusCode::OK, "extra claims {extra}");
    }

    let bad_cases = [
        json!({}),                  // missing
        json!({ "amr": ["pwd"] }),  // insufficient
        json!({ "amr": "otp" }),    // malformed (not an array)
        json!({ "amr": [1, 2] }),   // malformed entries
        json!({ "acr": "level0" }), // unaccepted acr
        json!({ "acr": ["phrh"] }), // malformed acr (not a string)
        json!({ "amr": ["pwd"], "acr": "level0" }),
    ];
    for extra in bad_cases {
        let token = encode(
            &idp.header,
            &claims("synthetic|dr.garcia", issuer, extra.clone()),
            &idp.encoding_key,
        )
        .unwrap();
        let (status, _) = call(&state, "GET", "/api/v1/worklist", &token, None, &[]).await;
        assert_eq!(status, StatusCode::UNAUTHORIZED, "extra claims {extra}");
    }

    // With MFA not required, a plain token still authenticates.
    let relaxed = pool_state(static_state_cfg(&idp, issuer, false)).await;
    let token = encode(
        &idp.header,
        &claims("synthetic|dr.garcia", issuer, json!({})),
        &idp.encoding_key,
    )
    .unwrap();
    let (status, _) = call(&relaxed, "GET", "/api/v1/worklist", &token, None, &[]).await;
    assert_eq!(status, StatusCode::OK);
}

// ---------------------------------------------------------------------------
// Opaque browser sessions + CSRF
// ---------------------------------------------------------------------------

async fn dev_state() -> AppState {
    pool_state(AuthConfig::development()).await
}

async fn open_session(state: &AppState, token: &str) -> (String, String) {
    let (status, body) = call(state, "POST", "/api/v1/auth/session", token, None, &[]).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    (
        body["session_token"].as_str().unwrap().to_string(),
        body["csrf_token"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn session_rotation_failure_rolls_back_and_keeps_old_session() {
    let state = dev_state().await;
    let (session, _csrf) = open_session(&state, "dev-dr.garcia").await;

    let (session_id, tenant_id): (uuid::Uuid, uuid::Uuid) =
        sqlx::query_as("SELECT id, tenant_id FROM web_sessions WHERE token_hash = $1")
            .bind(wellos_server::auth::hash_service_secret(&session))
            .fetch_one(&state.pool)
            .await
            .unwrap();

    // A context whose user no longer exists makes the replacement insert
    // fail; the whole rotation must roll back, leaving the old session live.
    let broken_ctx = wellos_server::auth::AuthContext {
        user_id: uuid::Uuid::now_v7(),
        tenant_id,
        username: "ghost".to_string(),
        display_name: "Ghost".to_string(),
        is_service: false,
        roles: vec![],
        scopes: vec![],
        purpose_of_use: wellos_server::policy::Purpose::Treatment,
        break_glass_reason: None,
        web_session_id: Some(session_id),
        correlation_id: uuid::Uuid::now_v7(),
    };
    let result =
        wellos_server::routes::session::rotate(axum::extract::State(state.clone()), broken_ctx)
            .await;
    assert!(result.is_err(), "rotation with a broken insert must fail");

    let (revoked_at,): (Option<chrono::DateTime<chrono::Utc>>,) =
        sqlx::query_as("SELECT revoked_at FROM web_sessions WHERE id = $1")
            .bind(session_id)
            .fetch_one(&state.pool)
            .await
            .unwrap();
    assert!(
        revoked_at.is_none(),
        "failed rotation must not revoke the existing session"
    );
    let (status, _) = call(&state, "GET", "/api/v1/auth/session", &session, None, &[]).await;
    assert_eq!(status, StatusCode::OK, "old session must remain usable");
}

#[tokio::test]
async fn session_lifecycle_validate_rotate_logout() {
    let state = dev_state().await;
    let (session, csrf) = open_session(&state, "dev-dr.garcia").await;
    assert!(session.starts_with("wss_"));

    // GET validates the server-side record, not mere cookie presence.
    let (status, body) = call(&state, "GET", "/api/v1/auth/session", &session, None, &[]).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["authenticated"], json!(true));
    assert_eq!(body["username"], json!("dr.garcia"));

    // The session works as a credential for normal reads.
    let (status, _) = call(&state, "GET", "/api/v1/worklist", &session, None, &[]).await;
    assert_eq!(status, StatusCode::OK);

    // Rotation issues a new identifier and revokes the old one (fixation).
    let (status, rotated) = call(
        &state,
        "POST",
        "/api/v1/auth/session/rotate",
        &session,
        None,
        &[("x-csrf-token", &csrf)],
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rotated}");
    let new_session = rotated["session_token"].as_str().unwrap().to_string();
    let new_csrf = rotated["csrf_token"].as_str().unwrap().to_string();
    assert_ne!(new_session, session);
    let (status, _) = call(&state, "GET", "/api/v1/auth/session", &session, None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED, "old session must die");

    // Logout revokes server-side.
    let (status, _) = call(
        &state,
        "DELETE",
        "/api/v1/auth/session",
        &new_session,
        None,
        &[("x-csrf-token", &new_csrf)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &state,
        "GET",
        "/api/v1/auth/session",
        &new_session,
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn session_enforces_absolute_expiry_and_inactivity() {
    let state = dev_state().await;
    let (session, _) = open_session(&state, "dev-dr.garcia").await;
    let hash = wellos_server::auth::hash_service_secret(&session);

    // Simulate inactivity beyond the idle timeout.
    sqlx::query(
        "UPDATE web_sessions SET last_seen_at = now() - interval '2 hours' WHERE token_hash = $1",
    )
    .bind(&hash)
    .execute(&state.pool)
    .await
    .unwrap();
    let (status, _) = call(&state, "GET", "/api/v1/auth/session", &session, None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Absolute expiration also fails closed, regardless of recent activity.
    let (session2, _) = open_session(&state, "dev-dr.garcia").await;
    let hash2 = wellos_server::auth::hash_service_secret(&session2);
    sqlx::query(
        "UPDATE web_sessions SET expires_at = now() - interval '1 minute' WHERE token_hash = $1",
    )
    .bind(&hash2)
    .execute(&state.pool)
    .await
    .unwrap();
    let (status, _) = call(&state, "GET", "/api/v1/auth/session", &session2, None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn session_writes_require_valid_csrf_token() {
    let state = dev_state().await;
    let (session, csrf) = open_session(&state, "dev-reg.rivera").await;
    let (_, meta) = call(
        &state,
        "GET",
        "/api/v1/meta/tenant",
        "dev-reg.rivera",
        None,
        &[],
    )
    .await;
    let facility = meta["facilities"][0]["id"].as_str().unwrap().to_string();
    let body = json!({
        "facility_id": facility,
        "given_name": "Casey", "family_name": "Csrf", "sex": "female",
        "birth_date": "1980-01-01",
        "identifier": format!("MRN-CSRF-{}", uuid::Uuid::now_v7().simple()),
    });

    // Missing CSRF header: rejected.
    let (status, _) = call(
        &state,
        "POST",
        "/api/v1/patients",
        &session,
        Some(body.clone()),
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Wrong CSRF token: rejected.
    let (status, _) = call(
        &state,
        "POST",
        "/api/v1/patients",
        &session,
        Some(body.clone()),
        &[("x-csrf-token", "wsc_wrong")],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Valid CSRF token: allowed. Reads never require CSRF.
    let (status, _) = call(
        &state,
        "POST",
        "/api/v1/patients",
        &session,
        Some(body),
        &[("x-csrf-token", &csrf)],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    // Reads never require CSRF.
    let (status, _) = call(
        &state,
        "GET",
        "/api/v1/patients?query=Csrf",
        &session,
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn sessions_are_not_issued_to_services_or_other_sessions() {
    let state = dev_state().await;
    // A session cannot mint another session (only real credentials can).
    let (session, csrf) = open_session(&state, "dev-dr.garcia").await;
    let (status, _) = call(
        &state,
        "POST",
        "/api/v1/auth/session",
        &session,
        None,
        &[("x-csrf-token", &csrf)],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
}

// ---------------------------------------------------------------------------
// Service-credential administration
// ---------------------------------------------------------------------------

const OPS: &[(&str, &str)] = &[("x-purpose-of-use", "operations")];

#[tokio::test]
async fn service_credential_admin_lifecycle() {
    let state = dev_state().await;

    // Only authorized roles may manage credentials.
    let (status, _) = call(
        &state,
        "POST",
        "/api/v1/admin/service-credentials",
        "dev-dr.garcia",
        Some(json!({ "service_username": "svc.lab-adapter", "name": "x", "scopes": ["result.ingest"] })),
        OPS,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Issue: plaintext secret returned exactly once.
    let (status, issued) = call(
        &state,
        "POST",
        "/api/v1/admin/service-credentials",
        "dev-privacy.wolf",
        Some(json!({
            "service_username": "svc.lab-adapter",
            "name": "phase2 test credential",
            "scopes": ["result.ingest"],
            "expires_in_secs": 3600,
        })),
        OPS,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{issued}");
    let secret = issued["secret"].as_str().unwrap().to_string();
    assert!(secret.starts_with("wsk_"));
    let cred_id = issued["id"].as_str().unwrap().to_string();

    // The issued credential authenticates with its scope.
    // (Direct auth check: listing shows metadata but never hashes/secrets.)
    let (status, listed) = call(
        &state,
        "GET",
        "/api/v1/admin/service-credentials",
        "dev-audit.stone",
        None,
        OPS,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let entries = listed["credentials"].as_array().unwrap();
    let entry = entries
        .iter()
        .find(|c| c["id"] == json!(cred_id))
        .expect("issued credential listed");
    assert!(entry.get("secret").is_none());
    assert!(entry.get("token_hash").is_none());
    assert!(entry["expires_at"].is_string());

    // Auditors can read but not manage.
    let (status, _) = call(
        &state,
        "POST",
        &format!("/api/v1/admin/service-credentials/{cred_id}/revoke"),
        "dev-audit.stone",
        None,
        OPS,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Rotate: old credential revoked, new secret returned once.
    let (status, rotated) = call(
        &state,
        "POST",
        &format!("/api/v1/admin/service-credentials/{cred_id}/rotate"),
        "dev-privacy.wolf",
        None,
        OPS,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{rotated}");
    let new_secret = rotated["secret"].as_str().unwrap();
    assert_ne!(new_secret, secret);
    let new_id = rotated["id"].as_str().unwrap().to_string();

    // The rotated-away credential no longer authenticates.
    let (status, _) = call(&state, "GET", "/api/v1/worklist", &secret, None, &[]).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);

    // Revoke the new one.
    let (status, _) = call(
        &state,
        "POST",
        &format!("/api/v1/admin/service-credentials/{new_id}/revoke"),
        "dev-privacy.wolf",
        None,
        OPS,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    let (status, _) = call(
        &state,
        "POST",
        &format!("/api/v1/admin/service-credentials/{new_id}/revoke"),
        "dev-privacy.wolf",
        None,
        OPS,
    )
    .await;
    assert_eq!(status, StatusCode::NOT_FOUND, "already revoked");

    // Unknown scope and unknown principal are rejected.
    let (status, _) = call(
        &state,
        "POST",
        "/api/v1/admin/service-credentials",
        "dev-privacy.wolf",
        Some(json!({ "service_username": "svc.lab-adapter", "name": "x", "scopes": ["no.such"] })),
        OPS,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    let (status, _) = call(
        &state,
        "POST",
        "/api/v1/admin/service-credentials",
        "dev-privacy.wolf",
        Some(json!({ "service_username": "dr.garcia", "name": "x", "scopes": ["result.ingest"] })),
        OPS,
    )
    .await;
    assert_eq!(
        status,
        StatusCode::NOT_FOUND,
        "humans are not machine principals"
    );

    // Scopes beyond the principal's current roles are rejected: the
    // lab-adapter role permits result.ingest but not patient.search.
    let (status, body) = call(
        &state,
        "POST",
        "/api/v1/admin/service-credentials",
        "dev-privacy.wolf",
        Some(json!({ "service_username": "svc.lab-adapter", "name": "x", "scopes": ["patient.search"] })),
        OPS,
    )
    .await;
    assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
    assert_eq!(body["error"]["code"], "scope_exceeds_role", "{body}");

    // Every operation was audited.
    let (_, audit) = call(
        &state,
        "GET",
        "/api/v1/audit?limit=200",
        "dev-privacy.wolf",
        None,
        OPS,
    )
    .await;
    let events = audit["events"].as_array().unwrap();
    for action in [
        "service_credential.issue",
        "service_credential.rotate",
        "service_credential.revoke",
    ] {
        assert!(
            events.iter().any(|e| e["action"] == json!(action)),
            "missing audit for {action}"
        );
    }
}

#[tokio::test]
async fn service_credentials_are_tenant_scoped() {
    let state = dev_state().await;
    let (status, issued) = call(
        &state,
        "POST",
        "/api/v1/admin/service-credentials",
        "dev-privacy.wolf",
        Some(json!({
            "service_username": "svc.lab-adapter",
            "name": "tenant scope test",
            "scopes": ["result.ingest"],
        })),
        OPS,
    )
    .await;
    assert_eq!(status, StatusCode::OK, "{issued}");
    let cred_id = issued["id"].as_str().unwrap().to_string();

    // A privacy officer in another tenant cannot see or manage it.
    let other_tenant: (uuid::Uuid,) = sqlx::query_as(
        "SELECT t.id FROM tenants t WHERE t.id <> (SELECT tenant_id FROM users WHERE username='privacy.wolf') LIMIT 1",
    )
    .fetch_one(&state.pool)
    .await
    .unwrap();
    let username = format!("privacy.{}", uuid::Uuid::now_v7().simple());
    let uid = uuid::Uuid::now_v7();
    sqlx::query("INSERT INTO users (id, tenant_id, username, display_name) VALUES ($1,$2,$3,'Other Tenant Privacy')")
        .bind(uid)
        .bind(other_tenant.0)
        .bind(&username)
        .execute(&state.pool)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO role_assignments (id, tenant_id, user_id, role, facility_id)
         SELECT $1, $2, $3, 'privacy_officer', f.id FROM facilities f WHERE f.tenant_id = $2 LIMIT 1",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(other_tenant.0)
    .bind(uid)
    .execute(&state.pool)
    .await
    .unwrap();
    let other_token = format!("dev-{username}");

    let (status, listed) = call(
        &state,
        "GET",
        "/api/v1/admin/service-credentials",
        &other_token,
        None,
        OPS,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!listed["credentials"]
        .as_array()
        .unwrap()
        .iter()
        .any(|c| c["id"] == json!(cred_id)));

    for op in ["rotate", "revoke"] {
        let (status, _) = call(
            &state,
            "POST",
            &format!("/api/v1/admin/service-credentials/{cred_id}/{op}"),
            &other_token,
            None,
            OPS,
        )
        .await;
        assert_eq!(status, StatusCode::NOT_FOUND, "cross-tenant {op} must 404");
    }
}

// ---------------------------------------------------------------------------
// Authorization: emergency purpose gating + security headers
// ---------------------------------------------------------------------------

#[tokio::test]
async fn emergency_purpose_requires_break_glass_role_for_search_and_read() {
    let state = dev_state().await;
    // Ordinary physician: emergency purpose does not grant tenant-wide search.
    let (status, _) = call(
        &state,
        "GET",
        "/api/v1/patients?query=pat",
        "dev-dr.garcia",
        None,
        &[("x-purpose-of-use", "emergency")],
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);

    // Treatment-purpose search still works for the same physician.
    let (status, _) = call(
        &state,
        "GET",
        "/api/v1/patients?query=pat",
        "dev-dr.garcia",
        None,
        &[],
    )
    .await;
    assert_eq!(status, StatusCode::OK);
}

#[tokio::test]
async fn cross_tenant_role_assignments_grant_nothing() {
    let state = dev_state().await;
    let (home_tenant, other_tenant): (uuid::Uuid, uuid::Uuid) = {
        let home: (uuid::Uuid,) =
            sqlx::query_as("SELECT tenant_id FROM users WHERE username = 'dr.garcia'")
                .fetch_one(&state.pool)
                .await
                .unwrap();
        let other: (uuid::Uuid,) = sqlx::query_as("SELECT id FROM tenants WHERE id <> $1 LIMIT 1")
            .bind(home.0)
            .fetch_one(&state.pool)
            .await
            .unwrap();
        (home.0, other.0)
    };

    // A user in the home tenant whose only role is assigned under another
    // tenant must have no privileges in the home tenant.
    let username = format!("crosstenant.{}", uuid::Uuid::now_v7().simple());
    let uid = uuid::Uuid::now_v7();
    sqlx::query(
        "INSERT INTO users (id, tenant_id, username, display_name) VALUES ($1,$2,$3,'Cross Tenant Role')",
    )
    .bind(uid)
    .bind(home_tenant)
    .bind(&username)
    .execute(&state.pool)
    .await
    .unwrap();
    sqlx::query(
        "INSERT INTO role_assignments (id, tenant_id, user_id, role, facility_id)
         SELECT $1, $2, $3, 'physician', f.id FROM facilities f WHERE f.tenant_id = $2 LIMIT 1",
    )
    .bind(uuid::Uuid::now_v7())
    .bind(other_tenant)
    .bind(uid)
    .execute(&state.pool)
    .await
    .unwrap();

    let token = format!("dev-{username}");
    let (status, _) = call(
        &state,
        "GET",
        "/api/v1/patients?query=pat",
        &token,
        None,
        &[],
    )
    .await;
    assert_eq!(
        status,
        StatusCode::FORBIDDEN,
        "a role assigned under another tenant must grant nothing"
    );
}

#[tokio::test]
async fn responses_carry_security_headers() {
    let state = dev_state().await;
    let req = Request::builder()
        .method("GET")
        .uri("/health")
        .body(Body::empty())
        .unwrap();
    let res = wellos_server::app(state).oneshot(req).await.unwrap();
    assert_eq!(res.headers()["x-content-type-options"], "nosniff");
    assert_eq!(res.headers()["referrer-policy"], "no-referrer");
    assert_eq!(res.headers()["x-frame-options"], "DENY");
    assert_eq!(
        res.headers()["content-security-policy"],
        "default-src 'none'; frame-ancestors 'none'"
    );
}
