//! Persistence for evaluation runs. Writes touch only the `dmind_eval_*`
//! tables (append-only; see migration 0018) and never the clinical AI
//! artifact, note, triage, risk, scheduling, diagnostic or notification
//! tables. Every row is bound to a synthetic tenant, enforced here and again
//! by a database trigger.

use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::{PgPool, Postgres, Row, Transaction};
use uuid::Uuid;

use super::corpus::Corpus;
use super::rubric::CaseStatus;
use super::runner::{CachedExecution, Mode, RunOutcome, RunPlan};
use crate::error::ApiError;

pub async fn require_synthetic_tenant(pool: &PgPool, tenant_id: Uuid) -> Result<(), ApiError> {
    let row: Option<(String,)> = sqlx::query_as("SELECT data_class FROM tenants WHERE id = $1")
        .bind(tenant_id)
        .fetch_optional(pool)
        .await?;
    match row {
        Some((class,)) if class == "synthetic" => Ok(()),
        Some((class,)) => Err(ApiError::forbidden(format!(
            "evaluation refuses tenant with data_class {class}"
        ))),
        None => Err(ApiError::not_found()),
    }
}

/// Find or append the suite row for this exact corpus (key, version, rubric,
/// hash). Cases are appended alongside a new suite row.
pub async fn ensure_suite(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    corpus: &Corpus,
    rubric_version: &str,
) -> Result<Uuid, ApiError> {
    let hash = corpus.hash();
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM dmind_eval_suites
         WHERE tenant_id = $1 AND suite_key = $2 AND suite_version = $3
           AND rubric_version = $4 AND corpus_hash = $5",
    )
    .bind(tenant_id)
    .bind(corpus.suite_key)
    .bind(corpus.version)
    .bind(rubric_version)
    .bind(&hash)
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((id,)) = existing {
        return Ok(id);
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO dmind_eval_suites
         (id, tenant_id, suite_key, suite_version, rubric_version, languages, case_count, corpus_hash)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(corpus.suite_key)
    .bind(corpus.version)
    .bind(rubric_version)
    .bind(corpus.languages())
    .bind(corpus.cases.len() as i32)
    .bind(&hash)
    .execute(&mut **tx)
    .await?;
    for case in &corpus.cases {
        sqlx::query(
            "INSERT INTO dmind_eval_cases
             (id, tenant_id, suite_id, case_key, operation, category, language, title,
              input_hash, expected_schema, allowed_evidence, expectations, synthetic)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, true)",
        )
        .bind(Uuid::now_v7())
        .bind(tenant_id)
        .bind(id)
        .bind(&case.key)
        .bind(case.operation.as_str())
        .bind(case.category.as_str())
        .bind(&case.language)
        .bind(&case.title)
        .bind(case.input_hash())
        .bind(case.operation.output_schema())
        .bind(Value::from(case.allowed_evidence()))
        .bind(serde_json::to_value(&case.expectations).map_err(ApiError::internal)?)
        .execute(&mut **tx)
        .await?;
    }
    Ok(id)
}

pub async fn ensure_fingerprint(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    fp: &super::runner::ProviderFingerprint,
    pricing: Option<&super::runner::Pricing>,
) -> Result<Uuid, ApiError> {
    let pricing_json = pricing
        .map(|p| json!({ "currency": p.currency, "input_per_1k_tokens": p.input_per_1k_tokens, "output_per_1k_tokens": p.output_per_1k_tokens }));
    let existing: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM dmind_eval_provider_fingerprints
         WHERE tenant_id = $1 AND config_hash = $2 AND pricing IS NOT DISTINCT FROM $3
           AND pricing_effective_date IS NOT DISTINCT FROM $4",
    )
    .bind(tenant_id)
    .bind(&fp.config_hash)
    .bind(&pricing_json)
    .bind(pricing.map(|p| p.effective_date))
    .fetch_optional(&mut **tx)
    .await?;
    if let Some((id,)) = existing {
        return Ok(id);
    }
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO dmind_eval_provider_fingerprints
         (id, tenant_id, provider, model, model_version, external, synthetic_provider, config_hash,
          pricing, pricing_effective_date)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(&fp.provider)
    .bind(&fp.model)
    .bind(&fp.model_version)
    .bind(fp.external)
    .bind(fp.synthetic_provider)
    .bind(&fp.config_hash)
    .bind(&pricing_json)
    .bind(pricing.map(|p| p.effective_date))
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

/// Append a planned run. `gate` is the evaluated live gate (or `{}`).
#[allow(clippy::too_many_arguments)]
pub async fn insert_run(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    suite_id: Uuid,
    fingerprint_id: Uuid,
    plan: &RunPlan,
    environment: &str,
    requested_by: Option<Uuid>,
    requested_via: &str,
    gate: Value,
) -> Result<Uuid, ApiError> {
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO dmind_eval_runs
         (id, tenant_id, suite_id, fingerprint_id, mode, environment, data_class, requested_by,
          requested_via, operations, budget, gate, stop_on_high_severity, status, version)
         VALUES ($1, $2, $3, $4, $5, $6, 'synthetic', $7, $8, $9, $10, $11, $12, 'planned', 1)",
    )
    .bind(id)
    .bind(tenant_id)
    .bind(suite_id)
    .bind(fingerprint_id)
    .bind(plan.mode.as_str())
    .bind(environment)
    .bind(requested_by)
    .bind(requested_via)
    .bind(Value::from(plan.operations.clone()))
    .bind(serde_json::to_value(&plan.budget).map_err(ApiError::internal)?)
    .bind(gate)
    .bind(plan.stop_on_high_severity)
    .execute(&mut **tx)
    .await?;
    Ok(id)
}

