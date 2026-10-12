//! dMind evaluation cockpit API. Read/manage access is centrally authorized
//! (`dmind_eval.read` / `dmind_eval.manage`), every row is tenant-scoped and
//! the store refuses non-synthetic tenants. Runs created here are dry-run or
//! offline only: live provider evaluation stays in the `dmind-eval` CLI
//! behind its explicit gates. Nothing here touches clinical tables.

use crate::auth::AuthContext;
use crate::error::ApiError;
use crate::eval::{self, store, Budget, Corpus, LiveGate, Mode, Runner};
use crate::policy::{actions, ResourceCtx};
use crate::routes::guard;
use crate::state::AppState;
use axum::extract::{Path, Query, State};
use axum::Json;
use dmind_gateway::Operation;
use serde::Deserialize;
use serde_json::{json, Value};
use sqlx::Row;
use uuid::Uuid;

const MAX_API_CALLS: u32 = 120;
const REVIEW_DECISIONS: &[&str] = &[
    "accept_result",
    "flag_quality",
    "flag_safety",
    "request_revision",
];

fn can_manage(ctx: &AuthContext) -> bool {
    ctx.roles
        .iter()
        .any(|r| crate::policy::role_allows(r, actions::DMIND_EVAL_MANAGE))
}

fn tenant_ctx(ctx: &AuthContext) -> Option<ResourceCtx> {
    Some(ResourceCtx {
        tenant_id: ctx.tenant_id,
        patient_id: None,
        facility_id: None,
    })
}

async fn authorize(state: &AppState, ctx: &AuthContext, action: &str) -> Result<(), ApiError> {
    guard(state, ctx, action, "dmind_evaluation", tenant_ctx(ctx))
        .await?
        .record_on_pool(state, ctx)
        .await
}

pub async fn suites(
    State(state): State<AppState>,
    ctx: AuthContext,
) -> Result<Json<Value>, ApiError> {
    authorize(&state, &ctx, actions::DMIND_EVAL_READ).await?;
    let corpus = Corpus::load().map_err(ApiError::internal)?;
    let registry: Vec<Value> = eval::registry()
        .iter()
        .map(|op| {
            json!({
                "operation": op.key,
                "output_schema": op.output_schema,
                "deterministic_baseline": op.deterministic_baseline,
                "cases": corpus.cases.iter().filter(|c| c.operation.as_str() == op.key).count(),
            })
        })
        .collect();
    let rows = sqlx::query(
        "SELECT id, suite_key, suite_version, rubric_version, languages, case_count, corpus_hash, created_at
         FROM dmind_eval_suites WHERE tenant_id = $1 ORDER BY created_at DESC",
    )
    .bind(ctx.tenant_id)
    .fetch_all(&state.pool)
    .await?;
    let recorded: Vec<Value> = rows
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid,_>("id"),
                "suite_key": r.get::<String,_>("suite_key"),
                "suite_version": r.get::<String,_>("suite_version"),
                "rubric_version": r.get::<String,_>("rubric_version"),
                "languages": r.get::<Vec<String>,_>("languages"),
                "case_count": r.get::<i32,_>("case_count"),
                "corpus_hash": r.get::<String,_>("corpus_hash"),
                "created_at": r.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
            })
        })
        .collect();
    Ok(Json(json!({
        "compiled": {
            "suite_key": corpus.suite_key,
            "suite_version": corpus.version,
            "rubric_version": eval::rubric::RUBRIC_VERSION,
            "case_count": corpus.cases.len(),
            "corpus_hash": corpus.hash(),
            "languages": ["en", "es"],
            "operations": registry,
        },
        "recorded": recorded,
        "synthetic_tenant": store::require_synthetic_tenant(&state.pool, ctx.tenant_id).await.is_ok(),
        "can_manage": can_manage(&ctx),
        "live_mode": "cli_only",
    })))
}

#[derive(Deserialize)]
pub struct ListQuery {
    pub limit: Option<i64>,
    /// Opaque keyset cursor returned as `next_cursor`: rows strictly older
    /// than `<created_at RFC 3339>|<run id>` in the stable listing order.
    pub cursor: Option<String>,
}

