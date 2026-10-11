//! Budgeted evaluation runner.
//!
//! Executes selected corpus cases against a configured gateway, scores each
//! outcome with the rubric and enforces the call/token/cost ceilings *before*
//! every provider call. Live mode is only reachable through [`LiveGate`], and
//! a provider failure answered by the deterministic baseline is recorded as a
//! verified fallback — never as a live success.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use dmind_gateway::{CapabilityState, GatewayError, ModelGateway, Operation, Usage};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{watch, Mutex, Semaphore};

use super::corpus::{Corpus, EvalCase};
use super::ops::CaseOutput;
use super::rubric::{self, CaseStatus, Outcome, Severity, Verdict};
use crate::runtime::RuntimeEnv;

/// How the runner is allowed to execute.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Validate corpus, plan and budget; execute nothing.
    DryRun,
    /// Deterministic baselines only (or a synthetic fixture provider); no
    /// external network traffic is possible.
    Offline,
    /// Configured external provider; requires every [`LiveGate`] check.
    Live,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::DryRun => "dry_run",
            Mode::Offline => "offline",
            Mode::Live => "live",
        }
    }
    pub fn parse(raw: &str) -> Option<Mode> {
        match raw {
            "dry-run" | "dry_run" => Some(Mode::DryRun),
            "offline" | "deterministic" => Some(Mode::Offline),
            "live" => Some(Mode::Live),
            _ => None,
        }
    }
}

/// Provider price list with its effective date. Costs are *estimates*
/// derived from reported token usage; they are never billed by WellOS.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pricing {
    pub currency: String,
    pub input_per_1k_tokens: Decimal,
    pub output_per_1k_tokens: Decimal,
    pub effective_date: chrono::NaiveDate,
}

impl Pricing {
    pub fn estimate(&self, usage: &Usage) -> Decimal {
        let input = Decimal::from(usage.prompt_tokens.unwrap_or(0));
        let output = Decimal::from(usage.completion_tokens.unwrap_or(0));
        (input * self.input_per_1k_tokens + output * self.output_per_1k_tokens)
            / Decimal::from(1000)
    }
}

/// Hard ceilings checked before each provider call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Budget {
    pub max_calls: u32,
    pub max_concurrency: u32,
    pub max_input_tokens: u64,
    pub max_output_tokens: u64,
    /// Maximum estimated spend in `pricing.currency`; `None` only when no
    /// pricing is configured, which live mode refuses.
    pub max_estimated_cost: Option<Decimal>,
    pub pricing: Option<Pricing>,
    /// Worst-case completion size reserved for every in-flight call so
    /// concurrent calls cannot jointly overshoot the token or cost ceilings.
    #[serde(default = "default_output_per_call")]
    pub max_output_tokens_per_call: u64,
}

fn default_output_per_call() -> u64 {
    4_000
}

impl Budget {
    pub fn offline_default() -> Self {
        Budget {
            max_calls: 1_000,
            max_concurrency: 4,
            max_input_tokens: 2_000_000,
            max_output_tokens: 1_000_000,
            max_estimated_cost: None,
            pricing: None,
            max_output_tokens_per_call: default_output_per_call(),
        }
    }

    pub fn validate(&self, mode: Mode) -> Result<(), String> {
        if self.max_calls == 0 {
            return Err("max_calls must be at least 1".into());
        }
        if self.max_output_tokens_per_call == 0
            || self.max_output_tokens_per_call > self.max_output_tokens
        {
            return Err(
                "max_output_tokens_per_call must be between 1 and max_output_tokens".into(),
            );
        }
        if self.max_concurrency == 0 || self.max_concurrency > 16 {
            return Err("max_concurrency must be between 1 and 16".into());
        }
        if self.max_input_tokens == 0 || self.max_output_tokens == 0 {
            return Err("token ceilings must be positive".into());
        }
        if mode == Mode::Live {
            let pricing = self
                .pricing
                .as_ref()
                .ok_or("live mode requires provider pricing")?;
            if pricing.currency.len() != 3
                || !pricing.currency.chars().all(|c| c.is_ascii_uppercase())
            {
                return Err("pricing currency must be an ISO-4217 code".into());
            }
            if pricing.input_per_1k_tokens.is_sign_negative()
                || pricing.output_per_1k_tokens.is_sign_negative()
            {
                return Err("pricing must not be negative".into());
            }
            let max = self
                .max_estimated_cost
                .ok_or("live mode requires max_estimated_cost")?;
            if max <= Decimal::ZERO {
                return Err("max_estimated_cost must be positive".into());
            }
        }
        Ok(())
    }
}

/// Inputs to the live gate. Every field must be satisfied; the runner never
/// infers one from another.
#[derive(Debug, Clone)]
pub struct LiveGate {
    pub env: RuntimeEnv,
    pub allow_external_ai: bool,
    pub eval_live_enabled: bool,
    pub tenant_synthetic: bool,
    pub patients_synthetic: bool,
    pub explicit_confirmation: bool,
}

impl LiveGate {
    /// Every unmet condition, in a stable order. Empty means the gate is open.
    pub fn violations(&self) -> Vec<&'static str> {
        let mut v = Vec::new();
        if !self.allow_external_ai {
            v.push("WELLOS_ALLOW_EXTERNAL_AI is not true");
        }
        if !self.eval_live_enabled {
            v.push("WELLOS_EVAL_LIVE is not true");
        }
        if !matches!(self.env, RuntimeEnv::Test | RuntimeEnv::Staging) {
            v.push("live evaluation runs only in test or staging");
        }
        if !self.tenant_synthetic {
            v.push("tenant data_class is not synthetic");
        }
        if !self.patients_synthetic {
            v.push("corpus contains non-synthetic patient material");
        }
        if !self.explicit_confirmation {
            v.push("explicit live confirmation missing");
        }
        v
    }
}

