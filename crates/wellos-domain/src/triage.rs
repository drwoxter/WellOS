//! Patient access (visit) state machine, operational priority, and the
//! deterministic triage safety rules that bound any AI proposal.
//!
//! A visit is the operational episode from registration or appointment
//! through arrival, triage, care-team assignment and the consultation
//! itself. Transitions are explicit; skipped or backward transitions are
//! rejected. Priority here is *operational* (who is seen first) and never a
//! diagnosis; the deterministic floor computed by [`safety_floor`] can be
//! raised by a human but never lowered, by a human or by dMind.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisitStatus {
    Scheduled,
    Arrived,
    TriageInProgress,
    ReadyForConsultation,
    InConsultation,
    Completed,
    Cancelled,
    NoShow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VisitTransition {
    Arrive,
    StartTriage,
    CompleteTriage,
    StartConsultation,
    /// The consultation was cancelled before signing: the patient is still
    /// waiting and returns to the ready queue.
    ReleaseConsultation,
    CompleteConsultation,
    Cancel,
    MarkNoShow,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid transition {transition:?} from visit status {from:?}")]
pub struct InvalidVisitTransition {
    pub from: VisitStatus,
    pub transition: VisitTransition,
}

impl VisitStatus {
    pub fn apply(self, t: VisitTransition) -> Result<VisitStatus, InvalidVisitTransition> {
        use VisitStatus::*;
        use VisitTransition::*;
        match (self, t) {
            (Scheduled, Arrive) => Ok(Arrived),
            (Arrived, StartTriage) => Ok(TriageInProgress),
            (TriageInProgress, StartTriage) => Ok(TriageInProgress),
            (TriageInProgress, CompleteTriage) => Ok(ReadyForConsultation),
            (ReadyForConsultation, StartConsultation) => Ok(InConsultation),
            (InConsultation, ReleaseConsultation) => Ok(ReadyForConsultation),
            (InConsultation, CompleteConsultation) => Ok(Completed),
            (Scheduled | Arrived | TriageInProgress | ReadyForConsultation, Cancel) => {
                Ok(Cancelled)
            }
            (Scheduled, MarkNoShow) => Ok(NoShow),
            (from, transition) => Err(InvalidVisitTransition { from, transition }),
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            VisitStatus::Completed | VisitStatus::Cancelled | VisitStatus::NoShow
        )
    }

    /// Statuses in which the patient is physically or virtually present and
    /// waiting: at most one such visit may exist per patient.
    pub fn is_open(self) -> bool {
        matches!(
            self,
            VisitStatus::Arrived
                | VisitStatus::TriageInProgress
                | VisitStatus::ReadyForConsultation
                | VisitStatus::InConsultation
        )
    }

    pub fn as_str(self) -> &'static str {
        match self {
            VisitStatus::Scheduled => "scheduled",
            VisitStatus::Arrived => "arrived",
            VisitStatus::TriageInProgress => "triage_in_progress",
            VisitStatus::ReadyForConsultation => "ready_for_consultation",
            VisitStatus::InConsultation => "in_consultation",
            VisitStatus::Completed => "completed",
            VisitStatus::Cancelled => "cancelled",
            VisitStatus::NoShow => "no_show",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "scheduled" => VisitStatus::Scheduled,
            "arrived" => VisitStatus::Arrived,
            "triage_in_progress" => VisitStatus::TriageInProgress,
            "ready_for_consultation" => VisitStatus::ReadyForConsultation,
            "in_consultation" => VisitStatus::InConsultation,
            "completed" => VisitStatus::Completed,
            "cancelled" => VisitStatus::Cancelled,
            "no_show" => VisitStatus::NoShow,
            _ => return None,
        })
    }
}

/// How the patient reached the facility. `Scheduled` visits start in the
/// `scheduled` status; every other kind is an arrival.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArrivalKind {
    Scheduled,
    WalkIn,
    Urgent,
    Remote,
}

impl ArrivalKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ArrivalKind::Scheduled => "scheduled",
            ArrivalKind::WalkIn => "walk_in",
            ArrivalKind::Urgent => "urgent",
            ArrivalKind::Remote => "remote",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "scheduled" => ArrivalKind::Scheduled,
            "walk_in" => ArrivalKind::WalkIn,
            "urgent" => ArrivalKind::Urgent,
            "remote" => ArrivalKind::Remote,
            _ => return None,
        })
    }
}