fn parse_cursor(raw: &str) -> Result<(chrono::DateTime<chrono::Utc>, Uuid), ApiError> {
    let invalid = || ApiError::bad_request("validation_error", "invalid cursor");
    let (ts, id) = raw.split_once('|').ok_or_else(invalid)?;
    let ts = chrono::DateTime::parse_from_rfc3339(ts)
        .map_err(|_| invalid())?
        .with_timezone(&chrono::Utc);
    let id = Uuid::parse_str(id).map_err(|_| invalid())?;
    Ok((ts, id))
}

fn cursor_of(r: &sqlx::postgres::PgRow) -> String {
    format!(
        "{}|{}",
        r.get::<chrono::DateTime<chrono::Utc>, _>("created_at")
            .to_rfc3339_opts(chrono::SecondsFormat::Micros, true),
        r.get::<Uuid, _>("id")
    )
}

fn run_json(r: &sqlx::postgres::PgRow) -> Value {
    json!({
        "id": r.get::<Uuid,_>("id"),
        "suite_key": r.get::<String,_>("suite_key"),
        "suite_version": r.get::<String,_>("suite_version"),
        "rubric_version": r.get::<String,_>("rubric_version"),
        "provider": r.get::<String,_>("provider"),
        "model": r.get::<String,_>("model"),
        "external": r.get::<bool,_>("external"),
        "synthetic_provider": r.get::<bool,_>("synthetic_provider"),
        "mode": r.get::<String,_>("mode"),
        "environment": r.get::<String,_>("environment"),
        "requested_via": r.get::<String,_>("requested_via"),
        "requested_by": r.get::<Option<String>,_>("requested_by_name"),
        "operations": r.get::<Value,_>("operations"),
        "budget": r.get::<Value,_>("budget"),
        "gate": r.get::<Value,_>("gate"),
        "stop_on_high_severity": r.get::<bool,_>("stop_on_high_severity"),
        "status": r.get::<String,_>("status"),
        "abort_reason": r.get::<Option<String>,_>("abort_reason"),
        "version": r.get::<i32,_>("version"),
        "created_at": r.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
        "started_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("started_at"),
        "finished_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("finished_at"),
        "summary": r.get::<Option<Value>,_>("summary"),
    })
}

const RUN_SELECT: &str = "SELECT r.id, s.suite_key, s.suite_version, s.rubric_version,
            f.provider, f.model, f.external, f.synthetic_provider,
            r.mode, r.environment, r.requested_via, u.display_name AS requested_by_name,
            r.operations, r.budget, r.gate, r.stop_on_high_severity, r.status, r.abort_reason,
            r.version, r.created_at, r.started_at, r.finished_at,
            CASE WHEN rs.id IS NULL THEN NULL ELSE jsonb_build_object(
                'totals', rs.totals, 'operations', rs.operations, 'languages', rs.languages,
                'safety_failures', rs.safety_failures, 'calls', rs.calls,
                'input_tokens', rs.input_tokens, 'output_tokens', rs.output_tokens,
                'estimated_cost', rs.estimated_cost::text, 'currency', rs.currency,
                'p50_latency_ms', rs.p50_latency_ms, 'p95_latency_ms', rs.p95_latency_ms) END AS summary
     FROM dmind_eval_runs r
     JOIN dmind_eval_suites s ON s.id = r.suite_id
     JOIN dmind_eval_provider_fingerprints f ON f.id = r.fingerprint_id
     LEFT JOIN users u ON u.id = r.requested_by
     LEFT JOIN dmind_eval_run_summaries rs ON rs.run_id = r.id
     WHERE r.tenant_id = $1";

