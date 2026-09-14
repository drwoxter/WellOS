//! Real external model provider: an OpenAI-compatible `chat/completions`
//! endpoint driven with JSON-only structured prompts.
//!
//! Security boundaries (the server validates the endpoint before
//! constructing this adapter; see `wellos_server::state`):
//! - the destination is pinned: redirects are refused, so a compromised
//!   endpoint cannot forward prompts or the credential elsewhere;
//! - connect and request timeouts, a response-size ceiling and a bounded
//!   number of retries (only for transient failures) cap every call;
//! - a semaphore bounds in-flight calls per process; tenant/task quotas are
//!   enforced by the server against persisted artifacts;
//! - errors never carry the URL, the credential, prompts or model text.
//!
//! Every operation renders a typed request into a prompt, requires a single
//! JSON object back, parses it into the versioned domain schema and rejects
//! anything that fails validation — including evidence references that do
//! not point at facts or transcript segments actually supplied. Nothing is
//! ever substituted for a failed or invalid response.

use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Semaphore;
use wellos_domain::ai::{
    Confidence, ProviderInfo, ResultSummaryV1, ScribeFlag, ScribeFlagKind, ScribeSection,
};
use wellos_domain::risk::RISK_SUMMARY_SCHEMA;
use wellos_domain::triage::{safety_floor, Priority, TriageProposalV1, TRIAGE_PROPOSAL_SCHEMA};

use crate::notes::{
    validate_note_draft, NoteDraftRequest, NoteDraftResponse, CLINICIAN_JUDGEMENT_SECTIONS,
    NOTE_DRAFT_TEMPLATE,
};
use crate::risk::{parse_summary, risk_input_hash, RiskSummaryRequest, RiskSummaryResponse};
use crate::triage::{triage_input_hash, TriageRequest, TriageResponse};
use crate::{
    input_hash, CapabilityStatus, GatewayError, GatewayResponse, ModelGateway, Operation,
    ProviderHealth, SummaryRequest, Usage,
};

pub const PROVIDER_NAME: &str = "openai-compatible";
pub const RESULT_PROMPT_VERSION: &str = "result-summary-openai.v1";
pub const TRIAGE_PROMPT_VERSION: &str = "triage-proposal-openai.v1";
pub const RISK_PROMPT_VERSION: &str = "risk-summary-openai.v1";
pub const NOTE_PROMPT_VERSION: &str = "note-draft-openai.v1";

/// How long after the last failure the capability keeps reporting
/// `degraded` when no call has succeeded since.
const MAX_SUMMARY_CHARS: usize = 4_000;
const MAX_LIST_ITEMS: usize = 20;
const MAX_ITEM_CHARS: usize = 600;

#[derive(Clone)]
pub struct OpenAiCompatibleModelConfig {
    /// Full chat-completions URL, e.g. `https://host/v1/chat/completions`.
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
    pub connect_timeout: Duration,
    pub timeout: Duration,
    pub max_retries: u32,
    pub retry_backoff: Duration,
    pub max_response_bytes: usize,
    pub max_concurrency: usize,
    /// Longest a call waits for a concurrency slot before reporting
    /// unavailability instead of queuing unboundedly.
    pub queue_timeout: Duration,
}

impl std::fmt::Debug for OpenAiCompatibleModelConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatibleModelConfig")
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .field("timeout", &self.timeout)
            .field("max_retries", &self.max_retries)
            .field("max_response_bytes", &self.max_response_bytes)
            .field("max_concurrency", &self.max_concurrency)
            .finish()
    }
}

pub struct OpenAiCompatibleModel {
    cfg: OpenAiCompatibleModelConfig,
    client: reqwest::Client,
    slots: Semaphore,
    health: ProviderHealth,
}

#[derive(Deserialize)]
struct ChatResponse {
    #[serde(default)]
    choices: Vec<ChatChoice>,
    usage: Option<RawUsage>,
}

#[derive(Deserialize)]
struct ChatChoice {
    message: ChatMessage,
}

#[derive(Deserialize)]
struct ChatMessage {
    content: Option<String>,
}

#[derive(Deserialize)]
struct RawUsage {
    prompt_tokens: Option<u64>,
    completion_tokens: Option<u64>,
    total_tokens: Option<u64>,
}

enum Retry {
    Yes(GatewayError),
    No(GatewayError),
}

impl OpenAiCompatibleModel {
    pub fn new(cfg: OpenAiCompatibleModelConfig) -> Result<Self, GatewayError> {
        if cfg.max_concurrency == 0 {
            return Err(GatewayError::Unavailable(
                "max_concurrency must be at least 1".into(),
            ));
        }
        let client = reqwest::Client::builder()
            .connect_timeout(cfg.connect_timeout)
            .timeout(cfg.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| GatewayError::Unavailable(format!("http client: {e}")))?;
        Ok(Self {
            slots: Semaphore::new(cfg.max_concurrency),
            cfg,
            client,
            health: ProviderHealth::new(),
        })
    }

    fn record_success(&self) {
        self.health.record_success();
    }

    fn record_failure(&self) {
        self.health.record_failure();
    }

