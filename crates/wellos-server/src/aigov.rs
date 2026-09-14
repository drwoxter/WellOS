//! Governed model execution shared by every dMind route.
//!
//! Before a route calls the model gateway it asks for an [`ExecutionPlan`]:
//! either an identical valid prior artifact exists (same tenant, task, input
//! hash, model, prompt version and output schema) and its output is reused
//! without spending provider credit, or a quota-checked execution is
//! reserved. Every successful generation is then annotated with the full
//! provenance ([`Provenance`]) the AIArtifact contract requires. Nothing in
//! this module ever fabricates or substitutes output: a disabled or
//! unavailable provider surfaces as a typed error the caller maps honestly.

use crate::error::ApiError;
use crate::state::AppState;
use axum::http::StatusCode;
use dmind_gateway::{GatewayError, Operation, Usage};
use serde_json::Value;
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wellos_domain::ai::ProviderInfo;

/// A prior artifact whose validated output applies verbatim to the current
/// request.
#[derive(Debug, Clone)]
pub struct ReusableArtifact {
    pub id: Uuid,
    pub output: Value,
    pub citations: Value,
    pub limitations: Value,
    pub model: String,
    pub model_version: String,
    pub route: String,
    pub prompt_version: String,
    pub usage: Option<Value>,
    pub synthetic: bool,
}

impl ReusableArtifact {
    pub fn output_as<T: serde::de::DeserializeOwned>(&self) -> Result<T, ApiError> {
        serde_json::from_value(self.output.clone()).map_err(ApiError::internal)
    }

    pub fn usage_as(&self) -> Option<Usage> {
        self.usage
            .clone()
            .and_then(|u| serde_json::from_value(u).ok())
    }
}

#[derive(Debug)]
pub enum ExecutionPlan {
    /// Reuse `artifact`; the provider is not called.
    Reuse(Box<ReusableArtifact>),
    /// A quota slot was reserved; call the provider now.
    Execute { execution_id: Uuid },
}

/// Provenance written on every generated artifact.
#[derive(Debug, Clone)]
pub struct Provenance<'a> {
    pub provider: &'a ProviderInfo,
    pub prompt_version: &'a str,
    pub input_refs: &'a [String],
    pub usage: Option<&'a Usage>,
    pub synthetic: bool,
    pub reused_from: Option<Uuid>,
}

/// Decide how to satisfy a model request for `artifact_type` on `tenant_id`.
///
/// Runs in its own short transaction so the quota reservation is durable
/// before any network call, and holds a per-tenant advisory lock so
/// concurrent requests cannot both pass the same quota check.
pub async fn plan(
    state: &AppState,
    tenant_id: Uuid,
    artifact_type: &str,
    op: Operation,
    input_hash: &str,
    output_schema: &str,
) -> Result<ExecutionPlan, ApiError> {
    let status = state.gateway.status();
    if !status.state.is_callable() {
        return Err(capability_error("model", &status));
    }
    let info = state.gateway.info();
    let prompt_version = state.gateway.prompt_version(op);

    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('ai_quota'), hashtext($1))")
        .bind(tenant_id.to_string())
        .execute(&mut *tx)
        .await?;

    if let Some(reusable) = find_reusable(
        &mut tx,
        tenant_id,
        artifact_type,
        input_hash,
        &info.model,
        &prompt_version,
        output_schema,
    )
    .await?
    {
        tx.commit().await?;
        return Ok(ExecutionPlan::Reuse(Box::new(reusable)));
    }

    let quotas = &state.runtime.ai_quotas;
    let row = sqlx::query(
        "SELECT
            COUNT(*) FILTER (WHERE true) AS tenant_count,
            COUNT(*) FILTER (WHERE artifact_type = $2) AS task_count,
            MIN(executed_at) AS oldest
         FROM ai_executions
         WHERE tenant_id = $1 AND executed_at > now() - interval '1 hour'",
    )
    .bind(tenant_id)
    .bind(artifact_type)
    .fetch_one(&mut *tx)
    .await?;
    let tenant_count: i64 = row.get("tenant_count");
    let task_count: i64 = row.get("task_count");
    if tenant_count >= quotas.tenant_per_hour || task_count >= quotas.task_per_hour {
        let oldest: Option<chrono::DateTime<chrono::Utc>> = row.get("oldest");
        let retry_after = oldest
            .map(|o| (o + chrono::Duration::hours(1) - chrono::Utc::now()).num_seconds())
            .filter(|s| *s > 0)
            .unwrap_or(60) as u64;
        tx.rollback().await?;
        let mut err = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "ai_quota_exceeded",
            if tenant_count >= quotas.tenant_per_hour {
                "the hourly dMind quota for this organization is exhausted; retry later"
            } else {
                "the hourly dMind quota for this task is exhausted; retry later"
            },
        );
        err.retry_after = Some(retry_after);
        return Err(err);
    }

    let execution_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO ai_executions (id, tenant_id, artifact_type, provider, model, external)
         VALUES ($1,$2,$3,$4,$5,$6)",
    )
    .bind(execution_id)
    .bind(tenant_id)
    .bind(artifact_type)
    .bind(&info.provider)
    .bind(&info.model)
    .bind(status.external)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(ExecutionPlan::Execute { execution_id })
}

