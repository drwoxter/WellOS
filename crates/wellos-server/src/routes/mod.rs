pub mod admin;
pub mod ai;
pub mod consent;
pub mod encounters;
pub mod fhir;
pub mod lab;
pub mod loops;
pub mod patients;

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{self, Decision, ResourceCtx};
use crate::state::AppState;
use axum::routing::{get, post};
use axum::Router;
use tower_http::cors::CorsLayer;

pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(admin::health))
        .route("/ready", get(admin::ready))
        .route("/api/v1/meta/tenant", get(admin::tenant_meta))
        .route(
            "/api/v1/patients",
            post(patients::register).get(patients::search),
        )
        .route("/api/v1/patients/:id", get(patients::chart))
        .route("/api/v1/encounters", post(encounters::start))
        .route(
            "/api/v1/service-requests",
            post(encounters::create_service_request),
        )
        .route("/api/v1/service-requests/:id", get(loops::detail))
        .route("/api/v1/service-requests/:id/review", post(loops::review))
        .route("/api/v1/service-requests/:id/notify", post(loops::notify))
        .route("/api/v1/service-requests/:id/close", post(loops::close))
        .route("/api/v1/worklist", get(loops::worklist))
        .route("/api/v1/lab/results", post(lab::ingest_result))
        .route("/api/v1/ai-artifacts/:id/review", post(ai::review_artifact))
        .route("/api/v1/consents", post(consent::set_consent))
        .route("/api/v1/audit", get(admin::audit_log))
        .route(
            "/api/v1/jobs/escalate-overdue",
            post(admin::escalate_overdue),
        )
        .route("/fhir/r4/Patient/:id", get(fhir::patient))
        .route("/fhir/r4/Observation/:id", get(fhir::observation))
        .route("/fhir/r4/ServiceRequest/:id", get(fhir::service_request))
        .layer(CorsLayer::permissive())
        .with_state(state)
}

/// Authorize an action, audit the decision, and reject on denial.
pub async fn guard(
    state: &AppState,
    ctx: &AuthContext,
    action: &str,
    resource_type: &str,
    resource: Option<ResourceCtx>,
) -> Result<Decision, ApiError> {
    let decision = policy::authorize(&state.pool, ctx, action, resource.as_ref()).await?;
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
        return Err(ApiError::forbidden(format!("action '{action}' denied")));
    }
    audit::record(
        &state.pool,
        ctx,
        action,
        Some(resource_type),
        resource_id,
        "allow",
        Some(&decision.reason),
    )
    .await
    .map_err(ApiError::internal)?;
    if decision.used_break_glass {
        audit::emit(
            &state.pool,
            ctx,
            "break_glass.activated",
            &state.cell,
            serde_json::json!({ "action": action }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
    }
    Ok(decision)
}
