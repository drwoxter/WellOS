//! Typed dispatch of one evaluation case to a [`ModelGateway`] and to the
//! deterministic baseline of the same operation.
//!
//! The case input *is* the operation's existing request contract (the same
//! whitelist DTO the product routes build), so the evaluation layer cannot
//! send a provider anything the product could not. Unknown fields are
//! rejected at deserialization time by [`CaseInput::parse`].

use dmind_gateway::access::{
    AccessIntentRequest, CapacityExplanationRequest, RankingRequest, RecoveryRankingRequest,
};
use dmind_gateway::diagnostics::{
    OrderSuggestionRequest, PatientExplanationRequest, ResultSynthesisRequest,
};
use dmind_gateway::notes::NoteDraftRequest;
use dmind_gateway::risk::RiskSummaryRequest;
use dmind_gateway::triage::TriageRequest;
use dmind_gateway::{
    access, diagnostics, hash_json, notes, risk, triage, GatewayError, ModelGateway, Operation,
    SummaryRequest, Usage,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use wellos_domain::ai::ProviderInfo;

/// Deterministic baseline identity recorded next to every execution.
pub const BASELINE_PROVIDER: &str = "wellos-deterministic";

/// One case's provider input, tagged by operation. The payload types are the
/// gateway request contracts themselves.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "operation", content = "request", rename_all = "snake_case")]
pub enum CaseInput {
    ResultSummary(SummaryRequest),
    TriageProposal(TriageRequest),
    RiskSummary(RiskSummaryRequest),
    NoteDraft(NoteDraftRequest),
    AccessIntent(AccessIntentRequest),
    AppointmentRanking(RankingRequest),
    CancellationRecovery(RecoveryRankingRequest),
    CapacityExplanation(CapacityExplanationRequest),
    DiagnosticOrderSuggestion(OrderSuggestionRequest),
    DiagnosticResultSynthesis(ResultSynthesisRequest),
    PatientResultExplanation(PatientExplanationRequest),
}

/// Typed output of one execution, flattened to the parts the rubric needs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseOutput {
    /// The typed structured output as JSON (never the raw provider text).
    pub output: Value,
    pub provider: ProviderInfo,
    pub prompt_version: String,
    pub input_hash: String,
    pub usage: Option<Usage>,
}

impl CaseInput {
    /// Strict parse: the request object must match the operation's contract
    /// exactly. Unknown fields anywhere in the payload are refused.
    pub fn parse(operation: Operation, request: &Value) -> Result<CaseInput, String> {
        let tagged = serde_json::json!({ "operation": operation.as_str(), "request": request });
        let parsed: CaseInput = serde_json::from_value(tagged).map_err(|e| e.to_string())?;
        if parsed.operation() != operation {
            return Err("operation tag mismatch".into());
        }
        // The request contracts tolerate unknown fields; the evaluation
        // layer does not. Anything the typed contract did not keep is an
        // unexpected field and is refused before any serialization.
        let kept = serde_json::to_value(&parsed).map_err(|e| e.to_string())?;
        let kept = kept.get("request").cloned().unwrap_or(Value::Null);
        unknown_fields(request, &kept, "request")?;
        Ok(parsed)
    }

    pub fn operation(&self) -> Operation {
        match self {
            CaseInput::ResultSummary(_) => Operation::ResultSummary,
            CaseInput::TriageProposal(_) => Operation::TriageProposal,
            CaseInput::RiskSummary(_) => Operation::RiskSummary,
            CaseInput::NoteDraft(_) => Operation::NoteDraft,
            CaseInput::AccessIntent(_) => Operation::AccessIntent,
            CaseInput::AppointmentRanking(_) => Operation::AppointmentRanking,
            CaseInput::CancellationRecovery(_) => Operation::CancellationRecovery,
            CaseInput::CapacityExplanation(_) => Operation::CapacityExplanation,
            CaseInput::DiagnosticOrderSuggestion(_) => Operation::DiagnosticOrderSuggestion,
            CaseInput::DiagnosticResultSynthesis(_) => Operation::DiagnosticResultSynthesis,
            CaseInput::PatientResultExplanation(_) => Operation::PatientResultExplanation,
        }
    }

    /// The language the case asks the provider to answer in. The patient
    /// explanation is bilingual by contract and carries no language field.
    pub fn language(&self) -> Option<&str> {
        match self {
            CaseInput::ResultSummary(r) => Some(&r.language),
            CaseInput::TriageProposal(r) => Some(&r.language),
            CaseInput::RiskSummary(r) => Some(&r.language),
            CaseInput::NoteDraft(r) => Some(&r.language),
            CaseInput::AccessIntent(r) => Some(&r.language),
            CaseInput::AppointmentRanking(r) => Some(&r.language),
            CaseInput::CancellationRecovery(r) => Some(&r.language),
            CaseInput::CapacityExplanation(r) => Some(&r.language),
            CaseInput::DiagnosticOrderSuggestion(r) => Some(&r.language),
            CaseInput::DiagnosticResultSynthesis(r) => Some(&r.language),
            CaseInput::PatientResultExplanation(_) => None,
        }
    }

