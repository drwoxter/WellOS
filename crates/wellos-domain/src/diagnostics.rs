//! Clinical orders and diagnostics: deterministic state machines, the open
//! orderable configuration, typed result values and the versioned
//! `diagnostic-safety.v1` preflight engine.
//!
//! Nothing here depends on model output. The result-review loop
//! ([`crate::result_loop`]) stays the authoritative review / notification /
//! closure state; the order status below describes *fulfilment* only.

use std::collections::{BTreeMap, BTreeSet};

use chrono::{DateTime, Duration, Utc};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::access::is_valid_code;

// ---------------------------------------------------------------------------
// Order fulfilment state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderStatus {
    Placed,
    Accepted,
    Scheduled,
    InProgress,
    Completed,
    OnHold,
    Cancelled,
    Rejected,
    EnteredInError,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderTransition {
    Accept,
    Schedule,
    /// Acquisition / collection started. From `accepted` this is only valid
    /// for an explicit non-scheduled fulfilment mode.
    Start,
    Complete,
    Hold,
    Resume,
    Cancel,
    Reject,
    EnterInError,
    /// The linked appointment was cancelled / no-showed: the order returns
    /// to `accepted` and carries a schedule conflict for a human decision.
    Unschedule,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FulfilmentMode {
    Scheduled,
    Immediate,
    Inpatient,
    Bedside,
    WalkIn,
}

impl FulfilmentMode {
    pub fn as_str(self) -> &'static str {
        match self {
            FulfilmentMode::Scheduled => "scheduled",
            FulfilmentMode::Immediate => "immediate",
            FulfilmentMode::Inpatient => "inpatient",
            FulfilmentMode::Bedside => "bedside",
            FulfilmentMode::WalkIn => "walk_in",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "scheduled" => FulfilmentMode::Scheduled,
            "immediate" => FulfilmentMode::Immediate,
            "inpatient" => FulfilmentMode::Inpatient,
            "bedside" => FulfilmentMode::Bedside,
            "walk_in" => FulfilmentMode::WalkIn,
            _ => return None,
        })
    }

    /// Whether this mode needs an Access appointment before acquisition.
    pub fn requires_appointment(self) -> bool {
        matches!(self, FulfilmentMode::Scheduled)
    }
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid order transition {transition:?} from {from:?} ({mode:?})")]
pub struct InvalidOrderTransition {
    pub from: OrderStatus,
    pub transition: OrderTransition,
    pub mode: FulfilmentMode,
}

impl OrderStatus {
    pub fn apply(
        self,
        t: OrderTransition,
        mode: FulfilmentMode,
    ) -> Result<OrderStatus, InvalidOrderTransition> {
        use OrderStatus::*;
        use OrderTransition::*;
        let next = match (self, t) {
            (Placed, Accept) => Some(Accepted),
            (Accepted, Schedule) => Some(Scheduled),
            (Scheduled, Schedule) => Some(Scheduled), // reschedule keeps state
            (Scheduled, Start) => Some(InProgress),
            // Immediate / inpatient / bedside / walk-in work skips the
            // appointment; a scheduled order must be scheduled first.
            (Accepted, Start) if !mode.requires_appointment() => Some(InProgress),
            (InProgress, Complete) => Some(Completed),
            (Placed, Hold) | (Accepted, Hold) | (Scheduled, Hold) => Some(OnHold),
            (OnHold, Resume) => Some(Accepted),
            (Placed, Cancel) | (Accepted, Cancel) | (Scheduled, Cancel) | (OnHold, Cancel) => {
                Some(Cancelled)
            }
            (InProgress, Cancel) => Some(Cancelled),
            (Placed, Reject) | (Accepted, Reject) => Some(Rejected),
            (Scheduled, Unschedule) => Some(Accepted),
            (EnteredInError, EnterInError) => None,
            (_, EnterInError) => Some(EnteredInError),
            _ => None,
        };
        next.ok_or(InvalidOrderTransition {
            from: self,
            transition: t,
            mode,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            OrderStatus::Completed
                | OrderStatus::Cancelled
                | OrderStatus::Rejected
                | OrderStatus::EnteredInError
        )
    }

    /// Whether the order still occupies a fulfilment worklist.
    pub fn is_open(self) -> bool {
        !self.is_terminal()
    }

    pub fn as_str(self) -> &'static str {
        match self {
            OrderStatus::Placed => "placed",
            OrderStatus::Accepted => "accepted",
            OrderStatus::Scheduled => "scheduled",
            OrderStatus::InProgress => "in_progress",
            OrderStatus::Completed => "completed",
            OrderStatus::OnHold => "on_hold",
            OrderStatus::Cancelled => "cancelled",
            OrderStatus::Rejected => "rejected",
            OrderStatus::EnteredInError => "entered_in_error",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "placed" => OrderStatus::Placed,
            "accepted" => OrderStatus::Accepted,
            "scheduled" => OrderStatus::Scheduled,
            "in_progress" => OrderStatus::InProgress,
            "completed" => OrderStatus::Completed,
            "on_hold" => OrderStatus::OnHold,
            "cancelled" => OrderStatus::Cancelled,
            "rejected" => OrderStatus::Rejected,
            "entered_in_error" => OrderStatus::EnteredInError,
            _ => return None,
        })
    }

    pub const ALL: [OrderStatus; 9] = [
        OrderStatus::Placed,
        OrderStatus::Accepted,
        OrderStatus::Scheduled,
        OrderStatus::InProgress,
        OrderStatus::Completed,
        OrderStatus::OnHold,
        OrderStatus::Cancelled,
        OrderStatus::Rejected,
        OrderStatus::EnteredInError,
    ];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OrderPriority {
    Routine,
    Timed,
    Urgent,
    Stat,
}