/// Operational priority, ordered from least to most urgent so that
/// `max(rule, proposal)` is the safety-preserving combination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    NonUrgent,
    Standard,
    Urgent,
    Immediate,
}

impl Priority {
    pub fn as_str(self) -> &'static str {
        match self {
            Priority::NonUrgent => "non_urgent",
            Priority::Standard => "standard",
            Priority::Urgent => "urgent",
            Priority::Immediate => "immediate",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "non_urgent" => Priority::NonUrgent,
            "standard" => Priority::Standard,
            "urgent" => Priority::Urgent,
            "immediate" => Priority::Immediate,
            _ => return None,
        })
    }
}

/// Explicit red flags a triage professional can record. Each maps to a
/// deterministic minimum priority; they are never inferred from free text.
pub const RED_FLAGS: &[(&str, Priority)] = &[
    ("airway_compromise", Priority::Immediate),
    ("unresponsive", Priority::Immediate),
    ("severe_bleeding", Priority::Immediate),
    ("chest_pain", Priority::Urgent),
    ("stroke_signs", Priority::Immediate),
    ("severe_breathing_difficulty", Priority::Urgent),
    ("anaphylaxis_signs", Priority::Immediate),
    ("severe_pain", Priority::Urgent),
    ("suicidal_ideation", Priority::Urgent),
    ("pregnancy_complication", Priority::Urgent),
];

pub fn is_red_flag(code: &str) -> bool {
    RED_FLAGS.iter().any(|(c, _)| *c == code)
}

/// Structured concerns offered in the triage form; free text supplements
/// them but never drives a rule.
pub const CONCERNS: &[&str] = &[
    "fever",
    "cough",
    "shortness_of_breath",
    "chest_pain",
    "abdominal_pain",
    "headache",
    "dizziness",
    "vomiting",
    "injury",
    "rash",
    "urinary_symptoms",
    "mental_health",
    "medication_review",
    "follow_up",
    "other",
];

pub fn is_concern(code: &str) -> bool {
    CONCERNS.contains(&code)
}

/// Requested services/specialties routable in this slice.
pub const SERVICES: &[&str] = &["general_medicine", "emergency", "nursing", "telehealth"];

pub fn is_service(code: &str) -> bool {
    SERVICES.contains(&code)
}

/// Vital-sign values considered by the deterministic rules. Missing values
/// never raise or lower priority.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriageVitals {
    pub systolic_mmhg: Option<Decimal>,
    pub heart_rate_bpm: Option<Decimal>,
    pub respiratory_rate_bpm: Option<Decimal>,
    pub temperature_c: Option<Decimal>,
    pub spo2_percent: Option<Decimal>,
}

/// One deterministic reason contributing to the safety floor.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyRuleHit {
    pub rule: String,
    pub priority: Priority,
}

/// Deterministic safety floor: the minimum operational priority implied by
/// explicit red flags, the urgent arrival kind and grossly abnormal vitals.
/// Rules version `triage-safety@1.0.0`.
pub const SAFETY_RULES_VERSION: &str = "triage-safety@1.0.0";

pub fn safety_floor(
    arrival: ArrivalKind,
    red_flags: &[String],
    vitals: &TriageVitals,
) -> (Priority, Vec<SafetyRuleHit>) {
    let mut hits: Vec<SafetyRuleHit> = Vec::new();
    if arrival == ArrivalKind::Urgent {
        hits.push(SafetyRuleHit {
            rule: "arrival:urgent".into(),
            priority: Priority::Urgent,
        });
    }
    for flag in red_flags {
        if let Some((code, p)) = RED_FLAGS.iter().find(|(c, _)| c == flag) {
            hits.push(SafetyRuleHit {
                rule: format!("red_flag:{code}"),
                priority: *p,
            });
        }
    }
    let d = |v: i64| Decimal::from(v);
    if let Some(spo2) = vitals.spo2_percent {
        if spo2 < d(90) {
            hits.push(SafetyRuleHit {
                rule: "vitals:spo2_below_90".into(),
                priority: Priority::Immediate,
            });
        } else if spo2 < d(94) {
            hits.push(SafetyRuleHit {
                rule: "vitals:spo2_below_94".into(),
                priority: Priority::Urgent,
            });
        }
    }
    if let Some(sbp) = vitals.systolic_mmhg {
        if sbp < d(90) {
            hits.push(SafetyRuleHit {
                rule: "vitals:systolic_below_90".into(),
                priority: Priority::Immediate,
            });
        } else if sbp > d(180) {
            hits.push(SafetyRuleHit {
                rule: "vitals:systolic_above_180".into(),
                priority: Priority::Urgent,
            });
        }
    }
    if let Some(hr) = vitals.heart_rate_bpm {
        if hr > d(130) || hr < d(40) {
            hits.push(SafetyRuleHit {
                rule: "vitals:heart_rate_extreme".into(),
                priority: Priority::Urgent,
            });
        }
    }
    if let Some(rr) = vitals.respiratory_rate_bpm {
        if rr > d(30) || rr < d(8) {
            hits.push(SafetyRuleHit {
                rule: "vitals:respiratory_rate_extreme".into(),
                priority: Priority::Urgent,
            });
        }
    }
    if let Some(t) = vitals.temperature_c {
        if t >= Decimal::new(395, 1) || t < d(35) {
            hits.push(SafetyRuleHit {
                rule: "vitals:temperature_extreme".into(),
                priority: Priority::Urgent,
            });
        }
    }
    let floor = hits
        .iter()
        .map(|h| h.priority)
        .max()
        .unwrap_or(Priority::NonUrgent);
    (floor, hits)
}