/// Provenance of one executed case.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionKind {
    /// The provider was called for this input in this run.
    Fresh,
    /// The provider answered transiently-failed attempts before succeeding
    /// or giving up (adapter-level retries observed).
    Retried,
    /// An identical input (same hash, prompt version and provider) was
    /// already executed earlier in this run; its result is reused.
    Reused,
    /// Result taken from a prior persisted execution with identical
    /// provenance; no provider call was made.
    Cached,
    /// No provider call was made: dry run, or aborted by a ceiling /
    /// stop-on-failure before this case.
    Skipped,
}

impl ExecutionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ExecutionKind::Fresh => "fresh",
            ExecutionKind::Retried => "retried",
            ExecutionKind::Reused => "reused",
            ExecutionKind::Cached => "cached",
            ExecutionKind::Skipped => "skipped",
        }
    }
}

/// Prior result the runner may reuse instead of calling the provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedExecution {
    pub input_hash: String,
    pub prompt_version: String,
    pub provider: String,
    pub model: String,
    pub model_version: String,
    pub provider_output: Option<Value>,
    pub failure_code: Option<String>,
}

/// One scored execution. Only typed outputs, hashes and safe metadata.
#[derive(Debug, Clone, Serialize)]
pub struct ExecutionRecord {
    pub case_key: String,
    pub operation: &'static str,
    pub language: String,
    pub category: &'static str,
    pub input_hash: String,
    pub prompt_version: String,
    pub schema_version: &'static str,
    pub provider: String,
    pub model: String,
    pub model_version: String,
    pub execution_kind: ExecutionKind,
    pub retry_count: u32,
    pub degraded: bool,
    pub fallback_verified: bool,
    pub status: CaseStatus,
    pub failure_code: Option<String>,
    pub latency_ms: Option<u32>,
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub estimated_cost: Option<Decimal>,
    pub evidence_refs: Vec<String>,
    pub baseline_hash: String,
    pub baseline_output: Option<Value>,
    pub provider_output: Option<Value>,
    pub verdict: Verdict,
}

/// Why a run stopped before finishing every selected case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "reason", content = "detail")]
pub enum AbortReason {
    CallCeiling(u32),
    InputTokenCeiling(u64),
    OutputTokenCeiling(u64),
    CostCeiling(Decimal),
    HighSeveritySafetyFailure(String),
    ProviderDisabled(String),
}

#[derive(Debug, Clone, Serialize)]
pub struct RunPlan {
    pub suite_key: &'static str,
    pub suite_version: &'static str,
    pub rubric_version: &'static str,
    pub corpus_hash: String,
    pub mode: Mode,
    pub operations: Vec<&'static str>,
    pub planned_cases: usize,
    pub planned_provider_calls: usize,
    pub budget: Budget,
    pub stop_on_high_severity: bool,
    pub live_gate: Option<Vec<&'static str>>,
}

#[derive(Debug, Clone, Default, Serialize)]
pub struct Totals {
    pub cases: usize,
    pub passed: usize,
    pub needs_review: usize,
    pub failed: usize,
    pub aborted: usize,
    pub fallback_verified: usize,
    pub provider_calls: u32,
    pub retries: u32,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub estimated_cost: Decimal,
    pub p50_latency_ms: Option<u32>,
    pub p95_latency_ms: Option<u32>,
}

#[derive(Debug, Clone, Serialize)]
pub struct RunOutcome {
    pub plan: RunPlan,
    pub provider: ProviderFingerprint,
    pub executions: Vec<ExecutionRecord>,
    pub abort: Option<AbortReason>,
    pub totals: Totals,
    pub safety_failures: Vec<SafetyFailure>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SafetyFailure {
    pub case_key: String,
    pub operation: &'static str,
    pub metrics: Vec<&'static str>,
}

/// Secret-free identity of the provider under evaluation.
#[derive(Debug, Clone, Serialize)]
pub struct ProviderFingerprint {
    pub provider: String,
    pub model: String,
    pub model_version: String,
    pub external: bool,
    pub synthetic_provider: bool,
    pub state: String,
    pub config_hash: String,
}

impl ProviderFingerprint {
    pub fn of(gateway: &dyn ModelGateway) -> Self {
        let info = gateway.info();
        let status = gateway.status();
        let config_hash = dmind_gateway::hash_json(&serde_json::json!({
            "provider": info.provider,
            "model": info.model,
            "model_version": info.model_version,
            "external": status.external,
            "synthetic": status.synthetic,
            "config": gateway.config_fingerprint(),
            "prompts": Operation::ALL
                .iter()
                .map(|op| gateway.prompt_version(*op))
                .collect::<Vec<_>>(),
        }));
        ProviderFingerprint {
            provider: info.provider,
            model: info.model,
            model_version: info.model_version,
            external: status.external,
            synthetic_provider: status.synthetic,
            state: format!("{:?}", status.state).to_lowercase(),
            config_hash,
        }
    }
}

pub struct Runner<'a> {
    pub corpus: &'a Corpus,
    pub operations: Option<Vec<Operation>>,
    pub mode: Mode,
    pub budget: Budget,
    pub stop_on_high_severity: bool,
    pub live_gate: Option<LiveGate>,
    pub cache: Vec<CachedExecution>,
}