impl OrderPriority {
    pub fn as_str(self) -> &'static str {
        match self {
            OrderPriority::Routine => "routine",
            OrderPriority::Urgent => "urgent",
            OrderPriority::Stat => "stat",
            OrderPriority::Timed => "timed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "routine" => OrderPriority::Routine,
            "urgent" => OrderPriority::Urgent,
            "stat" => OrderPriority::Stat,
            "timed" => OrderPriority::Timed,
            _ => return None,
        })
    }

    /// Worklist ordering weight (higher first).
    pub fn rank(self) -> i32 {
        match self {
            OrderPriority::Stat => 3,
            OrderPriority::Urgent => 2,
            OrderPriority::Timed => 1,
            OrderPriority::Routine => 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Report state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReportStatus {
    Preliminary,
    Final,
    Amended,
    Corrected,
    Cancelled,
    EnteredInError,
}

impl ReportStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ReportStatus::Preliminary => "preliminary",
            ReportStatus::Final => "final",
            ReportStatus::Amended => "amended",
            ReportStatus::Corrected => "corrected",
            ReportStatus::Cancelled => "cancelled",
            ReportStatus::EnteredInError => "entered_in_error",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "preliminary" => ReportStatus::Preliminary,
            "final" => ReportStatus::Final,
            "amended" => ReportStatus::Amended,
            "corrected" => ReportStatus::Corrected,
            "cancelled" => ReportStatus::Cancelled,
            "entered_in_error" => ReportStatus::EnteredInError,
            _ => return None,
        })
    }

    /// Whether a report with this status may be followed by one with `next`
    /// (the new row supersedes this one). `None` = first report of an order.
    pub fn can_be_replaced_by(prev: Option<ReportStatus>, next: ReportStatus) -> bool {
        use ReportStatus::*;
        match (prev, next) {
            (None, Preliminary) | (None, Final) => true,
            (None, _) => false,
            (Some(Preliminary), Preliminary | Final | Cancelled | EnteredInError) => true,
            (Some(Final | Amended | Corrected), Amended | Corrected | EnteredInError) => true,
            _ => false,
        }
    }

    /// Reports in these states are "complete" for the result loop and the
    /// professional review: they reach the clinician.
    pub fn is_reviewable(self) -> bool {
        matches!(
            self,
            ReportStatus::Final | ReportStatus::Amended | ReportStatus::Corrected
        )
    }

    /// Whether the report may ever be released to the patient.
    pub fn is_releasable(self) -> bool {
        self.is_reviewable()
    }
}

// ---------------------------------------------------------------------------
// Typed result values
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResultType {
    #[default]
    Quantity,
    Text,
    Coded,
    Boolean,
    Datetime,
    Narrative,
}

impl ResultType {
    pub fn as_str(self) -> &'static str {
        match self {
            ResultType::Quantity => "quantity",
            ResultType::Text => "text",
            ResultType::Coded => "coded",
            ResultType::Boolean => "boolean",
            ResultType::Datetime => "datetime",
            ResultType::Narrative => "narrative",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "quantity" => ResultType::Quantity,
            "text" => ResultType::Text,
            "coded" => ResultType::Coded,
            "boolean" => ResultType::Boolean,
            "datetime" => ResultType::Datetime,
            "narrative" => ResultType::Narrative,
            _ => return None,
        })
    }
}

/// One typed result component value. Exactly one representation per value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ResultValue {
    Quantity {
        value: Decimal,
        unit: String,
    },
    Text {
        text: String,
    },
    Coded {
        code: String,
        system: String,
        #[serde(default)]
        display: Option<String>,
    },
    Boolean {
        value: bool,
    },
    Datetime {
        value: DateTime<Utc>,
    },
    Narrative {
        text: String,
    },
}

pub const MAX_TEXT_VALUE_CHARS: usize = 4_000;
pub const MAX_NARRATIVE_CHARS: usize = 40_000;

impl ResultValue {
    pub fn result_type(&self) -> ResultType {
        match self {
            ResultValue::Quantity { .. } => ResultType::Quantity,
            ResultValue::Text { .. } => ResultType::Text,
            ResultValue::Coded { .. } => ResultType::Coded,
            ResultValue::Boolean { .. } => ResultType::Boolean,
            ResultValue::Datetime { .. } => ResultType::Datetime,
            ResultValue::Narrative { .. } => ResultType::Narrative,
        }
    }