    /// Stable hash of the exact payload that would be serialized for the
    /// provider (the request contract, nothing else).
    pub fn input_hash(&self) -> String {
        match self {
            CaseInput::ResultSummary(r) => hash_json(r),
            CaseInput::TriageProposal(r) => hash_json(r),
            CaseInput::RiskSummary(r) => hash_json(r),
            CaseInput::NoteDraft(r) => hash_json(r),
            CaseInput::AccessIntent(r) => hash_json(r),
            CaseInput::AppointmentRanking(r) => hash_json(r),
            CaseInput::CancellationRecovery(r) => hash_json(r),
            CaseInput::CapacityExplanation(r) => hash_json(r),
            CaseInput::DiagnosticOrderSuggestion(r) => hash_json(r),
            CaseInput::DiagnosticResultSynthesis(r) => hash_json(r),
            CaseInput::PatientResultExplanation(r) => hash_json(r),
        }
    }

    /// Evidence references the provider may cite: every `(reference, _)`
    /// fact pair plus the operation-specific identifiers (transcript
    /// segments, candidates, entries, components).
    pub fn evidence_refs(&self) -> Vec<String> {
        let facts = |f: &[(String, String)]| f.iter().map(|(r, _)| r.clone()).collect::<Vec<_>>();
        match self {
            CaseInput::ResultSummary(r) => facts(&r.facts),
            CaseInput::TriageProposal(r) => facts(&r.facts),
            CaseInput::RiskSummary(r) => facts(&r.facts),
            CaseInput::NoteDraft(r) => {
                let mut v = facts(&r.context_facts);
                v.extend(r.transcript.iter().map(|s| format!("segment:{}", s.index)));
                v
            }
            CaseInput::AccessIntent(r) => {
                let mut v = vec!["request:text".to_string()];
                v.extend(
                    r.services
                        .iter()
                        .map(|t| format!("catalog:service:{}", t.code)),
                );
                v.extend(
                    r.specialties
                        .iter()
                        .map(|t| format!("catalog:specialty:{}", t.code)),
                );
                v.extend(
                    r.modalities
                        .iter()
                        .map(|t| format!("catalog:modality:{}", t.code)),
                );
                v.extend(
                    r.facilities
                        .iter()
                        .map(|t| format!("catalog:facility:{}", t.code)),
                );
                v.extend(
                    r.accessibility
                        .iter()
                        .map(|t| format!("catalog:accessibility:{}", t.code)),
                );
                v
            }
            CaseInput::AppointmentRanking(r) => {
                let mut v = facts(&r.facts);
                v.extend(r.candidates.iter().map(|c| c.candidate_id.clone()));
                v
            }
            CaseInput::CancellationRecovery(r) => {
                let mut v = facts(&r.facts);
                v.extend(r.entries.iter().map(|e| format!("entry:{}", e.entry_id)));
                v
            }
            CaseInput::CapacityExplanation(r) => match &r.forecast {
                wellos_domain::capacity::Forecast::Ready { days, .. } => {
                    let mut v = vec!["forecast:summary".to_string()];
                    for d in days {
                        v.push(format!("forecast:day:{}", d.date));
                        v.extend(
                            d.factors
                                .iter()
                                .map(|f| format!("forecast:factor:{}", f.code)),
                        );
                    }
                    v
                }
                _ => vec!["forecast:summary".to_string()],
            },
            CaseInput::DiagnosticOrderSuggestion(r) => {
                let mut v = facts(&r.facts);
                v.extend(r.candidates.iter().map(|c| format!("catalog:{}", c.code)));
                v
            }
            CaseInput::DiagnosticResultSynthesis(r) => {
                let mut v = vec![r.report_ref.clone()];
                v.extend(r.components.iter().map(|c| c.component_ref.clone()));
                v.extend(facts(&r.facts));
                v
            }
            CaseInput::PatientResultExplanation(r) => {
                let mut v = vec![r.report_ref.clone()];
                v.extend(facts(&r.facts));
                v
            }
        }
    }