    async fn attempt(&self, body: &Value) -> Result<(Value, Option<Usage>), Retry> {
        let res = self
            .client
            .post(&self.cfg.endpoint)
            .bearer_auth(&self.cfg.api_key)
            .json(body)
            .send()
            .await
            .map_err(|e| {
                // Never include the error's display text: it can embed the URL.
                let reason = if e.is_timeout() {
                    "timeout"
                } else if e.is_connect() {
                    "connection failed"
                } else {
                    "request failed"
                };
                Retry::Yes(GatewayError::Unavailable(reason.into()))
            })?;
        let status = res.status();
        if status.is_server_error() || status.as_u16() == 429 {
            return Err(Retry::Yes(GatewayError::Unavailable(format!(
                "provider status {}",
                status.as_u16()
            ))));
        }
        if !status.is_success() {
            return Err(Retry::No(GatewayError::Unavailable(format!(
                "provider status {}",
                status.as_u16()
            ))));
        }
        if let Some(len) = res.content_length() {
            if len > self.cfg.max_response_bytes as u64 {
                return Err(Retry::No(GatewayError::InvalidOutput(
                    "response exceeds size limit".into(),
                )));
            }
        }
        let mut res = res;
        let mut buf: Vec<u8> = Vec::new();
        loop {
            let chunk = res.chunk().await.map_err(|e| {
                let reason = if e.is_timeout() {
                    "timeout"
                } else {
                    "response read failed"
                };
                Retry::Yes(GatewayError::Unavailable(reason.into()))
            })?;
            let Some(chunk) = chunk else { break };
            if buf.len() + chunk.len() > self.cfg.max_response_bytes {
                return Err(Retry::No(GatewayError::InvalidOutput(
                    "response exceeds size limit".into(),
                )));
            }
            buf.extend_from_slice(&chunk);
        }
        let parsed: ChatResponse = serde_json::from_slice(&buf).map_err(|_| {
            Retry::No(GatewayError::InvalidOutput(
                "response is not a chat completion".into(),
            ))
        })?;
        let content = parsed
            .choices
            .into_iter()
            .next()
            .and_then(|c| c.message.content)
            .ok_or_else(|| Retry::No(GatewayError::InvalidOutput("empty completion".into())))?;
        let object = parse_json_object(&content).ok_or_else(|| {
            Retry::No(GatewayError::InvalidOutput(
                "completion is not a JSON object".into(),
            ))
        })?;
        let usage = parsed.usage.map(|u| Usage {
            prompt_tokens: u.prompt_tokens,
            completion_tokens: u.completion_tokens,
            total_tokens: u.total_tokens,
        });
        Ok((object, usage))
    }

    /// One structured completion with bounded retries and concurrency.
    async fn complete(
        &self,
        system: &str,
        user: &Value,
    ) -> Result<(Value, Option<Usage>), GatewayError> {
        let permit = tokio::time::timeout(self.cfg.queue_timeout, self.slots.acquire())
            .await
            .map_err(|_| GatewayError::Unavailable("provider concurrency limit reached".into()))?
            .map_err(|_| GatewayError::Unavailable("provider closed".into()))?;
        let body = json!({
            "model": self.cfg.model,
            "temperature": 0,
            "response_format": { "type": "json_object" },
            "messages": [
                { "role": "system", "content": system },
                { "role": "user", "content": user.to_string() },
            ],
        });
        let mut attempt = 0u32;
        let result = loop {
            match self.attempt(&body).await {
                Ok(v) => break Ok(v),
                Err(Retry::No(e)) => break Err(e),
                Err(Retry::Yes(e)) => {
                    if attempt >= self.cfg.max_retries {
                        break Err(e);
                    }
                    attempt += 1;
                    tracing::warn!(attempt, "model provider retry");
                    tokio::time::sleep(self.cfg.retry_backoff * attempt).await;
                }
            }
        };
        drop(permit);
        match &result {
            Ok(_) => self.record_success(),
            Err(_) => self.record_failure(),
        }
        result
    }
}

/// Extract the single JSON object from a completion, tolerating a fenced
/// code block around it. Anything else is rejected.
fn parse_json_object(content: &str) -> Option<Value> {
    let trimmed = content.trim();
    let candidate = if trimmed.starts_with("```") {
        let inner = trimmed.trim_start_matches("```");
        let inner = inner.strip_prefix("json").unwrap_or(inner);
        inner.trim_end_matches("```").trim()
    } else {
        trimmed
    };
    let value: Value = serde_json::from_str(candidate).ok()?;
    value.is_object().then_some(value)
}

fn language_instruction(language: &str) -> String {
    format!(
        "Write every free-text field in the language with BCP-47 tag \"{}\".",
        language.replace('"', "")
    )
}

const COMMON_RULES: &str = "You are dMind, the assistive documentation component of a hospital \
operating system. You never diagnose, never prescribe, never recommend a specific treatment, \
dose or order, and never make insurance, coverage, pricing, authorization or denial decisions. \
You only restate, organize and explain the information you were given. Every claim must be \
traceable to the supplied evidence; do not add facts. Respond with exactly one JSON object and \
nothing else. Do not include markdown.";

fn check_list(items: &[String], name: &str) -> Result<(), GatewayError> {
    if items.len() > MAX_LIST_ITEMS {
        return Err(GatewayError::InvalidOutput(format!(
            "{name} has too many items"
        )));
    }
    if items
        .iter()
        .any(|s| s.trim().is_empty() || s.chars().count() > MAX_ITEM_CHARS)
    {
        return Err(GatewayError::InvalidOutput(format!(
            "{name} contains an empty or oversized item"
        )));
    }
    Ok(())
}

/// Every reference cited by the model must be one of the references
/// actually supplied to it; duplicates are collapsed.
fn check_citations(
    cited: &[String],
    allowed: &[(String, String)],
    name: &str,
) -> Result<Vec<String>, GatewayError> {
    let mut out: Vec<String> = Vec::new();
    for c in cited {
        if !allowed.iter().any(|(r, _)| r == c) {
            return Err(GatewayError::InvalidOutput(format!(
                "{name} cites a reference that was not supplied"
            )));
        }
        if !out.contains(c) {
            out.push(c.clone());
        }
    }
    Ok(out)
}

fn is_slug(s: &str) -> bool {
    let len = s.len();
    (1..=40).contains(&len)
        && s.chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_')
}

fn external_limitation(language: &str) -> String {
    if language.starts_with("es") {
        "Generado por un modelo de lenguaje externo a partir únicamente de los datos autorizados; no es una interpretación clínica y requiere revisión profesional.".into()
    } else {
        "Generated by an external language model from the authorized data only; not a clinical interpretation and requires professional review.".into()
    }
}

fn facts_json(facts: &[(String, String)]) -> Value {
    Value::Array(
        facts
            .iter()
            .map(|(r, s)| json!({ "reference": r, "statement": s }))
            .collect(),
    )
}

#[derive(Deserialize)]
struct RawSummary {
    summary: String,
    #[serde(default)]
    relevant_trend: Option<String>,
    #[serde(default)]
    cited_sources: Vec<String>,
    #[serde(default)]
    limitations: Vec<String>,
    #[serde(default)]
    suggested_next_step_categories: Vec<String>,
}

