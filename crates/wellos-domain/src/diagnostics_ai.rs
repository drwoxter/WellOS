//! Versioned output contracts of the dMind diagnostic operations.
//!
//! - `diagnostic-order-suggestion.v1`: suggests orderables from a bounded
//!   candidate list supplied by WellOS. It cannot invent an orderable, create
//!   an order, set final urgency, bypass a prerequisite or sign anything.
//! - `diagnostic-result-synthesis.v1`: summarizes a final report against
//!   prior results. It cites only supplied report / component identifiers,
//!   never changes the deterministic interpretation and never closes a loop.
//! - `patient-result-explanation.v1`: plain-language EN/ES draft for a
//!   *reviewed* report version. A clinician approves or edits it before any
//!   release; the model never releases or notifies.
//!
//! The same validators run for every provider, including the fixture one.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::ai::Confidence;
use crate::diagnostics::Interpretation;

pub const ORDER_SUGGESTION_SCHEMA: &str = "diagnostic-order-suggestion.v1";
pub const RESULT_SYNTHESIS_SCHEMA: &str = "diagnostic-result-synthesis.v1";
pub const PATIENT_EXPLANATION_SCHEMA: &str = "patient-result-explanation.v1";

const MAX_LIST: usize = 20;
const MAX_TEXT: usize = 800;
const MAX_EXPLANATION: usize = 2_500;

fn check_text(s: &str, name: &str, max: usize) -> Result<(), String> {
    if s.trim().is_empty() {
        return Err(format!("{name} is empty"));
    }
    if s.chars().count() > max {
        return Err(format!("{name} is oversized"));
    }
    Ok(())
}

fn check_list(items: &[String], name: &str) -> Result<(), String> {
    if items.len() > MAX_LIST {
        return Err(format!("{name} has too many items"));
    }
    for i in items {
        check_text(i, name, MAX_TEXT)?;
    }
    Ok(())
}

fn check_subset(cited: &[String], allowed: &BTreeSet<&str>, name: &str) -> Result<(), String> {
    for c in cited {
        if !allowed.contains(c.as_str()) {
            return Err(format!("{name} cites a reference that was not supplied"));
        }
    }
    Ok(())
}

/// Phrases that would turn an assistive text into a diagnosis, a
/// prescription or a release decision. Checked case-insensitively.
const FORBIDDEN_CLINICAL_PHRASES: &[&str] = &[
    "i diagnose",
    "diagnosis is",
    "you have cancer",
    "prescribe",
    "start taking",
    "stop taking",
    "no need to follow up",
    "nothing to worry about",
    "you are fine",
    "order placed",
    "i have ordered",
    "released to the patient",
    "diagnostico que",
    "el diagnóstico es",
    "te receto",
    "deja de tomar",
    "empieza a tomar",
    "no hay nada de qué preocuparse",
    "estás bien",
];

