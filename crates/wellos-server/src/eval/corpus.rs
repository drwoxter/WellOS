//! Versioned synthetic evaluation corpus.
//!
//! Every case is fictional: synthetic patients, synthetic facilities,
//! invented identifiers. The JSON files under `eval-corpus/<version>/` are
//! compiled into the binary so the corpus hash is reproducible and a case
//! can never be edited without a version bump showing in every run.
//!
//! Scope: corpus cases describe *clinical inputs and expected behaviour*
//! (stale inputs, contradictions, missing prerequisites, critical and negative
//! findings, prompt injection, unsafe autonomous actions, cross-tenant
//! evidence, deterministic fallback). Provider *transport* failure modes —
//! timeouts, 429/5xx, connection loss, retry exhaustion, circuit/concurrency
//! limits, budget exhaustion — are not cases: they are exercised by the
//! controlled local HTTP server tests in `dmind-gateway` and by the runner's
//! ceiling tests, because they depend on the transport, not on the input.

use super::ops::CaseInput;
use dmind_gateway::{hash_json, Operation};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;

pub const SUITE_KEY: &str = "dmind-core";
pub const CORPUS_VERSION: &str = "1.0.0";

/// Raw corpus files, one per operation, plus the shared edge-case file.
const FILES: &[(&str, &str)] = &[
    (
        "result_summary",
        include_str!("../../eval-corpus/v1/result_summary.json"),
    ),
    (
        "triage_proposal",
        include_str!("../../eval-corpus/v1/triage_proposal.json"),
    ),
    (
        "risk_summary",
        include_str!("../../eval-corpus/v1/risk_summary.json"),
    ),
    (
        "note_draft",
        include_str!("../../eval-corpus/v1/note_draft.json"),
    ),
    (
        "access_intent",
        include_str!("../../eval-corpus/v1/access_intent.json"),
    ),
    (
        "appointment_ranking",
        include_str!("../../eval-corpus/v1/appointment_ranking.json"),
    ),
    (
        "cancellation_recovery",
        include_str!("../../eval-corpus/v1/cancellation_recovery.json"),
    ),
    (
        "capacity_explanation",
        include_str!("../../eval-corpus/v1/capacity_explanation.json"),
    ),
    (
        "diagnostic_order_suggestion",
        include_str!("../../eval-corpus/v1/diagnostic_order_suggestion.json"),
    ),
    (
        "diagnostic_result_synthesis",
        include_str!("../../eval-corpus/v1/diagnostic_result_synthesis.json"),
    ),
    (
        "patient_result_explanation",
        include_str!("../../eval-corpus/v1/patient_result_explanation.json"),
    ),
];

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Typical,
    Incomplete,
    HighRisk,
    Adversarial,
    Edge,
}

impl Category {
    pub fn as_str(self) -> &'static str {
        match self {
            Category::Typical => "typical",
            Category::Incomplete => "incomplete",
            Category::HighRisk => "high_risk",
            Category::Adversarial => "adversarial",
            Category::Edge => "edge",
        }
    }
    /// The four categories every operation must cover.
    pub const REQUIRED: &'static [Category] = &[
        Category::Typical,
        Category::Incomplete,
        Category::HighRisk,
        Category::Adversarial,
    ];
}

/// Deterministic safety floor the provider output must not undercut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct SafetyFloor {
    /// triage: minimum `proposed_priority` (non_urgent < standard < urgent < immediate).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub priority: Option<String>,
    /// risk: minimum `overall_level`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_level: Option<String>,
    /// diagnostics: exact `criticality` the deterministic rules assigned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub criticality: Option<String>,
    /// access intent: `clinical_triage_suggested` must be true.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub triage_required: Option<bool>,
}

/// Case-specific pass/fail contract, evaluated by the rubric.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Expectations {
    /// Evidence references that must appear among the cited sources.
    #[serde(default)]
    pub required_citations: Vec<String>,
    /// Lower-cased substrings at least one of which must appear in the
    /// output's free text (per entry: alternatives separated by `|`).
    #[serde(default)]
    pub required_facts: Vec<String>,
    /// Lower-cased substrings that must not appear anywhere in the output
    /// (diagnoses, prescriptions, autonomous actions, injected payloads).
    #[serde(default)]
    pub prohibited_terms: Vec<String>,
    /// Output must list missing information / abstain instead of asserting.
    #[serde(default)]
    pub expect_missing_information: bool,
    /// Output must surface a contradiction.
    #[serde(default)]
    pub expect_contradiction: bool,
    /// Output confidence must not be `high`.
    #[serde(default)]
    pub expect_uncertainty: bool,
    #[serde(default)]
    pub safety_floor: SafetyFloor,
    /// The case embeds free-text instructions aimed at the model.
    #[serde(default)]
    pub prompt_injection: bool,
    /// Evidence references from another tenant/patient that must never be
    /// cited (they are not in the allowed evidence either).
    #[serde(default)]
    pub foreign_refs: Vec<String>,
    /// The input is a stale snapshot; the deterministic layer must refuse
    /// or flag it and the provider must not present it as current.
    #[serde(default)]
    pub stale_input: bool,
    /// The deterministic layer itself refuses this input (non-assistive
    /// statement in a clinician-authored field); a provider refusal is the
    /// correct outcome and the baseline error is expected.
    #[serde(default)]
    pub baseline_refusal_expected: bool,
    /// Upper bound for the serialized output size.
    #[serde(default = "default_max_output_chars")]
    pub max_output_chars: usize,
    /// Human notes shown in the cockpit (what a reviewer should look at).
    #[serde(default)]
    pub review_focus: Vec<String>,
}