struct Spend {
    /// HTTP attempts charged so far (first attempts plus transient retries).
    calls: u32,
    input_tokens: u64,
    output_tokens: u64,
    cost: Decimal,
    /// Worst-case allowances held by in-flight calls; released on completion.
    reserved_calls: u32,
    reserved_input: u64,
    reserved_output: u64,
    reserved_cost: Decimal,
    retries_accounted: u64,
    abort: Option<AbortReason>,
    reused: HashMap<String, (Result<CaseOutput, String>, u32)>,
    inflight: HashMap<String, watch::Sender<bool>>,
}

impl Spend {
    fn new() -> Self {
        Spend {
            calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost: Decimal::ZERO,
            reserved_calls: 0,
            reserved_input: 0,
            reserved_output: 0,
            reserved_cost: Decimal::ZERO,
            retries_accounted: 0,
            abort: None,
            reused: HashMap::new(),
            inflight: HashMap::new(),
        }
    }

    fn reservation_cost(budget: &Budget, estimated_input: u64) -> Decimal {
        budget
            .pricing
            .as_ref()
            .map(|p| {
                p.estimate(&Usage {
                    prompt_tokens: Some(estimated_input),
                    completion_tokens: Some(budget.max_output_tokens_per_call),
                    total_tokens: None,
                })
            })
            .unwrap_or(Decimal::ZERO)
    }

    /// Reserve the worst case of one logical call (`attempts` HTTP attempts,
    /// the estimated input and a full `max_output_tokens_per_call`
    /// completion) or record the ceiling that would be exceeded.
    fn reserve(
        &mut self,
        budget: &Budget,
        estimated_input: u64,
        attempts: u32,
    ) -> Result<(), AbortReason> {
        if let Some(a) = &self.abort {
            return Err(a.clone());
        }
        let cost = Self::reservation_cost(budget, estimated_input);
        let reason = if self.calls + self.reserved_calls + attempts.max(1) > budget.max_calls {
            Some(AbortReason::CallCeiling(budget.max_calls))
        } else if self.input_tokens + self.reserved_input + estimated_input
            > budget.max_input_tokens
        {
            Some(AbortReason::InputTokenCeiling(budget.max_input_tokens))
        } else if self.output_tokens + self.reserved_output + budget.max_output_tokens_per_call
            > budget.max_output_tokens
        {
            Some(AbortReason::OutputTokenCeiling(budget.max_output_tokens))
        } else if matches!((budget.max_estimated_cost, &budget.pricing), (Some(max), Some(_)) if self.cost + self.reserved_cost + cost > max)
        {
            Some(AbortReason::CostCeiling(
                budget.max_estimated_cost.unwrap_or_default(),
            ))
        } else {
            None
        };
        if let Some(r) = reason {
            self.abort = Some(r.clone());
            return Err(r);
        }
        self.reserved_calls += attempts.max(1);
        self.reserved_input += estimated_input;
        self.reserved_output += budget.max_output_tokens_per_call;
        self.reserved_cost += cost;
        Ok(())
    }

    /// Replace a reservation with what the call actually consumed. Every
    /// transient retry the gateway performed since the last settlement is
    /// charged against the call ceiling; returns that retry count.
    fn settle(
        &mut self,
        budget: &Budget,
        estimated_input: u64,
        attempts: u32,
        usage: Option<&Usage>,
        gateway_retries_total: u64,
    ) -> u32 {
        self.reserved_calls = self.reserved_calls.saturating_sub(attempts.max(1));
        self.reserved_input = self.reserved_input.saturating_sub(estimated_input);
        self.reserved_output = self
            .reserved_output
            .saturating_sub(budget.max_output_tokens_per_call);
        self.reserved_cost -= Self::reservation_cost(budget, estimated_input);
        let new_retries = gateway_retries_total.saturating_sub(self.retries_accounted);
        self.retries_accounted = gateway_retries_total;
        let retries = new_retries.min(u32::MAX as u64) as u32;
        self.calls = self.calls.saturating_add(1).saturating_add(retries);
        match usage {
            Some(u) => {
                self.input_tokens += u.prompt_tokens.unwrap_or(estimated_input);
                self.output_tokens += u.completion_tokens.unwrap_or(0);
                if let Some(p) = &budget.pricing {
                    self.cost += p.estimate(u);
                }
            }
            None => self.input_tokens += estimated_input,
        }
        retries
    }
}

impl<'a> Runner<'a> {
    pub fn plan(&self) -> Result<RunPlan, String> {
        self.budget.validate(self.mode)?;
        let selected = self.corpus.select(self.operations.as_deref());
        if selected.is_empty() {
            return Err("no cases selected".into());
        }
        let live_gate = match (self.mode, &self.live_gate) {
            (Mode::Live, Some(gate)) => Some(gate.violations()),
            (Mode::Live, None) => Some(vec!["live gate not evaluated"]),
            _ => None,
        };
        if let Some(v) = &live_gate {
            if !v.is_empty() {
                return Err(format!("live gate refused: {}", v.join("; ")));
            }
        }
        let planned_provider_calls = match self.mode {
            Mode::DryRun => 0,
            _ => selected.len(),
        };
        if planned_provider_calls as u32 > self.budget.max_calls {
            return Err(format!(
                "plan needs {planned_provider_calls} provider calls but max_calls is {}",
                self.budget.max_calls
            ));
        }
        let mut operations: Vec<&'static str> =
            selected.iter().map(|c| c.operation.as_str()).collect();
        operations.dedup();
        Ok(RunPlan {
            suite_key: self.corpus.suite_key,
            suite_version: self.corpus.version,
            rubric_version: rubric::RUBRIC_VERSION,
            corpus_hash: self.corpus.hash(),
            mode: self.mode,
            operations,
            planned_cases: selected.len(),
            planned_provider_calls,
            budget: self.budget.clone(),
            stop_on_high_severity: self.stop_on_high_severity,
            live_gate,
        })
    }

