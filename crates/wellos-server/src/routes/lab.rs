//! Laboratory adapter boundary (single-quantity deliveries).
//!
//! Inbound results are idempotent (duplicate deliveries with the same
//! idempotency key create nothing new). Each delivery is issued as a one
//! component diagnostic report through the generalized typed result path, so
//! deterministic rules, criticality, alerts and order/loop transitions are
//! shared with every other modality; AI summarization happens after commit
//! and can fail without affecting the clinical record.

use crate::aigov;
use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, ResourceCtx};
use crate::routes::diagnostics::reports::{issue_in, ComponentInput, IssueInput};
use crate::routes::diagnostics::{load_order, lock_order};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use chrono::{DateTime, DurationRound, Utc};
use dmind_gateway::{GatewayError, SummaryRequest};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;
use wellos_domain::ai::{ArtifactStatus, AutonomyLevel, ProviderInfo};
use wellos_domain::diagnostics::{ReportStatus, ResultValue};

#[derive(Deserialize)]
pub struct InboundResult {
    pub service_request_id: Uuid,
    pub code_loinc: String,
    pub value: Decimal,
    pub unit: String,
    pub reference_range: Option<String>,
    pub source_system: String,
    pub idempotency_key: String,
    pub effective_at: DateTime<Utc>,
    /// When set, this delivery amends a previous observation.
    pub amends_observation_id: Option<Uuid>,
}

/// Look up the observation already stored for this tenant + idempotency key.
/// The key must refer to the same delivery: reuse with a different service
/// request or payload is a conflict, never a silent no-op.
async fn find_duplicate(
    state: &AppState,
    tenant_id: Uuid,
    body: &InboundResult,
) -> Result<Option<Value>, ApiError> {
    let row = sqlx::query(
        "SELECT id, service_request_id, code_loinc, value_num, unit, reference_range,
                source_system, effective_at, amends
         FROM observations
         WHERE tenant_id = $1 AND idempotency_key = $2",
    )
    .bind(tenant_id)
    .bind(&body.idempotency_key)
    .fetch_optional(&state.pool)
    .await?;
    let Some(r) = row else { return Ok(None) };
    let same_delivery = r.get::<Uuid, _>("service_request_id") == body.service_request_id
        && r.get::<String, _>("code_loinc") == body.code_loinc
        && r.get::<Option<Decimal>, _>("value_num") == Some(body.value)
        && r.get::<String, _>("unit") == body.unit
        && r.get::<Option<String>, _>("reference_range") == body.reference_range
        && r.get::<String, _>("source_system") == body.source_system
        && r.get::<DateTime<Utc>, _>("effective_at") == body.effective_at
        && r.get::<Option<Uuid>, _>("amends") == body.amends_observation_id;
    if !same_delivery {
        return Err(ApiError::conflict(
            "idempotency_key_reuse",
            "idempotency_key was already used for a different delivery",
        ));
    }
    Ok(Some(json!({
        "observation_id": r.get::<Uuid, _>("id"),
        "duplicate": true
    })))
}