fn check_no_forbidden(text: &str, name: &str) -> Result<(), String> {
    let lower = text.to_lowercase();
    for p in FORBIDDEN_CLINICAL_PHRASES {
        if lower.contains(p) {
            return Err(format!("{name} contains a non-assistive statement"));
        }
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// diagnostic-order-suggestion.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SuggestedOrderable {
    /// Catalog entry id — must be one of the supplied candidates.
    pub orderable_id: Uuid,
    pub rationale: String,
    /// Evidence references (`fact:*`, `note:*`, `obs:*`, `order:*`,
    /// `catalog:*`) drawn only from the supplied list.
    pub cited_sources: Vec<String>,
    /// Proposed timing: `routine` | `urgent` | `timed` — advisory only; the
    /// clinician sets the final priority.
    #[serde(default)]
    pub proposed_timing: Option<String>,
    #[serde(default)]
    pub preparation_note: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticOrderSuggestionV1 {
    pub schema_version: String,
    pub suggestions: Vec<SuggestedOrderable>,
    /// Candidate ids the model believes duplicate a recent / pending test.
    #[serde(default)]
    pub duplicate_warnings: Vec<DuplicateWarning>,
    #[serde(default)]
    pub missing_information: Vec<String>,
    pub cited_sources: Vec<String>,
    pub confidence: Confidence,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateWarning {
    pub orderable_id: Uuid,
    pub reason: String,
    pub cited_sources: Vec<String>,
}

impl DiagnosticOrderSuggestionV1 {
    pub fn validate(&self, candidate_ids: &[Uuid], fact_refs: &[String]) -> Result<(), String> {
        if self.schema_version != ORDER_SUGGESTION_SCHEMA {
            return Err("unexpected schema_version".into());
        }
        let allowed: BTreeSet<&str> = fact_refs.iter().map(String::as_str).collect();
        let candidates: BTreeSet<Uuid> = candidate_ids.iter().copied().collect();
        if self.suggestions.len() > MAX_LIST {
            return Err("too many suggestions".into());
        }
        let mut seen = BTreeSet::new();
        for s in &self.suggestions {
            if !candidates.contains(&s.orderable_id) {
                return Err("suggestion names an orderable that was not supplied".into());
            }
            if !seen.insert(s.orderable_id) {
                return Err("suggestion repeats an orderable".into());
            }
            check_text(&s.rationale, "rationale", MAX_TEXT)?;
            check_no_forbidden(&s.rationale, "rationale")?;
            if s.cited_sources.is_empty() {
                return Err("suggestion has no citations".into());
            }
            check_list(&s.cited_sources, "cited_sources")?;
            check_subset(&s.cited_sources, &allowed, "suggestion cited_sources")?;
            if let Some(t) = &s.proposed_timing {
                if !matches!(t.as_str(), "routine" | "urgent" | "timed") {
                    return Err("proposed_timing must be routine, urgent or timed".into());
                }
            }
            if let Some(p) = &s.preparation_note {
                check_text(p, "preparation_note", MAX_TEXT)?;
            }
        }
        for d in &self.duplicate_warnings {
            if !candidates.contains(&d.orderable_id) {
                return Err("duplicate warning names an orderable that was not supplied".into());
            }
            check_text(&d.reason, "duplicate reason", MAX_TEXT)?;
            check_list(&d.cited_sources, "cited_sources")?;
            check_subset(&d.cited_sources, &allowed, "duplicate cited_sources")?;
        }
        check_list(&self.missing_information, "missing_information")?;
        check_list(&self.cited_sources, "cited_sources")?;
        check_subset(&self.cited_sources, &allowed, "cited_sources")?;
        if self.limitations.is_empty() {
            return Err("limitations must not be empty".into());
        }
        check_list(&self.limitations, "limitations")?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// diagnostic-result-synthesis.v1
// ---------------------------------------------------------------------------

/// One statement about a component, bound to the deterministic
/// interpretation WellOS computed for it (the model may not change it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentStatement {
    pub component_ref: String,
    pub interpretation: Interpretation,
    pub statement: String,
    #[serde(default)]
    pub change_from_prior: Option<String>,
    pub cited_sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiagnosticResultSynthesisV1 {
    pub schema_version: String,
    pub report_ref: String,
    pub summary: String,
    /// Deterministic report criticality echoed back; must equal the one
    /// supplied.
    pub criticality: Interpretation,
    pub components: Vec<ComponentStatement>,
    #[serde(default)]
    pub changes: Vec<String>,
    #[serde(default)]
    pub contradictions: Vec<String>,
    #[serde(default)]
    pub missing_information: Vec<String>,
    pub cited_sources: Vec<String>,
    pub confidence: Confidence,
    pub limitations: Vec<String>,
}

/// Deterministic facts the synthesis validator binds the output to.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SynthesisBounds {
    pub report_ref: String,
    pub criticality: Interpretation,
    /// `(component_ref, interpretation)` of the reviewed report.
    pub components: Vec<(String, Interpretation)>,
    pub fact_refs: Vec<String>,
}

impl DiagnosticResultSynthesisV1 {
    pub fn validate(&self, bounds: &SynthesisBounds) -> Result<(), String> {
        if self.schema_version != RESULT_SYNTHESIS_SCHEMA {
            return Err("unexpected schema_version".into());
        }
        if self.report_ref != bounds.report_ref {
            return Err("synthesis is bound to a different report".into());
        }
        if self.criticality != bounds.criticality {
            return Err("synthesis changed the deterministic criticality".into());
        }
        check_text(&self.summary, "summary", MAX_EXPLANATION)?;
        check_no_forbidden(&self.summary, "summary")?;
        let allowed: BTreeSet<&str> = bounds.fact_refs.iter().map(String::as_str).collect();
        if self.components.len() > 200 {
            return Err("too many component statements".into());
        }
        let mut seen = BTreeSet::new();
        for c in &self.components {
            let Some((_, expected)) = bounds
                .components
                .iter()
                .find(|(r, _)| *r == c.component_ref)
            else {
                return Err("component statement names an unknown component".into());
            };
            if !seen.insert(c.component_ref.as_str()) {
                return Err("component statement repeated".into());
            }
            if c.interpretation != *expected {
                return Err("component statement changed the deterministic interpretation".into());
            }
            check_text(&c.statement, "component statement", MAX_TEXT)?;
            check_no_forbidden(&c.statement, "component statement")?;
            if let Some(ch) = &c.change_from_prior {
                check_text(ch, "change_from_prior", MAX_TEXT)?;
            }
            if c.cited_sources.is_empty() {
                return Err("component statement has no citations".into());
            }
            check_list(&c.cited_sources, "cited_sources")?;
            check_subset(&c.cited_sources, &allowed, "component cited_sources")?;
        }
        check_list(&self.changes, "changes")?;
        check_list(&self.contradictions, "contradictions")?;
        check_list(&self.missing_information, "missing_information")?;
        if self.cited_sources.is_empty() {
            return Err("cited_sources must not be empty".into());
        }
        check_list(&self.cited_sources, "cited_sources")?;
        check_subset(&self.cited_sources, &allowed, "cited_sources")?;
        if !self.cited_sources.contains(&bounds.report_ref) {
            return Err("synthesis must cite the report it summarizes".into());
        }
        if self.limitations.is_empty() {
            return Err("limitations must not be empty".into());
        }
        check_list(&self.limitations, "limitations")?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// patient-result-explanation.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PatientResultExplanationV1 {
    pub schema_version: String,
    pub report_ref: String,
    /// Exact report version the draft was produced for.
    pub report_version: i64,
    pub explanation_en: String,
    pub explanation_es: String,
    /// Facts explained, as supplied references.
    pub cited_sources: Vec<String>,
    /// Always present: the patient is told to discuss with their clinician.
    pub next_steps_en: String,
    pub next_steps_es: String,
    pub confidence: Confidence,
    pub limitations: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExplanationBounds {
    pub report_ref: String,
    pub report_version: i64,
    pub criticality: Interpretation,
    pub fact_refs: Vec<String>,
}

/// Reassurance a draft may never contain when the report is abnormal or
/// critical.
const REASSURANCE_PHRASES: &[&str] = &[
    "normal",
    "nothing abnormal",
    "no concern",
    "reassuring",
    "all clear",
    "todo normal",
    "sin alteraciones",
    "tranquilizador",
    "nada preocupante",
];

impl PatientResultExplanationV1 {
    pub fn validate(&self, bounds: &ExplanationBounds) -> Result<(), String> {
        if self.schema_version != PATIENT_EXPLANATION_SCHEMA {
            return Err("unexpected schema_version".into());
        }
        if self.report_ref != bounds.report_ref || self.report_version != bounds.report_version {
            return Err("explanation is bound to a different report version".into());
        }
        for (text, name) in [
            (&self.explanation_en, "explanation_en"),
            (&self.explanation_es, "explanation_es"),
        ] {
            check_text(text, name, MAX_EXPLANATION)?;
            check_no_forbidden(text, name)?;
            if bounds.criticality >= Interpretation::Abnormal {
                let lower = text.to_lowercase();
                if REASSURANCE_PHRASES.iter().any(|p| lower.contains(p)) {
                    return Err(format!(
                        "{name} contains unsupported reassurance for a non-normal report"
                    ));
                }
            }
        }
        check_text(&self.next_steps_en, "next_steps_en", MAX_TEXT)?;
        check_text(&self.next_steps_es, "next_steps_es", MAX_TEXT)?;
        let allowed: BTreeSet<&str> = bounds.fact_refs.iter().map(String::as_str).collect();
        if self.cited_sources.is_empty() {
            return Err("cited_sources must not be empty".into());
        }
        check_list(&self.cited_sources, "cited_sources")?;
        check_subset(&self.cited_sources, &allowed, "cited_sources")?;
        if !self.cited_sources.contains(&bounds.report_ref) {
            return Err("explanation must cite the report".into());
        }
        if self.limitations.is_empty() {
            return Err("limitations must not be empty".into());
        }
        check_list(&self.limitations, "limitations")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggestion_rejects_unsupplied_orderable() {
        let a = Uuid::now_v7();
        let b = Uuid::now_v7();
        let out = DiagnosticOrderSuggestionV1 {
            schema_version: ORDER_SUGGESTION_SCHEMA.into(),
            suggestions: vec![SuggestedOrderable {
                orderable_id: b,
                rationale: "Chest pain with dyspnoea".into(),
                cited_sources: vec!["note:assessment".into()],
                proposed_timing: Some("urgent".into()),
                preparation_note: None,
            }],
            duplicate_warnings: vec![],
            missing_information: vec![],
            cited_sources: vec!["note:assessment".into()],
            confidence: Confidence::Medium,
            limitations: vec!["Assistive only".into()],
        };
        assert!(out.validate(&[a], &["note:assessment".into()]).is_err());
        assert!(out.validate(&[a, b], &["note:assessment".into()]).is_ok());
        let mut bad = out.clone();
        bad.suggestions[0].cited_sources = vec!["note:invented".into()];
        assert!(bad.validate(&[a, b], &["note:assessment".into()]).is_err());
        let mut bad = out.clone();
        bad.suggestions[0].rationale = "I diagnose pneumonia".into();
        assert!(bad.validate(&[a, b], &["note:assessment".into()]).is_err());
        let mut bad = out;
        bad.suggestions[0].proposed_timing = Some("stat".into());
        assert!(bad.validate(&[a, b], &["note:assessment".into()]).is_err());
    }

    #[test]
    fn synthesis_preserves_deterministic_interpretation() {
        let bounds = SynthesisBounds {
            report_ref: "report:r1".into(),
            criticality: Interpretation::Critical,
            components: vec![("component:c1".into(), Interpretation::Critical)],
            fact_refs: vec![
                "report:r1".into(),
                "component:c1".into(),
                "obs:prior".into(),
            ],
        };
        let out = DiagnosticResultSynthesisV1 {
            schema_version: RESULT_SYNTHESIS_SCHEMA.into(),
            report_ref: "report:r1".into(),
            summary: "Potassium is critically high and higher than the prior value.".into(),
            criticality: Interpretation::Critical,
            components: vec![ComponentStatement {
                component_ref: "component:c1".into(),
                interpretation: Interpretation::Critical,
                statement: "7.1 mmol/L, critical high".into(),
                change_from_prior: Some("up from 4.2".into()),
                cited_sources: vec!["component:c1".into(), "obs:prior".into()],
            }],
            changes: vec![],
            contradictions: vec![],
            missing_information: vec![],
            cited_sources: vec!["report:r1".into()],
            confidence: Confidence::High,
            limitations: vec!["Does not replace professional review".into()],
        };
        assert!(out.validate(&bounds).is_ok());
        let mut bad = out.clone();
        bad.criticality = Interpretation::Normal;
        assert!(bad.validate(&bounds).is_err());
        let mut bad = out.clone();
        bad.components[0].interpretation = Interpretation::Normal;
        assert!(bad.validate(&bounds).is_err());
        let mut bad = out.clone();
        bad.report_ref = "report:other".into();
        assert!(bad.validate(&bounds).is_err());
        let mut bad = out;
        bad.cited_sources = vec!["obs:prior".into()];
        assert!(bad.validate(&bounds).is_err(), "must cite the report");
    }

    #[test]
    fn explanation_rejects_reassurance_on_abnormal_and_wrong_version() {
        let bounds = ExplanationBounds {
            report_ref: "report:r1".into(),
            report_version: 2,
            criticality: Interpretation::Abnormal,
            fact_refs: vec!["report:r1".into()],
        };
        let out = PatientResultExplanationV1 {
            schema_version: PATIENT_EXPLANATION_SCHEMA.into(),
            report_ref: "report:r1".into(),
            report_version: 2,
            explanation_en: "Your potassium result is above the usual range.".into(),
            explanation_es: "Su resultado de potasio está por encima del rango habitual.".into(),
            cited_sources: vec!["report:r1".into()],
            next_steps_en: "Your clinician will discuss this with you.".into(),
            next_steps_es: "Su profesional lo comentará con usted.".into(),
            confidence: Confidence::Medium,
            limitations: vec!["Draft for clinician approval".into()],
        };
        assert!(out.validate(&bounds).is_ok());
        let mut bad = out.clone();
        bad.explanation_en = "Everything is normal, nothing to worry about.".into();
        assert!(bad.validate(&bounds).is_err());
        let mut bad = out;
        bad.report_version = 1;
        assert!(bad.validate(&bounds).is_err());
    }
}