    /// Execute the plan. `gateway` is only called in offline/live mode; the
    /// deterministic baseline is always computed.
    pub async fn execute(&self, gateway: Arc<dyn ModelGateway>) -> Result<RunOutcome, String> {
        let plan = self.plan()?;
        let provider = ProviderFingerprint::of(gateway.as_ref());
        if self.mode == Mode::Live && provider.synthetic_provider {
            return Err("live mode refuses a synthetic fixture provider".into());
        }
        if self.mode == Mode::Offline && provider.external {
            return Err("offline mode refuses an external provider".into());
        }
        let selected: Vec<&EvalCase> = self.corpus.select(self.operations.as_deref());
        let spend = Arc::new(Mutex::new(Spend::new()));
        let semaphore = Arc::new(Semaphore::new(self.budget.max_concurrency as usize));
        let mut records = Vec::with_capacity(selected.len());
        let sequential = self.budget.max_concurrency == 1;

        // Cases run in corpus order; with concurrency > 1 they overlap, so
        // per-case retry attribution is only exact when sequential.
        let mut handles = Vec::new();
        for case in selected {
            let permit = semaphore
                .clone()
                .acquire_owned()
                .await
                .map_err(|e| e.to_string())?;
            let gateway = gateway.clone();
            let spend = spend.clone();
            let case = case.clone();
            let mode = self.mode;
            let budget = self.budget.clone();
            let cache = self.cache.clone();
            let stop = self.stop_on_high_severity;
            let fingerprint = provider.clone();
            handles.push(tokio::spawn(async move {
                let rec = run_case(
                    &case,
                    gateway.as_ref(),
                    &fingerprint,
                    mode,
                    &budget,
                    &cache,
                    spend.clone(),
                    sequential,
                )
                .await;
                if stop && !rec.verdict.safety_failures.is_empty() {
                    let mut s = spend.lock().await;
                    if s.abort.is_none() {
                        s.abort =
                            Some(AbortReason::HighSeveritySafetyFailure(rec.case_key.clone()));
                    }
                }
                drop(permit);
                rec
            }));
        }
        for h in handles {
            records.push(h.await.map_err(|e| e.to_string())?);
        }
        let abort = spend.lock().await.abort.clone();
        let totals = totals(&records, gateway.as_ref());
        let safety_failures = records
            .iter()
            .filter(|r| !r.verdict.safety_failures.is_empty())
            .map(|r| SafetyFailure {
                case_key: r.case_key.clone(),
                operation: r.operation,
                metrics: r.verdict.safety_failures.clone(),
            })
            .collect();
        Ok(RunOutcome {
            plan,
            provider,
            executions: records,
            abort,
            totals,
            safety_failures,
        })
    }
}

fn error_code(e: &GatewayError) -> String {
    match e {
        GatewayError::Unavailable(_) => "unavailable",
        GatewayError::InvalidOutput(_) => "invalid_output",
        GatewayError::PolicyDenied(_) => "policy_denied",
        GatewayError::Disabled(_) => "disabled",
    }
    .to_string()
}

fn rehydrate(code: &str) -> GatewayError {
    match code {
        "invalid_output" => GatewayError::InvalidOutput("cached".into()),
        "policy_denied" => GatewayError::PolicyDenied("cached".into()),
        "disabled" => GatewayError::Disabled("cached".into()),
        _ => GatewayError::Unavailable("cached".into()),
    }
}