#[derive(Deserialize)]
struct RawTriage {
    proposed_priority: Priority,
    proposed_service: String,
    #[serde(default)]
    important_facts: Vec<String>,
    #[serde(default)]
    missing_information: Vec<String>,
    #[serde(default)]
    contradictions: Vec<String>,
    handoff_summary: String,
    #[serde(default)]
    rationale: Vec<String>,
    confidence: Confidence,
    #[serde(default)]
    limitations: Vec<String>,
    #[serde(default)]
    cited_sources: Vec<String>,
}

#[derive(Deserialize)]
struct RawDraft {
    #[serde(default)]
    sections: Vec<RawSection>,
    #[serde(default)]
    flags: Vec<RawFlag>,
    #[serde(default)]
    limitations: Vec<String>,
}

#[derive(Deserialize)]
struct RawSection {
    section: String,
    text: String,
    confidence: Option<Confidence>,
    #[serde(default)]
    review_needed: bool,
    #[serde(default)]
    reasons: Vec<String>,
    #[serde(default)]
    segments: Vec<u32>,
}

#[derive(Deserialize)]
struct RawFlag {
    kind: ScribeFlagKind,
    message: String,
    #[serde(default)]
    segments: Vec<u32>,
    #[serde(default)]
    sections: Vec<String>,
}

fn invalid(e: impl std::fmt::Display) -> GatewayError {
    GatewayError::InvalidOutput(e.to_string())
}