fn default_max_output_chars() -> usize {
    12_000
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawCase {
    key: String,
    category: Category,
    language: String,
    title: String,
    request: Value,
    #[serde(default)]
    expectations: Expectations,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawFile {
    operation: String,
    cases: Vec<RawCase>,
}

#[derive(Debug, Clone, Serialize)]
pub struct EvalCase {
    pub key: String,
    pub operation: Operation,
    pub category: Category,
    pub language: String,
    pub title: String,
    pub input: CaseInput,
    pub expectations: Expectations,
}

impl EvalCase {
    pub fn input_hash(&self) -> String {
        self.input.input_hash()
    }
    pub fn allowed_evidence(&self) -> Vec<String> {
        self.input.evidence_refs()
    }
    /// Stable identity of the case definition (input + expectations).
    pub fn case_hash(&self) -> String {
        hash_json(&(&self.input, &self.expectations))
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct Corpus {
    pub suite_key: &'static str,
    pub version: &'static str,
    pub cases: Vec<EvalCase>,
}

/// UUID namespaces reserved for synthetic fixtures; any identifier in the
/// corpus outside them is treated as potentially real and refused.
pub const SYNTHETIC_UUID_NAMESPACES: [&str; 5] = [
    "11111111-1111-4111-8111-",
    "22222222-2222-4222-8222-",
    "33333333-3333-4333-8333-",
    "44444444-4444-4444-8444-",
    "55555555-5555-4555-8555-",
];

fn uuid_like_tokens(text: &str) -> impl Iterator<Item = &str> {
    text.split(|c: char| !(c.is_ascii_hexdigit() || c == '-'))
        .filter(|t| {
            t.len() == 36
                && t.bytes()
                    .enumerate()
                    .all(|(i, b)| matches!(i, 8 | 13 | 18 | 23) == (b == b'-'))
        })
}

/// Structural proof that a case references only synthetic fixtures: every
/// UUID-shaped identifier in the serialized request must live in a reserved
/// synthetic namespace. This cannot judge free text; it refuses real record
/// identifiers, which is what the live gate relies on.
pub fn verify_case_synthetic(key: &str, input: &serde_json::Value) -> Result<(), String> {
    let text = input.to_string();
    for token in uuid_like_tokens(&text) {
        let lower = token.to_ascii_lowercase();
        if !SYNTHETIC_UUID_NAMESPACES
            .iter()
            .any(|ns| lower.starts_with(ns))
        {
            return Err(format!(
                "case {key}: identifier {token} is outside the synthetic namespaces"
            ));
        }
    }
    Ok(())
}

impl Corpus {
    /// Parse and validate the compiled-in corpus. Fails loudly on any
    /// malformed case so a broken corpus cannot silently shrink coverage.
    /// Every case must pass [`verify_case_synthetic`]; the live gate derives
    /// `patients_synthetic` from this instead of asserting it.
    pub fn verify_synthetic(&self) -> Result<(), String> {
        for case in &self.cases {
            let input = serde_json::to_value(&case.input).map_err(|e| e.to_string())?;
            verify_case_synthetic(&case.key, &input)?;
        }
        Ok(())
    }

    pub fn load() -> Result<Corpus, String> {
        let mut cases = Vec::new();
        let mut keys = std::collections::BTreeSet::new();
        for (name, text) in FILES {
            let file: RawFile =
                serde_json::from_str(text).map_err(|e| format!("corpus {name}: {e}"))?;
            if file.operation != *name {
                return Err(format!("corpus {name}: operation tag {}", file.operation));
            }
            let operation = Operation::parse(&file.operation)
                .ok_or_else(|| format!("corpus {name}: unknown operation"))?;
            for raw in file.cases {
                if !keys.insert(raw.key.clone()) {
                    return Err(format!("corpus {name}: duplicate case key {}", raw.key));
                }
                if !matches!(raw.language.as_str(), "en" | "es") {
                    return Err(format!("case {}: language {}", raw.key, raw.language));
                }
                if raw.title.chars().count() > 200 || raw.title.trim().is_empty() {
                    return Err(format!("case {}: title length", raw.key));
                }
                let input = CaseInput::parse(operation, &raw.request)
                    .map_err(|e| format!("case {}: {e}", raw.key))?;
                if let Some(lang) = input.language() {
                    if lang != raw.language {
                        return Err(format!(
                            "case {}: declared language {} but request language {lang}",
                            raw.key, raw.language
                        ));
                    }
                }
                for f in &raw.expectations.foreign_refs {
                    if input.evidence_refs().contains(f) {
                        return Err(format!(
                            "case {}: foreign ref {f} is allowed evidence",
                            raw.key
                        ));
                    }
                }
                cases.push(EvalCase {
                    key: raw.key,
                    operation,
                    category: raw.category,
                    language: raw.language,
                    title: raw.title,
                    input,
                    expectations: raw.expectations,
                });
            }
        }
        let corpus = Corpus {
            suite_key: SUITE_KEY,
            version: CORPUS_VERSION,
            cases,
        };
        corpus.check_coverage()?;
        Ok(corpus)
    }

    /// Every registered operation needs the four required categories in
    /// both languages combined, and at least four cases.
    pub fn check_coverage(&self) -> Result<(), String> {
        let mut missing = Vec::new();
        for reg in super::registry::registry() {
            let cases: Vec<&EvalCase> = self
                .cases
                .iter()
                .filter(|c| c.operation == reg.operation)
                .collect();
            if cases.len() < 4 {
                missing.push(format!(
                    "{}: {} case(s), need at least 4",
                    reg.key,
                    cases.len()
                ));
            }
            for cat in Category::REQUIRED {
                if !cases.iter().any(|c| c.category == *cat) {
                    missing.push(format!("{}: no {} case", reg.key, cat.as_str()));
                }
            }
            for lang in ["en", "es"] {
                if !cases.iter().any(|c| c.language == lang) {
                    missing.push(format!("{}: no {lang} case", reg.key));
                }
            }
        }
        if missing.is_empty() {
            Ok(())
        } else {
            Err(format!(
                "evaluation coverage gaps:\n  {}",
                missing.join("\n  ")
            ))
        }
    }

    pub fn hash(&self) -> String {
        hash_json(
            &self
                .cases
                .iter()
                .map(|c| (&c.key, c.case_hash()))
                .collect::<Vec<_>>(),
        )
    }

    pub fn languages(&self) -> Vec<String> {
        let mut v: Vec<String> = self.cases.iter().map(|c| c.language.clone()).collect();
        v.sort();
        v.dedup();
        v
    }

    pub fn by_operation(&self) -> BTreeMap<&'static str, Vec<&EvalCase>> {
        let mut m: BTreeMap<&'static str, Vec<&EvalCase>> = BTreeMap::new();
        for c in &self.cases {
            m.entry(c.operation.as_str()).or_default().push(c);
        }
        m
    }

    pub fn select(&self, operations: Option<&[Operation]>) -> Vec<&EvalCase> {
        self.cases
            .iter()
            .filter(|c| {
                operations
                    .map(|ops| ops.contains(&c.operation))
                    .unwrap_or(true)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_loads_and_covers_every_registered_operation() {
        let corpus = Corpus::load().expect("corpus valid");
        assert!(corpus.cases.len() >= 4 * Operation::ALL.len());
        assert_eq!(corpus.languages(), vec!["en".to_string(), "es".to_string()]);
    }

    #[test]
    fn synthetic_verification_refuses_identifiers_outside_the_fixture_namespaces() {
        let corpus = Corpus::load().unwrap();
        corpus.verify_synthetic().unwrap();
        let ok = serde_json::json!({ "patient_id": "44444444-4444-4444-8444-444444444442" });
        verify_case_synthetic("ok", &ok).unwrap();
        let real_looking = serde_json::json!({
            "facts": [["observation:obs-1", "Patient 7c9e6679-7425-40de-944b-e07fc1f90ae7 potassium 5.6"]]
        });
        let err = verify_case_synthetic("leak", &real_looking).unwrap_err();
        assert!(err.contains("outside the synthetic namespaces"), "{err}");
    }

    #[test]
    fn coverage_check_fails_for_an_uncovered_operation() {
        let mut corpus = Corpus::load().unwrap();
        corpus
            .cases
            .retain(|c| c.operation != Operation::TriageProposal);
        let err = corpus.check_coverage().unwrap_err();
        assert!(err.contains("triage_proposal"), "{err}");
    }

    #[test]
    fn unknown_fields_are_refused() {
        let req = serde_json::json!({
            "template": "result-summary@1.0.0",
            "facts": [["observation:1", "Potassium 5.1 mmol/L"]],
            "language": "en",
            "patient_row": { "name": "x" }
        });
        let err = CaseInput::parse(Operation::ResultSummary, &req).unwrap_err();
        assert!(err.contains("patient_row"), "{err}");
    }

    #[test]
    fn corpus_hash_is_stable_and_cases_are_bounded() {
        let a = Corpus::load().unwrap();
        let b = Corpus::load().unwrap();
        assert_eq!(a.hash(), b.hash());
        for c in &a.cases {
            let bytes = serde_json::to_string(&c.input).unwrap().len();
            assert!(bytes < 32_000, "case {} input is {bytes} bytes", c.key);
        }
    }
}
