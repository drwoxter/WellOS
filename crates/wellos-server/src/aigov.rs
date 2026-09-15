//! Governed model execution shared by every dMind route.
//!
//! Before a route calls the model gateway it asks for an [`ExecutionPlan`]:
//! either an identical valid prior artifact exists for the very same
//! clinical scope (tenant, patient, task, bound clinical resource, input
//! hash, provider, model, model version, prompt version and output schema)
//! and its output is reused without spending provider credit, or a
//! quota-checked execution is reserved. Every successful generation is then
//! annotated with the full provenance ([`Provenance`]) the AIArtifact
//! contract requires, including whether any synthetic provider took part.
//! Nothing in this module ever fabricates or substitutes output: a disabled
//! or unavailable provider surfaces as a typed error the caller maps
//! honestly.

use crate::error::ApiError;
use crate::state::AppState;
use axum::http::StatusCode;
use dmind_gateway::{GatewayError, Operation, Usage};
use serde_json::Value;
use sqlx::{PgConnection, Row};
use uuid::Uuid;
use wellos_domain::ai::{ArtifactStatus, ProviderInfo};

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
    /// Reuse `artifact`; the provider is not called. Its stored `synthetic`
    /// flag travels with the output.
    Reuse(Box<ReusableArtifact>),
    /// A quota slot was reserved; call the provider now. `synthetic` is the
    /// model provider's own declaration for this execution.
    Execute { execution_id: Uuid, synthetic: bool },
}

impl ExecutionPlan {
    /// Whether model output produced under this plan is synthetic: a reused
    /// artifact keeps its recorded flag, a new execution takes the provider's.
    pub fn model_synthetic(&self) -> bool {
        match self {
            ExecutionPlan::Reuse(prior) => prior.synthetic,
            ExecutionPlan::Execute { synthetic, .. } => *synthetic,
        }
    }
}

/// The exact clinical resource a dMind task is bound to. Every task declares
/// its scope here, so an artifact can only ever be reused for the same
/// patient *and* the same encounter, visit, observation or risk assessment
/// it was generated for. The variant fixes the `artifact_type`, the gateway
/// [`Operation`] and the `ai_artifacts` column that holds the resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReuseScope {
    ResultSummary { observation_id: Uuid },
    EncounterSummary { encounter_id: Uuid },
    ScribeDraft { encounter_id: Uuid },
    TriageProposal { visit_id: Uuid },
    RiskSummary { risk_assessment_id: Uuid },
}

impl ReuseScope {
    pub fn artifact_type(self) -> &'static str {
        match self {
            ReuseScope::ResultSummary { .. } => "result_summary",
            ReuseScope::EncounterSummary { .. } => "encounter_summary",
            ReuseScope::ScribeDraft { .. } => "scribe_draft",
            ReuseScope::TriageProposal { .. } => "triage_proposal",
            ReuseScope::RiskSummary { .. } => "risk_summary",
        }
    }

    pub fn operation(self) -> Operation {
        match self {
            ReuseScope::ResultSummary { .. } | ReuseScope::EncounterSummary { .. } => {
                Operation::ResultSummary
            }
            ReuseScope::ScribeDraft { .. } => Operation::NoteDraft,
            ReuseScope::TriageProposal { .. } => Operation::TriageProposal,
            ReuseScope::RiskSummary { .. } => Operation::RiskSummary,
        }
    }

    /// The `ai_artifacts` column binding the artifact to its resource. A
    /// closed set of identifiers, safe to splice into SQL.
    pub fn resource_column(self) -> &'static str {
        match self {
            ReuseScope::ResultSummary { .. } => "observation_id",
            ReuseScope::EncounterSummary { .. } | ReuseScope::ScribeDraft { .. } => "encounter_id",
            ReuseScope::TriageProposal { .. } => "visit_id",
            ReuseScope::RiskSummary { .. } => "risk_assessment_id",
        }
    }

    pub fn resource_id(self) -> Uuid {
        match self {
            ReuseScope::ResultSummary { observation_id } => observation_id,
            ReuseScope::EncounterSummary { encounter_id }
            | ReuseScope::ScribeDraft { encounter_id } => encounter_id,
            ReuseScope::TriageProposal { visit_id } => visit_id,
            ReuseScope::RiskSummary { risk_assessment_id } => risk_assessment_id,
        }
    }
}