/// Versioned state transition (`planned → running → completed|failed|aborted`).
pub async fn transition_run(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    run_id: Uuid,
    expected_version: i32,
    status: &str,
    abort_reason: Option<&str>,
) -> Result<i32, ApiError> {
    let next = expected_version + 1;
    let updated = sqlx::query(
        "UPDATE dmind_eval_runs
         SET status = $1, version = $2, abort_reason = COALESCE($3, abort_reason),
             started_at = CASE WHEN $1 = 'running' THEN now() ELSE started_at END,
             finished_at = CASE WHEN $1 IN ('completed','failed','aborted') THEN now() ELSE finished_at END
         WHERE id = $4 AND tenant_id = $5 AND version = $6",
    )
    .bind(status)
    .bind(next)
    .bind(abort_reason)
    .bind(run_id)
    .bind(tenant_id)
    .bind(expected_version)
    .execute(&mut **tx)
    .await?;
    if updated.rows_affected() == 0 {
        return Err(ApiError::conflict(
            "run_version_conflict",
            "evaluation run changed concurrently",
        ));
    }
    Ok(next)
}

/// Append every execution, its metrics and the run summary.
pub async fn insert_outcome(
    tx: &mut Transaction<'_, Postgres>,
    tenant_id: Uuid,
    run_id: Uuid,
    suite_id: Uuid,
    outcome: &RunOutcome,
) -> Result<(), ApiError> {
    let case_ids: Vec<(String, Uuid)> =
        sqlx::query("SELECT case_key, id FROM dmind_eval_cases WHERE suite_id = $1")
            .bind(suite_id)
            .fetch_all(&mut **tx)
            .await?
            .iter()
            .map(|r| (r.get("case_key"), r.get("id")))
            .collect();
    for e in &outcome.executions {
        let case_id = case_ids
            .iter()
            .find(|(k, _)| k == &e.case_key)
            .map(|(_, id)| *id)
            .ok_or_else(|| ApiError::internal(format!("case {} not persisted", e.case_key)))?;
        let exec_id = Uuid::now_v7();
        sqlx::query(
            "INSERT INTO dmind_eval_executions
             (id, tenant_id, run_id, case_id, operation, language, prompt_version, schema_version,
              provider, model, model_version, input_hash, evidence_refs, baseline_hash,
              baseline_output, provider_output, execution_kind, retry_count, degraded,
              fallback_verified, status, failure_code, latency_ms, input_tokens, output_tokens,
              estimated_cost, currency, started_at, finished_at)
             VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$16,$17,$18,$19,$20,$21,$22,
                     $23,$24,$25,$26,$27, now(), now())",
        )
        .bind(exec_id)
        .bind(tenant_id)
        .bind(run_id)
        .bind(case_id)
        .bind(e.operation)
        .bind(&e.language)
        .bind(&e.prompt_version)
        .bind(e.schema_version)
        .bind(&e.provider)
        .bind(&e.model)
        .bind(&e.model_version)
        .bind(&e.input_hash)
        .bind(Value::from(e.evidence_refs.clone()))
        .bind(&e.baseline_hash)
        .bind(&e.baseline_output)
        .bind(&e.provider_output)
        .bind(e.execution_kind.as_str())
        .bind(e.retry_count as i32)
        .bind(e.degraded)
        .bind(e.fallback_verified)
        .bind(status_str(e.status))
        .bind(&e.failure_code)
        .bind(e.latency_ms.map(|v| v as i32))
        .bind(e.input_tokens.map(|v| v as i64))
        .bind(e.output_tokens.map(|v| v as i64))
        .bind(e.estimated_cost)
        .bind(
            outcome
                .plan
                .budget
                .pricing
                .as_ref()
                .map(|p| p.currency.clone()),
        )
        .execute(&mut **tx)
        .await?;
        for m in &e.verdict.metrics {
            sqlx::query(
                "INSERT INTO dmind_eval_metrics
                 (id, tenant_id, execution_id, metric, verification, severity, outcome, score, detail)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(Uuid::now_v7())
            .bind(tenant_id)
            .bind(exec_id)
            .bind(m.metric)
            .bind(snake(&m.verification)?)
            .bind(snake(&m.severity)?)
            .bind(snake(&m.outcome)?)
            .bind(m.score.and_then(|s| Decimal::try_from(s).ok()))
            .bind(&m.detail)
            .execute(&mut **tx)
            .await?;
        }
    }
    let summary = super::report::summarize(outcome);
    let t = &outcome.totals;
    sqlx::query(
        "INSERT INTO dmind_eval_run_summaries
         (id, tenant_id, run_id, totals, operations, languages, safety_failures, calls,
          input_tokens, output_tokens, estimated_cost, currency, p50_latency_ms, p95_latency_ms)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13, $14)",
    )
    .bind(Uuid::now_v7())
    .bind(tenant_id)
    .bind(run_id)
    .bind(serde_json::to_value(t).map_err(ApiError::internal)?)
    .bind(serde_json::to_value(&summary.by_operation).map_err(ApiError::internal)?)
    .bind(serde_json::to_value(&summary.by_language).map_err(ApiError::internal)?)
    .bind(serde_json::to_value(&outcome.safety_failures).map_err(ApiError::internal)?)
    .bind(t.provider_calls as i32)
    .bind(t.input_tokens as i64)
    .bind(t.output_tokens as i64)
    .bind(t.estimated_cost)
    .bind(
        outcome
            .plan
            .budget
            .pricing
            .as_ref()
            .map(|p| p.currency.clone()),
    )
    .bind(t.p50_latency_ms.map(|v| v as i32))
    .bind(t.p95_latency_ms.map(|v| v as i32))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

