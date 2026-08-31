pub mod admin;
pub mod ai;
pub mod consent;
pub mod creds;
pub mod encounter_docs;
pub mod encounters;
pub mod fhir;
pub mod lab;
pub mod loops;
pub mod oidc_login;
pub mod patients;
pub mod session;

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{self, Decision, ResourceCtx};
use crate::ratelimit;
use crate::state::AppState;
use axum::http::header::{HeaderName, HeaderValue, AUTHORIZATION, CONTENT_TYPE};
use axum::http::Method;
use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::CorsLayer;
use tower_http::set_header::SetResponseHeaderLayer;

pub fn router(state: AppState) -> Router {
    let router = Router::new()
        .route("/health", get(admin::health))
        .route("/ready", get(admin::ready))
        .route("/api/v1/meta/tenant", get(admin::tenant_meta))
        .route(
            "/api/v1/auth/session",
            post(session::create)
                .get(session::get)
                .delete(session::delete),
        )
        .route("/api/v1/auth/session/rotate", post(session::rotate))
        .route("/api/v1/auth/oidc/login", post(oidc_login::start))
        .route("/api/v1/auth/oidc/callback", post(oidc_login::callback))
        .route(
            "/api/v1/admin/service-credentials",
            post(creds::issue).get(creds::list),
        )
        .route(
            "/api/v1/admin/service-credentials/:id/rotate",
            post(creds::rotate),
        )
        .route(
            "/api/v1/admin/service-credentials/:id/revoke",
            post(creds::revoke),
        )
        .route(
            "/api/v1/patients",
            post(patients::register).get(patients::search),
        )
        .route("/api/v1/patients/:id", get(patients::chart))
        .route("/api/v1/encounters", post(encounters::start))
        .route("/api/v1/encounters/:id", get(encounter_docs::workspace))
        .route(
            "/api/v1/encounters/:id/note",
            post(encounter_docs::save_note),
        )
        .route("/api/v1/encounters/:id/sign", post(encounter_docs::sign))
        .route(
            "/api/v1/encounters/:id/addenda",
            post(encounter_docs::add_addendum),
        )
        .route(
            "/api/v1/encounters/:id/vitals",
            post(encounter_docs::record_vitals),
        )
        .route(
            "/api/v1/encounters/:id/diagnoses",
            post(encounter_docs::add_diagnosis),
        )
        .route(
            "/api/v1/encounters/:id/cancel",
            post(encounter_docs::cancel),
        )
        .route(
            "/api/v1/encounters/:id/ai-draft",
            post(encounter_docs::ai_draft),
        )
        .route(
            "/api/v1/service-requests",
            post(encounters::create_service_request),
        )
        .route("/api/v1/service-requests/:id", get(loops::detail))
        .route("/api/v1/service-requests/:id/review", post(loops::review))
        .route("/api/v1/service-requests/:id/notify", post(loops::notify))
        .route("/api/v1/service-requests/:id/close", post(loops::close))
        .route("/api/v1/worklist", get(loops::worklist))
        .route("/api/v1/worklist/summary", get(loops::worklist_summary))
        .route("/api/v1/lab/results", post(lab::ingest_result))
        .route("/api/v1/ai-artifacts/:id/review", post(ai::review_artifact))
        .route("/api/v1/consents", post(consent::set_consent))
        .route("/api/v1/audit", get(admin::audit_log))
        .route("/api/v1/break-glass", get(admin::break_glass_events))
        .route(
            "/api/v1/break-glass/:id/review",
            post(admin::review_break_glass),
        )
        .route(
            "/api/v1/jobs/escalate-overdue",
            post(admin::escalate_overdue),
        )
        .route("/fhir/r4/Patient/:id", get(fhir::patient))
        .route("/fhir/r4/Observation/:id", get(fhir::observation))
        .route("/fhir/r4/ServiceRequest/:id", get(fhir::service_request))
        .layer(cors_layer());
    // Defense-in-depth response headers for the API surface. The API is
    // JSON-only, so a restrictive CSP with frame protection is safe
    // everywhere; HSTS is added outside development.
    let router = router
        .layer(header_layer("x-content-type-options", "nosniff"))
        .layer(header_layer("referrer-policy", "no-referrer"))
        .layer(header_layer("x-frame-options", "DENY"))
        .layer(header_layer(
            "content-security-policy",
            "default-src 'none'; frame-ancestors 'none'",
        ));
    let env = std::env::var("WELLOS_ENV").unwrap_or_else(|_| "development".to_string());
    let router = if env != "development" {
        router.layer(header_layer(
            "strict-transport-security",
            "max-age=63072000; includeSubDomains",
        ))
    } else {
        router
    };
    router.with_state(state)
}

