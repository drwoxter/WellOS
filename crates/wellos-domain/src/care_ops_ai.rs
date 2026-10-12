//! Typed, bounded output contracts for the governed care-operations dMind
//! operations. Every contract is validated against the exact evidence the
//! deterministic layer supplied: references outside that set, dates outside
//! the forecast, candidates the matcher did not produce, clinical-clearance
//! claims and autonomous-action language are all rejected before an artifact
//! can exist. dMind explains, ranks and drafts; humans decide.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};

use crate::ai::Confidence;

pub const CARE_FLOW_BRIEF_SCHEMA: &str = "care-flow-brief.v1";
pub const PLACEMENT_RANKING_SCHEMA: &str = "placement-ranking.v1";
pub const DISCHARGE_COORDINATION_SCHEMA: &str = "discharge-coordination.v1";
pub const THEATRE_PLAN_EXPLANATION_SCHEMA: &str = "theatre-plan-explanation.v1";
pub const OPERATIONAL_FORECAST_EXPLANATION_SCHEMA: &str = "operational-forecast-explanation.v1";
pub const SHIFT_HANDOFF_DRAFT_SCHEMA: &str = "shift-handoff-draft.v1";

pub const MAX_TEXT: usize = 600;
pub const MAX_LIST: usize = 20;

/// Recommendation categories an operational forecast explanation may use.
/// Every one is a review prompt for a human, never an action.
pub const OPS_RECOMMENDATION_CATEGORIES: &[&str] = &[
    "review_staffing",
    "review_discharge_pipeline",
    "review_theatre_plan",
    "review_transport",
    "review_placement_queue",
    "collect_more_history",
    "escalate_to_operations_manager",
];

pub const THEATRE_READINESS: &[&str] = &["ready", "blocked", "needs_review"];

/// Phrases that would turn an operational note into a clinical clearance.
/// `discharge_ready` is operational; medical fitness is never dMind's call.
const CLINICAL_CLEARANCE_PHRASES: &[&str] = &[
    "medically fit",
    "medically cleared",
    "clinically cleared",
    "clinically stable",
    "safe to discharge",
    "fit for discharge",
    "can be discharged",
    "alta médica",
    "apto para el alta",
    "apta para el alta",
    "clínicamente estable",
    "puede ser dado de alta",
    "puede ser dada de alta",
];

/// First-person completed actions: the model may only propose.
const AUTONOMOUS_ACTION_PHRASES: &[&str] = &[
    "i have assigned",
    "i assigned",
    "i have moved",
    "i moved",
    "i have transferred",
    "i transferred",
    "i have booked",
    "i booked",
    "i have discharged",
    "i discharged",
    "i have cancelled",
    "i cancelled",
    "i have notified",
    "i notified",
    "he asignado",
    "he trasladado",
    "he reservado",
    "he dado el alta",
    "he cancelado",
    "he notificado",
    "he movido",
];

fn check_text(s: &str, what: &str) -> Result<(), String> {
    if s.trim().is_empty() {
        return Err(format!("{what} is empty"));
    }
    if s.chars().count() > MAX_TEXT {
        return Err(format!("{what} is oversized"));
    }
    if s.chars().any(|c| c.is_control() && c != '\n') {
        return Err(format!("{what} contains control characters"));
    }
    let lower = s.to_lowercase();
    if AUTONOMOUS_ACTION_PHRASES.iter().any(|p| lower.contains(p)) {
        return Err(format!("{what} claims an autonomous action"));
    }
    Ok(())
}

fn check_no_clinical_clearance(s: &str, what: &str) -> Result<(), String> {
    let lower = s.to_lowercase();
    if CLINICAL_CLEARANCE_PHRASES.iter().any(|p| lower.contains(p)) {
        return Err(format!("{what} asserts clinical clearance"));
    }
    Ok(())
}