fn snake<T: serde::Serialize>(v: &T) -> Result<String, ApiError> {
    serde_json::to_value(v)
        .map_err(ApiError::internal)?
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| ApiError::internal("enum did not serialize to a string"))
}

/// Terminal run state and abort reason recorded for an outcome.
pub fn run_status(outcome: &RunOutcome) -> (&'static str, Option<String>) {
    match (&outcome.abort, outcome.plan.mode) {
        (_, Mode::DryRun) => ("aborted", Some("dry_run".to_string())),
        (Some(a), _) => ("aborted", Some(format!("{a:?}"))),
        (None, _) if outcome.totals.failed > 0 => ("failed", None),
        (None, _) => ("completed", None),
    }
}

pub fn status_str(s: CaseStatus) -> &'static str {
    match s {
        CaseStatus::Passed => "passed",
        CaseStatus::NeedsReview => "needs_review",
        CaseStatus::Failed => "failed",
        CaseStatus::Aborted => "aborted",
        CaseStatus::ProviderUnavailableFallbackVerified => "provider_unavailable_fallback_verified",
    }
}

/// Prior executions with identical provenance that a new run may reuse
/// instead of calling the provider (only successful or terminal results).
pub async fn cached_executions(
    pool: &PgPool,
    tenant_id: Uuid,
    config_hash: &str,
) -> Result<Vec<CachedExecution>, ApiError> {
    let rows = sqlx::query(
        "SELECT e.input_hash, e.prompt_version, e.provider, e.model, e.model_version,
                e.provider_output, e.failure_code
         FROM dmind_eval_executions e
         JOIN dmind_eval_runs r ON r.id = e.run_id
         JOIN dmind_eval_provider_fingerprints f ON f.id = r.fingerprint_id
         WHERE e.tenant_id = $1 AND f.config_hash = $2
           AND e.execution_kind IN ('fresh','retried')
           AND e.status IN ('passed','needs_review','failed')
         ORDER BY e.started_at DESC LIMIT 2000",
    )
    .bind(tenant_id)
    .bind(config_hash)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .iter()
        .map(|r| CachedExecution {
            input_hash: r.get("input_hash"),
            prompt_version: r.get("prompt_version"),
            provider: r.get("provider"),
            model: r.get("model"),
            model_version: r.get("model_version"),
            provider_output: r.get("provider_output"),
            failure_code: r.get("failure_code"),
        })
        .collect())
}

/// Persist a complete run in one transaction: suite, fingerprint, run
/// (planned → running → terminal), executions, metrics and summary.
#[allow(clippy::too_many_arguments)]
pub async fn persist_run(
    pool: &PgPool,
    tenant_id: Uuid,
    corpus: &Corpus,
    outcome: &RunOutcome,
    environment: &str,
    requested_by: Option<Uuid>,
    requested_via: &str,
    gate: Value,
) -> Result<Uuid, ApiError> {
    require_synthetic_tenant(pool, tenant_id).await?;
    let mut tx = pool.begin().await?;
    let suite_id = ensure_suite(&mut tx, tenant_id, corpus, outcome.plan.rubric_version).await?;
    let fp_id = ensure_fingerprint(
        &mut tx,
        tenant_id,
        &outcome.provider,
        outcome.plan.budget.pricing.as_ref(),
    )
    .await?;
    let run_id = insert_run(
        &mut tx,
        tenant_id,
        suite_id,
        fp_id,
        &outcome.plan,
        environment,
        requested_by,
        requested_via,
        gate,
    )
    .await?;
    let v = if outcome.plan.mode == Mode::DryRun {
        1
    } else {
        transition_run(&mut tx, tenant_id, run_id, 1, "running", None).await?
    };
    insert_outcome(&mut tx, tenant_id, run_id, suite_id, outcome).await?;
    let (terminal, reason) = run_status(outcome);
    transition_run(&mut tx, tenant_id, run_id, v, terminal, reason.as_deref()).await?;
    tx.commit().await?;
    Ok(run_id)
}