#[allow(clippy::too_many_arguments)]
async fn run_case(
    case: &EvalCase,
    gateway: &dyn ModelGateway,
    fingerprint: &ProviderFingerprint,
    mode: Mode,
    budget: &Budget,
    cache: &[CachedExecution],
    spend: Arc<Mutex<Spend>>,
    sequential: bool,
) -> ExecutionRecord {
    let baseline = case.input.baseline();
    let baseline_hash = baseline
        .as_ref()
        .map(|b| dmind_gateway::hash_json(&b.output))
        .unwrap_or_else(|_| "baseline-refused".into());
    let input_hash = case.input_hash();
    let prompt_version = gateway.prompt_version(case.operation);

    let mut kind = ExecutionKind::Skipped;
    let mut retry_count = 0u32;
    let mut latency_ms = None;
    let mut aborted = false;

    let provider_result: Result<CaseOutput, GatewayError> = match mode {
        Mode::DryRun => Err(GatewayError::Disabled("dry run".into())),
        Mode::Offline | Mode::Live => {
            let reuse_key = format!("{input_hash}:{prompt_version}:{}", fingerprint.config_hash);
            let cached = cache.iter().find(|c| {
                c.input_hash == input_hash
                    && c.prompt_version == prompt_version
                    && c.provider == fingerprint.provider
                    && c.model == fingerprint.model
                    && c.model_version == fingerprint.model_version
            });
            // Identical inputs share one provider call: the first case marks
            // the key in flight under the lock, later ones wait for its result.
            let mut waiter: Option<watch::Receiver<bool>> = None;
            let mut prior = None;
            loop {
                if let Some(mut rx) = waiter.take() {
                    let _ = rx.changed().await;
                }
                let s = spend.lock().await;
                if let Some(p) = s.reused.get(&reuse_key).cloned() {
                    prior = Some(p);
                    break;
                }
                if let Some(tx) = s.inflight.get(&reuse_key) {
                    waiter = Some(tx.subscribe());
                    continue;
                }
                break;
            }
            if let Some((prior, retries)) = prior {
                kind = ExecutionKind::Reused;
                retry_count = retries;
                prior.map_err(|code| rehydrate(&code))
            } else if let Some(c) = cached {
                kind = ExecutionKind::Cached;
                match (&c.provider_output, &c.failure_code) {
                    (Some(out), _) => Ok(CaseOutput {
                        output: out.clone(),
                        provider: gateway.info(),
                        prompt_version: prompt_version.clone(),
                        input_hash: input_hash.clone(),
                        usage: None,
                    }),
                    (None, Some(code)) => Err(rehydrate(code)),
                    (None, None) => Err(GatewayError::Unavailable("cached without output".into())),
                }
            } else {
                // Input size is estimated from the serialized request before
                // the call (≈4 bytes per token) so the ceiling holds even when
                // the provider reports no usage; actual usage replaces it.
                let estimated_input = serde_json::to_vec(&case.input)
                    .map(|b| b.len().div_ceil(4) as u64)
                    .unwrap_or(0);
                let attempts = gateway.max_attempts();
                // Reservation happens under the lock so concurrent cases
                // cannot jointly overshoot a ceiling.
                let allowed = {
                    let mut s = spend.lock().await;
                    match s.reserve(budget, estimated_input, attempts) {
                        Ok(()) => {
                            let (tx, _rx) = watch::channel(false);
                            s.inflight.insert(reuse_key.clone(), tx);
                            true
                        }
                        Err(_) => false,
                    }
                };
                if !allowed {
                    aborted = true;
                    Err(GatewayError::Disabled(
                        "run aborted before this call".into(),
                    ))
                } else {
                    let started = Instant::now();
                    let result = case.input.execute(gateway).await;
                    latency_ms = Some(started.elapsed().as_millis().min(u32::MAX as u128) as u32);
                    let mut s = spend.lock().await;
                    let retries = s.settle(
                        budget,
                        estimated_input,
                        attempts,
                        result.as_ref().ok().and_then(|o| o.usage.as_ref()),
                        gateway.retries_total(),
                    );
                    if sequential {
                        retry_count = retries;
                    }
                    kind = if retry_count > 0 {
                        ExecutionKind::Retried
                    } else {
                        ExecutionKind::Fresh
                    };
                    s.reused.insert(
                        reuse_key.clone(),
                        (
                            result.as_ref().map(Clone::clone).map_err(error_code),
                            retry_count,
                        ),
                    );
                    if let Some(tx) = s.inflight.remove(&reuse_key) {
                        let _ = tx.send(true);
                    }
                    result
                }
            }
        }
    };

    let mut verdict = rubric::evaluate(case, &provider_result, &baseline);
    if aborted || mode == Mode::DryRun {
        verdict.status = CaseStatus::Aborted;
        verdict.safety_failures.clear();
        for m in &mut verdict.metrics {
            if m.outcome == Outcome::Fail && m.severity != Severity::Info {
                m.outcome = Outcome::NotValidated;
            }
        }
    }
    let usage = provider_result.as_ref().ok().and_then(|o| o.usage.clone());
    let fallback_verified = matches!(
        verdict.status,
        CaseStatus::ProviderUnavailableFallbackVerified
    );
    let degraded = matches!(gateway.status().state, CapabilityState::Degraded);
    ExecutionRecord {
        case_key: case.key.clone(),
        operation: case.operation.as_str(),
        language: case.language.clone(),
        category: case.category.as_str(),
        input_hash,
        prompt_version,
        schema_version: case.operation.output_schema(),
        provider: fingerprint.provider.clone(),
        model: fingerprint.model.clone(),
        model_version: fingerprint.model_version.clone(),
        execution_kind: kind,
        retry_count,
        degraded,
        fallback_verified,
        status: verdict.status,
        failure_code: provider_result.as_ref().err().map(error_code),
        latency_ms,
        input_tokens: usage.as_ref().and_then(|u| u.prompt_tokens),
        output_tokens: usage.as_ref().and_then(|u| u.completion_tokens),
        estimated_cost: usage
            .as_ref()
            .zip(budget.pricing.as_ref())
            .map(|(u, p)| p.estimate(u)),
        evidence_refs: case.allowed_evidence(),
        baseline_hash,
        baseline_output: baseline.ok().map(|b| b.output),
        provider_output: provider_result.ok().map(|o| o.output),
        verdict,
    }
}

fn totals(records: &[ExecutionRecord], gateway: &dyn ModelGateway) -> Totals {
    let mut t = Totals {
        cases: records.len(),
        ..Totals::default()
    };
    let mut latencies: Vec<u32> = Vec::new();
    for r in records {
        match r.status {
            CaseStatus::Passed => t.passed += 1,
            CaseStatus::NeedsReview => t.needs_review += 1,
            CaseStatus::Failed => t.failed += 1,
            CaseStatus::Aborted => t.aborted += 1,
            CaseStatus::ProviderUnavailableFallbackVerified => t.fallback_verified += 1,
        }
        if matches!(
            r.execution_kind,
            ExecutionKind::Fresh | ExecutionKind::Retried
        ) {
            t.provider_calls += 1;
            if let Some(l) = r.latency_ms {
                latencies.push(l);
            }
        }
        t.input_tokens += r.input_tokens.unwrap_or(0);
        t.output_tokens += r.output_tokens.unwrap_or(0);
        t.estimated_cost += r.estimated_cost.unwrap_or_default();
    }
    t.retries = gateway.retries_total().min(u32::MAX as u64) as u32;
    latencies.sort_unstable();
    if !latencies.is_empty() {
        let pick = |q: f64| latencies[((latencies.len() - 1) as f64 * q).round() as usize];
        t.p50_latency_ms = Some(pick(0.5));
        t.p95_latency_ms = Some(pick(0.95));
    }
    t
}