    /// Execute against `gateway` (the provider under evaluation).
    pub async fn execute(&self, gateway: &dyn ModelGateway) -> Result<CaseOutput, GatewayError> {
        fn out<T: Serialize>(
            output: &T,
            provider: ProviderInfo,
            prompt_version: &str,
            input_hash: &str,
            usage: Option<Usage>,
        ) -> CaseOutput {
            CaseOutput {
                output: serde_json::to_value(output).expect("serializable output"),
                provider,
                prompt_version: prompt_version.to_string(),
                input_hash: input_hash.to_string(),
                usage,
            }
        }
        let info = gateway.info();
        Ok(match self {
            CaseInput::ResultSummary(r) => {
                let g = gateway.summarize_result(r).await?;
                let provider = ProviderInfo {
                    provider: g.route.clone(),
                    model: g.model.clone(),
                    model_version: g.model_version.clone(),
                };
                out(
                    &g.output,
                    provider,
                    &g.prompt_version,
                    &g.input_hash,
                    g.usage,
                )
            }
            CaseInput::TriageProposal(r) => {
                let g = gateway.propose_triage(r).await?;
                let provider = ProviderInfo {
                    provider: g.route.clone(),
                    model: g.model.clone(),
                    model_version: g.model_version.clone(),
                };
                out(
                    &g.output,
                    provider,
                    &g.prompt_version,
                    &g.input_hash,
                    g.usage,
                )
            }
            CaseInput::RiskSummary(r) => {
                let g = gateway.summarize_risk(r).await?;
                let provider = ProviderInfo {
                    provider: g.route.clone(),
                    model: g.model.clone(),
                    model_version: g.model_version.clone(),
                };
                out(
                    &g.output,
                    provider,
                    &g.prompt_version,
                    &g.input_hash,
                    g.usage,
                )
            }
            CaseInput::NoteDraft(r) => {
                let g = gateway.draft_note(r).await?;
                let body = serde_json::json!({
                    "schema_version": wellos_domain::ai::SCRIBE_DRAFT_SCHEMA,
                    "sections": g.sections,
                    "flags": g.flags,
                    "limitations": g.limitations,
                });
                out(&body, g.provider, &g.prompt_version, &g.input_hash, g.usage)
            }
            CaseInput::AccessIntent(r) => {
                let g = gateway.interpret_access_intent(r).await?;
                out(
                    &g.output,
                    g.provider,
                    &g.prompt_version,
                    &g.input_hash,
                    g.usage,
                )
            }
            CaseInput::AppointmentRanking(r) => {
                let g = gateway.rank_appointments(r).await?;
                out(
                    &g.output,
                    g.provider,
                    &g.prompt_version,
                    &g.input_hash,
                    g.usage,
                )
            }
            CaseInput::CancellationRecovery(r) => {
                let g = gateway.rank_cancellation_recovery(r).await?;
                out(
                    &g.output,
                    g.provider,
                    &g.prompt_version,
                    &g.input_hash,
                    g.usage,
                )
            }
            CaseInput::CapacityExplanation(r) => {
                let g = gateway.explain_capacity(r).await?;
                out(
                    &g.output,
                    g.provider,
                    &g.prompt_version,
                    &g.input_hash,
                    g.usage,
                )
            }
            CaseInput::DiagnosticOrderSuggestion(r) => {
                let g = gateway.suggest_orders(r).await?;
                out(
                    &g.output,
                    g.provider,
                    &g.prompt_version,
                    &g.input_hash,
                    g.usage,
                )
            }
            CaseInput::DiagnosticResultSynthesis(r) => {
                let g = gateway.synthesize_result(r).await?;
                out(
                    &g.output,
                    g.provider,
                    &g.prompt_version,
                    &g.input_hash,
                    g.usage,
                )
            }
            CaseInput::PatientResultExplanation(r) => {
                let g = gateway.explain_result_for_patient(r).await?;
                out(
                    &g.output,
                    g.provider,
                    &g.prompt_version,
                    &g.input_hash,
                    g.usage,
                )
            }
        }
        .with_default_provider(info))
    }