fn check_list(list: &[String], what: &str) -> Result<(), String> {
    if list.len() > MAX_LIST {
        return Err(format!("too many {what}"));
    }
    for s in list {
        check_text(s, what)?;
    }
    Ok(())
}

fn check_subset(refs: &[String], allowed: &BTreeSet<&str>, what: &str) -> Result<(), String> {
    if refs.len() > MAX_LIST * 4 {
        return Err(format!("{what} cites too many sources"));
    }
    for r in refs {
        if !allowed.contains(r.as_str()) {
            return Err(format!("{what} cites unknown evidence {r}"));
        }
    }
    Ok(())
}

fn check_cited(refs: &[String], allowed: &BTreeSet<&str>, what: &str) -> Result<(), String> {
    check_subset(refs, allowed, what)?;
    if refs.is_empty() {
        return Err(format!("{what} cites no evidence"));
    }
    Ok(())
}

pub fn is_code(code: &str) -> bool {
    !code.is_empty()
        && code.len() <= 64
        && code
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
}

/// A statement grounded in cited deterministic evidence.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidenceItem {
    pub text: String,
    pub cited_sources: Vec<String>,
}

fn check_items(items: &[EvidenceItem], allowed: &BTreeSet<&str>, what: &str) -> Result<(), String> {
    if items.len() > MAX_LIST {
        return Err(format!("too many {what}"));
    }
    for i in items {
        check_text(&i.text, what)?;
        check_cited(&i.cited_sources, allowed, what)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------
// care-flow-brief.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FlowPressure {
    /// Snapshot area code (`emergency`, `placement`, `discharge`, `theatre`,
    /// `workforce`, `transport`, `alerts`, `capacity`).
    pub area: String,
    pub explanation: String,
    pub cited_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CareFlowBriefV1 {
    pub schema_version: String,
    pub summary: String,
    pub pressures: Vec<FlowPressure>,
    /// Decisions only a human may take, each tied to the evidence.
    pub human_decisions: Vec<EvidenceItem>,
    pub cited_sources: Vec<String>,
    pub confidence: Confidence,
    pub limitations: Vec<String>,
    /// Evidence the deterministic input lacked; honest abstention, not output.
    #[serde(default)]
    pub missing_information: Vec<String>,
}

impl CareFlowBriefV1 {
    pub fn validate(&self, areas: &[String], fact_refs: &[String]) -> Result<(), String> {
        if self.schema_version != CARE_FLOW_BRIEF_SCHEMA {
            return Err("unexpected schema_version".into());
        }
        check_text(&self.summary, "summary")?;
        let allowed: BTreeSet<&str> = fact_refs.iter().map(String::as_str).collect();
        if self.pressures.len() > MAX_LIST {
            return Err("too many pressures".into());
        }
        for p in &self.pressures {
            if !areas.iter().any(|a| a == &p.area) {
                return Err(format!("pressure names unknown area {}", p.area));
            }
            check_text(&p.explanation, "pressure explanation")?;
            check_cited(&p.cited_sources, &allowed, "pressure")?;
        }
        check_items(&self.human_decisions, &allowed, "human decisions")?;
        check_cited(&self.cited_sources, &allowed, "brief")?;
        check_list(&self.limitations, "limitations")
    }
}

// ---------------------------------------------------------------------------
// placement-ranking.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RankedPlacement {
    /// `candidate:<location_id>` as supplied by the matcher.
    pub candidate_ref: String,
    pub rationale: String,
    pub cited_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlacementRankingV1 {
    pub schema_version: String,
    pub summary: String,
    /// A permutation of the feasible candidates: nothing added, nothing dropped.
    pub ranked: Vec<RankedPlacement>,
    pub cited_sources: Vec<String>,
    pub confidence: Confidence,
    pub limitations: Vec<String>,
    /// Evidence the deterministic input lacked; honest abstention, not output.
    #[serde(default)]
    pub missing_information: Vec<String>,
}

impl PlacementRankingV1 {
    pub fn validate(&self, candidate_refs: &[String], fact_refs: &[String]) -> Result<(), String> {
        if self.schema_version != PLACEMENT_RANKING_SCHEMA {
            return Err("unexpected schema_version".into());
        }
        check_text(&self.summary, "summary")?;
        let allowed: BTreeSet<&str> = fact_refs.iter().map(String::as_str).collect();
        let want: BTreeSet<&str> = candidate_refs.iter().map(String::as_str).collect();
        let got: BTreeSet<&str> = self
            .ranked
            .iter()
            .map(|r| r.candidate_ref.as_str())
            .collect();
        if self.ranked.len() != candidate_refs.len() || got != want {
            return Err("ranking is not a permutation of the feasible candidates".into());
        }
        for r in &self.ranked {
            check_text(&r.rationale, "rationale")?;
            check_cited(&r.cited_sources, &allowed, "ranked candidate")?;
        }
        check_cited(&self.cited_sources, &allowed, "ranking")?;
        check_list(&self.limitations, "limitations")
    }
}

// ---------------------------------------------------------------------------
// discharge-coordination.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DischargeBarrier {
    /// `task:<id>` of the open coordination task that is the barrier.
    pub task_ref: String,
    pub category: String,
    pub explanation: String,
    pub cited_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct SuggestedStep {
    pub text: String,
    pub requires_confirmation: bool,
    pub cited_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DischargeCoordinationV1 {
    pub schema_version: String,
    pub summary: String,
    pub barriers: Vec<DischargeBarrier>,
    pub suggested_steps: Vec<SuggestedStep>,
    /// Always true: this is coordination, never clinical clearance.
    pub operational_only: bool,
    pub cited_sources: Vec<String>,
    pub confidence: Confidence,
    pub limitations: Vec<String>,
    /// Evidence the deterministic input lacked; honest abstention, not output.
    #[serde(default)]
    pub missing_information: Vec<String>,
}

impl DischargeCoordinationV1 {
    pub fn validate(&self, task_refs: &[String], fact_refs: &[String]) -> Result<(), String> {
        if self.schema_version != DISCHARGE_COORDINATION_SCHEMA {
            return Err("unexpected schema_version".into());
        }
        if !self.operational_only {
            return Err("discharge coordination must be marked operational_only".into());
        }
        check_text(&self.summary, "summary")?;
        check_no_clinical_clearance(&self.summary, "summary")?;
        let allowed: BTreeSet<&str> = fact_refs.iter().map(String::as_str).collect();
        if self.barriers.len() > MAX_LIST {
            return Err("too many barriers".into());
        }
        for b in &self.barriers {
            if !task_refs.iter().any(|t| t == &b.task_ref) {
                return Err(format!("barrier names unknown task {}", b.task_ref));
            }
            if !is_code(&b.category) {
                return Err("barrier category is not a code".into());
            }
            check_text(&b.explanation, "barrier explanation")?;
            check_no_clinical_clearance(&b.explanation, "barrier explanation")?;
            check_cited(&b.cited_sources, &allowed, "barrier")?;
        }
        if self.suggested_steps.len() > MAX_LIST {
            return Err("too many suggested steps".into());
        }
        for s in &self.suggested_steps {
            if !s.requires_confirmation {
                return Err("suggested steps must require confirmation".into());
            }
            check_text(&s.text, "suggested step")?;
            check_no_clinical_clearance(&s.text, "suggested step")?;
            check_cited(&s.cited_sources, &allowed, "suggested step")?;
        }
        check_cited(&self.cited_sources, &allowed, "discharge coordination")?;
        check_list(&self.limitations, "limitations")
    }
}

// ---------------------------------------------------------------------------
// theatre-plan-explanation.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TheatreCaseNote {
    /// `case:<id>` of a case in the session plan.
    pub case_ref: String,
    pub readiness: String,
    pub explanation: String,
    pub cited_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TheatreConflictNote {
    pub code: String,
    pub explanation: String,
    pub cited_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TheatrePlanExplanationV1 {
    pub schema_version: String,
    pub summary: String,
    pub cases: Vec<TheatreCaseNote>,
    pub conflicts: Vec<TheatreConflictNote>,
    pub cited_sources: Vec<String>,
    pub confidence: Confidence,
    pub limitations: Vec<String>,
    /// Evidence the deterministic input lacked; honest abstention, not output.
    #[serde(default)]
    pub missing_information: Vec<String>,
}

impl TheatrePlanExplanationV1 {
    pub fn validate(
        &self,
        case_refs: &[String],
        conflict_codes: &[String],
        fact_refs: &[String],
    ) -> Result<(), String> {
        if self.schema_version != THEATRE_PLAN_EXPLANATION_SCHEMA {
            return Err("unexpected schema_version".into());
        }
        check_text(&self.summary, "summary")?;
        let allowed: BTreeSet<&str> = fact_refs.iter().map(String::as_str).collect();
        if self.cases.len() > MAX_LIST {
            return Err("too many case notes".into());
        }
        for c in &self.cases {
            if !case_refs.iter().any(|r| r == &c.case_ref) {
                return Err(format!("case note names unknown case {}", c.case_ref));
            }
            if !THEATRE_READINESS.contains(&c.readiness.as_str()) {
                return Err("unknown readiness value".into());
            }
            check_text(&c.explanation, "case explanation")?;
            check_cited(&c.cited_sources, &allowed, "case note")?;
        }
        if self.conflicts.len() > MAX_LIST {
            return Err("too many conflicts".into());
        }
        for c in &self.conflicts {
            if !conflict_codes.iter().any(|k| k == &c.code) {
                return Err(format!("conflict note names unknown conflict {}", c.code));
            }
            check_text(&c.explanation, "conflict explanation")?;
            check_cited(&c.cited_sources, &allowed, "conflict note")?;
        }
        check_cited(&self.cited_sources, &allowed, "theatre plan explanation")?;
        check_list(&self.limitations, "limitations")
    }
}

// ---------------------------------------------------------------------------
// operational-forecast-explanation.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForecastPressurePoint {
    pub date: NaiveDate,
    pub explanation: String,
    pub cited_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpsRecommendation {
    pub category: String,
    pub text: String,
    pub requires_confirmation: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OperationalForecastExplanationV1 {
    pub schema_version: String,
    pub summary: String,
    pub pressure_points: Vec<ForecastPressurePoint>,
    pub recommendations: Vec<OpsRecommendation>,
    pub cited_sources: Vec<String>,
    pub confidence: Confidence,
    pub limitations: Vec<String>,
    /// Evidence the deterministic input lacked; honest abstention, not output.
    #[serde(default)]
    pub missing_information: Vec<String>,
}

impl OperationalForecastExplanationV1 {
    pub fn validate(&self, dates: &[NaiveDate], fact_refs: &[String]) -> Result<(), String> {
        if self.schema_version != OPERATIONAL_FORECAST_EXPLANATION_SCHEMA {
            return Err("unexpected schema_version".into());
        }
        check_text(&self.summary, "summary")?;
        let allowed: BTreeSet<&str> = fact_refs.iter().map(String::as_str).collect();
        if self.pressure_points.len() > MAX_LIST {
            return Err("too many pressure points".into());
        }
        for p in &self.pressure_points {
            if !dates.contains(&p.date) {
                return Err("pressure point names a date outside the forecast".into());
            }
            check_text(&p.explanation, "pressure explanation")?;
            check_cited(&p.cited_sources, &allowed, "pressure point")?;
        }
        if self.recommendations.len() > MAX_LIST {
            return Err("too many recommendations".into());
        }
        for r in &self.recommendations {
            if !OPS_RECOMMENDATION_CATEGORIES.contains(&r.category.as_str()) {
                return Err("unknown recommendation category".into());
            }
            if !r.requires_confirmation {
                return Err("recommendations must require confirmation".into());
            }
            check_text(&r.text, "recommendation")?;
        }
        check_cited(&self.cited_sources, &allowed, "forecast explanation")?;
        check_list(&self.limitations, "limitations")
    }
}

// ---------------------------------------------------------------------------
// shift-handoff-draft.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ShiftHandoffDraftV1 {
    pub schema_version: String,
    /// Operational situation (placement, episode state, pending work).
    pub situation: String,
    /// Operational background (episode type, service, since when).
    pub background: String,
    pub pending: Vec<EvidenceItem>,
    pub risks: Vec<EvidenceItem>,
    pub barriers: Vec<EvidenceItem>,
    pub next_steps: Vec<EvidenceItem>,
    pub cited_sources: Vec<String>,
    pub confidence: Confidence,
    pub limitations: Vec<String>,
    /// Evidence the deterministic input lacked; honest abstention, not output.
    #[serde(default)]
    pub missing_information: Vec<String>,
}

impl ShiftHandoffDraftV1 {
    pub fn validate(&self, fact_refs: &[String]) -> Result<(), String> {
        if self.schema_version != SHIFT_HANDOFF_DRAFT_SCHEMA {
            return Err("unexpected schema_version".into());
        }
        check_text(&self.situation, "situation")?;
        check_no_clinical_clearance(&self.situation, "situation")?;
        check_text(&self.background, "background")?;
        let allowed: BTreeSet<&str> = fact_refs.iter().map(String::as_str).collect();
        check_items(&self.pending, &allowed, "pending items")?;
        check_items(&self.risks, &allowed, "risks")?;
        check_items(&self.barriers, &allowed, "barriers")?;
        check_items(&self.next_steps, &allowed, "next steps")?;
        for i in self.next_steps.iter().chain(&self.risks) {
            check_no_clinical_clearance(&i.text, "handoff item")?;
        }
        check_cited(&self.cited_sources, &allowed, "handoff draft")?;
        check_list(&self.limitations, "limitations")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn refs(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn ranking(ids: &[&str]) -> PlacementRankingV1 {
        PlacementRankingV1 {
            schema_version: PLACEMENT_RANKING_SCHEMA.into(),
            summary: "Two feasible beds.".into(),
            ranked: ids
                .iter()
                .map(|id| RankedPlacement {
                    candidate_ref: id.to_string(),
                    rationale: "Closest match.".into(),
                    cited_sources: refs(&[id]),
                })
                .collect(),
            cited_sources: refs(&["request:requirements"]),
            confidence: Confidence::Medium,
            limitations: vec![],
        }
    }

    #[test]
    fn ranking_must_be_a_permutation_of_feasible_candidates() {
        let allowed = refs(&["candidate:a", "candidate:b", "request:requirements"]);
        let cands = refs(&["candidate:a", "candidate:b"]);
        assert!(ranking(&["candidate:b", "candidate:a"])
            .validate(&cands, &allowed)
            .is_ok());
        assert!(ranking(&["candidate:a"])
            .validate(&cands, &allowed)
            .is_err());
        assert!(ranking(&["candidate:a", "candidate:a"])
            .validate(&cands, &allowed)
            .is_err());
        assert!(ranking(&["candidate:a", "candidate:zzz"])
            .validate(&cands, &allowed)
            .is_err());
    }

    #[test]
    fn foreign_evidence_and_autonomous_actions_are_rejected() {
        let allowed = refs(&["candidate:a", "request:requirements"]);
        let cands = refs(&["candidate:a"]);
        let mut r = ranking(&["candidate:a"]);
        r.ranked[0].cited_sources = refs(&["tenant-b:candidate:x"]);
        assert!(r.validate(&cands, &allowed).is_err());
        let mut r = ranking(&["candidate:a"]);
        r.summary = "I have moved the patient to bed A.".into();
        assert!(r.validate(&cands, &allowed).is_err());
    }

    #[test]
    fn discharge_coordination_never_claims_clinical_clearance() {
        let allowed = refs(&["episode:status", "task:1"]);
        let base = DischargeCoordinationV1 {
            schema_version: DISCHARGE_COORDINATION_SCHEMA.into(),
            summary: "One open barrier: transport not booked.".into(),
            barriers: vec![DischargeBarrier {
                task_ref: "task:1".into(),
                category: "transport".into(),
                explanation: "Transport task still pending.".into(),
                cited_sources: refs(&["task:1"]),
            }],
            suggested_steps: vec![SuggestedStep {
                text: "Ask the transport team to confirm a slot.".into(),
                requires_confirmation: true,
                cited_sources: refs(&["task:1"]),
            }],
            operational_only: true,
            cited_sources: refs(&["episode:status"]),
            confidence: Confidence::Medium,
            limitations: vec!["Operational view only.".into()],
        };
        assert!(base.validate(&refs(&["task:1"]), &allowed).is_ok());
        let mut c = base.clone();
        c.summary = "Patient is medically fit for discharge.".into();
        assert!(c.validate(&refs(&["task:1"]), &allowed).is_err());
        let mut c = base.clone();
        c.operational_only = false;
        assert!(c.validate(&refs(&["task:1"]), &allowed).is_err());
        let mut c = base.clone();
        c.suggested_steps[0].requires_confirmation = false;
        assert!(c.validate(&refs(&["task:1"]), &allowed).is_err());
        let mut c = base;
        c.barriers[0].task_ref = "task:other-patient".into();
        assert!(c.validate(&refs(&["task:1"]), &allowed).is_err());
    }

    #[test]
    fn forecast_explanation_is_bounded_to_forecast_dates_and_categories() {
        let d = NaiveDate::from_ymd_opt(2026, 3, 10).unwrap();
        let allowed = refs(&["forecast:summary", "forecast:day:2026-03-10"]);
        let mut e = OperationalForecastExplanationV1 {
            schema_version: OPERATIONAL_FORECAST_EXPLANATION_SCHEMA.into(),
            summary: "Pressure on 10 March.".into(),
            pressure_points: vec![ForecastPressurePoint {
                date: d,
                explanation: "Arrivals above capacity.".into(),
                cited_sources: refs(&["forecast:day:2026-03-10"]),
            }],
            recommendations: vec![OpsRecommendation {
                category: "review_staffing".into(),
                text: "Review the day shift.".into(),
                requires_confirmation: true,
            }],
            cited_sources: refs(&["forecast:summary"]),
            confidence: Confidence::Medium,
            limitations: vec![],
        };
        assert!(e.validate(&[d], &allowed).is_ok());
        e.pressure_points[0].date = NaiveDate::from_ymd_opt(2026, 3, 11).unwrap();
        assert!(e.validate(&[d], &allowed).is_err());
        e.pressure_points[0].date = d;
        e.recommendations[0].category = "hire_staff".into();
        assert!(e.validate(&[d], &allowed).is_err());
    }

    #[test]
    fn oversized_text_is_rejected() {
        let allowed = refs(&["episode:status"]);
        let draft = ShiftHandoffDraftV1 {
            schema_version: SHIFT_HANDOFF_DRAFT_SCHEMA.into(),
            situation: "x".repeat(MAX_TEXT + 1),
            background: "Inpatient since Monday.".into(),
            pending: vec![],
            risks: vec![],
            barriers: vec![],
            next_steps: vec![],
            cited_sources: refs(&["episode:status"]),
            confidence: Confidence::Low,
            limitations: vec![],
        };
        assert!(draft.validate(&allowed).is_err());
    }
}
