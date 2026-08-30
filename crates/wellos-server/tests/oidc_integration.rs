//! OIDC/OAuth 2.1 boundary tests: JWT validation against a configured JWKS,
//! stable-subject mapping to local identities, and fail-closed behavior when
//! dev authentication is disabled or no provider is configured. All keys are
//! generated in-test and purely synthetic.

use axum::body::Body;
use axum::http::{Request, StatusCode};
use base64::Engine;
use ed25519_dalek::pkcs8::EncodePrivateKey;
use ed25519_dalek::SigningKey;
use http_body_util::BodyExt;
use jsonwebtoken::{encode, EncodingKey, Header};
use serde_json::json;
use std::sync::Arc;
use tower::ServiceExt;
use wellos_server::oidc::JwksKeys;
use wellos_server::state::{AppState, AuthConfig, OidcConfig};

const ISSUER: &str = "https://idp.example.test/";
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

async fn state_with(dev_auth: bool, jwks_json: Option<&str>) -> AppState {
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
    let oidc = jwks_json.map(|raw| OidcConfig {
        issuer: ISSUER.to_string(),
        audience: AUDIENCE.to_string(),
        keys: JwksKeys::Static(serde_json::from_str(raw).unwrap()),
        leeway_secs: 60,
        require_mfa: false,
        accepted_amr: vec!["mfa".into(), "otp".into(), "hwk".into()],
        accepted_acr: vec!["phrh".into()],
    });
    AppState::with_auth(
        pool,
        gateway,
        AuthConfig {
            dev_auth_enabled: dev_auth,
            oidc,
            ..AuthConfig::development()
        },
    )
}

fn claims(sub: &str, iss: &str, aud: &str, exp_offset: i64) -> serde_json::Value {
    let now = chrono::Utc::now().timestamp();
    json!({
        "sub": sub,
        "iss": iss,
        "aud": aud,
        "exp": now + exp_offset,
        "iat": now,
        "nbf": now - 10,
    })
}