pub async fn list_runs(
    State(state): State<AppState>,
    ctx: AuthContext,
    Query(q): Query<ListQuery>,
) -> Result<Json<Value>, ApiError> {
    authorize(&state, &ctx, actions::DMIND_EVAL_READ).await?;
    let limit = q.limit.unwrap_or(50).clamp(1, 200);
    let cursor = q.cursor.as_deref().map(parse_cursor).transpose()?;
    // Keyset pagination on (created_at, id): stable under concurrent inserts,
    // tenant-scoped by RUN_SELECT; one extra row tells whether more exist.
    let mut rows = match cursor {
        Some((ts, id)) => {
            sqlx::query(&format!(
                "{RUN_SELECT} AND (r.created_at, r.id) < ($3, $4) \
                 ORDER BY r.created_at DESC, r.id DESC LIMIT $2"
            ))
            .bind(ctx.tenant_id)
            .bind(limit + 1)
            .bind(ts)
            .bind(id)
            .fetch_all(&state.pool)
            .await?
        }
        None => {
            sqlx::query(&format!(
                "{RUN_SELECT} ORDER BY r.created_at DESC, r.id DESC LIMIT $2"
            ))
            .bind(ctx.tenant_id)
            .bind(limit + 1)
            .fetch_all(&state.pool)
            .await?
        }
    };
    let next_cursor = if rows.len() as i64 > limit {
        rows.truncate(limit as usize);
        rows.last().map(cursor_of)
    } else {
        None
    };
    Ok(Json(json!({
        "runs": rows.iter().map(run_json).collect::<Vec<_>>(),
        "next_cursor": next_cursor,
    })))
}

#[derive(Deserialize)]
pub struct CreateRun {
    pub mode: String,
    #[serde(default)]
    pub operations: Vec<String>,
    #[serde(default = "default_true")]
    pub stop_on_high_severity: bool,
    pub max_calls: Option<u32>,
    #[serde(default)]
    pub reuse_cache: Option<bool>,
}

fn default_true() -> bool {
    true
}

pub async fn create_run(
    State(state): State<AppState>,
    ctx: AuthContext,
    Json(body): Json<CreateRun>,
) -> Result<(axum::http::StatusCode, Json<Value>), ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::DMIND_EVAL_MANAGE,
        "dmind_evaluation",
        tenant_ctx(&ctx),
    )
    .await?;
    store::require_synthetic_tenant(&state.pool, ctx.tenant_id).await?;
    let mode = Mode::parse(&body.mode).ok_or_else(|| {
        ApiError::bad_request("validation_error", "mode must be dry-run or offline")
    })?;
    if mode == Mode::Live {
        return Err(ApiError::bad_request(
            "validation_error",
            "live evaluation is only available through the dmind-eval CLI with explicit gates",
        ));
    }
    let mut operations = Vec::new();
    for key in &body.operations {
        operations.push(Operation::parse(key).ok_or_else(|| {
            ApiError::bad_request("validation_error", format!("unknown operation {key}"))
        })?);
    }
    let corpus = Corpus::load().map_err(ApiError::internal)?;
    let budget = Budget {
        max_calls: body.max_calls.unwrap_or(MAX_API_CALLS).min(MAX_API_CALLS),
        max_concurrency: 2,
        max_input_tokens: 400_000,
        max_output_tokens: 200_000,
        max_estimated_cost: None,
        pricing: None,
        max_output_tokens_per_call: 2_000,
    };
    let fingerprint = eval::runner::ProviderFingerprint::of(state.gateway.as_ref());
    let cache = if body.reuse_cache.unwrap_or(true) {
        store::cached_executions(&state.pool, ctx.tenant_id, &fingerprint.config_hash).await?
    } else {
        Vec::new()
    };
    let runner = Runner {
        corpus: &corpus,
        operations: if operations.is_empty() {
            None
        } else {
            Some(operations)
        },
        mode,
        budget,
        stop_on_high_severity: body.stop_on_high_severity,
        live_gate: Some(LiveGate {
            env: state.runtime.env,
            allow_external_ai: state.runtime.allow_external_ai,
            eval_live_enabled: false,
            tenant_synthetic: true,
            patients_synthetic: corpus.verify_synthetic().is_ok(),
            explicit_confirmation: false,
        }),
        cache,
    };
    runner
        .plan()
        .map_err(|e| ApiError::bad_request("validation_error", e))?;
    let outcome = runner
        .execute(state.gateway.clone())
        .await
        .map_err(|e| ApiError::conflict("evaluation_refused", e))?;
    let gate = json!({
        "allow_external_ai": state.runtime.allow_external_ai,
        "eval_live_enabled": false,
        "env": state.runtime.env.as_str(),
        "tenant_synthetic": true,
        "explicit_confirmation": false,
    });
    let run_id = store::persist_run(
        &state.pool,
        ctx.tenant_id,
        &corpus,
        &outcome,
        state.runtime.env.as_str(),
        Some(ctx.user_id),
        "api",
        gate,
    )
    .await?;
    allowed.record_on_pool(&state, &ctx).await?;
    let summary = eval::report::summarize(&outcome);
    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({
            "run_id": run_id,
            "status": store::run_status(&outcome).0,
            "abort": outcome.abort.as_ref().map(|a| format!("{a:?}")),
            "totals": outcome.totals,
            "safety_failures": outcome.safety_failures,
            "summary": summary,
        })),
    ))
}