    pub fn validate(&self) -> Result<(), String> {
        match self {
            ResultValue::Quantity { unit, .. } => {
                if unit.trim().is_empty() || unit.chars().count() > 32 {
                    return Err("quantity unit is empty or oversized".into());
                }
            }
            ResultValue::Text { text } => {
                if text.trim().is_empty() || text.chars().count() > MAX_TEXT_VALUE_CHARS {
                    return Err("text value is empty or oversized".into());
                }
            }
            ResultValue::Coded {
                code,
                system,
                display,
            } => {
                if code.trim().is_empty() || code.chars().count() > 64 {
                    return Err("coded value code is empty or oversized".into());
                }
                if system.trim().is_empty() || system.chars().count() > 200 {
                    return Err("coded value system is empty or oversized".into());
                }
                if display.as_deref().is_some_and(|d| d.chars().count() > 400) {
                    return Err("coded value display is oversized".into());
                }
            }
            ResultValue::Boolean { .. } | ResultValue::Datetime { .. } => {}
            ResultValue::Narrative { text } => {
                if text.trim().is_empty() || text.chars().count() > MAX_NARRATIVE_CHARS {
                    return Err("narrative is empty or oversized".into());
                }
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Interpretation {
    Unknown,
    Normal,
    Abnormal,
    Critical,
}

impl Interpretation {
    pub fn as_str(self) -> &'static str {
        match self {
            Interpretation::Unknown => "unknown",
            Interpretation::Normal => "normal",
            Interpretation::Abnormal => "abnormal",
            Interpretation::Critical => "critical",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "unknown" => Interpretation::Unknown,
            "normal" => Interpretation::Normal,
            "abnormal" => Interpretation::Abnormal,
            "critical" => Interpretation::Critical,
            _ => return None,
        })
    }
}

/// Configured deterministic interpretation of a coded / boolean component:
/// which values count as critical or abnormal. Numeric components use the
/// versioned critical rules and the reference range.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CodedInterpretationRule {
    #[serde(default)]
    pub critical_codes: Vec<String>,
    #[serde(default)]
    pub abnormal_codes: Vec<String>,
    #[serde(default)]
    pub normal_codes: Vec<String>,
    /// For boolean components: the value that is abnormal (`true` by default).
    #[serde(default)]
    pub abnormal_when: Option<bool>,
    /// For boolean components: whether the abnormal value is critical.
    #[serde(default)]
    pub boolean_critical: bool,
}

/// Parse a reference range of the form `a-b`, `a - b`, `<b`, `>a`,
/// `≤b`, `≥a` (signed decimals allowed) into inclusive bounds.
pub fn parse_reference_range(range: &str) -> Option<(Option<Decimal>, Option<Decimal>)> {
    let r = range.trim().replace(',', ".");
    if r.is_empty() {
        return None;
    }
    let parse = |s: &str| s.trim().parse::<Decimal>().ok();
    if let Some(rest) = r.strip_prefix("<=").or_else(|| r.strip_prefix('≤')) {
        return parse(rest).map(|hi| (None, Some(hi)));
    }
    if let Some(rest) = r.strip_prefix(">=").or_else(|| r.strip_prefix('≥')) {
        return parse(rest).map(|lo| (Some(lo), None));
    }
    if let Some(rest) = r.strip_prefix('<') {
        return parse(rest).map(|hi| (None, Some(hi)));
    }
    if let Some(rest) = r.strip_prefix('>') {
        return parse(rest).map(|lo| (Some(lo), None));
    }
    // "a-b" where either side may be negative: split on the dash that is
    // followed by a digit and preceded by a digit or whitespace.
    let chars: Vec<char> = r.chars().collect();
    for i in 1..chars.len() {
        if chars[i] == '-' || chars[i] == '–' {
            let prev = chars[..i].iter().rev().find(|c| !c.is_whitespace());
            if prev.is_some_and(|c| c.is_ascii_digit() || *c == '.') {
                let lo: String = chars[..i].iter().collect();
                let hi: String = chars[i + 1..].iter().collect();
                if let (Some(lo), Some(hi)) = (parse(&lo), parse(&hi)) {
                    if lo <= hi {
                        return Some((Some(lo), Some(hi)));
                    }
                }
            }
        }
    }
    None
}

/// Deterministic interpretation of one component. `critical` is the outcome
/// of the versioned critical rules (numeric only); the reference range
/// supplies abnormality; coded / boolean values use the configured rule.
pub fn interpret_component(
    value: &ResultValue,
    reference_range: Option<&str>,
    critical_by_rule: bool,
    coded_rule: Option<&CodedInterpretationRule>,
) -> Interpretation {
    if critical_by_rule {
        return Interpretation::Critical;
    }
    match value {
        ResultValue::Quantity { value, .. } => match reference_range.and_then(parse_reference_range)
        {
            Some((lo, hi)) => {
                if lo.is_some_and(|lo| *value < lo) || hi.is_some_and(|hi| *value > hi) {
                    Interpretation::Abnormal
                } else {
                    Interpretation::Normal
                }
            }
            None => Interpretation::Unknown,
        },
        ResultValue::Coded { code, .. } => match coded_rule {
            Some(rule) if rule.critical_codes.iter().any(|c| c == code) => Interpretation::Critical,
            Some(rule) if rule.abnormal_codes.iter().any(|c| c == code) => Interpretation::Abnormal,
            Some(rule) if rule.normal_codes.iter().any(|c| c == code) => Interpretation::Normal,
            _ => Interpretation::Unknown,
        },
        ResultValue::Boolean { value } => match coded_rule {
            Some(rule) => {
                let abnormal_when = rule.abnormal_when.unwrap_or(true);
                if *value == abnormal_when {
                    if rule.boolean_critical {
                        Interpretation::Critical
                    } else {
                        Interpretation::Abnormal
                    }
                } else {
                    Interpretation::Normal
                }
            }
            None => Interpretation::Unknown,
        },
        ResultValue::Text { .. } | ResultValue::Datetime { .. } | ResultValue::Narrative { .. } => {
            Interpretation::Unknown
        }
    }
}

/// Report-level criticality: the worst component interpretation, plus a
/// configured list of conclusion codes that are critical by themselves.
pub fn report_criticality(
    components: &[Interpretation],
    conclusion_codes: &[String],
    critical_conclusion_codes: &[String],
) -> Interpretation {
    let mut worst = components
        .iter()
        .copied()
        .max()
        .unwrap_or(Interpretation::Unknown);
    if conclusion_codes
        .iter()
        .any(|c| critical_conclusion_codes.iter().any(|k| k == c))
    {
        worst = Interpretation::Critical;
    }
    worst
}

// ---------------------------------------------------------------------------
// Specimen custody
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecimenStatus {
    Planned,
    Collected,
    InTransit,
    Received,
    Processing,
    Processed,
    Rejected,
    Consumed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpecimenEvent {
    Collected,
    Dispatched,
    Received,
    ProcessingStarted,
    Processed,
    Rejected,
    Consumed,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid specimen event {event:?} from {from:?}")]
pub struct InvalidSpecimenEvent {
    pub from: SpecimenStatus,
    pub event: SpecimenEvent,
}

impl SpecimenStatus {
    pub fn apply(self, e: SpecimenEvent) -> Result<SpecimenStatus, InvalidSpecimenEvent> {
        type S = SpecimenStatus;
        type E = SpecimenEvent;
        let next = match (self, e) {
            (S::Planned, E::Collected) => Some(S::Collected),
            (S::Collected, E::Dispatched) => Some(S::InTransit),
            (S::Collected, E::Received) | (S::InTransit, E::Received) => Some(S::Received),
            // Bedside / point-of-care processing without transport.
            (S::Collected, E::ProcessingStarted) | (S::Received, E::ProcessingStarted) => {
                Some(S::Processing)
            }
            (S::Processing, E::Processed) => Some(S::Processed),
            (S::Processed, E::Consumed) | (S::Processing, E::Consumed) => Some(S::Consumed),
            (S::Collected, E::Rejected)
            | (S::InTransit, E::Rejected)
            | (S::Received, E::Rejected)
            | (S::Processing, E::Rejected) => Some(S::Rejected),
            _ => None,
        };
        next.ok_or(InvalidSpecimenEvent { from: self, event: e })
    }

    pub fn as_str(self) -> &'static str {
        match self {
            SpecimenStatus::Planned => "planned",
            SpecimenStatus::Collected => "collected",
            SpecimenStatus::InTransit => "in_transit",
            SpecimenStatus::Received => "received",
            SpecimenStatus::Processing => "processing",
            SpecimenStatus::Processed => "processed",
            SpecimenStatus::Rejected => "rejected",
            SpecimenStatus::Consumed => "consumed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "planned" => SpecimenStatus::Planned,
            "collected" => SpecimenStatus::Collected,
            "in_transit" => SpecimenStatus::InTransit,
            "received" => SpecimenStatus::Received,
            "processing" => SpecimenStatus::Processing,
            "processed" => SpecimenStatus::Processed,
            "rejected" => SpecimenStatus::Rejected,
            "consumed" => SpecimenStatus::Consumed,
            _ => return None,
        })
    }
}

impl SpecimenEvent {
    pub fn as_str(self) -> &'static str {
        match self {
            SpecimenEvent::Collected => "collected",
            SpecimenEvent::Dispatched => "dispatched",
            SpecimenEvent::Received => "received",
            SpecimenEvent::ProcessingStarted => "processing_started",
            SpecimenEvent::Processed => "processed",
            SpecimenEvent::Rejected => "rejected",
            SpecimenEvent::Consumed => "consumed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "collected" => SpecimenEvent::Collected,
            "dispatched" => SpecimenEvent::Dispatched,
            "received" => SpecimenEvent::Received,
            "processing_started" => SpecimenEvent::ProcessingStarted,
            "processed" => SpecimenEvent::Processed,
            "rejected" => SpecimenEvent::Rejected,
            "consumed" => SpecimenEvent::Consumed,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Open diagnostic orderable configuration (catalog kind `diagnostic_orderable`)
// ---------------------------------------------------------------------------

pub const DIAGNOSTIC_ORDERABLE_KIND: &str = "diagnostic_orderable";

/// One expected result component of an orderable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComponentSpec {
    pub code: String,
    #[serde(default = "default_loinc")]
    pub system: String,
    pub display: String,
    #[serde(default = "default_quantity")]
    pub result_type: ResultType,
    #[serde(default)]
    pub unit: Option<String>,
    #[serde(default)]
    pub reference_range: Option<String>,
    #[serde(default)]
    pub interpretation: Option<CodedInterpretationRule>,
}

fn default_loinc() -> String {
    "http://loinc.org".into()
}

fn default_quantity() -> ResultType {
    ResultType::Quantity
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct SpecimenSpec {
    pub type_code: String,
    #[serde(default)]
    pub container_code: Option<String>,
    #[serde(default)]
    pub minimum_volume_ml: Option<u32>,
    #[serde(default)]
    pub fasting_hours: Option<u32>,
}

/// A structured prerequisite or contraindication the deterministic safety
/// engine evaluates before the clinician confirms.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyRuleSpec {
    /// Stable id within the orderable, e.g. `pregnancy`, `egfr`, `implant`.
    pub id: String,
    pub kind: SafetyRuleKind,
    pub text_en: String,
    pub text_es: String,
    /// `hard_stop` requires correction or an authorized reasoned override;
    /// `warning` requires explicit acknowledgement.
    #[serde(default)]
    pub severity: SafetySeverity,
    /// For `fact` rules: the patient-fact key that triggers the finding
    /// (`pregnancy_possible`, `renal_impairment`, `metal_implant`,
    /// `contrast_allergy`, `anticoagulant`, `pacemaker`, ...).
    #[serde(default)]
    pub fact_key: Option<String>,
    /// For `question` rules: the answer that resolves the finding; an
    /// unanswered or opposite answer leaves it open.
    #[serde(default)]
    pub satisfied_by_answer: Option<bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetyRuleKind {
    /// A structured yes/no question that must be answered before ordering.
    Question,
    /// A patient fact that, when present, raises the finding.
    Fact,
    /// A prerequisite the tenant requires (e.g. recent creatinine); raised
    /// when the named fact is *absent*.
    Prerequisite,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SafetySeverity {
    #[default]
    Warning,
    HardStop,
}

/// Kind-specific configuration of a `diagnostic_orderable` catalog entry.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct OrderableConfig {
    /// Open, tenant-defined category code (`laboratory`, `radiology`,
    /// `cardiology`, `pathology`, `dentistry`, `endoscopy`, ...). Never an
    /// authorization role.
    pub category_code: String,
    #[serde(default)]
    pub modality_code: Option<String>,
    #[serde(default = "default_quantity")]
    pub result_type: ResultType,
    #[serde(default)]
    pub components: Vec<ComponentSpec>,
    /// Member orderable codes when this entry is a panel.
    #[serde(default)]
    pub panel_member_codes: Vec<String>,
    #[serde(default)]
    pub specimen: Option<SpecimenSpec>,
    #[serde(default)]
    pub preparation_en: Option<String>,
    #[serde(default)]
    pub preparation_es: Option<String>,
    /// Existing `clinical_service` code used to schedule the acquisition
    /// through dMind Access. Absent = no appointment is ever needed.
    #[serde(default)]
    pub scheduling_service_code: Option<String>,
    #[serde(default)]
    pub required_resource_types: Vec<String>,
    /// Fulfilment modes the tenant allows for this orderable; defaults to
    /// `scheduled` when a scheduling service exists, otherwise `immediate`.
    #[serde(default)]
    pub fulfilment_modes: Vec<FulfilmentMode>,
    #[serde(default)]
    pub safety_rules: Vec<SafetyRuleSpec>,
    /// Equivalent recent orders within this many days raise a duplicate warning.
    #[serde(default = "default_duplicate_window")]
    pub duplicate_window_days: u32,
    /// Orderable codes that are redundant with this one (e.g. a panel that
    /// already contains it).
    #[serde(default)]
    pub redundant_with_codes: Vec<String>,
    /// Conclusion codes (coded value) that make a report critical.
    #[serde(default)]
    pub critical_conclusion_codes: Vec<String>,
    /// Whether a (collected) specimen is required before results can be
    /// recorded.
    #[serde(default)]
    pub requires_specimen: Option<bool>,
    /// External imaging-study reference expected (modality orderables).
    #[serde(default)]
    pub expects_imaging_study: bool,
    #[serde(default)]
    pub legacy_migrated: bool,
}

fn default_duplicate_window() -> u32 {
    7
}

impl OrderableConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !is_valid_code(&self.category_code) {
            return Err("category_code must be a stable code".into());
        }
        if let Some(m) = &self.modality_code {
            if !is_valid_code(m) {
                return Err("modality_code must be a stable code".into());
            }
        }
        if let Some(s) = &self.scheduling_service_code {
            if !is_valid_code(s) {
                return Err("scheduling_service_code must be a stable code".into());
            }
        }
        for c in self
            .panel_member_codes
            .iter()
            .chain(self.required_resource_types.iter())
            .chain(self.redundant_with_codes.iter())
        {
            if !is_valid_code(c) {
                return Err(format!("invalid code {c:?}"));
            }
        }
        if self.components.len() > 200 {
            return Err("too many components".into());
        }
        let mut seen = BTreeSet::new();
        for c in &self.components {
            if c.code.trim().is_empty() || c.code.chars().count() > 64 {
                return Err("component code is empty or oversized".into());
            }
            if c.display.trim().is_empty() || c.display.chars().count() > 200 {
                return Err("component display is empty or oversized".into());
            }
            if !seen.insert(c.code.as_str()) {
                return Err(format!("duplicate component {}", c.code));
            }
            if let Some(r) = &c.reference_range {
                if parse_reference_range(r).is_none() {
                    return Err(format!("unparseable reference range {r:?}"));
                }
            }
        }
        if self.panel_member_codes.len() > 100 {
            return Err("too many panel members".into());
        }
        let mut ids = BTreeSet::new();
        for r in &self.safety_rules {
            if !is_valid_code(&r.id) || !ids.insert(r.id.as_str()) {
                return Err(format!("safety rule id {:?} is invalid or duplicated", r.id));
            }
            if r.text_en.trim().is_empty() || r.text_es.trim().is_empty() {
                return Err(format!("safety rule {} needs EN and ES text", r.id));
            }
            match r.kind {
                SafetyRuleKind::Fact | SafetyRuleKind::Prerequisite => {
                    if r.fact_key.as_deref().is_none_or(|k| !is_valid_code(k)) {
                        return Err(format!("safety rule {} needs a fact_key", r.id));
                    }
                }
                SafetyRuleKind::Question => {}
            }
        }
        if let Some(sp) = &self.specimen {
            if !is_valid_code(&sp.type_code) {
                return Err("specimen.type_code must be a stable code".into());
            }
        }
        if self.duplicate_window_days > 3650 {
            return Err("duplicate_window_days must not exceed 3650".into());
        }
        for p in [&self.preparation_en, &self.preparation_es]
            .into_iter()
            .flatten()
        {
            if p.chars().count() > 4_000 {
                return Err("preparation text is oversized".into());
            }
        }
        if self.fulfilment_modes.contains(&FulfilmentMode::Scheduled)
            && self.scheduling_service_code.is_none()
        {
            return Err("scheduled fulfilment needs a scheduling_service_code".into());
        }
        Ok(())
    }