async fn get_worklist(state: &AppState, token: &str) -> StatusCode {
    let req = Request::builder()
        .method("GET")
        .uri("/api/v1/worklist")
        .header("Authorization", format!("Bearer {token}"))
        .body(Body::empty())
        .unwrap();
    let res = wellos_server::app(state.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = res.status();
    let _ = res.into_body().collect().await;
    status
}

#[tokio::test]
async fn valid_oidc_token_maps_subject_to_local_identity() {
    let idp = test_idp(7, "test-key");
    let state = state_with(false, Some(&idp.jwks_json)).await;
    let token = encode(
        &idp.header,
        &claims("synthetic|dr.garcia", ISSUER, AUDIENCE, 600),
        &idp.encoding_key,
    )
    .unwrap();
    assert_eq!(get_worklist(&state, &token).await, StatusCode::OK);
}

#[tokio::test]
async fn oidc_rejects_bad_issuer_audience_expiry_subject_and_signature() {
    let idp = test_idp(7, "test-key");
    let state = state_with(false, Some(&idp.jwks_json)).await;

    let cases = [
        claims(
            "synthetic|dr.garcia",
            "https://evil.example.test/",
            AUDIENCE,
            600,
        ),
        claims("synthetic|dr.garcia", ISSUER, "other-api", 600),
        claims("synthetic|dr.garcia", ISSUER, AUDIENCE, -600),
        claims("synthetic|unknown.subject", ISSUER, AUDIENCE, 600),
    ];
    for c in cases {
        let token = encode(&idp.header, &c, &idp.encoding_key).unwrap();
        assert_eq!(
            get_worklist(&state, &token).await,
            StatusCode::UNAUTHORIZED,
            "claims {c}"
        );
    }

    // Signed by a different key than the configured JWKS.
    let rogue = test_idp(9, "test-key");
    let token = encode(
        &rogue.header,
        &claims("synthetic|dr.garcia", ISSUER, AUDIENCE, 600),
        &rogue.encoding_key,
    )
    .unwrap();
    assert_eq!(get_worklist(&state, &token).await, StatusCode::UNAUTHORIZED);

    // Unknown kid.
    let unknown_kid = test_idp(7, "other-key");
    let token = encode(
        &unknown_kid.header,
        &claims("synthetic|dr.garcia", ISSUER, AUDIENCE, 600),
        &unknown_kid.encoding_key,
    )
    .unwrap();
    assert_eq!(get_worklist(&state, &token).await, StatusCode::UNAUTHORIZED);
}

#[tokio::test]
async fn dev_tokens_are_rejected_when_dev_auth_disabled() {
    let idp = test_idp(7, "test-key");
    let state = state_with(false, Some(&idp.jwks_json)).await;
    assert_eq!(
        get_worklist(&state, "dev-dr.garcia").await,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn jwt_without_configured_provider_fails_closed_with_config_error() {
    let state = state_with(true, None).await;
    let idp = test_idp(7, "test-key");
    let token = encode(
        &idp.header,
        &claims("synthetic|dr.garcia", ISSUER, AUDIENCE, 600),
        &idp.encoding_key,
    )
    .unwrap();
    assert_eq!(
        get_worklist(&state, &token).await,
        StatusCode::SERVICE_UNAVAILABLE
    );
}

#[test]
fn auth_config_fails_closed_from_env() {
    // These tests mutate process env, so they run in one test to avoid races.
    let lock = [("WELLOS_ENV", "production"), ("WELLOS_DEV_AUTH", "true")];
    for (k, v) in lock {
        std::env::set_var(k, v);
    }
    assert!(
        AuthConfig::from_env().is_err(),
        "dev auth in production must fail"
    );

    std::env::set_var("WELLOS_ENV", "staging");
    assert!(
        AuthConfig::from_env().is_err(),
        "dev auth in staging must fail"
    );

    std::env::set_var("WELLOS_ENV", "production");
    std::env::set_var("WELLOS_DEV_AUTH", "false");
    assert!(
        AuthConfig::from_env().is_err(),
        "production without an identity provider must fail closed"
    );

    std::env::set_var("WELLOS_ENV", "development");
    std::env::set_var("WELLOS_DEV_AUTH", "true");
    let cfg = AuthConfig::from_env().unwrap();
    assert!(cfg.dev_auth_enabled);

    // Malformed security-sensitive values abort startup instead of
    // silently falling back to a weaker default.
    std::env::set_var("WELLOS_DEV_AUTH", "yes");
    assert!(
        AuthConfig::from_env().is_err(),
        "malformed WELLOS_DEV_AUTH must fail"
    );
    std::env::set_var("WELLOS_DEV_AUTH", "true");

    std::env::set_var("WELLOS_OIDC_DISCOVERY", "enabled");
    assert!(
        AuthConfig::from_env().is_err(),
        "malformed WELLOS_OIDC_DISCOVERY must fail"
    );
    std::env::remove_var("WELLOS_OIDC_DISCOVERY");

    std::env::set_var("WELLOS_OIDC_ISSUER", "https://idp.example.test");
    std::env::set_var("WELLOS_OIDC_AUDIENCE", "wellos");
    std::env::set_var("WELLOS_OIDC_JWKS_JSON", r#"{"keys":[]}"#);

    std::env::set_var("WELLOS_OIDC_REQUIRE_MFA", "1");
    assert!(
        AuthConfig::from_env().is_err(),
        "malformed WELLOS_OIDC_REQUIRE_MFA must fail"
    );
    std::env::set_var("WELLOS_OIDC_REQUIRE_MFA", "true");

    std::env::set_var("WELLOS_OIDC_LEEWAY_SECS", "sixty");
    assert!(
        AuthConfig::from_env().is_err(),
        "malformed WELLOS_OIDC_LEEWAY_SECS must fail"
    );
    std::env::remove_var("WELLOS_OIDC_LEEWAY_SECS");

    std::env::set_var("WELLOS_OIDC_ACCEPTED_AMR", " , ");
    std::env::set_var("WELLOS_OIDC_ACCEPTED_ACR", " , ");
    assert!(
        AuthConfig::from_env().is_err(),
        "MFA required with no accepted amr/acr values must fail"
    );

    for k in [
        "WELLOS_OIDC_ISSUER",
        "WELLOS_OIDC_AUDIENCE",
        "WELLOS_OIDC_JWKS_JSON",
        "WELLOS_OIDC_REQUIRE_MFA",
        "WELLOS_OIDC_ACCEPTED_AMR",
        "WELLOS_OIDC_ACCEPTED_ACR",
    ] {
        std::env::remove_var(k);
    }
    for (k, _) in lock {
        std::env::remove_var(k);
    }
}