/// Provenance written on every generated artifact.
#[derive(Debug, Clone)]
pub struct Provenance<'a> {
    pub scope: ReuseScope,
    pub provider: &'a ProviderInfo,
    pub prompt_version: &'a str,
    pub input_refs: &'a [String],
    pub usage: Option<&'a Usage>,
    /// True when any provider on the execution path (model, or transcription
    /// feeding it) is synthetic, or when a synthetic artifact was reused.
    pub synthetic: bool,
    pub reused_from: Option<Uuid>,
}

/// Decide how to satisfy a model request for `scope` on `patient_id` of
/// `tenant_id`.
///
/// Runs in its own short transaction so the quota reservation is durable
/// before any network call, and holds a per-tenant advisory lock so
/// concurrent requests cannot both pass the same quota check.
pub async fn plan(
    state: &AppState,
    tenant_id: Uuid,
    patient_id: Uuid,
    scope: ReuseScope,
    input_hash: &str,
    output_schema: &str,
) -> Result<ExecutionPlan, ApiError> {
    let status = state.gateway.status();
    if !status.state.is_callable() {
        return Err(capability_error("model", &status));
    }
    if status.external {
        external_processing_allowed(state, tenant_id, patient_id).await?;
    }
    let info = state.gateway.info();
    let prompt_version = state.gateway.prompt_version(scope.operation());
    let artifact_type = scope.artifact_type();

    let mut tx = state.pool.begin().await?;
    sqlx::query("SELECT pg_advisory_xact_lock(hashtext('ai_quota'), hashtext($1))")
        .bind(tenant_id.to_string())
        .execute(&mut *tx)
        .await?;

    if let Some(reusable) = find_reusable(
        &mut tx,
        &ReuseKey {
            tenant_id,
            patient_id,
            scope,
            input_hash,
            provider: &info,
            prompt_version: &prompt_version,
            output_schema,
        },
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
            COUNT(*) FILTER (WHERE artifact_type = $2) AS task_count
         FROM ai_executions
         WHERE tenant_id = $1 AND executed_at > now() - interval '1 hour'",
    )
    .bind(tenant_id)
    .bind(artifact_type)
    .fetch_one(&mut *tx)
    .await?;
    let tenant_count: i64 = row.get("tenant_count");
    let task_count: i64 = row.get("task_count");
    let tenant_exhausted = tenant_count >= quotas.tenant_per_hour;
    let task_exhausted = task_count >= quotas.task_per_hour;
    if tenant_exhausted || task_exhausted {
        // Each exhausted window frees a slot when its N-th newest execution
        // ages out; a request is admitted only once every exhausted window
        // has done so, so the later of the two governs.
        let mut frees_at: Option<chrono::DateTime<chrono::Utc>> = None;
        if tenant_exhausted {
            frees_at = window_frees_at(&mut tx, tenant_id, None, quotas.tenant_per_hour).await?;
        }
        if task_exhausted {
            let task = window_frees_at(
                &mut tx,
                tenant_id,
                Some(artifact_type),
                quotas.task_per_hour,
            )
            .await?;
            frees_at = frees_at.max(task);
        }
        let retry_after = frees_at
            .map(|t| (t - chrono::Utc::now()).num_seconds())
            .filter(|s| *s > 0)
            .unwrap_or(1) as u64;
        tx.rollback().await?;
        let mut err = ApiError::new(
            StatusCode::TOO_MANY_REQUESTS,
            "ai_quota_exceeded",
            if tenant_exhausted {
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
    Ok(ExecutionPlan::Execute {
        execution_id,
        synthetic: status.synthetic,
    })
}

/// Patient data may only leave the cell when the deployment permits it and
/// the patient's `ai_external_processing` consent is active. Reuse of an
/// existing artifact is not exempt: it still discloses the model's reading
/// of that patient's data.
pub async fn external_processing_allowed(
    state: &AppState,
    tenant_id: Uuid,
    patient_id: Uuid,
) -> Result<(), ApiError> {
    if !state.allow_external_ai {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "ai_external_disallowed",
            "this deployment does not permit external AI processing of patient data",
        ));
    }
    let consent: Option<(String,)> = sqlx::query_as(
        "SELECT status FROM consents
         WHERE tenant_id = $1 AND patient_id = $2 AND purpose = 'ai_external_processing'
         ORDER BY version DESC, recorded_at DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_optional(&state.pool)
    .await?;
    if !matches!(consent, Some((ref s,)) if s == "active") {
        return Err(ApiError::new(
            StatusCode::CONFLICT,
            "ai_external_consent_required",
            "the patient has no active consent for external AI processing; the AI action is unavailable for this patient",
        ));
    }
    Ok(())
}

/// When the hourly window for the tenant (`task = None`) or one task admits
/// a request again: one hour after its `limit`-th newest execution, i.e.
/// the moment the in-window count drops below `limit`.
async fn window_frees_at(
    conn: &mut PgConnection,
    tenant_id: Uuid,
    task: Option<&str>,
    limit: i64,
) -> Result<Option<chrono::DateTime<chrono::Utc>>, ApiError> {
    let row: Option<(chrono::DateTime<chrono::Utc>,)> = sqlx::query_as(
        "SELECT executed_at + interval '1 hour'
         FROM ai_executions
         WHERE tenant_id = $1 AND ($2::text IS NULL OR artifact_type = $2)
           AND executed_at > now() - interval '1 hour'
         ORDER BY executed_at DESC, id DESC
         OFFSET $3 LIMIT 1",
    )
    .bind(tenant_id)
    .bind(task)
    .bind((limit - 1).max(0))
    .fetch_optional(&mut *conn)
    .await?;
    Ok(row.map(|r| r.0))
}

/// Statuses of finished generations. Only the newest of these decides reuse:
/// `awaiting_review`, `approved` and `superseded` (merely replaced by a newer
/// proposal) still stand; `rejected` and `withdrawn` record a professional's
/// decision against that output, which no older copy may override. Drafts,
/// invalidated and unavailable artifacts never qualify.
fn decided_statuses() -> [&'static str; 5] {
    [
        ArtifactStatus::AwaitingReview,
        ArtifactStatus::Approved,
        ArtifactStatus::Superseded,
        ArtifactStatus::Rejected,
        ArtifactStatus::Withdrawn,
    ]
    .map(ArtifactStatus::as_str)
}

fn output_stands(status: &str) -> bool {
    status != ArtifactStatus::Rejected.as_str() && status != ArtifactStatus::Withdrawn.as_str()
}

/// Everything that must be identical for a prior artifact to stand in for a
/// new request. Any difference — another patient, another encounter or
/// visit, another provider or model version — is a different request.
struct ReuseKey<'a> {
    tenant_id: Uuid,
    patient_id: Uuid,
    scope: ReuseScope,
    input_hash: &'a str,
    provider: &'a ProviderInfo,
    prompt_version: &'a str,
    output_schema: &'a str,
}

async fn find_reusable(
    conn: &mut PgConnection,
    key: &ReuseKey<'_>,
) -> Result<Option<ReusableArtifact>, ApiError> {
    let sql = format!(
        "SELECT id, status, output, citations, limitations, model, model_version, route,
                prompt_version, usage, synthetic
         FROM ai_artifacts
         WHERE tenant_id = $1 AND patient_id = $2 AND artifact_type = $3 AND {resource} = $4
           AND input_hash = $5
           AND provider = $6 AND model = $7 AND model_version = $8
           AND prompt_version = $9 AND output_schema = $10
           AND output IS NOT NULL
           AND status = ANY($11)
         ORDER BY generated_at DESC NULLS LAST, id DESC
         LIMIT 1",
        resource = key.scope.resource_column()
    );
    let row = sqlx::query(&sql)
        .bind(key.tenant_id)
        .bind(key.patient_id)
        .bind(key.scope.artifact_type())
        .bind(key.scope.resource_id())
        .bind(key.input_hash)
        .bind(&key.provider.provider)
        .bind(&key.provider.model)
        .bind(&key.provider.model_version)
        .bind(key.prompt_version)
        .bind(key.output_schema)
        .bind(&decided_statuses()[..])
        .fetch_optional(&mut *conn)
        .await?;
    let r = match row {
        Some(r) if output_stands(r.get::<String, _>("status").as_str()) => r,
        _ => return Ok(None),
    };
    Ok(Some(ReusableArtifact {
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
///
/// The update only applies when the artifact row is bound to the declared
/// scope and, if output is reused, the prior artifact shares the artifact's
/// tenant, patient, task and clinical resource; otherwise nothing is written
/// and the caller's transaction fails. The database additionally enforces
/// tenant and patient agreement for `reused_from`.
pub async fn annotate(
    conn: &mut PgConnection,
    artifact_id: Uuid,
    provenance: &Provenance<'_>,
) -> Result<(), ApiError> {
    let resource = provenance.scope.resource_column();
    let sql = format!(
        "UPDATE ai_artifacts a
         SET provider = $2, prompt_version = $3, input_refs = $4, usage = $5,
             synthetic = $6, reused_from = $7
         WHERE a.id = $1 AND a.artifact_type = $8 AND a.{resource} = $9
           AND ($7::uuid IS NULL OR EXISTS (
                 SELECT 1 FROM ai_artifacts p
                 WHERE p.id = $7 AND p.tenant_id = a.tenant_id AND p.patient_id = a.patient_id
                   AND p.artifact_type = a.artifact_type AND p.{resource} = a.{resource}))"
    );
    let updated = sqlx::query(&sql)
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
        .bind(provenance.scope.artifact_type())
        .bind(provenance.scope.resource_id())
        .execute(&mut *conn)
        .await?;
    if updated.rows_affected() != 1 {
        return Err(ApiError::internal(format!(
            "artifact {artifact_id} provenance refused: scope {:?} or reuse link {:?} does not match the artifact",
            provenance.scope, provenance.reused_from
        )));
    }
    Ok(())
}

/// Honest availability of every AI capability, as reported by readiness and
/// consumed by the UI. `structured_note` is the end-to-end scribe path
/// (transcription feeding the note-draft model operation), so it is only as
/// available as the weaker of its two providers.
pub fn capabilities(state: &AppState) -> Value {
    use dmind_gateway::{CapabilityState, CapabilityStatus};
    let model = state.gateway.status();
    let transcription = state.scribe.status();
    let structured_note = {
        let rank = |s: CapabilityState| match s {
            CapabilityState::InvalidConfiguration => 3,
            CapabilityState::Disabled => 2,
            CapabilityState::Degraded => 1,
            CapabilityState::Ready => 0,
        };
        let (limiting, name) = if rank(transcription.state) >= rank(model.state) {
            (&transcription, "transcription")
        } else {
            (&model, "model")
        };
        let reason = match limiting.state {
            CapabilityState::Ready => None,
            state => Some(format!(
                "{name} capability is {}: {}",
                state.as_str(),
                limiting.reason.clone().unwrap_or_default()
            )),
        };
        CapabilityStatus {
            state: limiting.state,
            provider: format!("{}+{}", transcription.provider, model.provider),
            model: model.model.clone(),
            reason,
            external: transcription.external || model.external,
            synthetic: transcription.synthetic || model.synthetic,
        }
    };
    serde_json::json!({
        "model": model,
        "transcription": transcription,
        "structured_note": structured_note,
        "transcription_languages": state.runtime.scribe_languages,
    })
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