    pub fn is_panel(&self) -> bool {
        !self.panel_member_codes.is_empty()
    }

    /// Fulfilment modes allowed for this orderable.
    pub fn allowed_modes(&self) -> Vec<FulfilmentMode> {
        if !self.fulfilment_modes.is_empty() {
            return self.fulfilment_modes.clone();
        }
        if self.scheduling_service_code.is_some() {
            vec![FulfilmentMode::Scheduled]
        } else {
            vec![FulfilmentMode::Immediate]
        }
    }

    pub fn default_mode(&self) -> FulfilmentMode {
        self.allowed_modes()[0]
    }

    pub fn needs_specimen(&self) -> bool {
        self.requires_specimen
            .unwrap_or(self.specimen.is_some())
    }

    pub fn component(&self, code: &str) -> Option<&ComponentSpec> {
        self.components.iter().find(|c| c.code == code)
    }
}

// ---------------------------------------------------------------------------
// diagnostic-safety.v1
// ---------------------------------------------------------------------------

pub const DIAGNOSTIC_SAFETY_VERSION: &str = "diagnostic-safety.v1";

/// A candidate orderable as the clinician composed it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyCandidate {
    pub orderable_id: Uuid,
    pub code: String,
    pub name_en: String,
    pub name_es: String,
    pub config: OrderableConfig,
    pub fulfilment_mode: FulfilmentMode,
    pub priority: OrderPriority,
    #[serde(default)]
    pub requested_window_start: Option<DateTime<Utc>>,
    #[serde(default)]
    pub requested_window_end: Option<DateTime<Utc>>,
}