async fn find_reusable(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    artifact_type: &str,
    input_hash: &str,
    model: &str,
    prompt_version: &str,
    output_schema: &str,
) -> Result<Option<ReusableArtifact>, ApiError> {
    let row = sqlx::query(
        "SELECT id, output, citations, limitations, model, model_version, route,
                prompt_version, usage, synthetic
         FROM ai_artifacts
         WHERE tenant_id = $1 AND artifact_type = $2 AND input_hash = $3
           AND model = $4 AND prompt_version = $5 AND output_schema = $6
           AND output IS NOT NULL
           AND status NOT IN ('invalidated', 'unavailable')
         ORDER BY generated_at DESC NULLS LAST, id DESC
         LIMIT 1",
    )
    .bind(tenant_id)
    .bind(artifact_type)
    .bind(input_hash)
    .bind(model)
    .bind(prompt_version)
    .bind(output_schema)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| ReusableArtifact {
        id: r.get("id"),
        output: r.get("output"),
        citations: r.get("citations"),
        limitations: r.get("limitations"),
        model: r.get("model"),
        model_version: r
            .get::<Option<String>, _>("model_version")
            .unwrap_or_default(),
        route: r.get::<Option<String>, _>("route").unwrap_or_default(),
        prompt_version: r.get("prompt_version"),
        usage: r.get("usage"),
        synthetic: r.get("synthetic"),
    }))
}

/// Link a reserved execution to the artifact it produced.
pub async fn bind_execution(
    conn: &mut PgConnection,
    execution_id: Uuid,
    artifact_id: Uuid,
) -> Result<(), ApiError> {
    sqlx::query("UPDATE ai_executions SET artifact_id = $2 WHERE id = $1")
        .bind(execution_id)
        .bind(artifact_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Write the provenance columns of an artifact inside the caller's
/// transaction (the route already wrote the typed output and citations).
pub async fn annotate(
    conn: &mut PgConnection,
    artifact_id: Uuid,
    provenance: &Provenance<'_>,
) -> Result<(), ApiError> {
    sqlx::query(
        "UPDATE ai_artifacts
         SET provider = $2, prompt_version = $3, input_refs = $4, usage = $5,
             synthetic = $6, reused_from = $7
         WHERE id = $1",
    )
    .bind(artifact_id)
    .bind(&provenance.provider.provider)
    .bind(provenance.prompt_version)
    .bind(serde_json::to_value(provenance.input_refs).map_err(ApiError::internal)?)
    .bind(
        provenance
            .usage
            .map(serde_json::to_value)
            .transpose()
            .map_err(ApiError::internal)?,
    )
    .bind(provenance.synthetic)
    .bind(provenance.reused_from)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Honest HTTP mapping of a capability that cannot serve requests.
pub fn capability_error(capability: &str, status: &dmind_gateway::CapabilityStatus) -> ApiError {
    use dmind_gateway::CapabilityState;
    let reason = status.reason.clone().unwrap_or_default();
    match status.state {
        CapabilityState::Disabled => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "ai_disabled",
            format!("the {capability} capability is disabled by configuration: {reason}"),
        ),
        CapabilityState::InvalidConfiguration => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "ai_misconfigured",
            format!("the {capability} capability is unavailable because its configuration is invalid: {reason}"),
        ),
        CapabilityState::Degraded | CapabilityState::Ready => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "ai_unavailable",
            format!("the {capability} capability is currently unavailable; care continues without it"),
        ),
    }
}

/// Map a gateway failure to a recoverable client error. Provider output is
/// never echoed; only the failure class is exposed.
pub fn gateway_error(err: GatewayError) -> ApiError {
    match err {
        GatewayError::Disabled(reason) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "ai_disabled",
            format!("the model capability is disabled by configuration: {reason}"),
        ),
        GatewayError::Unavailable(_) => ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "ai_unavailable",
            "the model provider is currently unavailable; care continues without it",
        ),
        GatewayError::InvalidOutput(_) => {
            tracing::warn!("model output rejected by schema validation");
            ApiError::new(
                StatusCode::BAD_GATEWAY,
                "ai_invalid_output",
                "the model returned output that failed validation; no draft was created",
            )
        }
        GatewayError::PolicyDenied(reason) => {
            ApiError::new(StatusCode::FORBIDDEN, "ai_policy_denied", reason)
        }
    }
}