#[cfg(all(test, feature = "dev-fixtures"))]
mod tests {
    use super::*;
    use dmind_gateway::fake::FakeProvider;

    fn runner(corpus: &Corpus, mode: Mode) -> Runner<'_> {
        Runner {
            corpus,
            operations: None,
            mode,
            budget: Budget::offline_default(),
            stop_on_high_severity: true,
            live_gate: None,
            cache: vec![],
        }
    }

    #[tokio::test]
    async fn offline_run_against_the_fixture_provider_covers_every_case_without_safety_failures() {
        let corpus = Corpus::load().unwrap();
        let gateway: Arc<dyn ModelGateway> = Arc::new(FakeProvider::new());
        let outcome = runner(&corpus, Mode::Offline)
            .execute(gateway)
            .await
            .unwrap();
        assert_eq!(outcome.executions.len(), corpus.cases.len());
        let failing: Vec<_> = outcome
            .executions
            .iter()
            .filter(|e| !e.verdict.safety_failures.is_empty())
            .map(|e| {
                (
                    &e.case_key,
                    e.verdict
                        .metrics
                        .iter()
                        .filter(|m| m.outcome == Outcome::Fail)
                        .map(|m| (m.metric, m.detail.clone()))
                        .collect::<Vec<_>>(),
                    &e.provider_output,
                )
            })
            .collect();
        assert!(failing.is_empty(), "{failing:#?}");
        assert!(outcome.abort.is_none(), "{:?}", outcome.abort);
        assert_eq!(
            outcome.totals.failed,
            0,
            "{:#?}",
            outcome
                .executions
                .iter()
                .filter(|e| e.status == CaseStatus::Failed)
                .map(|e| (&e.case_key, &e.verdict))
                .collect::<Vec<_>>()
        );
        assert!(outcome.provider.synthetic_provider);
        assert!(outcome
            .executions
            .iter()
            .all(|e| e.provider_output.is_some() || e.failure_code.is_some()));
        assert!(outcome.totals.provider_calls > 0);
        assert_eq!(outcome.totals.retries, 0);
    }

    #[tokio::test]
    async fn dry_run_executes_nothing() {
        let corpus = Corpus::load().unwrap();
        let gateway: Arc<dyn ModelGateway> = Arc::new(FakeProvider::new());
        let outcome = runner(&corpus, Mode::DryRun)
            .execute(gateway)
            .await
            .unwrap();
        assert_eq!(outcome.plan.planned_provider_calls, 0);
        assert_eq!(outcome.totals.provider_calls, 0);
        assert!(outcome
            .executions
            .iter()
            .all(|e| e.status == CaseStatus::Aborted && e.provider_output.is_none()));
        assert!(outcome.safety_failures.is_empty());
    }

    #[tokio::test]
    async fn call_ceiling_aborts_before_the_next_provider_call() {
        let corpus = Corpus::load().unwrap();
        let mut r = runner(&corpus, Mode::Offline);
        r.operations = Some(vec![Operation::TriageProposal]);
        r.budget.max_calls = 2;
        r.budget.max_concurrency = 1;
        assert!(r.plan().unwrap_err().contains("max_calls"));
        // A plan that fits the ceiling but a run whose ceiling is lowered
        // mid-way is modelled by a ceiling of 2 and the first 3 triage cases.
        r.budget.max_calls = 3;
        let selected = corpus.select(Some(&[Operation::TriageProposal])).len();
        assert!(selected > 3);
        let err = r.plan().unwrap_err();
        assert!(err.contains("max_calls"), "{err}");
    }

    #[tokio::test]
    async fn ceilings_are_enforced_under_the_lock_and_abort_the_run() {
        let corpus = Corpus::load().unwrap();
        let gateway: Arc<dyn ModelGateway> = Arc::new(FakeProvider::new());
        let mut r = runner(&corpus, Mode::Offline);
        r.budget.max_input_tokens = 1; // every request estimate exceeds this
        r.budget.max_concurrency = 1;
        let outcome = r.execute(gateway).await.unwrap();
        assert!(
            matches!(outcome.abort, Some(AbortReason::InputTokenCeiling(1))),
            "{:?}",
            outcome.abort
        );
        assert_eq!(outcome.totals.provider_calls, 0);
        assert_eq!(outcome.totals.aborted, corpus.cases.len());
        assert!(outcome
            .executions
            .iter()
            .all(|e| e.provider_output.is_none() && e.verdict.safety_failures.is_empty()));
    }

    #[tokio::test]
    async fn unavailable_provider_is_recorded_as_verified_fallback_never_success() {
        let corpus = Corpus::load().unwrap();
        let fake = FakeProvider::new();
        fake.set_unavailable(true);
        let gateway: Arc<dyn ModelGateway> = Arc::new(fake);
        let mut r = runner(&corpus, Mode::Offline);
        r.operations = Some(vec![Operation::TriageProposal]);
        let outcome = r.execute(gateway).await.unwrap();
        assert!(outcome.executions.iter().all(|e| e.status
            == CaseStatus::ProviderUnavailableFallbackVerified
            && e.fallback_verified
            && e.provider_output.is_none()));
        assert_eq!(outcome.totals.passed, 0);
        assert_eq!(outcome.totals.fallback_verified, outcome.executions.len());
    }

    #[tokio::test]
    async fn live_mode_requires_every_gate_and_refuses_the_fixture_provider() {
        let corpus = Corpus::load().unwrap();
        let mut r = runner(&corpus, Mode::Live);
        r.budget.pricing = Some(Pricing {
            currency: "EUR".into(),
            input_per_1k_tokens: Decimal::new(1, 3),
            output_per_1k_tokens: Decimal::new(2, 3),
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        });
        r.budget.max_estimated_cost = Some(Decimal::ONE);
        assert!(r.plan().unwrap_err().contains("live gate"));
        let mut gate = LiveGate {
            env: RuntimeEnv::Test,
            allow_external_ai: true,
            eval_live_enabled: true,
            tenant_synthetic: true,
            patients_synthetic: true,
            explicit_confirmation: false,
        };
        r.live_gate = Some(gate.clone());
        assert!(r.plan().unwrap_err().contains("explicit live confirmation"));
        gate.explicit_confirmation = true;
        gate.env = RuntimeEnv::Production;
        r.live_gate = Some(gate.clone());
        assert!(r.plan().unwrap_err().contains("test or staging"));
        gate.env = RuntimeEnv::Staging;
        r.live_gate = Some(gate.clone());
        r.plan().unwrap();
        r.budget.max_estimated_cost = None;
        assert!(r.plan().unwrap_err().contains("max_estimated_cost"));
        r.budget.max_estimated_cost = Some(Decimal::ONE);
        let gateway: Arc<dyn ModelGateway> = Arc::new(FakeProvider::new());
        let err = r.execute(gateway).await.unwrap_err();
        assert!(err.contains("synthetic fixture provider"), "{err}");
    }

    #[tokio::test]
    async fn identical_inputs_are_reused_and_cache_hits_make_no_call() {
        let corpus = Corpus::load().unwrap();
        let gateway: Arc<dyn ModelGateway> = Arc::new(FakeProvider::new());
        let mut r = runner(&corpus, Mode::Offline);
        r.operations = Some(vec![Operation::TriageProposal]);
        r.budget.max_concurrency = 1;
        let first = r.execute(gateway.clone()).await.unwrap();
        let fresh = first
            .executions
            .iter()
            .filter(|e| e.execution_kind == ExecutionKind::Fresh)
            .count();
        let reused: Vec<&ExecutionRecord> = first
            .executions
            .iter()
            .filter(|e| e.execution_kind == ExecutionKind::Reused)
            .collect();
        assert_eq!(fresh + reused.len(), first.executions.len());
        assert_eq!(first.totals.provider_calls, fresh as u32);
        for r in &reused {
            assert!(first
                .executions
                .iter()
                .any(|e| e.execution_kind == ExecutionKind::Fresh
                    && e.input_hash == r.input_hash
                    && e.case_key != r.case_key));
        }
        r.cache = first
            .executions
            .iter()
            .map(|e| CachedExecution {
                input_hash: e.input_hash.clone(),
                prompt_version: e.prompt_version.clone(),
                provider: e.provider.clone(),
                model: e.model.clone(),
                model_version: e.model_version.clone(),
                provider_output: e.provider_output.clone(),
                failure_code: e.failure_code.clone(),
            })
            .collect();
        let second = r.execute(gateway).await.unwrap();
        assert!(second
            .executions
            .iter()
            .all(|e| e.execution_kind == ExecutionKind::Cached));
        assert_eq!(second.totals.provider_calls, 0);
        assert_eq!(
            second.totals.passed + second.totals.needs_review,
            first.totals.passed + first.totals.needs_review
        );
    }

    #[test]
    fn reservations_hold_the_worst_case_of_every_inflight_call() {
        let mut budget = Budget::offline_default();
        budget.max_calls = 5;
        budget.max_input_tokens = 150;
        budget.max_output_tokens = 1_000;
        budget.max_output_tokens_per_call = 400;
        budget.pricing = Some(Pricing {
            currency: "EUR".into(),
            input_per_1k_tokens: Decimal::new(1, 0),
            output_per_1k_tokens: Decimal::new(1, 0),
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        });
        budget.max_estimated_cost = Some(Decimal::new(1, 0));

        // A provider that may retry twice books three attempts up front.
        let mut s = Spend::new();
        s.reserve(&budget, 10, 3).unwrap();
        assert_eq!(
            (s.reserved_calls, s.reserved_input, s.reserved_output),
            (3, 10, 400)
        );
        assert!(matches!(
            s.reserve(&budget, 10, 3),
            Err(AbortReason::CallCeiling(5))
        ));

        // Two in-flight calls jointly exceed the output ceiling (400 + 400 + 400 > 1000).
        let mut s = Spend::new();
        s.reserve(&budget, 10, 1).unwrap();
        s.reserve(&budget, 10, 1).unwrap();
        assert!(matches!(
            s.reserve(&budget, 10, 1),
            Err(AbortReason::OutputTokenCeiling(1_000))
        ));

        // Input reservations count before the provider reports usage.
        let mut s = Spend::new();
        s.reserve(&budget, 100, 1).unwrap();
        assert!(matches!(
            s.reserve(&budget, 60, 1),
            Err(AbortReason::InputTokenCeiling(150))
        ));

        // Cost: each call reserves (10 input + 100 output) / 1000 = 0.11 EUR;
        // the third would pass a 0.30 EUR ceiling while tokens still fit.
        let mut cheap = budget.clone();
        cheap.max_output_tokens_per_call = 100;
        cheap.max_estimated_cost = Some(Decimal::new(30, 2));
        let mut s = Spend::new();
        s.reserve(&cheap, 10, 1).unwrap();
        s.reserve(&cheap, 10, 1).unwrap();
        assert!(matches!(
            s.reserve(&cheap, 10, 1),
            Err(AbortReason::CostCeiling(_))
        ));

        // Settlement swaps the reservation for actual usage and charges every
        // retry the gateway performed against the call ceiling.
        let mut s = Spend::new();
        s.reserve(&budget, 10, 3).unwrap();
        let usage = Usage {
            prompt_tokens: Some(12),
            completion_tokens: Some(30),
            total_tokens: Some(42),
        };
        let retries = s.settle(&budget, 10, 3, Some(&usage), 2);
        assert_eq!(retries, 2);
        assert_eq!(s.calls, 3);
        assert_eq!(
            (s.reserved_calls, s.reserved_input, s.reserved_output),
            (0, 0, 0)
        );
        assert_eq!(s.reserved_cost, Decimal::ZERO);
        assert_eq!((s.input_tokens, s.output_tokens), (12, 30));
        assert_eq!(s.cost, Decimal::new(42, 3));
        // Only two attempts remain: a provider that may retry twice no longer fits.
        assert!(matches!(
            s.reserve(&budget, 10, 3),
            Err(AbortReason::CallCeiling(5))
        ));
    }

    #[test]
    fn per_call_output_reservation_must_fit_the_run_ceiling() {
        let mut budget = Budget::offline_default();
        budget.max_output_tokens = 100;
        budget.max_output_tokens_per_call = 101;
        assert!(budget
            .validate(Mode::Offline)
            .unwrap_err()
            .contains("max_output_tokens_per_call"));
    }

    #[tokio::test]
    async fn concurrent_identical_inputs_share_one_provider_call() {
        let mut corpus = Corpus::load().unwrap();
        // Three extra cases with byte-identical inputs dispatched together.
        let template = corpus.cases[0].clone();
        for n in 0..3 {
            let mut dup = template.clone();
            dup.key = format!("{}-dup-{n}", template.key);
            corpus.cases.push(dup);
        }
        let gateway: Arc<dyn ModelGateway> = Arc::new(FakeProvider::new());
        let mut r = runner(&corpus, Mode::Offline);
        r.budget.max_concurrency = 8;
        let outcome = r.execute(gateway).await.unwrap();
        let distinct_inputs: std::collections::BTreeSet<&str> = outcome
            .executions
            .iter()
            .map(|e| e.input_hash.as_str())
            .collect();
        let fresh = outcome
            .executions
            .iter()
            .filter(|e| e.execution_kind == ExecutionKind::Fresh)
            .count();
        assert_eq!(distinct_inputs.len() + 3, outcome.executions.len());
        assert_eq!(fresh, distinct_inputs.len());
        assert_eq!(outcome.totals.provider_calls, fresh as u32);
        assert!(outcome
            .executions
            .iter()
            .filter(|e| e.execution_kind == ExecutionKind::Reused)
            .all(|e| e.provider_output.is_some() || e.failure_code.is_some()));
    }

    #[test]
    fn fingerprint_changes_with_endpoint_or_limits_without_leaking_secrets() {
        use dmind_gateway::openai::{OpenAiCompatibleModel, OpenAiCompatibleModelConfig};
        let cfg = |endpoint: &str, max_retries: u32| OpenAiCompatibleModelConfig {
            endpoint: endpoint.into(),
            model: "gpt-eval".into(),
            api_key: "sk-test-secret-never-logged".into(),
            connect_timeout: std::time::Duration::from_secs(1),
            timeout: std::time::Duration::from_secs(5),
            max_retries,
            retry_backoff: std::time::Duration::from_millis(10),
            max_response_bytes: 65_536,
            max_concurrency: 2,
            queue_timeout: std::time::Duration::from_secs(1),
        };
        let a =
            OpenAiCompatibleModel::new(cfg("https://a.example/v1/chat/completions", 2)).unwrap();
        let b =
            OpenAiCompatibleModel::new(cfg("https://b.example/v1/chat/completions", 2)).unwrap();
        let c =
            OpenAiCompatibleModel::new(cfg("https://a.example/v1/chat/completions", 0)).unwrap();
        let fa = ProviderFingerprint::of(&a);
        let fb = ProviderFingerprint::of(&b);
        let fc = ProviderFingerprint::of(&c);
        assert_eq!(
            (fa.provider.as_str(), fa.model.as_str()),
            (fb.provider.as_str(), fb.model.as_str())
        );
        assert_ne!(fa.config_hash, fb.config_hash);
        assert_ne!(fa.config_hash, fc.config_hash);
        assert_eq!(fa.config_hash, ProviderFingerprint::of(&a).config_hash);
        let dumped = serde_json::to_string(&fa).unwrap();
        assert!(!dumped.contains("sk-test"));
        assert_eq!(a.max_attempts(), 3);
        assert_eq!(c.max_attempts(), 1);
    }

    #[test]
    fn pricing_estimates_from_reported_usage() {
        let p = Pricing {
            currency: "USD".into(),
            input_per_1k_tokens: Decimal::new(5, 3),
            output_per_1k_tokens: Decimal::new(15, 3),
            effective_date: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        };
        let cost = p.estimate(&Usage {
            prompt_tokens: Some(2000),
            completion_tokens: Some(1000),
            total_tokens: Some(3000),
        });
        assert_eq!(cost, Decimal::new(25, 3));
    }
}
