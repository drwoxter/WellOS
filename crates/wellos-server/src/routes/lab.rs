//! Synthetic laboratory adapter boundary.
//!
//! Inbound results are idempotent (duplicate deliveries with the same
//! idempotency key create nothing new). The deterministic critical rule runs
//! in the ingestion transaction; AI summarization happens after commit and can
//! fail without affecting the clinical record.

use crate::audit;
use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::policy::{actions, ResourceCtx};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::State;
use axum::Json;
use chrono::{DateTime, Utc};
use dmind_gateway::{GatewayError, SummaryRequest};
use rust_decimal::Decimal;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;
use wellos_domain::ai::{ArtifactStatus, AutonomyLevel};
use wellos_domain::result_loop::{LoopState, LoopTransition};
use wellos_domain::rules::{baseline_rules, RuleOutcome};
use wellos_domain::units::Quantity;

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

pub async fn ingest_result(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<InboundResult>,
) -> Result<Json<Value>, ApiError> {
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
    let sr = sqlx::query(
        "SELECT tenant_id, patient_id, code_loinc, loop_state, version FROM service_requests WHERE id = $1",
    )
    .bind(body.service_request_id)
    .fetch_optional(&state.pool)
    .await?
    .ok_or_else(ApiError::not_found)?;
    let tenant_id: Uuid = sr.get("tenant_id");
    let patient_id: Uuid = sr.get("patient_id");
    let ordered_code: String = sr.get("code_loinc");
    if body.code_loinc != ordered_code {
        return Err(ApiError::bad_request(
            "code_mismatch",
            "result code_loinc does not match the ordered test",
        ));
    }
    let loop_state = LoopState::parse(sr.get::<String, _>("loop_state").as_str())
        .ok_or_else(|| ApiError::internal("invalid loop state in database"))?;

    let allowed = guard(
        &state,
        &ctx,
        actions::RESULT_INGEST,
        "observation",
        Some(ResourceCtx {
            tenant_id,
            patient_id: Some(patient_id),
        }),
    )
    .await?;

    // Idempotency: same key -> return the existing observation, create nothing.
    let existing: Option<(Uuid,)> =
        sqlx::query_as("SELECT id FROM observations WHERE tenant_id = $1 AND idempotency_key = $2")
            .bind(tenant_id)
            .bind(&body.idempotency_key)
            .fetch_optional(&state.pool)
            .await?;
    if let Some((obs_id,)) = existing {
        return Ok(Json(json!({
            "observation_id": obs_id,
            "duplicate": true
        })));
    }

    let is_amendment = body.amends_observation_id.is_some();
    let transition = if is_amendment {
        LoopTransition::ResultAmended
    } else {
        LoopTransition::ResultReceived
    };
    let next_state = loop_state
        .apply(transition)
        .map_err(|e| ApiError::conflict("invalid_loop_transition", e.to_string()))?;

    let obs_id = Uuid::now_v7();
    let mut tx = state.pool.begin().await?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;

    if let Some(amended_id) = body.amends_observation_id {
        // Preserve the previous version; never overwrite clinical history.
        let updated = sqlx::query(
            "UPDATE observations SET status = 'amended-superseded'
             WHERE id = $1 AND tenant_id = $2 AND service_request_id = $3",
        )
        .bind(amended_id)
        .bind(tenant_id)
        .bind(body.service_request_id)
        .execute(&mut *tx)
        .await?;
        if updated.rows_affected() == 0 {
            return Err(ApiError::bad_request(
                "unknown_amended_observation",
                "amends_observation_id does not match an observation of this request",
            ));
        }
        // Summaries of the superseded result must not remain reviewable;
        // already reviewed artifacts stay as historical provenance.
        sqlx::query(
            "UPDATE ai_artifacts SET status='superseded'
             WHERE tenant_id=$1 AND observation_id=$2
               AND status IN ('draft','awaiting_review','unavailable')",
        )
        .bind(tenant_id)
        .bind(amended_id)
        .execute(&mut *tx)
        .await?;
    }

    let inserted = sqlx::query(
        "INSERT INTO observations
         (id, tenant_id, service_request_id, patient_id, code_loinc, value_num, unit,
          reference_range, status, amends, source_system, idempotency_key, effective_at)
         VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13)",
    )
    .bind(obs_id)
    .bind(tenant_id)
    .bind(body.service_request_id)
    .bind(patient_id)
    .bind(&body.code_loinc)
    .bind(body.value)
    .bind(&body.unit)
    .bind(&body.reference_range)
    .bind(if is_amendment { "corrected" } else { "final" })
    .bind(body.amends_observation_id)
    .bind(&body.source_system)
    .bind(&body.idempotency_key)
    .bind(body.effective_at)
    .execute(&mut *tx)
    .await;
    if let Err(e) = inserted {
        // A concurrent delivery with the same idempotency key won the race:
        // discard this attempt and return the winner's observation.
        if matches!(&e, sqlx::Error::Database(db) if db.is_unique_violation()) {
            tx.rollback().await?;
            let (winner,): (Uuid,) = sqlx::query_as(
                "SELECT id FROM observations WHERE tenant_id = $1 AND idempotency_key = $2",
            )
            .bind(tenant_id)
            .bind(&body.idempotency_key)
            .fetch_one(&state.pool)
            .await?;
            return Ok(Json(json!({
                "observation_id": winner,
                "duplicate": true
            })));
        }
        return Err(e.into());
    }

    // Loop transition with optimistic concurrency on the service request.
    let version: i64 = sr.get("version");
    let updated = sqlx::query(
        "UPDATE service_requests SET loop_state = $1, version = version + 1
         WHERE id = $2 AND version = $3",
    )
    .bind(next_state.as_str())
    .bind(body.service_request_id)
    .bind(version)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "version_conflict",
            "service request was modified concurrently",
        ));
    }

    audit::emit(
        &mut *tx,
        &ctx,
        if is_amendment {
            "result.amended"
        } else {
            "result.received"
        },
        &state.cell,
        json!({ "observation_id": obs_id, "service_request_id": body.service_request_id }),
        None,
    )
    .await
    .map_err(ApiError::internal)?;

    // Deterministic critical evaluation — never depends on AI availability.
    let observed = Quantity {
        value: body.value,
        unit: body.unit.clone(),
    };
    let mut critical = false;
    let mut unit_mismatch = false;
    for rule in baseline_rules() {
        let outcome = rule.evaluate(&body.code_loinc, &observed);
        if matches!(outcome, RuleOutcome::NotApplicable) {
            continue;
        }
        sqlx::query(
            "INSERT INTO rule_evaluations (id, tenant_id, observation_id, rule_id, rule_version, outcome)
             VALUES ($1,$2,$3,$4,$5,$6)",
        )
        .bind(Uuid::now_v7())
        .bind(tenant_id)
        .bind(obs_id)
        .bind(&rule.rule_id)
        .bind(&rule.version)
        .bind(serde_json::to_value(&outcome).map_err(ApiError::internal)?)
        .execute(&mut *tx)
        .await?;
        match outcome {
            RuleOutcome::Critical { .. } => critical = true,
            RuleOutcome::UnitMismatch { reason } => {
                unit_mismatch = true;
                // Unsafe comparison refused: record a data-quality issue
                // instead of silently applying a threshold.
                sqlx::query(
                    "INSERT INTO data_quality_issues (id, tenant_id, resource_type, resource_id, issue)
                     VALUES ($1,$2,'observation',$3,$4)",
                )
                .bind(Uuid::now_v7())
                .bind(tenant_id)
                .bind(obs_id)
                .bind(&reason)
                .execute(&mut *tx)
                .await?;
            }
            _ => {}
        }
    }

    if critical {
        let alert_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO alerts (id, tenant_id, patient_id, observation_id, severity, message)
             VALUES ($1,$2,$3,$4,'critical','Critical laboratory result requires review')",
        )
        .bind(alert_id)
        .bind(tenant_id)
        .bind(patient_id)
        .bind(obs_id)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "INSERT INTO follow_up_tasks (id, tenant_id, patient_id, service_request_id, description, priority, due_at)
             VALUES ($1,$2,$3,$4,'Review critical laboratory result and document follow-up','high', now() + interval '1 hour')",
        )
        .bind(Uuid::now_v7())
        .bind(tenant_id)
        .bind(patient_id)
        .bind(body.service_request_id)
        .execute(&mut *tx)
        .await?;
        audit::emit(
            &mut *tx,
            &ctx,
            "result.critical_flagged",
            &state.cell,
            json!({ "observation_id": obs_id, "alert_id": alert_id }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
        audit::emit(
            &mut *tx,
            &ctx,
            "follow_up.created",
            &state.cell,
            json!({ "service_request_id": body.service_request_id }),
            None,
        )
        .await
        .map_err(ApiError::internal)?;
    }

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

    // Post-commit AI generation (asynchronous relative to the clinical record).
    generate_summary(
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
    .await?;

    Ok(Json(json!({
        "observation_id": obs_id,
        "critical": critical,
        "unit_mismatch": unit_mismatch,
        "ai_artifact_id": artifact_id,
        "loop_state": next_state.as_str(),
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
    // Consent/policy gate for optional EXTERNAL processing. Local (in-cell)
    // deterministic summarization is part of care delivery; external routes
    // additionally require active consent and deployment permission.
    let external_consent: Option<(String,)> = sqlx::query_as(
        "SELECT status FROM consents
         WHERE tenant_id = $1 AND patient_id = $2 AND purpose = 'ai_external_processing'
         ORDER BY version DESC, recorded_at DESC LIMIT 1",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_optional(&state.pool)
    .await?;
    let external_allowed =
        state.allow_external_ai && matches!(external_consent, Some((ref s,)) if s == "active");
    if !external_allowed {
        audit::record(
            &state.pool,
            ctx,
            "ai.external_processing",
            Some("ai_artifact"),
            Some(artifact_id.to_string()),
            "deny",
            Some(if state.allow_external_ai {
                "consent_not_active"
            } else {
                "deployment_disallows_external"
            }),
        )
        .await
        .map_err(ApiError::internal)?;
    }

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

    match state.gateway.summarize_result(&req).await {
        Ok(resp) => {
            sqlx::query(
                "UPDATE ai_artifacts SET status=$1, model=$2, model_version=$3, route=$4,
                 template=$5, input_hash=$6, output=$7, citations=$8, limitations=$9, generated_at=now()
                 WHERE id=$10",
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
            .execute(&state.pool)
            .await?;
            audit::emit(
                &state.pool,
                ctx,
                "ai.artifact.generated",
                &state.cell,
                json!({ "artifact_id": artifact_id }),
                None,
            )
            .await
            .map_err(ApiError::internal)?;
        }
        Err(GatewayError::Unavailable(reason)) => {
            // Care continues; the artifact visibly reports unavailability.
            sqlx::query("UPDATE ai_artifacts SET status=$1 WHERE id=$2")
                .bind(ArtifactStatus::Unavailable.as_str())
                .bind(artifact_id)
                .execute(&state.pool)
                .await?;
            audit::emit(
                &state.pool,
                ctx,
                "ai.provider.unavailable",
                &state.cell,
                json!({ "artifact_id": artifact_id, "reason": reason }),
                None,
            )
            .await
            .map_err(ApiError::internal)?;
        }
        Err(other) => {
            sqlx::query("UPDATE ai_artifacts SET status=$1 WHERE id=$2")
                .bind(ArtifactStatus::Invalidated.as_str())
                .bind(artifact_id)
                .execute(&state.pool)
                .await?;
            tracing::warn!(error = %other, "ai artifact generation failed");
        }
    }
    Ok(())
}