/// Versioned structured output of the A2 triage assistant. Every field is a
/// proposal for the triage professional; nothing here is applied without an
/// explicit accept/override decision and the deterministic floor is never
/// undercut.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TriageProposalV1 {
    pub schema_version: String,
    pub proposed_priority: Priority,
    /// The deterministic floor the proposal was clamped to, when it applied.
    pub safety_floor: Priority,
    pub raised_to_floor: bool,
    pub proposed_service: String,
    pub important_facts: Vec<String>,
    pub missing_information: Vec<String>,
    pub contradictions: Vec<String>,
    pub handoff_summary: String,
    pub rationale: Vec<String>,
    pub confidence: crate::ai::Confidence,
    pub limitations: Vec<String>,
    /// References of the source facts/fields used.
    pub cited_sources: Vec<String>,
}

pub const TRIAGE_PROPOSAL_SCHEMA: &str = "triage-proposal.v1";

impl TriageProposalV1 {
    /// Enforce the deterministic floor after generation: a proposal may
    /// never be less urgent than the rules, whatever the provider returned.
    pub fn clamp_to_floor(mut self, floor: Priority) -> Self {
        self.safety_floor = floor;
        if self.proposed_priority < floor {
            self.proposed_priority = floor;
            self.raised_to_floor = true;
        }
        self
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != TRIAGE_PROPOSAL_SCHEMA {
            return Err(format!("unexpected schema {}", self.schema_version));
        }
        if !is_service(&self.proposed_service) {
            return Err(format!("unknown service {}", self.proposed_service));
        }
        if self.handoff_summary.trim().is_empty() {
            return Err("handoff summary is empty".into());
        }
        if self.limitations.is_empty() {
            return Err("limitations are required".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use VisitStatus::*;
    use VisitTransition::*;

    #[test]
    fn golden_path_transitions() {
        let s = Scheduled;
        let s = s.apply(Arrive).unwrap();
        let s = s.apply(StartTriage).unwrap();
        let s = s.apply(CompleteTriage).unwrap();
        let s = s.apply(StartConsultation).unwrap();
        assert_eq!(s, InConsultation);
        assert_eq!(s.apply(CompleteConsultation).unwrap(), Completed);
    }

    #[test]
    fn skipped_and_backward_transitions_are_rejected() {
        assert!(Scheduled.apply(StartTriage).is_err());
        assert!(Scheduled.apply(StartConsultation).is_err());
        assert!(Arrived.apply(CompleteTriage).is_err());
        assert!(Arrived.apply(StartConsultation).is_err());
        assert!(ReadyForConsultation.apply(Arrive).is_err());
        assert!(InConsultation.apply(Cancel).is_err());
        assert!(Completed.apply(Cancel).is_err());
        assert!(Arrived.apply(MarkNoShow).is_err());
        assert!(Cancelled.apply(Arrive).is_err());
    }

    #[test]
    fn release_returns_to_ready() {
        assert_eq!(
            InConsultation.apply(ReleaseConsultation).unwrap(),
            ReadyForConsultation
        );
        assert!(ReadyForConsultation.apply(ReleaseConsultation).is_err());
    }

    #[test]
    fn status_round_trips() {
        for s in [
            Scheduled,
            Arrived,
            TriageInProgress,
            ReadyForConsultation,
            InConsultation,
            Completed,
            Cancelled,
            NoShow,
        ] {
            assert_eq!(VisitStatus::parse(s.as_str()), Some(s));
            assert_eq!(s.is_terminal(), matches!(s, Completed | Cancelled | NoShow));
        }
        assert!(VisitStatus::parse("waiting").is_none());
    }

    #[test]
    fn priority_orders_from_least_to_most_urgent() {
        assert!(Priority::NonUrgent < Priority::Standard);
        assert!(Priority::Standard < Priority::Urgent);
        assert!(Priority::Urgent < Priority::Immediate);
    }

    #[test]
    fn floor_without_findings_is_non_urgent() {
        let (floor, hits) = safety_floor(ArrivalKind::WalkIn, &[], &TriageVitals::default());
        assert_eq!(floor, Priority::NonUrgent);
        assert!(hits.is_empty());
    }

    #[test]
    fn urgent_arrival_and_red_flags_raise_floor() {
        let (floor, _) = safety_floor(ArrivalKind::Urgent, &[], &TriageVitals::default());
        assert_eq!(floor, Priority::Urgent);
        let (floor, hits) = safety_floor(
            ArrivalKind::WalkIn,
            &["chest_pain".into(), "stroke_signs".into()],
            &TriageVitals::default(),
        );
        assert_eq!(floor, Priority::Immediate);
        assert_eq!(hits.len(), 2);
        // Unknown flags are ignored rather than trusted.
        let (floor, hits) = safety_floor(
            ArrivalKind::WalkIn,
            &["made_up".into()],
            &TriageVitals::default(),
        );
        assert_eq!(floor, Priority::NonUrgent);
        assert!(hits.is_empty());
    }

    #[test]
    fn abnormal_vitals_raise_floor() {
        let v = TriageVitals {
            spo2_percent: Some(Decimal::from(88)),
            ..Default::default()
        };
        assert_eq!(
            safety_floor(ArrivalKind::WalkIn, &[], &v).0,
            Priority::Immediate
        );
        let v = TriageVitals {
            spo2_percent: Some(Decimal::from(92)),
            ..Default::default()
        };
        assert_eq!(
            safety_floor(ArrivalKind::WalkIn, &[], &v).0,
            Priority::Urgent
        );
        let v = TriageVitals {
            temperature_c: Some(Decimal::new(395, 1)),
            ..Default::default()
        };
        assert_eq!(
            safety_floor(ArrivalKind::WalkIn, &[], &v).0,
            Priority::Urgent
        );
        let v = TriageVitals {
            systolic_mmhg: Some(Decimal::from(120)),
            heart_rate_bpm: Some(Decimal::from(80)),
            respiratory_rate_bpm: Some(Decimal::from(16)),
            temperature_c: Some(Decimal::new(370, 1)),
            spo2_percent: Some(Decimal::from(98)),
        };
        assert_eq!(
            safety_floor(ArrivalKind::WalkIn, &[], &v).0,
            Priority::NonUrgent
        );
    }

    fn proposal(p: Priority) -> TriageProposalV1 {
        TriageProposalV1 {
            schema_version: TRIAGE_PROPOSAL_SCHEMA.into(),
            proposed_priority: p,
            safety_floor: Priority::NonUrgent,
            raised_to_floor: false,
            proposed_service: "general_medicine".into(),
            important_facts: vec![],
            missing_information: vec![],
            contradictions: vec![],
            handoff_summary: "x".into(),
            rationale: vec![],
            confidence: crate::ai::Confidence::Medium,
            limitations: vec!["l".into()],
            cited_sources: vec![],
        }
    }

    #[test]
    fn proposal_is_clamped_to_floor_but_never_lowered() {
        let p = proposal(Priority::Standard).clamp_to_floor(Priority::Urgent);
        assert_eq!(p.proposed_priority, Priority::Urgent);
        assert!(p.raised_to_floor);
        let p = proposal(Priority::Immediate).clamp_to_floor(Priority::Urgent);
        assert_eq!(p.proposed_priority, Priority::Immediate);
        assert!(!p.raised_to_floor);
        assert!(p.validate().is_ok());
    }

    #[test]
    fn proposal_validation_rejects_unknown_service_and_missing_limitations() {
        let mut p = proposal(Priority::Standard);
        p.proposed_service = "cardiology".into();
        assert!(p.validate().is_err());
        let mut p = proposal(Priority::Standard);
        p.limitations.clear();
        assert!(p.validate().is_err());
    }
}