/// An equivalent order the patient already has (same orderable code).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentOrder {
    pub service_request_id: Uuid,
    pub orderable_code: String,
    pub created_at: DateTime<Utc>,
    pub order_status: OrderStatus,
    /// Whether a final report exists for it.
    pub has_result: bool,
}

/// Patient facts the engine consults. Keys are open (`pregnancy_possible`,
/// `renal_impairment`, `metal_implant`, `contrast_allergy`, `anticoagulant`,
/// `pacemaker`, `recent_creatinine`, ...); their presence/absence drives
/// `fact` and `prerequisite` rules. Allergy / medication substances are
/// matched case-insensitively against rule fact keys of the form
/// `allergy:<substance>` / `medication:<substance>`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct PatientSafetyFacts {
    #[serde(default)]
    pub facts: BTreeSet<String>,
    #[serde(default)]
    pub allergies: Vec<String>,
    #[serde(default)]
    pub medications: Vec<String>,
}

impl PatientSafetyFacts {
    fn has(&self, key: &str) -> bool {
        if self.facts.contains(key) {
            return true;
        }
        if let Some(sub) = key.strip_prefix("allergy:") {
            return self
                .allergies
                .iter()
                .any(|a| a.to_lowercase().contains(&sub.to_lowercase()));
        }
        if let Some(sub) = key.strip_prefix("medication:") {
            return self
                .medications
                .iter()
                .any(|m| m.to_lowercase().contains(&sub.to_lowercase()));
        }
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyInput {
    pub now: DateTime<Utc>,
    pub candidates: Vec<SafetyCandidate>,
    pub recent_orders: Vec<RecentOrder>,
    pub facts: PatientSafetyFacts,
    /// Answers to `question` rules, keyed `<orderable_code>:<rule_id>`.
    #[serde(default)]
    pub answers: BTreeMap<String, bool>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FindingKind {
    DuplicateRecent,
    PendingEquivalent,
    SpecimenRequirement,
    Preparation,
    Prerequisite,
    Contraindication,
    UnansweredQuestion,
    Timing,
    RedundantCombination,
    FulfilmentModeNotAllowed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyFinding {
    /// Stable id: `<kind>:<orderable_code>[:<rule_id>]`.
    pub id: String,
    pub kind: FindingKind,
    pub severity: SafetySeverity,
    pub orderable_id: Uuid,
    pub orderable_code: String,
    pub text_en: String,
    pub text_es: String,
    /// Deterministic evidence references (`order:<id>`, `fact:<key>`,
    /// `rule:<orderable>:<id>`).
    pub evidence: Vec<String>,
    /// `true` when the finding is a structured question the clinician can
    /// resolve by answering.
    pub answerable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SafetyEvaluation {
    pub engine_version: String,
    pub findings: Vec<SafetyFinding>,
}

impl SafetyEvaluation {
    pub fn hard_stops(&self) -> Vec<&SafetyFinding> {
        self.findings
            .iter()
            .filter(|f| f.severity == SafetySeverity::HardStop)
            .collect()
    }

    pub fn warnings(&self) -> Vec<&SafetyFinding> {
        self.findings
            .iter()
            .filter(|f| f.severity == SafetySeverity::Warning)
            .collect()
    }

    /// Whether confirmation may proceed given the clinician's explicit
    /// acknowledgements and (optional, authorized) override reason.
    pub fn confirmable(
        &self,
        acknowledged: &BTreeSet<String>,
        override_reason: Option<&str>,
    ) -> Result<(), SafetyBlock> {
        let unacknowledged: Vec<String> = self
            .warnings()
            .iter()
            .filter(|f| !acknowledged.contains(&f.id))
            .map(|f| f.id.clone())
            .collect();
        if !unacknowledged.is_empty() {
            return Err(SafetyBlock::Unacknowledged(unacknowledged));
        }
        let stops: Vec<String> = self.hard_stops().iter().map(|f| f.id.clone()).collect();
        if !stops.is_empty() {
            match override_reason.map(str::trim) {
                Some(r) if r.chars().count() >= 10 => Ok(()),
                _ => Err(SafetyBlock::HardStops(stops)),
            }
        } else {
            Ok(())
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SafetyBlock {
    #[error("warnings require explicit acknowledgement")]
    Unacknowledged(Vec<String>),
    #[error("hard stops require correction or an authorized reasoned override")]
    HardStops(Vec<String>),
}

fn finding(
    kind: FindingKind,
    severity: SafetySeverity,
    c: &SafetyCandidate,
    suffix: Option<&str>,
    text_en: String,
    text_es: String,
    evidence: Vec<String>,
    answerable: bool,
) -> SafetyFinding {
    let kind_str = serde_json::to_value(kind)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default();
    let id = match suffix {
        Some(s) => format!("{kind_str}:{}:{s}", c.code),
        None => format!("{kind_str}:{}", c.code),
    };
    SafetyFinding {
        id,
        kind,
        severity,
        orderable_id: c.orderable_id,
        orderable_code: c.code.clone(),
        text_en,
        text_es,
        evidence,
        answerable,
    }
}

/// Evaluate the configured rules. Deterministic: the same input always
/// yields the same findings in the same order. This is a bounded preflight
/// over tenant-configured rules, not a complete clinical decision-support
/// system.
pub fn evaluate_safety(input: &SafetyInput) -> SafetyEvaluation {
    let mut findings = Vec::new();
    let codes_in_group: BTreeSet<&str> = input.candidates.iter().map(|c| c.code.as_str()).collect();
    for c in &input.candidates {
        let cfg = &c.config;
        // Fulfilment mode must be one the tenant allows for this orderable.
        if !cfg.allowed_modes().contains(&c.fulfilment_mode) {
            findings.push(finding(
                FindingKind::FulfilmentModeNotAllowed,
                SafetySeverity::HardStop,
                c,
                None,
                format!(
                    "{} cannot be fulfilled as '{}' at this facility; allowed: {}",
                    c.name_en,
                    c.fulfilment_mode.as_str(),
                    cfg.allowed_modes()
                        .iter()
                        .map(|m| m.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                format!(
                    "{} no puede realizarse como '{}'; modos permitidos: {}",
                    c.name_es,
                    c.fulfilment_mode.as_str(),
                    cfg.allowed_modes()
                        .iter()
                        .map(|m| m.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                ),
                vec![format!("catalog:{}", c.code)],
                false,
            ));
        }
        // Duplicate / pending equivalents.
        let window = Duration::days(i64::from(cfg.duplicate_window_days));
        let mut pending = Vec::new();
        let mut recent = Vec::new();
        for o in input
            .recent_orders
            .iter()
            .filter(|o| o.orderable_code == c.code)
        {
            if o.order_status.is_open() {
                pending.push(o);
            } else if o.has_result && input.now - o.created_at <= window {
                recent.push(o);
            }
        }
        if !pending.is_empty() {
            findings.push(finding(
                FindingKind::PendingEquivalent,
                SafetySeverity::Warning,
                c,
                None,
                format!(
                    "{} is already ordered and not yet completed ({} pending)",
                    c.name_en,
                    pending.len()
                ),
                format!(
                    "{} ya está solicitado y aún no se ha completado ({} pendiente)",
                    c.name_es,
                    pending.len()
                ),
                pending
                    .iter()
                    .map(|o| format!("order:{}", o.service_request_id))
                    .collect(),
                false,
            ));
        }
        if !recent.is_empty() {
            findings.push(finding(
                FindingKind::DuplicateRecent,
                SafetySeverity::Warning,
                c,
                None,
                format!(
                    "{} was resulted within the last {} day(s)",
                    c.name_en, cfg.duplicate_window_days
                ),
                format!(
                    "{} ya tiene resultado en los últimos {} día(s)",
                    c.name_es, cfg.duplicate_window_days
                ),
                recent
                    .iter()
                    .map(|o| format!("order:{}", o.service_request_id))
                    .collect(),
                false,
            ));
        }
        // Redundant combinations within the same group.
        for r in &cfg.redundant_with_codes {
            if codes_in_group.contains(r.as_str()) {
                findings.push(finding(
                    FindingKind::RedundantCombination,
                    SafetySeverity::Warning,
                    c,
                    Some(r),
                    format!("{} is redundant with {} in the same request", c.name_en, r),
                    format!("{} es redundante con {} en la misma solicitud", c.name_es, r),
                    vec![format!("catalog:{}", c.code), format!("catalog:{r}")],
                    false,
                ));
            }
        }
        // Specimen / preparation requirements surface as informational
        // warnings the clinician acknowledges.
        if let Some(sp) = &cfg.specimen {
            if let Some(h) = sp.fasting_hours.filter(|h| *h > 0) {
                findings.push(finding(
                    FindingKind::SpecimenRequirement,
                    SafetySeverity::Warning,
                    c,
                    Some("fasting"),
                    format!("{} requires {h} h fasting before specimen collection", c.name_en),
                    format!("{} requiere {h} h de ayuno antes de la toma de muestra", c.name_es),
                    vec![format!("catalog:{}", c.code)],
                    false,
                ));
            }
        }
        if cfg.preparation_en.is_some() || cfg.preparation_es.is_some() {
            findings.push(finding(
                FindingKind::Preparation,
                SafetySeverity::Warning,
                c,
                None,
                format!(
                    "{} has patient preparation: {}",
                    c.name_en,
                    cfg.preparation_en.clone().unwrap_or_default()
                ),
                format!(
                    "{} tiene preparación para el paciente: {}",
                    c.name_es,
                    cfg.preparation_es.clone().unwrap_or_default()
                ),
                vec![format!("catalog:{}", c.code)],
                false,
            ));
        }
        // Configured structured rules.
        for r in &cfg.safety_rules {
            let answer_key = format!("{}:{}", c.code, r.id);
            match r.kind {
                SafetyRuleKind::Question => {
                    let satisfied = match (input.answers.get(&answer_key), r.satisfied_by_answer) {
                        (Some(a), Some(expected)) => *a == expected,
                        (Some(_), None) => true,
                        (None, _) => false,
                    };
                    if !satisfied {
                        let answered = input.answers.contains_key(&answer_key);
                        findings.push(finding(
                            if answered {
                                FindingKind::Contraindication
                            } else {
                                FindingKind::UnansweredQuestion
                            },
                            if answered { r.severity } else { SafetySeverity::Warning },
                            c,
                            Some(&r.id),
                            r.text_en.clone(),
                            r.text_es.clone(),
                            vec![format!("rule:{}:{}", c.code, r.id)],
                            !answered,
                        ));
                    }
                }
                SafetyRuleKind::Fact => {
                    let key = r.fact_key.as_deref().unwrap_or("");
                    if input.facts.has(key) {
                        findings.push(finding(
                            FindingKind::Contraindication,
                            r.severity,
                            c,
                            Some(&r.id),
                            r.text_en.clone(),
                            r.text_es.clone(),
                            vec![format!("rule:{}:{}", c.code, r.id), format!("fact:{key}")],
                            false,
                        ));
                    }
                }
                SafetyRuleKind::Prerequisite => {
                    let key = r.fact_key.as_deref().unwrap_or("");
                    if !input.facts.has(key) {
                        findings.push(finding(
                            FindingKind::Prerequisite,
                            r.severity,
                            c,
                            Some(&r.id),
                            r.text_en.clone(),
                            r.text_es.clone(),
                            vec![format!("rule:{}:{}", c.code, r.id), format!("missing:{key}")],
                            false,
                        ));
                    }
                }
            }
        }
        // Timing.
        if c.priority == OrderPriority::Timed
            && c.requested_window_start.is_none()
            && c.requested_window_end.is_none()
        {
            findings.push(finding(
                FindingKind::Timing,
                SafetySeverity::HardStop,
                c,
                None,
                format!("{} is clinically timed but has no requested window", c.name_en),
                format!(
                    "{} está programado clínicamente pero no tiene ventana solicitada",
                    c.name_es
                ),
                vec![format!("catalog:{}", c.code)],
                false,
            ));
        }
        if let Some(end) = c.requested_window_end {
            if end < input.now {
                findings.push(finding(
                    FindingKind::Timing,
                    SafetySeverity::HardStop,
                    c,
                    Some("past"),
                    format!("{} requested window ends in the past", c.name_en),
                    format!("{} tiene una ventana solicitada que termina en el pasado", c.name_es),
                    vec![format!("catalog:{}", c.code)],
                    false,
                ));
            }
        }
    }
    findings.sort_by(|a, b| a.id.cmp(&b.id));
    findings.dedup_by(|a, b| a.id == b.id);
    SafetyEvaluation {
        engine_version: DIAGNOSTIC_SAFETY_VERSION.into(),
        findings,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> OrderableConfig {
        OrderableConfig {
            category_code: "radiology".into(),
            modality_code: Some("mri".into()),
            result_type: ResultType::Narrative,
            scheduling_service_code: Some("mri_brain".into()),
            fulfilment_modes: vec![FulfilmentMode::Scheduled],
            safety_rules: vec![
                SafetyRuleSpec {
                    id: "implant".into(),
                    kind: SafetyRuleKind::Question,
                    text_en: "Does the patient have a metal implant or pacemaker?".into(),
                    text_es: "¿Tiene el paciente un implante metálico o marcapasos?".into(),
                    severity: SafetySeverity::HardStop,
                    fact_key: None,
                    satisfied_by_answer: Some(false),
                },
                SafetyRuleSpec {
                    id: "pacemaker".into(),
                    kind: SafetyRuleKind::Fact,
                    text_en: "Pacemaker recorded".into(),
                    text_es: "Marcapasos registrado".into(),
                    severity: SafetySeverity::HardStop,
                    fact_key: Some("pacemaker".into()),
                    satisfied_by_answer: None,
                },
            ],
            duplicate_window_days: 30,
            ..Default::default()
        }
    }

    fn cand(code: &str, cfg: OrderableConfig, mode: FulfilmentMode) -> SafetyCandidate {
        SafetyCandidate {
            orderable_id: Uuid::now_v7(),
            code: code.into(),
            name_en: code.into(),
            name_es: code.into(),
            config: cfg,
            fulfilment_mode: mode,
            priority: OrderPriority::Routine,
            requested_window_start: None,
            requested_window_end: None,
        }
    }

    #[test]
    fn order_state_machine_requires_explicit_mode_for_immediate_start() {
        assert_eq!(
            OrderStatus::Accepted.apply(OrderTransition::Start, FulfilmentMode::Scheduled),
            Err(InvalidOrderTransition {
                from: OrderStatus::Accepted,
                transition: OrderTransition::Start,
                mode: FulfilmentMode::Scheduled
            })
        );
        assert_eq!(
            OrderStatus::Accepted.apply(OrderTransition::Start, FulfilmentMode::Immediate),
            Ok(OrderStatus::InProgress)
        );
        let s = OrderStatus::Placed
            .apply(OrderTransition::Accept, FulfilmentMode::Scheduled)
            .and_then(|s| s.apply(OrderTransition::Schedule, FulfilmentMode::Scheduled))
            .and_then(|s| s.apply(OrderTransition::Start, FulfilmentMode::Scheduled))
            .and_then(|s| s.apply(OrderTransition::Complete, FulfilmentMode::Scheduled))
            .unwrap();
        assert_eq!(s, OrderStatus::Completed);
        assert!(OrderStatus::Completed
            .apply(OrderTransition::Cancel, FulfilmentMode::Scheduled)
            .is_err());
        assert_eq!(
            OrderStatus::Scheduled.apply(OrderTransition::Unschedule, FulfilmentMode::Scheduled),
            Ok(OrderStatus::Accepted)
        );
        for s in OrderStatus::ALL {
            assert_eq!(OrderStatus::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn report_chain_rules() {
        assert!(ReportStatus::can_be_replaced_by(None, ReportStatus::Final));
        assert!(!ReportStatus::can_be_replaced_by(None, ReportStatus::Amended));
        assert!(ReportStatus::can_be_replaced_by(
            Some(ReportStatus::Preliminary),
            ReportStatus::Final
        ));
        assert!(ReportStatus::can_be_replaced_by(
            Some(ReportStatus::Final),
            ReportStatus::Corrected
        ));
        assert!(!ReportStatus::can_be_replaced_by(
            Some(ReportStatus::Final),
            ReportStatus::Preliminary
        ));
        assert!(!ReportStatus::can_be_replaced_by(
            Some(ReportStatus::Cancelled),
            ReportStatus::Final
        ));
    }

    #[test]
    fn reference_ranges_and_interpretation() {
        assert_eq!(
            parse_reference_range("3.5-5.1"),
            Some((Some(Decimal::new(35, 1)), Some(Decimal::new(51, 1))))
        );
        assert_eq!(
            parse_reference_range("-2.0 - 2.0"),
            Some((Some(Decimal::new(-20, 1)), Some(Decimal::new(20, 1))))
        );
        assert_eq!(parse_reference_range("<200"), Some((None, Some(Decimal::new(200, 0)))));
        assert_eq!(parse_reference_range("≥60"), Some((Some(Decimal::new(60, 0)), None)));
        assert_eq!(parse_reference_range("negative"), None);
        let q = ResultValue::Quantity {
            value: Decimal::new(61, 1),
            unit: "mmol/L".into(),
        };
        assert_eq!(
            interpret_component(&q, Some("3.5-5.1"), false, None),
            Interpretation::Abnormal
        );
        assert_eq!(
            interpret_component(&q, Some("3.5-5.1"), true, None),
            Interpretation::Critical
        );
        assert_eq!(interpret_component(&q, None, false, None), Interpretation::Unknown);
        let coded = ResultValue::Coded {
            code: "malignant".into(),
            system: "http://snomed.info/sct".into(),
            display: None,
        };
        let rule = CodedInterpretationRule {
            critical_codes: vec!["malignant".into()],
            ..Default::default()
        };
        assert_eq!(
            interpret_component(&coded, None, false, Some(&rule)),
            Interpretation::Critical
        );
        assert_eq!(
            report_criticality(
                &[Interpretation::Normal, Interpretation::Abnormal],
                &[],
                &[]
            ),
            Interpretation::Abnormal
        );
        assert_eq!(
            report_criticality(&[Interpretation::Normal], &["c1".into()], &["c1".into()]),
            Interpretation::Critical
        );
    }

    #[test]
    fn specimen_chain() {
        let s = SpecimenStatus::Planned
            .apply(SpecimenEvent::Collected)
            .and_then(|s| s.apply(SpecimenEvent::Dispatched))
            .and_then(|s| s.apply(SpecimenEvent::Received))
            .and_then(|s| s.apply(SpecimenEvent::ProcessingStarted))
            .and_then(|s| s.apply(SpecimenEvent::Processed))
            .unwrap();
        assert_eq!(s, SpecimenStatus::Processed);
        assert!(SpecimenStatus::Planned
            .apply(SpecimenEvent::Received)
            .is_err());
        assert_eq!(
            SpecimenStatus::Received.apply(SpecimenEvent::Rejected),
            Ok(SpecimenStatus::Rejected)
        );
        assert!(SpecimenStatus::Rejected
            .apply(SpecimenEvent::Received)
            .is_err());
    }

    #[test]
    fn orderable_config_validation() {
        assert!(cfg().validate().is_ok());
        let mut bad = cfg();
        bad.category_code = "Radiology!".into();
        assert!(bad.validate().is_err());
        let mut bad = cfg();
        bad.scheduling_service_code = None;
        assert!(bad.validate().is_err(), "scheduled mode needs a service");
        let mut bad = cfg();
        bad.safety_rules[1].fact_key = None;
        assert!(bad.validate().is_err());
    }

    #[test]
    fn safety_engine_is_deterministic_and_blocks_on_hard_stops() {
        let now = Utc::now();
        let c = cand("mri_brain", cfg(), FulfilmentMode::Scheduled);
        let input = SafetyInput {
            now,
            candidates: vec![c.clone()],
            recent_orders: vec![RecentOrder {
                service_request_id: Uuid::now_v7(),
                orderable_code: "mri_brain".into(),
                created_at: now - Duration::days(3),
                order_status: OrderStatus::Accepted,
                has_result: false,
            }],
            facts: PatientSafetyFacts::default(),
            answers: BTreeMap::new(),
        };
        let a = evaluate_safety(&input);
        let b = evaluate_safety(&input);
        assert_eq!(a, b);
        let ids: Vec<&str> = a.findings.iter().map(|f| f.id.as_str()).collect();
        assert!(ids.contains(&"pending_equivalent:mri_brain"));
        assert!(ids.contains(&"unanswered_question:mri_brain:implant"));
        let q = a
            .findings
            .iter()
            .find(|f| f.id == "unanswered_question:mri_brain:implant")
            .unwrap();
        assert!(q.answerable);
        assert_eq!(q.severity, SafetySeverity::Warning);
        // Nothing acknowledged: blocked.
        assert!(matches!(
            a.confirmable(&BTreeSet::new(), None),
            Err(SafetyBlock::Unacknowledged(_))
        ));
        // Answering "yes, implant" turns the question into a hard stop.
        let mut answered = input.clone();
        answered
            .answers
            .insert("mri_brain:implant".into(), true);
        let e = evaluate_safety(&answered);
        let stop = e
            .findings
            .iter()
            .find(|f| f.id == "contraindication:mri_brain:implant")
            .unwrap();
        assert_eq!(stop.severity, SafetySeverity::HardStop);
        let acks: BTreeSet<String> = e.warnings().iter().map(|f| f.id.clone()).collect();
        assert!(matches!(
            e.confirmable(&acks, None),
            Err(SafetyBlock::HardStops(_))
        ));
        assert!(e.confirmable(&acks, Some("too short")).is_err());
        assert!(e
            .confirmable(&acks, Some("Radiology confirmed MRI-conditional implant"))
            .is_ok());
        // Answering "no" resolves it.
        let mut resolved = input.clone();
        resolved
            .answers
            .insert("mri_brain:implant".into(), false);
        let r = evaluate_safety(&resolved);
        assert!(!r.findings.iter().any(|f| f.id.contains("implant")));
    }

    #[test]
    fn fact_rules_match_allergies_and_recorded_facts() {
        let mut c = cfg();
        c.safety_rules.push(SafetyRuleSpec {
            id: "contrast".into(),
            kind: SafetyRuleKind::Fact,
            text_en: "Contrast allergy".into(),
            text_es: "Alergia al contraste".into(),
            severity: SafetySeverity::Warning,
            fact_key: Some("allergy:iodinated contrast".into()),
            satisfied_by_answer: None,
        });
        let input = SafetyInput {
            now: Utc::now(),
            candidates: vec![cand("ct_abdomen", c, FulfilmentMode::Scheduled)],
            recent_orders: vec![],
            facts: PatientSafetyFacts {
                facts: ["pacemaker".to_string()].into_iter().collect(),
                allergies: vec!["Iodinated Contrast Media".into()],
                medications: vec![],
            },
            answers: [("ct_abdomen:implant".to_string(), false)]
                .into_iter()
                .collect(),
        };
        let e = evaluate_safety(&input);
        let ids: Vec<&str> = e.findings.iter().map(|f| f.id.as_str()).collect();
        assert!(ids.contains(&"contraindication:ct_abdomen:contrast"));
        assert!(ids.contains(&"contraindication:ct_abdomen:pacemaker"));
        assert_eq!(e.hard_stops().len(), 1);
    }

    #[test]
    fn mode_not_allowed_and_timing_are_hard_stops() {
        let mut c = cand("mri_brain", cfg(), FulfilmentMode::Immediate);
        c.priority = OrderPriority::Timed;
        let input = SafetyInput {
            now: Utc::now(),
            candidates: vec![c],
            recent_orders: vec![],
            facts: PatientSafetyFacts::default(),
            answers: [("mri_brain:implant".to_string(), false)]
                .into_iter()
                .collect(),
        };
        let e = evaluate_safety(&input);
        let ids: Vec<&str> = e.findings.iter().map(|f| f.id.as_str()).collect();
        assert!(ids.contains(&"fulfilment_mode_not_allowed:mri_brain"));
        assert!(ids.contains(&"timing:mri_brain"));
        assert_eq!(e.hard_stops().len(), 2);
    }

    #[test]
    fn typed_values_validate() {
        assert!(ResultValue::Text { text: "  ".into() }.validate().is_err());
        assert!(ResultValue::Coded {
            code: "x".into(),
            system: "".into(),
            display: None
        }
        .validate()
        .is_err());
        assert!(ResultValue::Boolean { value: true }.validate().is_ok());
        let v: ResultValue = serde_json::from_str(
            r#"{"type":"quantity","value":"4.2","unit":"mmol/L"}"#,
        )
        .unwrap();
        assert_eq!(v.result_type(), ResultType::Quantity);
    }
}