pub async fn ingest_result(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(mut body): Json<InboundResult>,
) -> Result<Json<Value>, ApiError> {
    // Normalize to Postgres timestamp precision so stored and delivered
    // effective times compare exactly.
    body.effective_at = body
        .effective_at
        .duration_round(chrono::Duration::microseconds(1))
        .map_err(|_| ApiError::bad_request("validation_failed", "effective_at out of range"))?;
    if body.idempotency_key.trim().is_empty() {
        return Err(ApiError::bad_request(
            "validation_failed",
            "idempotency_key is required",
        ));
    }
    for (field, value, max) in [
        ("code_loinc", body.code_loinc.as_str(), 32),
        ("unit", body.unit.as_str(), 64),
        ("source_system", body.source_system.as_str(), 128),
        ("idempotency_key", body.idempotency_key.as_str(), 128),
        (
            "reference_range",
            body.reference_range.as_deref().unwrap_or(""),
            128,
        ),
    ] {
        if value.len() > max {
            return Err(ApiError::bad_request(
                "validation_failed",
                format!("{field} exceeds {max} characters"),
            ));
        }
    }
    let mut conn = state.pool.acquire().await?;
    let o = load_order(&mut conn, ctx.tenant_id, body.service_request_id).await?;
    drop(conn);
    if o.code_loinc.as_deref() != Some(body.code_loinc.as_str()) {
        return Err(ApiError::bad_request(
            "code_mismatch",
            "result code_loinc does not match the ordered test",
        ));
    }
    let tenant_id = o.tenant_id;
    let patient_id = o.patient_id;
    let allowed = guard(
        &state,
        &ctx,
        actions::RESULT_INGEST,
        "observation",
        Some(ResourceCtx {
            tenant_id,
            patient_id: Some(patient_id),
            facility_id: Some(o.patient_facility_id),
        }),
    )
    .await?;

    // Idempotency: same key -> return the existing observation, create nothing.
    if let Some(dup) = find_duplicate(&state, tenant_id, &body).await? {
        return Ok(Json(dup));
    }

    let is_amendment = body.amends_observation_id.is_some();
    let mut tx = state.pool.begin().await?;
    let o = lock_order(&mut tx, tenant_id, o.id).await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;

    // One quantity component per delivery, issued through the generalized
    // report path: typed observation, deterministic rules and criticality,
    // alerts/tasks, order and result-loop transitions all happen there.
    let input = IssueInput {
        status: if is_amendment {
            ReportStatus::Corrected
        } else {
            ReportStatus::Final
        },
        components: vec![ComponentInput {
            code: body.code_loinc.clone(),
            system: Some("http://loinc.org".to_string()),
            display: None,
            value: ResultValue::Quantity {
                value: body.value,
                unit: body.unit.clone(),
            },
            reference_range: body.reference_range.clone(),
            effective_at: Some(body.effective_at),
            amends_observation_id: body.amends_observation_id,
        }],
        conclusion: None,
        conclusion_codes: Vec::new(),
        change_reason: is_amendment.then(|| "corrected laboratory delivery".to_string()),
        idempotency_key: body.idempotency_key.clone(),
        source_system: body.source_system.clone(),
        effective_at: Some(body.effective_at),
        external_report_id: None,
        sign: true,
        performer_id: None,
        legacy_observation_key: true,
    };
    let out = match issue_in(&mut tx, &ctx, &state, &o, input).await {
        Ok(out) => out,
        // A concurrent delivery with the same idempotency key won the race:
        // discard this attempt and return the winner's observation.
        Err(e) if e.code == "report_conflict" => {
            tx.rollback().await?;
            if let Some(dup) = find_duplicate(&state, tenant_id, &body).await? {
                return Ok(Json(dup));
            }
            return Err(e);
        }
        Err(e) => return Err(e),
    };
    if out.duplicate {
        tx.rollback().await?;
        return Ok(Json(json!({
            "observation_id": out.observation_ids.first(),
            "duplicate": true
        })));
    }
    let obs_id = *out
        .observation_ids
        .first()
        .ok_or_else(|| ApiError::internal("report issued without an observation"))?;
    let critical = out.critical;
    let unit_mismatch = out.unit_mismatch;

    // AI artifact request is recorded transactionally; generation happens
    // after commit so a slow/failed model never holds the clinical
    // transaction open.
    let artifact_id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO ai_artifacts
         (id, tenant_id, patient_id, service_request_id, observation_id, artifact_type,
          autonomy_level, status, output_schema)
         VALUES ($1,$2,$3,$4,$5,'result_summary',$6,$7,'result-summary.v1')",
    )
    .bind(artifact_id)
    .bind(tenant_id)
    .bind(patient_id)
    .bind(body.service_request_id)
    .bind(obs_id)
    .bind(
        serde_json::to_value(AutonomyLevel::A2)
            .map_err(ApiError::internal)?
            .as_str()
            .map(|s| s.to_string()),
    )
    .bind(ArtifactStatus::Draft.as_str())
    .execute(&mut *tx)
    .await?;
    audit::emit(
        &mut *tx,
        &ctx,
        "ai.artifact.requested",
        &state.cell,
        json!({ "artifact_id": artifact_id, "observation_id": obs_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;

    tx.commit().await?;

    // Post-commit AI generation (asynchronous relative to the clinical
    // record). The clinical result is already committed, so generation
    // failures never fail the ingestion response; the artifact stays in a
    // recoverable non-final state instead.
    if let Err(e) = generate_summary(
        &state,
        &ctx,
        artifact_id,
        tenant_id,
        patient_id,
        obs_id,
        &body,
        critical,
        unit_mismatch,
    )
    .await
    {
        tracing::warn!(%artifact_id, error = ?e, "post-commit ai summary generation failed");
        // Internal failure, not a provider outage: invalidate with its audit
        // record atomically. If persistence itself is unavailable, the draft
        // stays recoverable and only the operational failure is logged.
        if let Err(mark) = mark_generation_failed(&state, &ctx, artifact_id, "internal_error").await
        {
            tracing::warn!(%artifact_id, error = ?mark, "failed to invalidate draft artifact");
        }
    }

    Ok(Json(json!({
        "observation_id": obs_id,
        "diagnostic_report_id": out.report.id,
        "critical": critical,
        "unit_mismatch": unit_mismatch,
        "ai_artifact_id": artifact_id,
        "loop_state": out.order.loop_state,
        "order_status": out.order.order_status.as_str(),
        "duplicate": false
    })))
}

#[allow(clippy::too_many_arguments)]
async fn generate_summary(
    state: &AppState,
    ctx: &AuthContext,
    artifact_id: Uuid,
    tenant_id: Uuid,
    patient_id: Uuid,
    obs_id: Uuid,
    body: &InboundResult,
    critical: bool,
    unit_mismatch: bool,
) -> Result<(), ApiError> {
    let mut facts = vec![(
        format!("observation:{obs_id}"),
        format!(
            "{} result {} {} (reference range {})",
            body.code_loinc,
            body.value,
            body.unit,
            body.reference_range.as_deref().unwrap_or("not provided")
        ),
    )];
    if critical {
        facts.push((
            format!("rule_evaluation:observation:{obs_id}"),
            "Deterministic rule flagged this result as CRITICAL".to_string(),
        ));
    }
    if unit_mismatch {
        facts.push((
            format!("data_quality:observation:{obs_id}"),
            "Unit could not be safely normalized; deterministic evaluation refused".to_string(),
        ));
    }
    let req = SummaryRequest {
        template: "result-summary@1.0.0".into(),
        facts,
        language: "en".into(),
    };
    let input_refs: Vec<String> = req.facts.iter().map(|(r, _)| r.clone()).collect();
    let hash = dmind_gateway::input_hash(&req);

    // Governance first: capability, external-processing consent, artifact
    // reuse and quota. A refusal leaves the draft visibly unavailable rather
    // than fabricating a summary.
    let scope = aigov::ReuseScope::ResultSummary {
        observation_id: obs_id,
    };
    let plan = match aigov::plan(
        state,
        tenant_id,
        patient_id,
        scope,
        &hash,
        "result-summary.v1",
    )
    .await
    {
        Ok(plan) => plan,
        Err(refusal) => {
            mark_generation_unavailable(state, ctx, artifact_id, refusal.code).await?;
            return Ok(());
        }
    };
    let synthetic = plan.model_synthetic();
    let (outcome, reused_from, execution_id) = match plan {
        aigov::ExecutionPlan::Reuse(prior) => {
            let output: wellos_domain::ai::ResultSummaryV1 = prior.output_as()?;
            let resp = dmind_gateway::GatewayResponse {
                output,
                model: prior.model.clone(),
                model_version: prior.model_version.clone(),
                route: prior.route.clone(),
                prompt_version: prior.prompt_version.clone(),
                input_hash: hash.clone(),
                usage: prior.usage_as(),
            };
            (Ok(resp), Some(prior.id), None)
        }
        aigov::ExecutionPlan::Execute { execution_id, .. } => (
            state.gateway.summarize_result(&req).await,
            None,
            Some(execution_id),
        ),
    };

    // Each lifecycle transition and its outbox event commit atomically, and
    // only apply while the artifact is still a draft (a concurrent amendment
    // may already have superseded it).
    match outcome {
        Ok(resp) => {
            let mut tx = state.pool.begin().await?;
            let updated = sqlx::query(
                "UPDATE ai_artifacts SET status=$1, model=$2, model_version=$3, route=$4,
                 template=$5, input_hash=$6, output=$7, citations=$8, limitations=$9, generated_at=now()
                 WHERE id=$10 AND status=$11",
            )
            .bind(ArtifactStatus::AwaitingReview.as_str())
            .bind(&resp.model)
            .bind(&resp.model_version)
            .bind(&resp.route)
            .bind(&req.template)
            .bind(&resp.input_hash)
            .bind(serde_json::to_value(&resp.output).map_err(ApiError::internal)?)
            .bind(serde_json::to_value(&resp.output.cited_sources).map_err(ApiError::internal)?)
            .bind(serde_json::to_value(&resp.output.limitations).map_err(ApiError::internal)?)
            .bind(artifact_id)
            .bind(ArtifactStatus::Draft.as_str())
            .execute(&mut *tx)
            .await?;
            if updated.rows_affected() > 0 {
                let provider = ProviderInfo {
                    provider: resp.route.clone(),
                    model: resp.model.clone(),
                    model_version: resp.model_version.clone(),
                };
                aigov::annotate(
                    &mut tx,
                    artifact_id,
                    &aigov::Provenance {
                        scope,
                        provider: &provider,
                        prompt_version: &resp.prompt_version,
                        input_refs: &input_refs,
                        usage: resp.usage.as_ref(),
                        synthetic,
                        reused_from,
                    },
                )
                .await?;
                if let Some(execution_id) = execution_id {
                    aigov::bind_execution(&mut tx, execution_id, artifact_id).await?;
                }
                audit::emit(
                    &mut *tx,
                    ctx,
                    "ai.artifact.generated",
                    &state.cell,
                    json!({
                        "artifact_id": artifact_id,
                        "reused_from": reused_from,
                        "synthetic": synthetic,
                    }),
                    None,
                )
                .await
                .map_err(ApiError::internal)?;
            }
            tx.commit().await?;
        }
        Err(GatewayError::Unavailable(_)) | Err(GatewayError::Disabled(_)) => {
            // Care continues; the artifact visibly reports unavailability.
            mark_generation_unavailable(state, ctx, artifact_id, "provider_unavailable").await?;
        }
        Err(GatewayError::InvalidOutput(_)) => {
            tracing::warn!(%artifact_id, "model output rejected by schema validation");
            mark_generation_failed(state, ctx, artifact_id, "invalid_output").await?;
        }
        Err(GatewayError::PolicyDenied(_)) => {
            mark_generation_failed(state, ctx, artifact_id, "policy_denied").await?;
        }
    }
    Ok(())
}

/// The provider could not be used (disabled, unavailable, consent, quota):
/// the draft becomes `unavailable` with the failure class audited. No
/// provider content or secret is ever recorded.
async fn mark_generation_unavailable(
    state: &AppState,
    ctx: &AuthContext,
    artifact_id: Uuid,
    reason: &str,
) -> Result<(), ApiError> {
    let mut tx = state.pool.begin().await?;
    let updated = sqlx::query("UPDATE ai_artifacts SET status=$1 WHERE id=$2 AND status=$3")
        .bind(ArtifactStatus::Unavailable.as_str())
        .bind(artifact_id)
        .bind(ArtifactStatus::Draft.as_str())
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() > 0 {
        audit::emit(
            &mut *tx,
            ctx,
            "ai.provider.unavailable",
            &state.cell,
            json!({ "artifact_id": artifact_id, "reason": reason }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
    }
    tx.commit().await?;
    Ok(())
}

/// Terminal path for generation failures that are not a provider outage:
/// the draft becomes `invalidated`, with the lifecycle audit record written
/// in the same transaction.
async fn mark_generation_failed(
    state: &AppState,
    ctx: &AuthContext,
    artifact_id: Uuid,
    reason: &str,
) -> Result<(), ApiError> {
    let mut tx = state.pool.begin().await?;
    let updated = sqlx::query("UPDATE ai_artifacts SET status=$1 WHERE id=$2 AND status=$3")
        .bind(ArtifactStatus::Invalidated.as_str())
        .bind(artifact_id)
        .bind(ArtifactStatus::Draft.as_str())
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() > 0 {
        audit::emit(
            &mut *tx,
            ctx,
            "ai.generation.failed",
            &state.cell,
            json!({ "artifact_id": artifact_id, "reason": reason }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
    }
    tx.commit().await?;
    Ok(())
}