#[async_trait]
impl ModelGateway for OpenAiCompatibleModel {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            provider: PROVIDER_NAME.into(),
            model: self.cfg.model.clone(),
            model_version: "api".into(),
        }
    }

    fn status(&self) -> CapabilityStatus {
        let (state, reason) = self.health.state();
        CapabilityStatus {
            state,
            provider: PROVIDER_NAME.into(),
            model: Some(self.cfg.model.clone()),
            reason,
            external: true,
            synthetic: false,
        }
    }

    fn prompt_version(&self, op: Operation) -> String {
        match op {
            Operation::ResultSummary => RESULT_PROMPT_VERSION,
            Operation::TriageProposal => TRIAGE_PROMPT_VERSION,
            Operation::RiskSummary => RISK_PROMPT_VERSION,
            Operation::NoteDraft => NOTE_PROMPT_VERSION,
        }
        .into()
    }

    async fn summarize_result(
        &self,
        req: &SummaryRequest,
    ) -> Result<GatewayResponse, GatewayError> {
        let system = format!(
            "{COMMON_RULES} Task: summarize the supplied diagnostic-result facts for a clinician. \
             {} Output schema: {{\"summary\": string, \"relevant_trend\": string|null, \
             \"cited_sources\": string[] (only references from the input), \"limitations\": string[], \
             \"suggested_next_step_categories\": string[] (short lowercase category slugs such as \
             \"repeat-test\"; never orders or prescriptions)}}.",
            language_instruction(&req.language)
        );
        let user = json!({
            "template": req.template,
            "language": req.language,
            "facts": facts_json(&req.facts),
        });
        let (raw, usage) = self.complete(&system, &user).await?;
        let parsed: RawSummary = serde_json::from_value(raw).map_err(invalid)?;
        if parsed.summary.trim().is_empty() || parsed.summary.chars().count() > MAX_SUMMARY_CHARS {
            return Err(GatewayError::InvalidOutput(
                "summary is empty or oversized".into(),
            ));
        }
        let cited_sources = check_citations(&parsed.cited_sources, &req.facts, "summary")?;
        if cited_sources.is_empty() && !req.facts.is_empty() {
            return Err(GatewayError::InvalidOutput(
                "summary cites no source".into(),
            ));
        }
        check_list(&parsed.limitations, "limitations")?;
        check_list(
            &parsed.suggested_next_step_categories,
            "suggested_next_step_categories",
        )?;
        if !parsed
            .suggested_next_step_categories
            .iter()
            .all(|c| is_slug(c))
        {
            return Err(GatewayError::InvalidOutput(
                "next-step categories must be short slugs".into(),
            ));
        }
        let mut limitations = parsed.limitations;
        limitations.push(external_limitation(&req.language));
        self.record_success();
        Ok(GatewayResponse {
            output: ResultSummaryV1 {
                schema_version: "result-summary.v1".into(),
                summary: parsed.summary,
                relevant_trend: parsed.relevant_trend.filter(|t| !t.trim().is_empty()),
                cited_sources,
                limitations,
                suggested_next_step_categories: parsed.suggested_next_step_categories,
            },
            model: self.cfg.model.clone(),
            model_version: "api".into(),
            route: PROVIDER_NAME.into(),
            prompt_version: RESULT_PROMPT_VERSION.into(),
            input_hash: input_hash(req),
            usage,
        })
    }

    async fn propose_triage(&self, req: &TriageRequest) -> Result<TriageResponse, GatewayError> {
        let system = format!(
            "{COMMON_RULES} Task: propose an operational triage priority and destination service \
             from the supplied arrival facts, for a triage professional's decision. {} \
             Allowed priorities: \"non_urgent\", \"standard\", \"urgent\", \"immediate\". Allowed services: {}. \
             Output schema: {{\"proposed_priority\": string, \"proposed_service\": string, \
             \"important_facts\": string[], \"missing_information\": string[], \"contradictions\": string[], \
             \"handoff_summary\": string, \"rationale\": string[], \"confidence\": \"low\"|\"medium\"|\"high\", \
             \"limitations\": string[], \"cited_sources\": string[] (only references from the input)}}.",
            language_instruction(&req.language),
            wellos_domain::triage::SERVICES
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let user = json!({
            "template": req.template,
            "language": req.language,
            "arrival_kind": req.arrival_kind,
            "age_years": req.age_years,
            "reason": req.reason,
            "concerns": req.concerns,
            "onset": req.onset,
            "red_flags": req.red_flags,
            "vitals": req.vitals,
            "allergies": req.allergies,
            "requested_service": req.requested_service,
            "facts": facts_json(&req.facts),
        });
        let (raw, usage) = self.complete(&system, &user).await?;
        let parsed: RawTriage = serde_json::from_value(raw).map_err(invalid)?;
        for (items, name) in [
            (&parsed.important_facts, "important_facts"),
            (&parsed.missing_information, "missing_information"),
            (&parsed.contradictions, "contradictions"),
            (&parsed.rationale, "rationale"),
            (&parsed.limitations, "limitations"),
        ] {
            check_list(items, name)?;
        }
        if parsed.handoff_summary.chars().count() > MAX_SUMMARY_CHARS {
            return Err(GatewayError::InvalidOutput(
                "handoff summary oversized".into(),
            ));
        }
        let cited_sources = check_citations(&parsed.cited_sources, &req.facts, "triage proposal")?;
        let (floor, _) = safety_floor(req.arrival_kind, &req.red_flags, &req.vitals);
        let mut limitations = parsed.limitations;
        limitations.push(external_limitation(&req.language));
        let output = TriageProposalV1 {
            schema_version: TRIAGE_PROPOSAL_SCHEMA.into(),
            proposed_priority: parsed.proposed_priority,
            safety_floor: floor,
            raised_to_floor: false,
            proposed_service: parsed.proposed_service,
            important_facts: parsed.important_facts,
            missing_information: parsed.missing_information,
            contradictions: parsed.contradictions,
            handoff_summary: parsed.handoff_summary,
            rationale: parsed.rationale,
            confidence: parsed.confidence,
            limitations,
            cited_sources,
        }
        .clamp_to_floor(floor);
        output.validate().map_err(GatewayError::InvalidOutput)?;
        Ok(TriageResponse {
            output,
            model: self.cfg.model.clone(),
            model_version: "api".into(),
            route: PROVIDER_NAME.into(),
            prompt_version: TRIAGE_PROMPT_VERSION.into(),
            input_hash: triage_input_hash(req),
            usage,
        })
    }

    async fn summarize_risk(
        &self,
        req: &RiskSummaryRequest,
    ) -> Result<RiskSummaryResponse, GatewayError> {
        let det = &req.assessment;
        let system = format!(
            "{COMMON_RULES} Task: explain a deterministic risk assessment to a clinician. The levels \
             were computed by versioned rules and are authoritative: repeat them exactly, never \
             lower or raise them. {} Output schema: {{\"overall_level\": string, \
             \"domains\": [{{\"domain\": string (exactly the supplied domain codes, each once), \
             \"level\": string (the supplied level), \"summary\": string, \"reasons\": string[] (one per \
             contributing factor, same order), \"cited_sources\": string[] (only references from the \
             input facts)}}], \"missing_information\": string[], \"contradictions\": string[], \
             \"follow_up_suggestions\": [{{\"category\": \"review_result\"|\"medication_reconciliation\"|\
             \"schedule_follow_up\"|\"preventive_screening\"|\"care_team_assignment\"|\"complete_record\"|\
             \"discuss_with_patient\", \"domain\": string, \"text\": string}}], \"limitations\": string[], \
             \"cited_sources\": string[], \"confidence\": \"low\"|\"medium\"|\"high\"}}. A factor derived \
             from the absence of data cites the matching \"rule:<rules_version>:<domain>\" reference \
             listed under rule_references.",
            language_instruction(&req.language)
        );
        let mut allowed: Vec<(String, String)> = req.facts.clone();
        for d in &det.domains {
            allowed.push((
                format!("rule:{}:{}", det.rules_version, d.domain.as_str()),
                "deterministic rule".into(),
            ));
        }
        let rule_references: Vec<String> = allowed[req.facts.len()..]
            .iter()
            .map(|(r, _)| r.clone())
            .collect();
        let user = json!({
            "template": req.template,
            "language": req.language,
            "assessment": det,
            "facts": facts_json(&req.facts),
            "rule_references": rule_references,
        });
        let (mut raw, usage) = self.complete(&system, &user).await?;
        {
            let obj = raw
                .as_object_mut()
                .ok_or_else(|| invalid("not an object"))?;
            // Provenance and invariants are set by WellOS, never by the model.
            obj.insert("schema_version".into(), json!(RISK_SUMMARY_SCHEMA));
            obj.insert("ai_generated".into(), json!(true));
            obj.insert("rules_version".into(), json!(det.rules_version));
            obj.insert("safety_floor".into(), json!(det.safety_floor));
            obj.insert("raised_to_floor".into(), json!(false));
            if let Some(list) = obj
                .get_mut("follow_up_suggestions")
                .and_then(Value::as_array_mut)
            {
                for s in list.iter_mut() {
                    if let Some(s) = s.as_object_mut() {
                        s.insert("requires_confirmation".into(), json!(true));
                    }
                }
            }
            let mut limitations: Vec<String> = obj
                .get("limitations")
                .and_then(Value::as_array)
                .map(|a| {
                    a.iter()
                        .filter_map(|v| v.as_str().map(str::to_string))
                        .collect()
                })
                .unwrap_or_default();
            limitations.push(external_limitation(&req.language));
            obj.insert("limitations".into(), json!(limitations));
        }
        let output = parse_summary(&raw, det)?;
        let mut cited = output.cited_sources.clone();
        for d in &output.domains {
            cited.extend(d.cited_sources.iter().cloned());
            check_list(&d.reasons, "domain reasons")?;
            if d.summary.chars().count() > MAX_SUMMARY_CHARS {
                return Err(GatewayError::InvalidOutput(
                    "domain summary oversized".into(),
                ));
            }
        }
        check_citations(&cited, &allowed, "risk summary")?;
        check_list(&output.missing_information, "missing_information")?;
        check_list(&output.contradictions, "contradictions")?;
        check_list(&output.limitations, "limitations")?;
        if output.follow_up_suggestions.len() > MAX_LIST_ITEMS
            || output
                .follow_up_suggestions
                .iter()
                .any(|s| s.text.trim().is_empty() || s.text.chars().count() > MAX_ITEM_CHARS)
        {
            return Err(GatewayError::InvalidOutput(
                "follow-up suggestions are empty, oversized or too many".into(),
            ));
        }
        Ok(RiskSummaryResponse {
            output,
            model: self.cfg.model.clone(),
            model_version: "api".into(),
            route: PROVIDER_NAME.into(),
            prompt_version: RISK_PROMPT_VERSION.into(),
            input_hash: risk_input_hash(req),
            usage,
        })
    }

    async fn draft_note(&self, req: &NoteDraftRequest) -> Result<NoteDraftResponse, GatewayError> {
        if req.template != NOTE_DRAFT_TEMPLATE {
            return Err(GatewayError::PolicyDenied(format!(
                "unsupported template {}",
                req.template
            )));
        }
        if req.transcript.is_empty() {
            return Err(GatewayError::InvalidOutput("transcript is empty".into()));
        }
        let system = format!(
            "{COMMON_RULES} Task: organize a consultation transcript into draft note sections for \
             the treating clinician's review. Only restate what was said; never infer a diagnosis, \
             add findings or propose treatment. Each section must cite the transcript segment \
             indexes it restates. {} Allowed section codes, in order: {}. Output schema: \
             {{\"sections\": [{{\"section\": string, \"text\": string, \"confidence\": \"low\"|\"medium\"|\"high\", \
             \"review_needed\": boolean, \"reasons\": string[], \"segments\": integer[]}}], \
             \"flags\": [{{\"kind\": \"contradiction\"|\"uncertain\", \"message\": string, \"segments\": integer[], \
             \"sections\": string[]}}], \"limitations\": string[]}}. Omit sections the conversation \
             does not cover. Mark review_needed when medications, doses, numeric values or \
             clinical impressions are restated.",
            language_instruction(&req.language),
            wellos_domain::ai::NOTE_SECTIONS
                .iter()
                .map(|s| format!("\"{s}\""))
                .collect::<Vec<_>>()
                .join(", ")
        );
        let user = json!({
            "template": req.template,
            "language": req.language,
            "transcript": req.transcript.iter().map(|s| json!({
                "index": s.index,
                "speaker": s.speaker,
                "text": s.text,
            })).collect::<Vec<_>>(),
            "context": facts_json(&req.context_facts),
        });
        let (raw, usage) = self.complete(&system, &user).await?;
        let parsed: RawDraft = serde_json::from_value(raw).map_err(invalid)?;
        let sections: Vec<ScribeSection> = parsed
            .sections
            .into_iter()
            .map(|s| {
                let judgement = CLINICIAN_JUDGEMENT_SECTIONS.contains(&s.section.as_str());
                let mut reasons = s.reasons;
                if judgement && !reasons.iter().any(|r| r == "clinician_judgement") {
                    reasons.push("clinician_judgement".into());
                }
                ScribeSection {
                    section: s.section,
                    text: s.text,
                    confidence: s.confidence.unwrap_or(Confidence::Medium),
                    review_needed: s.review_needed || judgement,
                    reasons,
                    segments: s.segments,
                }
            })
            .collect();
        let flags: Vec<ScribeFlag> = parsed
            .flags
            .into_iter()
            .map(|f| ScribeFlag {
                kind: f.kind,
                message: f.message,
                segments: f.segments,
                sections: f.sections,
            })
            .collect();
        validate_note_draft(&sections, &flags, req.transcript.len())
            .map_err(GatewayError::InvalidOutput)?;
        for s in &sections {
            check_list(&s.reasons, "section reasons")?;
        }
        check_list(&parsed.limitations, "limitations")?;
        let mut limitations = parsed.limitations;
        limitations.push(external_limitation(&req.language));
        Ok(NoteDraftResponse {
            sections,
            flags,
            limitations,
            provider: self.info(),
            prompt_version: NOTE_PROMPT_VERSION.into(),
            input_hash: crate::notes::note_input_hash(req),
            usage,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::CapabilityState;
    use axum::{extract::State, http::HeaderMap, routing::post, Json, Router};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use wellos_domain::ai::TranscriptSegment;

    type Seen = Arc<Mutex<Vec<Value>>>;

    struct Mock {
        calls: Arc<AtomicUsize>,
        seen: Seen,
        auth: Arc<Mutex<Option<String>>>,
    }

    type MockState = (
        Arc<AtomicUsize>,
        Arc<Vec<(u16, Value)>>,
        Seen,
        Arc<Mutex<Option<String>>>,
    );

    /// Controlled local HTTP server standing in for an external provider.
    /// `responses` are (status, chat-completion body) in order; the last
    /// repeats.
    async fn mock_server(responses: Vec<(u16, Value)>) -> (String, Mock) {
        let mock = Mock {
            calls: Arc::new(AtomicUsize::new(0)),
            seen: Arc::new(Mutex::new(Vec::new())),
            auth: Arc::new(Mutex::new(None)),
        };
        let st: MockState = (
            mock.calls.clone(),
            Arc::new(responses),
            mock.seen.clone(),
            mock.auth.clone(),
        );
        let app = Router::new()
            .route(
                "/v1/chat/completions",
                post(
                    |State((calls, responses, seen, auth)): State<MockState>,
                     headers: HeaderMap,
                     Json(body): Json<Value>| async move {
                        let n = calls.fetch_add(1, Ordering::SeqCst);
                        *auth.lock().unwrap() = headers
                            .get("authorization")
                            .map(|v| v.to_str().unwrap().to_string());
                        seen.lock().unwrap().push(body);
                        let (status, body) = &responses[n.min(responses.len() - 1)];
                        (
                            axum::http::StatusCode::from_u16(*status).unwrap(),
                            Json(body.clone()),
                        )
                    },
                ),
            )
            .with_state(st);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/v1/chat/completions"), mock)
    }

    fn completion(content: Value) -> Value {
        json!({
            "choices": [{ "message": { "role": "assistant", "content": content.to_string() } }],
            "usage": { "prompt_tokens": 120, "completion_tokens": 40, "total_tokens": 160 }
        })
    }

    fn cfg(endpoint: String, retries: u32) -> OpenAiCompatibleModelConfig {
        OpenAiCompatibleModelConfig {
            endpoint,
            model: "test-model".into(),
            api_key: "sk-test-credential".into(),
            connect_timeout: Duration::from_secs(2),
            timeout: Duration::from_secs(5),
            max_retries: retries,
            retry_backoff: Duration::from_millis(1),
            max_response_bytes: 64 * 1024,
            max_concurrency: 2,
            queue_timeout: Duration::from_millis(200),
        }
    }

    fn summary_req() -> SummaryRequest {
        SummaryRequest {
            template: "result-summary@1.0.0".into(),
            facts: vec![(
                "observation:1".into(),
                "Potassium 7.1 mmol/L (critical high)".into(),
            )],
            language: "en".into(),
        }
    }

    #[tokio::test]
    async fn result_summary_parses_validates_and_records_usage() {
        let (endpoint, mock) = mock_server(vec![(
            200,
            completion(json!({
                "summary": "Potassium is critically high.",
                "relevant_trend": null,
                "cited_sources": ["observation:1", "observation:1"],
                "limitations": ["Single result."],
                "suggested_next_step_categories": ["repeat-test"]
            })),
        )])
        .await;
        let m = OpenAiCompatibleModel::new(cfg(endpoint, 0)).unwrap();
        let out = m.summarize_result(&summary_req()).await.unwrap();
        assert_eq!(out.output.cited_sources, vec!["observation:1".to_string()]);
        assert_eq!(out.usage.unwrap().total_tokens, Some(160));
        assert_eq!(out.prompt_version, RESULT_PROMPT_VERSION);
        assert_eq!(out.route, PROVIDER_NAME);
        assert!(out.output.limitations.len() == 2);
        assert_eq!(
            mock.auth.lock().unwrap().as_deref(),
            Some("Bearer sk-test-credential")
        );
        let sent = mock.seen.lock().unwrap()[0].clone();
        assert_eq!(sent["model"], "test-model");
        assert_eq!(sent["response_format"]["type"], "json_object");
        assert_eq!(m.status().state, CapabilityState::Ready);
    }

    #[tokio::test]
    async fn result_summary_rejects_unsupplied_citation() {
        let (endpoint, _mock) = mock_server(vec![(
            200,
            completion(json!({
                "summary": "Potassium is critically high.",
                "cited_sources": ["observation:999"],
            })),
        )])
        .await;
        let m = OpenAiCompatibleModel::new(cfg(endpoint, 0)).unwrap();
        assert!(matches!(
            m.summarize_result(&summary_req()).await,
            Err(GatewayError::InvalidOutput(_))
        ));
    }

    #[tokio::test]
    async fn malformed_completion_is_rejected_not_substituted() {
        let (endpoint, _mock) = mock_server(vec![(
            200,
            json!({ "choices": [{ "message": { "content": "Potassium looks high, repeat it." } }] }),
        )])
        .await;
        let m = OpenAiCompatibleModel::new(cfg(endpoint, 0)).unwrap();
        assert!(matches!(
            m.summarize_result(&summary_req()).await,
            Err(GatewayError::InvalidOutput(_))
        ));
        assert_eq!(m.status().state, CapabilityState::Degraded);
    }

    #[tokio::test]
    async fn retries_transient_failures_bounded_then_reports_unavailable() {
        let (endpoint, mock) = mock_server(vec![(503, json!({}))]).await;
        let m = OpenAiCompatibleModel::new(cfg(endpoint, 2)).unwrap();
        let err = m.summarize_result(&summary_req()).await.unwrap_err();
        assert!(matches!(err, GatewayError::Unavailable(_)));
        assert_eq!(mock.calls.load(Ordering::SeqCst), 3);
        assert_eq!(m.status().state, CapabilityState::Degraded);
    }

    #[tokio::test]
    async fn recovers_after_transient_failure_and_status_returns_to_ready() {
        let (endpoint, mock) = mock_server(vec![
            (429, json!({})),
            (
                200,
                completion(json!({
                    "summary": "ok",
                    "cited_sources": ["observation:1"]
                })),
            ),
        ])
        .await;
        let m = OpenAiCompatibleModel::new(cfg(endpoint, 1)).unwrap();
        m.summarize_result(&summary_req()).await.unwrap();
        assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
        assert_eq!(m.status().state, CapabilityState::Ready);
    }

    #[tokio::test]
    async fn client_errors_are_not_retried_and_never_leak_endpoint() {
        let (endpoint, mock) = mock_server(vec![(401, json!({ "error": "bad key" }))]).await;
        let m = OpenAiCompatibleModel::new(cfg(endpoint.clone(), 3)).unwrap();
        let err = m.summarize_result(&summary_req()).await.unwrap_err();
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
        let text = err.to_string();
        assert!(!text.contains("127.0.0.1"), "{text}");
        assert!(!text.contains("sk-test"), "{text}");
    }

    #[tokio::test]
    async fn oversized_response_is_rejected() {
        let big = "x".repeat(70 * 1024);
        let (endpoint, _mock) = mock_server(vec![(
            200,
            completion(json!({ "summary": big, "cited_sources": ["observation:1"] })),
        )])
        .await;
        let m = OpenAiCompatibleModel::new(cfg(endpoint, 0)).unwrap();
        assert!(matches!(
            m.summarize_result(&summary_req()).await,
            Err(GatewayError::InvalidOutput(_))
        ));
    }

    #[tokio::test]
    async fn connection_failure_is_unavailable_without_endpoint_leak() {
        // Reserve a port and close it so the connection is refused.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);
        let m = OpenAiCompatibleModel::new(cfg(format!("http://{addr}/v1/chat/completions"), 0))
            .unwrap();
        let err = m.summarize_result(&summary_req()).await.unwrap_err();
        assert!(matches!(err, GatewayError::Unavailable(_)));
        assert!(!err.to_string().contains(&addr.port().to_string()));
    }

    fn triage_req() -> TriageRequest {
        TriageRequest {
            template: crate::triage::TRIAGE_TEMPLATE.into(),
            language: "en".into(),
            arrival_kind: wellos_domain::triage::ArrivalKind::Urgent,
            age_years: Some(54),
            reason: Some("chest pain".into()),
            concerns: vec!["chest_pain".into()],
            onset: None,
            red_flags: vec![],
            vitals: Default::default(),
            allergies: vec![],
            requested_service: None,
            facts: vec![("visit:reason".into(), "Chest pain for one hour".into())],
        }
    }

    #[tokio::test]
    async fn triage_proposal_is_clamped_to_deterministic_floor() {
        let (endpoint, _mock) = mock_server(vec![(
            200,
            completion(json!({
                "proposed_priority": "non_urgent",
                "proposed_service": "general_medicine",
                "handoff_summary": "Adult with chest pain.",
                "confidence": "medium",
                "cited_sources": ["visit:reason"]
            })),
        )])
        .await;
        let m = OpenAiCompatibleModel::new(cfg(endpoint, 0)).unwrap();
        let out = m.propose_triage(&triage_req()).await.unwrap();
        assert_eq!(out.output.proposed_priority, Priority::Urgent);
        assert!(out.output.raised_to_floor);
        assert_eq!(out.output.safety_floor, Priority::Urgent);
        assert_eq!(out.prompt_version, TRIAGE_PROMPT_VERSION);
    }

    #[tokio::test]
    async fn triage_proposal_rejects_unknown_service_and_priority() {
        let (endpoint, _mock) = mock_server(vec![(
            200,
            completion(json!({
                "proposed_priority": "urgent",
                "proposed_service": "cardiology",
                "handoff_summary": "x",
                "confidence": "medium"
            })),
        )])
        .await;
        let m = OpenAiCompatibleModel::new(cfg(endpoint, 0)).unwrap();
        assert!(matches!(
            m.propose_triage(&triage_req()).await,
            Err(GatewayError::InvalidOutput(_))
        ));
        let (endpoint, _mock) = mock_server(vec![(
            200,
            completion(json!({
                "proposed_priority": "critical",
                "proposed_service": "emergency",
                "handoff_summary": "x",
                "confidence": "medium"
            })),
        )])
        .await;
        let m = OpenAiCompatibleModel::new(cfg(endpoint, 0)).unwrap();
        assert!(matches!(
            m.propose_triage(&triage_req()).await,
            Err(GatewayError::InvalidOutput(_))
        ));
    }

    fn seg(i: u32, speaker: Option<&str>, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            index: i,
            start_ms: u64::from(i) * 1000,
            end_ms: u64::from(i) * 1000 + 900,
            speaker: speaker.map(Into::into),
            text: text.into(),
            confidence: Confidence::High,
        }
    }

    fn note_req() -> NoteDraftRequest {
        NoteDraftRequest {
            template: NOTE_DRAFT_TEMPLATE.into(),
            language: "en".into(),
            transcript: vec![
                seg(0, None, "What brings you in today?"),
                seg(1, None, "I have had a cough for three days."),
                seg(2, None, "Let's plan a chest x-ray."),
            ],
            context_facts: vec![("allergy:1".into(), "Penicillin allergy".into())],
        }
    }

    #[tokio::test]
    async fn note_draft_links_sections_to_segments_and_forces_judgement_review() {
        let (endpoint, mock) = mock_server(vec![(
            200,
            completion(json!({
                "sections": [
                    { "section": "reason_for_encounter", "text": "Cough for three days.", "confidence": "high", "review_needed": false, "reasons": [], "segments": [1] },
                    { "section": "plan", "text": "Chest x-ray discussed.", "confidence": "medium", "review_needed": false, "reasons": [], "segments": [2] }
                ],
                "flags": [
                    { "kind": "uncertain", "message": "Duration unclear.", "segments": [1], "sections": ["reason_for_encounter"] }
                ],
                "limitations": []
            })),
        )])
        .await;
        let m = OpenAiCompatibleModel::new(cfg(endpoint, 0)).unwrap();
        let out = m.draft_note(&note_req()).await.unwrap();
        assert_eq!(out.sections.len(), 2);
        let plan = out.sections.iter().find(|s| s.section == "plan").unwrap();
        assert!(plan.review_needed);
        assert!(plan.reasons.contains(&"clinician_judgement".to_string()));
        assert_eq!(out.prompt_version, NOTE_PROMPT_VERSION);
        let sent = mock.seen.lock().unwrap()[0].clone();
        let user: Value =
            serde_json::from_str(sent["messages"][1]["content"].as_str().unwrap()).unwrap();
        assert_eq!(user["transcript"].as_array().unwrap().len(), 3);
        assert_eq!(user["context"][0]["reference"], "allergy:1");
    }

    #[tokio::test]
    async fn note_draft_rejects_uncited_or_dangling_sections() {
        let (endpoint, _mock) = mock_server(vec![(
            200,
            completion(json!({
                "sections": [
                    { "section": "assessment", "text": "Likely viral bronchitis.", "confidence": "high", "segments": [] }
                ]
            })),
        )])
        .await;
        let m = OpenAiCompatibleModel::new(cfg(endpoint, 0)).unwrap();
        assert!(matches!(
            m.draft_note(&note_req()).await,
            Err(GatewayError::InvalidOutput(_))
        ));
        let (endpoint, _mock) = mock_server(vec![(
            200,
            completion(json!({
                "sections": [
                    { "section": "plan", "text": "x", "confidence": "high", "segments": [7] }
                ]
            })),
        )])
        .await;
        let m = OpenAiCompatibleModel::new(cfg(endpoint, 0)).unwrap();
        assert!(matches!(
            m.draft_note(&note_req()).await,
            Err(GatewayError::InvalidOutput(_))
        ));
    }

    fn risk_req() -> RiskSummaryRequest {
        use chrono::{DateTime, Duration, NaiveDate, Utc};
        use wellos_domain::risk::{assess, EncounterFact, ResultFact, RiskInput};
        let now: DateTime<Utc> = DateTime::parse_from_rfc3339("2026-09-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc);
        let obs = uuid::Uuid::now_v7();
        let input = RiskInput {
            now,
            birth_date: NaiveDate::from_ymd_opt(1980, 1, 1).unwrap(),
            alerts: vec![],
            current_visit: None,
            visits: vec![],
            latest_vitals: None,
            conditions: vec![],
            medications: vec![],
            allergies: vec![],
            results: vec![ResultFact {
                observation_id: obs,
                service_request_id: uuid::Uuid::now_v7(),
                code_loinc: "2823-3".into(),
                display: "Potassium".into(),
                value: Some("7.1".into()),
                unit: Some("mmol/L".into()),
                critical: true,
                abnormal: true,
                loop_state: "received".into(),
                effective_at: now - Duration::hours(2),
            }],
            open_requests: vec![],
            encounters: vec![EncounterFact {
                id: uuid::Uuid::now_v7(),
                status: "completed".into(),
                encounter_type: "consultation".into(),
                started_at: now - Duration::days(20),
                completed_at: Some(now - Duration::days(20)),
            }],
            tasks: vec![],
            care_team: vec![],
            previous: None,
        };
        let assessment = assess(&input);
        let mut facts: Vec<(String, String)> = Vec::new();
        for d in &assessment.domains {
            for f in &d.factors {
                for e in &f.evidence {
                    let r = format!("{}:{}", e.record_type, e.record_id);
                    if !facts.iter().any(|(x, _)| x == &r) {
                        facts.push((r, e.label.clone()));
                    }
                }
            }
        }
        RiskSummaryRequest {
            template: crate::risk::RISK_TEMPLATE.into(),
            language: "en".into(),
            assessment,
            facts,
        }
    }

    fn risk_completion(req: &RiskSummaryRequest, level_override: Option<&str>) -> Value {
        let det = &req.assessment;
        let refs: Vec<String> = req.facts.iter().map(|(r, _)| r.clone()).collect();
        let domains: Vec<Value> = det
            .domains
            .iter()
            .map(|d| {
                let mut cited: Vec<String> = d
                    .factors
                    .iter()
                    .flat_map(|f| f.evidence.iter())
                    .map(|e| format!("{}:{}", e.record_type, e.record_id))
                    .filter(|r| refs.contains(r))
                    .collect();
                if !d.factors.is_empty() && cited.is_empty() {
                    cited.push(format!("rule:{}:{}", det.rules_version, d.domain.as_str()));
                }
                json!({
                    "domain": d.domain,
                    "level": match level_override { Some(l) => json!(l), None => json!(d.level) },
                    "summary": format!("Explanation for {}", d.domain.as_str()),
                    "reasons": d.factors.iter().map(|f| f.code.clone()).collect::<Vec<_>>(),
                    "cited_sources": cited,
                })
            })
            .collect();
        completion(json!({
            "overall_level": match level_override { Some(l) => json!(l), None => json!(det.overall_level) },
            "domains": domains,
            "missing_information": [],
            "contradictions": [],
            "follow_up_suggestions": [
                { "category": "review_result", "domain": "diagnostic_result", "text": "Review the potassium result." }
            ],
            "limitations": [],
            "cited_sources": refs,
            "confidence": "medium"
        }))
    }

    #[tokio::test]
    async fn risk_summary_keeps_deterministic_levels_and_forces_confirmation() {
        let req = risk_req();
        let (endpoint, _mock) = mock_server(vec![(200, risk_completion(&req, Some("low")))]).await;
        let m = OpenAiCompatibleModel::new(cfg(endpoint, 0)).unwrap();
        let out = m.summarize_risk(&req).await.unwrap();
        assert_eq!(out.output.overall_level, req.assessment.overall_level);
        assert!(out.output.raised_to_floor);
        assert!(out
            .output
            .follow_up_suggestions
            .iter()
            .all(|s| s.requires_confirmation));
        assert!(out
            .output
            .limitations
            .contains(&"ai_level_raised_to_deterministic".to_string()));
        assert_eq!(out.prompt_version, RISK_PROMPT_VERSION);
    }

    #[tokio::test]
    async fn risk_summary_rejects_unsupplied_citation() {
        let req = risk_req();
        let mut body = risk_completion(&req, None);
        let content: Value =
            serde_json::from_str(body["choices"][0]["message"]["content"].as_str().unwrap())
                .unwrap();
        let mut content = content;
        content["cited_sources"] = json!(["observation:not-supplied"]);
        body["choices"][0]["message"]["content"] = json!(content.to_string());
        let (endpoint, _mock) = mock_server(vec![(200, body)]).await;
        let m = OpenAiCompatibleModel::new(cfg(endpoint, 0)).unwrap();
        assert!(matches!(
            m.summarize_risk(&req).await,
            Err(GatewayError::InvalidOutput(_))
        ));
    }

    #[tokio::test]
    async fn concurrency_limit_reports_unavailable_instead_of_queuing() {
        // A server that never answers within the queue timeout.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            loop {
                let (sock, _) = listener.accept().await.unwrap();
                tokio::spawn(async move {
                    tokio::time::sleep(Duration::from_secs(30)).await;
                    drop(sock);
                });
            }
        });
        let mut c = cfg(format!("http://{addr}/v1/chat/completions"), 0);
        c.max_concurrency = 1;
        c.timeout = Duration::from_secs(3);
        let m = Arc::new(OpenAiCompatibleModel::new(c).unwrap());
        let m1 = m.clone();
        let first = tokio::spawn(async move { m1.summarize_result(&summary_req()).await });
        tokio::time::sleep(Duration::from_millis(50)).await;
        let second = m.summarize_result(&summary_req()).await;
        match second {
            Err(GatewayError::Unavailable(reason)) => assert!(reason.contains("concurrency")),
            other => panic!("expected concurrency unavailability, got {other:?}"),
        }
        let first = first.await.unwrap();
        assert!(matches!(first, Err(GatewayError::Unavailable(_))));
    }

    #[test]
    fn debug_output_redacts_credential() {
        let c = cfg("http://127.0.0.1:1/v1/chat/completions".into(), 0);
        let dbg = format!("{c:?}");
        assert!(!dbg.contains("sk-test"));
        assert!(!dbg.contains("127.0.0.1"));
    }

    #[test]
    fn fenced_json_is_accepted_but_prose_is_not() {
        assert!(parse_json_object("```json\n{\"a\":1}\n```").is_some());
        assert!(parse_json_object("{\"a\":1}").is_some());
        assert!(parse_json_object("[1,2]").is_none());
        assert!(parse_json_object("The result is {\"a\":1}").is_none());
    }
}