fn header_layer(name: &'static str, value: &'static str) -> SetResponseHeaderLayer<HeaderValue> {
    SetResponseHeaderLayer::overriding(
        HeaderName::from_static(name),
        HeaderValue::from_static(value),
    )
}

/// Restrict browser callers to an explicit origin allowlist
/// (`WELLOS_ALLOWED_ORIGINS`, comma-separated). The bundled web app talks to
/// the API through a same-origin rewrite, so only the dev UI origin is needed
/// by default.
fn cors_layer() -> CorsLayer {
    let origins: Vec<HeaderValue> = std::env::var("WELLOS_ALLOWED_ORIGINS")
        .unwrap_or_else(|_| "http://localhost:3000".to_string())
        .split(',')
        .filter_map(|o| o.trim().parse().ok())
        .collect();
    CorsLayer::new()
        .allow_origin(origins)
        .allow_methods([Method::GET, Method::POST, Method::DELETE])
        .allow_headers([
            AUTHORIZATION,
            CONTENT_TYPE,
            HeaderName::from_static("x-purpose-of-use"),
            HeaderName::from_static("x-break-glass-reason"),
            HeaderName::from_static("x-csrf-token"),
        ])
}

/// An authorization decision that allowed the action. The allow audit row is
/// written by [`Allowed::record`], which state-changing routes call inside the
/// same transaction as the mutation so a rolled-back action is never audited
/// as successful. Denials are audited immediately, outside any transaction.
pub struct Allowed {
    action: String,
    resource_type: String,
    resource_id: Option<String>,
    reason: String,
    pub used_break_glass: bool,
}

impl Allowed {
    pub async fn record(
        &self,
        conn: &mut sqlx::PgConnection,
        ctx: &AuthContext,
        cell: &str,
    ) -> Result<(), ApiError> {
        audit::record(
            &mut *conn,
            ctx,
            &self.action,
            Some(&self.resource_type),
            self.resource_id.clone(),
            "allow",
            Some(&self.reason),
        )
        .await
        .map_err(ApiError::internal)?;
        if self.used_break_glass {
            audit::emit(
                &mut *conn,
                ctx,
                "break_glass.activated",
                cell,
                serde_json::json!({ "action": self.action }),
                None,
            )
            .await
            .map_err(ApiError::internal)?;
        }
        Ok(())
    }

    /// Record the allow audit row on the pool, for read-only routes that have
    /// no surrounding transaction.
    pub async fn record_on_pool(
        &self,
        state: &AppState,
        ctx: &AuthContext,
    ) -> Result<(), ApiError> {
        let mut conn = state.pool.acquire().await?;
        self.record(&mut conn, ctx, &state.cell).await
    }
}

/// Authorize an action and reject (with a denial audit record) when denied.
pub async fn guard(
    state: &AppState,
    ctx: &AuthContext,
    action: &str,
    resource_type: &str,
    resource: Option<ResourceCtx>,
) -> Result<Allowed, ApiError> {
    // General authenticated-traffic limit, shared across API replicas.
    ratelimit::enforce_for_principal(state, ctx, ratelimit::Family::Api).await?;
    let decision: Decision = policy::authorize_with_limit(
        &state.pool,
        ctx,
        action,
        resource.as_ref(),
        state.auth.break_glass_hourly_limit,
    )
    .await?;
    let resource_id = resource
        .as_ref()
        .and_then(|r| r.patient_id.map(|p| p.to_string()));
    if !decision.allowed {
        audit::record_denial(
            &state.pool,
            ctx,
            action,
            Some(resource_type),
            resource_id,
            &decision.reason,
        )
        .await
        .map_err(ApiError::internal)?;
        // Cross-tenant and out-of-facility probes get the same shape as
        // nonexistent resources, so protected resource IDs are not
        // discoverable; the denials are still fully audited above.
        if decision.reason == "cross_tenant_access" || decision.reason == "facility_scope_denied" {
            return Err(ApiError::not_found());
        }
        return Err(ApiError::forbidden(format!("action '{action}' denied")));
    }
    Ok(Allowed {
        action: action.to_string(),
        resource_type: resource_type.to_string(),
        resource_id,
        reason: decision.reason,
        used_break_glass: decision.used_break_glass,
    })
}