pub async fn run_detail(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(run_id): Path<Uuid>,
) -> Result<Json<Value>, ApiError> {
    authorize(&state, &ctx, actions::DMIND_EVAL_READ).await?;
    let run = sqlx::query(&format!("{RUN_SELECT} AND r.id = $2"))
        .bind(ctx.tenant_id)
        .bind(run_id)
        .fetch_optional(&state.pool)
        .await?
        .ok_or_else(ApiError::not_found)?;
    let executions = sqlx::query(
        "SELECT e.id, c.case_key, c.title, c.category, c.expectations, e.operation, e.language,
                e.prompt_version, e.schema_version, e.provider, e.model, e.model_version,
                e.input_hash, e.evidence_refs, e.baseline_hash, e.baseline_output, e.provider_output,
                e.execution_kind, e.retry_count, e.degraded, e.fallback_verified, e.status,
                e.failure_code, e.latency_ms, e.input_tokens, e.output_tokens,
                e.estimated_cost::text AS estimated_cost, e.currency, e.started_at, e.finished_at,
                COALESCE((SELECT jsonb_agg(jsonb_build_object(
                    'metric', m.metric, 'verification', m.verification, 'severity', m.severity,
                    'outcome', m.outcome, 'score', m.score, 'detail', m.detail) ORDER BY m.created_at, m.metric)
                    FROM dmind_eval_metrics m WHERE m.execution_id = e.id), '[]'::jsonb) AS metrics,
                COALESCE((SELECT jsonb_agg(jsonb_build_object(
                    'id', v.id, 'reviewer', ru.display_name, 'decision', v.decision,
                    'comment', v.comment, 'version', v.version, 'rubric_version', v.rubric_version,
                    'created_at', v.created_at) ORDER BY v.version)
                    FROM dmind_eval_reviews v JOIN users ru ON ru.id = v.reviewer_id
                    WHERE v.execution_id = e.id), '[]'::jsonb) AS reviews
         FROM dmind_eval_executions e
         JOIN dmind_eval_cases c ON c.id = e.case_id
         WHERE e.tenant_id = $1 AND e.run_id = $2
         ORDER BY e.operation, c.case_key",
    )
    .bind(ctx.tenant_id)
    .bind(run_id)
    .fetch_all(&state.pool)
    .await?;
    let executions: Vec<Value> = executions
        .iter()
        .map(|r| {
            json!({
                "id": r.get::<Uuid,_>("id"),
                "case_key": r.get::<String,_>("case_key"),
                "title": r.get::<String,_>("title"),
                "category": r.get::<String,_>("category"),
                "expectations": r.get::<Value,_>("expectations"),
                "operation": r.get::<String,_>("operation"),
                "language": r.get::<String,_>("language"),
                "prompt_version": r.get::<String,_>("prompt_version"),
                "schema_version": r.get::<String,_>("schema_version"),
                "provider": r.get::<String,_>("provider"),
                "model": r.get::<String,_>("model"),
                "model_version": r.get::<String,_>("model_version"),
                "input_hash": r.get::<String,_>("input_hash"),
                "evidence_refs": r.get::<Value,_>("evidence_refs"),
                "baseline_hash": r.get::<String,_>("baseline_hash"),
                "baseline_output": r.get::<Option<Value>,_>("baseline_output"),
                "provider_output": r.get::<Option<Value>,_>("provider_output"),
                "execution_kind": r.get::<String,_>("execution_kind"),
                "retry_count": r.get::<i32,_>("retry_count"),
                "degraded": r.get::<bool,_>("degraded"),
                "fallback_verified": r.get::<bool,_>("fallback_verified"),
                "status": r.get::<String,_>("status"),
                "failure_code": r.get::<Option<String>,_>("failure_code"),
                "latency_ms": r.get::<Option<i32>,_>("latency_ms"),
                "input_tokens": r.get::<Option<i64>,_>("input_tokens"),
                "output_tokens": r.get::<Option<i64>,_>("output_tokens"),
                "estimated_cost": r.get::<Option<String>,_>("estimated_cost"),
                "currency": r.get::<Option<String>,_>("currency"),
                "started_at": r.get::<chrono::DateTime<chrono::Utc>,_>("started_at"),
                "finished_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("finished_at"),
                "metrics": r.get::<Value,_>("metrics"),
                "reviews": r.get::<Value,_>("reviews"),
            })
        })
        .collect();
    Ok(Json(json!({
        "run": run_json(&run),
        "executions": executions,
        "can_manage": can_manage(&ctx),
    })))
}