    /// The deterministic baseline every workflow falls back to when the
    /// provider is unavailable. Pure, offline, no feature flag.
    pub fn baseline(&self) -> Result<CaseOutput, GatewayError> {
        let info = ProviderInfo {
            provider: BASELINE_PROVIDER.into(),
            model: "deterministic".into(),
            model_version: "rules".into(),
        };
        let hash = self.input_hash();
        let wrap = |output: Value, prompt_version: String| CaseOutput {
            output,
            provider: info.clone(),
            prompt_version,
            input_hash: hash.clone(),
            usage: None,
        };
        Ok(match self {
            CaseInput::ResultSummary(r) => wrap(
                serde_json::to_value(deterministic_result_summary(r)).expect("serializable"),
                "result-summary-deterministic.v1".into(),
            ),
            CaseInput::TriageProposal(r) => {
                let g = triage::propose(r)?;
                wrap(
                    serde_json::to_value(g.output).expect("serializable"),
                    g.prompt_version,
                )
            }
            CaseInput::RiskSummary(r) => {
                let g = risk::summarize(r)?;
                wrap(
                    serde_json::to_value(g.output).expect("serializable"),
                    g.prompt_version,
                )
            }
            CaseInput::NoteDraft(r) => {
                let g = notes::deterministic_draft(r)?;
                wrap(
                    serde_json::json!({
                        "schema_version": wellos_domain::ai::SCRIBE_DRAFT_SCHEMA,
                        "sections": g.sections,
                        "flags": g.flags,
                        "limitations": g.limitations,
                    }),
                    g.prompt_version,
                )
            }
            CaseInput::AccessIntent(r) => {
                let g = access::deterministic_intent(r, info.clone())?;
                wrap(
                    serde_json::to_value(g.output).expect("serializable"),
                    g.prompt_version,
                )
            }
            CaseInput::AppointmentRanking(r) => {
                let g = access::deterministic_ranking(r, info.clone())?;
                wrap(
                    serde_json::to_value(g.output).expect("serializable"),
                    g.prompt_version,
                )
            }
            CaseInput::CancellationRecovery(r) => {
                let g = access::deterministic_recovery(r, info.clone())?;
                wrap(
                    serde_json::to_value(g.output).expect("serializable"),
                    g.prompt_version,
                )
            }
            CaseInput::CapacityExplanation(r) => {
                let g = access::deterministic_capacity(r, info.clone())?;
                wrap(
                    serde_json::to_value(g.output).expect("serializable"),
                    g.prompt_version,
                )
            }
            CaseInput::DiagnosticOrderSuggestion(r) => {
                let g = diagnostics::deterministic_suggestion(r, info.clone())?;
                wrap(
                    serde_json::to_value(g.output).expect("serializable"),
                    g.prompt_version,
                )
            }
            CaseInput::DiagnosticResultSynthesis(r) => {
                let g = diagnostics::deterministic_synthesis(r, info.clone())?;
                wrap(
                    serde_json::to_value(g.output).expect("serializable"),
                    g.prompt_version,
                )
            }
            CaseInput::PatientResultExplanation(r) => {
                let g = diagnostics::deterministic_explanation(r, info.clone())?;
                wrap(
                    serde_json::to_value(g.output).expect("serializable"),
                    g.prompt_version,
                )
            }
        })
    }
}

impl CaseOutput {
    fn with_default_provider(mut self, info: ProviderInfo) -> Self {
        if self.provider.provider.is_empty() {
            self.provider = info;
        }
        self
    }
}

/// Deterministic result-summary baseline: restates every fact with its
/// reference, cites all of them and claims nothing else. Mirrors the
/// fixture provider's template without depending on `dev-fixtures`.
pub fn deterministic_result_summary(req: &SummaryRequest) -> wellos_domain::ai::ResultSummaryV1 {
    let es = req.language.starts_with("es");
    let summary = if req.facts.is_empty() {
        if es {
            "No hay hechos de origen disponibles; no se puede resumir.".to_string()
        } else {
            "No source facts are available; nothing can be summarized.".to_string()
        }
    } else {
        req.facts
            .iter()
            .map(|(r, s)| format!("{s} [{r}]"))
            .collect::<Vec<_>>()
            .join(" ")
    };
    wellos_domain::ai::ResultSummaryV1 {
        schema_version: "result-summary.v1".into(),
        summary,
        relevant_trend: None,
        cited_sources: req.facts.iter().map(|(r, _)| r.clone()).collect(),
        limitations: {
            let mut l = vec![if es {
                "Resumen determinista de hechos autorizados; no es una interpretación clínica."
                    .to_string()
            } else {
                "Deterministic restatement of authorized facts; not a clinical interpretation."
                    .to_string()
            }];
            if req.facts.is_empty() {
                l.push(if es {
                    "Faltan hechos de origen: información insuficiente.".to_string()
                } else {
                    "Source facts missing: insufficient information.".to_string()
                });
            }
            l
        },
        suggested_next_step_categories: vec![],
    }
}

/// Every key present in `input` must survive the typed round trip in `kept`.
fn unknown_fields(input: &Value, kept: &Value, path: &str) -> Result<(), String> {
    match (input, kept) {
        (Value::Object(i), Value::Object(k)) => {
            for (key, value) in i {
                match k.get(key) {
                    Some(kv) => unknown_fields(value, kv, &format!("{path}.{key}"))?,
                    None => return Err(format!("unexpected field {path}.{key}")),
                }
            }
            Ok(())
        }
        (Value::Array(i), Value::Array(k)) if i.len() == k.len() => {
            for (n, (iv, kv)) in i.iter().zip(k).enumerate() {
                unknown_fields(iv, kv, &format!("{path}[{n}]"))?;
            }
            Ok(())
        }
        _ => Ok(()),
    }
}