#[derive(Deserialize)]
pub struct CreateReview {
    pub decision: String,
    #[serde(default)]
    pub comment: String,
}

pub async fn create_review(
    State(state): State<AppState>,
    ctx: AuthContext,
    Path(execution_id): Path<Uuid>,
    Json(body): Json<CreateReview>,
) -> Result<(axum::http::StatusCode, Json<Value>), ApiError> {
    let allowed = guard(
        &state,
        &ctx,
        actions::DMIND_EVAL_MANAGE,
        "dmind_evaluation",
        tenant_ctx(&ctx),
    )
    .await?;
    if !REVIEW_DECISIONS.contains(&body.decision.as_str()) {
        return Err(ApiError::bad_request(
            "validation_error",
            "unknown review decision",
        ));
    }
    let comment = body.comment.trim();
    if comment.chars().count() > 1000 {
        return Err(ApiError::bad_request(
            "validation_error",
            "comment exceeds 1000 characters",
        ));
    }
    if body.decision != "accept_result" && comment.is_empty() {
        return Err(ApiError::bad_request(
            "validation_error",
            "a flag or revision request needs a comment",
        ));
    }
    let mut tx = state.pool.begin().await?;
    let exists: Option<(Uuid,)> = sqlx::query_as(
        "SELECT id FROM dmind_eval_executions WHERE tenant_id = $1 AND id = $2 FOR SHARE",
    )
    .bind(ctx.tenant_id)
    .bind(execution_id)
    .fetch_optional(&mut *tx)
    .await?;
    if exists.is_none() {
        return Err(ApiError::not_found());
    }
    // The review is attributed to the rubric that scored this execution's
    // run, not to whatever rubric the server currently compiles.
    let (rubric_version,): (String,) = sqlx::query_as(
        "SELECT s.rubric_version FROM dmind_eval_executions e
         JOIN dmind_eval_runs r ON r.id = e.run_id
         JOIN dmind_eval_suites s ON s.id = r.suite_id
         WHERE e.id = $1",
    )
    .bind(execution_id)
    .fetch_one(&mut *tx)
    .await?;
    let (version,): (i32,) = sqlx::query_as(
        "SELECT COALESCE(MAX(version), 0) + 1 FROM dmind_eval_reviews WHERE execution_id = $1",
    )
    .bind(execution_id)
    .fetch_one(&mut *tx)
    .await?;
    let id = Uuid::now_v7();
    sqlx::query(
        "INSERT INTO dmind_eval_reviews
            (id, tenant_id, execution_id, reviewer_id, rubric_version, decision, comment, version)
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
    )
    .bind(id)
    .bind(ctx.tenant_id)
    .bind(execution_id)
    .bind(ctx.user_id)
    .bind(&rubric_version)
    .bind(&body.decision)
    .bind(comment)
    .bind(version)
    .execute(&mut *tx)
    .await
    .map_err(|e| match &e {
        sqlx::Error::Database(d) if d.is_unique_violation() => ApiError::conflict(
            "review_conflict",
            "a concurrent review was recorded; reload and retry",
        ),
        _ => ApiError::from(e),
    })?;
    allowed.record(&mut tx, &ctx, &state.cell).await?;
    tx.commit().await?;
    Ok((
        axum::http::StatusCode::CREATED,
        Json(json!({
            "id": id,
            "execution_id": execution_id,
            "decision": body.decision,
            "comment": comment,
            "version": version,
            "rubric_version": rubric_version,
        })),
    ))
}
