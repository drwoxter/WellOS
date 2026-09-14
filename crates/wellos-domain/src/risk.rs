//! Deterministic, versioned and explainable patient risk assessment.
//!
//! Risk is expressed per *domain* (acute safety, chronic complexity,
//! medication/allergy safety, diagnostic results, preventive care, access
//! and utilization, care coordination) as an ordinal level with the
//! contributing factors, the WellOS records that support each factor, the
//! data that is missing or stale and the trend against the previous
//! assessment. There is deliberately no universal numeric score: a single
//! number hides which domain drives the risk and invites comparisons across
//! patients that the rules do not support.
//!
//! Every level here is computed from recorded facts by the rules in this
//! module (rules version [`RISK_RULES_VERSION`]). A dMind summary may
//! explain these findings but never changes them: [`RiskSummaryV1::align_to_deterministic`]
//! forces the summary's levels back to the deterministic ones, so a critical
//! signal can never be lowered or hidden by an AI result.

use crate::ai::Confidence;
use crate::triage::{safety_floor, ArrivalKind, Priority, TriageVitals};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const RISK_RULES_VERSION: &str = "risk-rules.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskLevel {
    InsufficientData,
    Low,
    Moderate,
    High,
    Critical,
}

impl RiskLevel {
    pub fn as_str(self) -> &'static str {
        match self {
            RiskLevel::InsufficientData => "insufficient_data",
            RiskLevel::Low => "low",
            RiskLevel::Moderate => "moderate",
            RiskLevel::High => "high",
            RiskLevel::Critical => "critical",
        }
    }

    pub fn parse(s: &str) -> Option<RiskLevel> {
        Some(match s {
            "insufficient_data" => RiskLevel::InsufficientData,
            "low" => RiskLevel::Low,
            "moderate" => RiskLevel::Moderate,
            "high" => RiskLevel::High,
            "critical" => RiskLevel::Critical,
            _ => return None,
        })
    }

    /// Worklist ordering: critical and high first, unknown last.
    pub fn priority_rank(self) -> i32 {
        match self {
            RiskLevel::Critical => 0,
            RiskLevel::High => 1,
            RiskLevel::Moderate => 2,
            RiskLevel::Low => 3,
            RiskLevel::InsufficientData => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskDomain {
    AcuteSafety,
    ChronicComplexity,
    MedicationAllergySafety,
    DiagnosticResult,
    PreventiveCare,
    AccessUtilization,
    CareCoordination,
}

impl RiskDomain {
    pub const ALL: [RiskDomain; 7] = [
        RiskDomain::AcuteSafety,
        RiskDomain::ChronicComplexity,
        RiskDomain::MedicationAllergySafety,
        RiskDomain::DiagnosticResult,
        RiskDomain::PreventiveCare,
        RiskDomain::AccessUtilization,
        RiskDomain::CareCoordination,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            RiskDomain::AcuteSafety => "acute_safety",
            RiskDomain::ChronicComplexity => "chronic_complexity",
            RiskDomain::MedicationAllergySafety => "medication_allergy_safety",
            RiskDomain::DiagnosticResult => "diagnostic_result",
            RiskDomain::PreventiveCare => "preventive_care",
            RiskDomain::AccessUtilization => "access_utilization",
            RiskDomain::CareCoordination => "care_coordination",
        }
    }

    pub fn parse(s: &str) -> Option<RiskDomain> {
        RiskDomain::ALL.into_iter().find(|d| d.as_str() == s)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskTrend {
    Improving,
    Stable,
    Worsening,
    Unknown,
}

impl RiskTrend {
    pub fn as_str(self) -> &'static str {
        match self {
            RiskTrend::Improving => "improving",
            RiskTrend::Stable => "stable",
            RiskTrend::Worsening => "worsening",
            RiskTrend::Unknown => "unknown",
        }
    }

    pub fn parse(s: &str) -> Option<RiskTrend> {
        Some(match s {
            "improving" => RiskTrend::Improving,
            "stable" => RiskTrend::Stable,
            "worsening" => RiskTrend::Worsening,
            "unknown" => RiskTrend::Unknown,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RiskReviewStatus {
    Unreviewed,
    Acknowledged,
    Reviewed,
}

impl RiskReviewStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            RiskReviewStatus::Unreviewed => "unreviewed",
            RiskReviewStatus::Acknowledged => "acknowledged",
            RiskReviewStatus::Reviewed => "reviewed",
        }
    }

    pub fn parse(s: &str) -> Option<RiskReviewStatus> {
        Some(match s {
            "unreviewed" => RiskReviewStatus::Unreviewed,
            "acknowledged" => RiskReviewStatus::Acknowledged,
            "reviewed" => RiskReviewStatus::Reviewed,
            _ => return None,
        })
    }
}

/// A pointer to the WellOS record that supports a factor. `record_type`
/// names the table (`observation`, `alert`, `medication`, ...), so the
/// interface can link to the governed view of that record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRef {
    pub record_type: String,
    pub record_id: Uuid,
    pub label: String,
    pub observed_at: Option<DateTime<Utc>>,
}

/// One deterministic reason contributing to a domain level.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskFactor {
    /// Stable rule identifier such as `critical_result_unreviewed`; the
    /// interface translates it into plain language.
    pub code: String,
    pub level: RiskLevel,
    /// Short technical detail (e.g. the analyte and value), never a
    /// diagnosis.
    pub detail: Option<String>,
    pub evidence: Vec<EvidenceRef>,
    /// When the underlying signal was observed (the newest evidence).
    pub detected_at: DateTime<Utc>,
}

/// Missing or stale data that limited a domain assessment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataGap {
    pub code: String,
    pub record_type: String,
    pub record_id: Option<Uuid>,
    pub observed_at: Option<DateTime<Utc>>,
    pub max_age_days: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainAssessment {
    pub domain: RiskDomain,
    pub level: RiskLevel,
    pub factors: Vec<RiskFactor>,
    pub missing_data: Vec<DataGap>,
    pub stale_data: Vec<DataGap>,
    pub trend: RiskTrend,
    /// Newest detection among the factors; `None` when nothing was detected.
    pub detected_at: Option<DateTime<Utc>>,
    pub calculated_at: DateTime<Utc>,
    pub rules_version: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskAssessment {
    pub rules_version: String,
    pub calculated_at: DateTime<Utc>,
    pub overall_level: RiskLevel,
    /// The deterministic level any AI summary is clamped to. Equal to the
    /// overall level: nothing downstream may present the patient as less at
    /// risk than the rules did.
    pub safety_floor: RiskLevel,
    pub trend: RiskTrend,
    pub domains: Vec<DomainAssessment>,
}

impl RiskAssessment {
    pub fn domain(&self, d: RiskDomain) -> Option<&DomainAssessment> {
        self.domains.iter().find(|x| x.domain == d)
    }
}

// ---------------------------------------------------------------------------
// Input facts (plain data read from the record; no I/O here)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AlertFact {
    pub id: Uuid,
    pub severity: String,
    pub status: String,
    pub message: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VisitFact {
    pub id: Uuid,
    pub status: String,
    pub arrival_kind: String,
    pub service: String,
    pub priority: Option<String>,
    pub safety_floor: Option<String>,
    pub occurred_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VitalsFact {
    pub id: Uuid,
    pub recorded_at: DateTime<Utc>,
    pub vitals: TriageVitals,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConditionFact {
    pub id: Uuid,
    pub code: String,
    pub display: String,
    pub clinical_status: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MedicationFact {
    pub id: Uuid,
    pub name: String,
    pub status: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AllergyFact {
    pub id: Uuid,
    pub substance: String,
    pub criticality: String,
    pub recorded_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResultFact {
    pub observation_id: Uuid,
    pub service_request_id: Uuid,
    pub code_loinc: String,
    pub display: String,
    pub value: Option<String>,
    pub unit: Option<String>,
    /// Deterministic critical-rule outcome recorded when the result arrived.
    pub critical: bool,
    /// Outside its reference range (deterministic comparison).
    pub abnormal: bool,
    pub loop_state: String,
    pub effective_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServiceRequestFact {
    pub id: Uuid,
    pub display: String,
    pub loop_state: String,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncounterFact {
    pub id: Uuid,
    pub status: String,
    pub encounter_type: String,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFact {
    pub id: Uuid,
    pub description: String,
    pub status: String,
    pub priority: String,
    pub due_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CareTeamFact {
    pub id: Uuid,
    pub function: String,
    pub has_professional: bool,
}

/// Domain levels of the previous assessment, for trend calculation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PreviousAssessment {
    pub calculated_at: DateTime<Utc>,
    pub levels: Vec<(RiskDomain, RiskLevel)>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskInput {
    pub now: DateTime<Utc>,
    pub birth_date: NaiveDate,
    pub alerts: Vec<AlertFact>,
    /// The patient's open visit, if any.
    pub current_visit: Option<VisitFact>,
    /// Visit history (closed or open) within the look-back window.
    pub visits: Vec<VisitFact>,
    pub latest_vitals: Option<VitalsFact>,
    pub conditions: Vec<ConditionFact>,
    pub medications: Vec<MedicationFact>,
    pub allergies: Vec<AllergyFact>,
    pub results: Vec<ResultFact>,
    pub open_requests: Vec<ServiceRequestFact>,
    pub encounters: Vec<EncounterFact>,
    pub tasks: Vec<TaskFact>,
    pub care_team: Vec<CareTeamFact>,
    pub previous: Option<PreviousAssessment>,
}

impl RiskInput {
    /// Whether any clinical contact was ever recorded. Without it every
    /// domain is `insufficient_data`: an empty record is not a low-risk
    /// record.
    pub fn has_clinical_record(&self) -> bool {
        !self.encounters.is_empty()
            || !self.visits.is_empty()
            || self.current_visit.is_some()
            || self.latest_vitals.is_some()
            || !self.results.is_empty()
            || !self.conditions.is_empty()
            || !self.medications.is_empty()
            || !self.allergies.is_empty()
    }

    pub fn age_years(&self) -> i32 {
        let today = self.now.date_naive();
        let mut age = today.year_ce().1 as i32 - self.birth_date.year_ce().1 as i32;
        if (today.month(), today.day()) < (self.birth_date.month(), self.birth_date.day()) {
            age -= 1;
        }
        age
    }
}

use chrono::Datelike;

// ---------------------------------------------------------------------------
// Rule constants (risk-rules.v1)
// ---------------------------------------------------------------------------

const ACUTE_VITALS_MAX_AGE_HOURS: i64 = 24;
const UNSCHEDULED_WINDOW_DAYS: i64 = 90;
const NO_SHOW_WINDOW_DAYS: i64 = 365;
const FOLLOW_UP_MAX_AGE_DAYS: i64 = 365;
const RESULT_STALE_DAYS: i64 = 365;
const PENDING_RESULT_MAX_DAYS: i64 = 14;
const ALERT_UNHANDLED_MAX_HOURS: i64 = 24;
const POLYPHARMACY_MODERATE: usize = 5;
const POLYPHARMACY_HIGH: usize = 10;
const HBA1C_LOINC: &str = "4548-4";
const HBA1C_MAX_DAYS: i64 = 180;
const CREATININE_LOINC: &str = "2160-0";
const CREATININE_MAX_DAYS: i64 = 365;
const CHOLESTEROL_LOINC: &str = "2093-3";
const LIPID_MAX_DAYS: i64 = 5 * 365;
const BP_MAX_DAYS: i64 = 365;
const LIPID_MIN_AGE: i32 = 40;
const BP_MIN_AGE: i32 = 40;

/// ICD-10 prefixes treated as major chronic conditions for complexity.
const MAJOR_CHRONIC_PREFIXES: &[&str] = &[
    "E10", "E11", "E13", "E14", // diabetes
    "N18", // chronic kidney disease
    "I50", // heart failure
    "I10", "I11", "I12", "I13", // hypertensive disease
    "I48", // atrial fibrillation
    "J44", // COPD
    "J45", // asthma
    "I25", // chronic ischaemic heart disease
    "I63", "I69", // stroke and sequelae
    "F20", "F31", "F33", // severe mental illness
    "C",   // malignant neoplasms
];

fn is_major_chronic(code: &str) -> bool {
    let c = code.trim().to_ascii_uppercase();
    MAJOR_CHRONIC_PREFIXES.iter().any(|p| c.starts_with(p))
}

fn is_diabetes(code: &str) -> bool {
    let c = code.trim().to_ascii_uppercase();
    ["E10", "E11", "E13", "E14"]
        .iter()
        .any(|p| c.starts_with(p))
}

fn is_hypertension(code: &str) -> bool {
    let c = code.trim().to_ascii_uppercase();
    ["I10", "I11", "I12", "I13"]
        .iter()
        .any(|p| c.starts_with(p))
}

fn is_renal_or_cardiac(code: &str) -> bool {
    let c = code.trim().to_ascii_uppercase();
    ["N18", "I50", "E10", "E11", "E13", "E14"]
        .iter()
        .any(|p| c.starts_with(p))
}

/// Allergen families and the medication name fragments that belong to them.
/// Matching is deliberately conservative: a substring hit flags a *possible*
/// conflict for a professional to review; it never withholds or changes a
/// medication.
const ALLERGEN_FAMILIES: &[(&str, &[&str])] = &[
    (
        "penicillin",
        &[
            "penicillin",
            "amoxicillin",
            "ampicillin",
            "piperacillin",
            "amoxicilina",
            "penicilina",
        ],
    ),
    (
        "sulfonamide",
        &["sulfamethoxazole", "sulfadiazine", "sulfa", "cotrimoxazol"],
    ),
    (
        "nsaid",
        &[
            "ibuprofen",
            "naproxen",
            "diclofenac",
            "ketorolac",
            "ibuprofeno",
            "naproxeno",
        ],
    ),
    ("aspirin", &["aspirin", "acetylsalicylic", "aspirina"]),
    (
        "opioid",
        &[
            "codeine",
            "morphine",
            "tramadol",
            "oxycodone",
            "codeina",
            "morfina",
        ],
    ),
    ("iodine", &["iodine", "iodinated", "yodo"]),
];

fn normalize(s: &str) -> String {
    s.trim().to_lowercase()
}

/// Whether an active medication plausibly belongs to a recorded allergen.
pub fn medication_matches_allergy(medication: &str, allergen: &str) -> bool {
    let m = normalize(medication);
    let a = normalize(allergen);
    if a.is_empty() || m.is_empty() {
        return false;
    }
    if m.contains(&a) || a.contains(&m) {
        return true;
    }
    ALLERGEN_FAMILIES.iter().any(|(family, members)| {
        let allergen_in_family = a.contains(family) || members.iter().any(|x| a.contains(x));
        allergen_in_family && members.iter().any(|x| m.contains(x))
    })
}

fn priority_to_level(p: Priority) -> RiskLevel {
    match p {
        Priority::Immediate => RiskLevel::Critical,
        Priority::Urgent => RiskLevel::High,
        Priority::Standard => RiskLevel::Moderate,
        Priority::NonUrgent => RiskLevel::Low,
    }
}

fn parse_priority(s: &str) -> Option<Priority> {
    Some(match s {
        "immediate" => Priority::Immediate,
        "urgent" => Priority::Urgent,
        "standard" => Priority::Standard,
        "non_urgent" => Priority::NonUrgent,
        _ => return None,
    })
}

fn days_between(now: DateTime<Utc>, then: DateTime<Utc>) -> i64 {
    (now - then).num_days()
}

struct DomainBuilder {
    domain: RiskDomain,
    factors: Vec<RiskFactor>,
    missing: Vec<DataGap>,
    stale: Vec<DataGap>,
}

impl DomainBuilder {
    fn new(domain: RiskDomain) -> Self {
        Self {
            domain,
            factors: Vec::new(),
            missing: Vec::new(),
            stale: Vec::new(),
        }
    }

    fn factor(
        &mut self,
        code: &str,
        level: RiskLevel,
        detail: Option<String>,
        evidence: Vec<EvidenceRef>,
        detected_at: DateTime<Utc>,
    ) {
        self.factors.push(RiskFactor {
            code: code.to_string(),
            level,
            detail,
            evidence,
            detected_at,
        });
    }

    fn missing(&mut self, code: &str, record_type: &str) {
        self.missing.push(DataGap {
            code: code.to_string(),
            record_type: record_type.to_string(),
            record_id: None,
            observed_at: None,
            max_age_days: None,
        });
    }

    fn stale(
        &mut self,
        code: &str,
        record_type: &str,
        record_id: Option<Uuid>,
        observed_at: DateTime<Utc>,
        max_age_days: i64,
    ) {
        self.stale.push(DataGap {
            code: code.to_string(),
            record_type: record_type.to_string(),
            record_id,
            observed_at: Some(observed_at),
            max_age_days: Some(max_age_days),
        });
    }

    fn finish(
        self,
        baseline: RiskLevel,
        now: DateTime<Utc>,
        previous: Option<&PreviousAssessment>,
    ) -> DomainAssessment {
        let level = self
            .factors
            .iter()
            .map(|f| f.level)
            .max()
            .map(|m| m.max(baseline))
            .unwrap_or(baseline);
        let detected_at = self.factors.iter().map(|f| f.detected_at).max();
        let trend = trend_for(self.domain, level, previous);
        DomainAssessment {
            domain: self.domain,
            level,
            factors: self.factors,
            missing_data: self.missing,
            stale_data: self.stale,
            trend,
            detected_at,
            calculated_at: now,
            rules_version: RISK_RULES_VERSION.to_string(),
        }
    }
}

fn trend_for(
    domain: RiskDomain,
    level: RiskLevel,
    previous: Option<&PreviousAssessment>,
) -> RiskTrend {
    let Some(prev) = previous else {
        return RiskTrend::Unknown;
    };
    let Some((_, before)) = prev.levels.iter().find(|(d, _)| *d == domain) else {
        return RiskTrend::Unknown;
    };
    if *before == RiskLevel::InsufficientData || level == RiskLevel::InsufficientData {
        return RiskTrend::Unknown;
    }
    match level.cmp(before) {
        std::cmp::Ordering::Greater => RiskTrend::Worsening,
        std::cmp::Ordering::Less => RiskTrend::Improving,
        std::cmp::Ordering::Equal => RiskTrend::Stable,
    }
}

fn evidence(
    record_type: &str,
    id: Uuid,
    label: impl Into<String>,
    at: DateTime<Utc>,
) -> EvidenceRef {
    EvidenceRef {
        record_type: record_type.to_string(),
        record_id: id,
        label: label.into(),
        observed_at: Some(at),
    }
}

fn insufficient_domain(domain: RiskDomain, now: DateTime<Utc>) -> DomainAssessment {
    let mut b = DomainBuilder::new(domain);
    b.missing("no_clinical_record", "patient");
    DomainAssessment {
        domain,
        level: RiskLevel::InsufficientData,
        factors: Vec::new(),
        missing_data: b.missing,
        stale_data: Vec::new(),
        trend: RiskTrend::Unknown,
        detected_at: None,
        calculated_at: now,
        rules_version: RISK_RULES_VERSION.to_string(),
    }
}

/// Compute the versioned deterministic assessment for one patient.
pub fn assess(input: &RiskInput) -> RiskAssessment {
    let now = input.now;
    if !input.has_clinical_record() {
        return RiskAssessment {
            rules_version: RISK_RULES_VERSION.to_string(),
            calculated_at: now,
            overall_level: RiskLevel::InsufficientData,
            safety_floor: RiskLevel::InsufficientData,
            trend: RiskTrend::Unknown,
            domains: RiskDomain::ALL
                .iter()
                .map(|d| insufficient_domain(*d, now))
                .collect(),
        };
    }
    let previous = input.previous.as_ref();
    let domains = vec![
        acute_safety(input).finish(RiskLevel::Low, now, previous),
        chronic_complexity(input).finish(RiskLevel::Low, now, previous),
        medication_safety(input).finish(RiskLevel::Low, now, previous),
        diagnostic_result(input),
        preventive_care(input).finish(RiskLevel::Low, now, previous),
        access_utilization(input).finish(RiskLevel::Low, now, previous),
        care_coordination(input).finish(RiskLevel::Low, now, previous),
    ];
    let overall_level = domains
        .iter()
        .map(|d| d.level)
        .filter(|l| *l != RiskLevel::InsufficientData)
        .max()
        .unwrap_or(RiskLevel::InsufficientData);
    let trend = if domains.iter().any(|d| d.trend == RiskTrend::Worsening) {
        RiskTrend::Worsening
    } else if domains.iter().any(|d| d.trend == RiskTrend::Improving) {
        RiskTrend::Improving
    } else if domains.iter().any(|d| d.trend == RiskTrend::Stable) {
        RiskTrend::Stable
    } else {
        RiskTrend::Unknown
    };
    RiskAssessment {
        rules_version: RISK_RULES_VERSION.to_string(),
        calculated_at: now,
        overall_level,
        safety_floor: overall_level,
        trend,
        domains,
    }
}

fn acute_safety(input: &RiskInput) -> DomainBuilder {
    let now = input.now;
    let mut b = DomainBuilder::new(RiskDomain::AcuteSafety);
    for a in input.alerts.iter().filter(|a| a.status == "open") {
        let level = if a.severity == "critical" {
            RiskLevel::Critical
        } else {
            RiskLevel::High
        };
        b.factor(
            "open_critical_alert",
            level,
            Some(a.message.clone()),
            vec![evidence("alert", a.id, a.message.clone(), a.created_at)],
            a.created_at,
        );
    }
    if let Some(v) = &input.current_visit {
        let floor = v
            .safety_floor
            .as_deref()
            .and_then(parse_priority)
            .map(priority_to_level);
        let priority = v
            .priority
            .as_deref()
            .and_then(parse_priority)
            .map(priority_to_level);
        let level = floor.into_iter().chain(priority).max();
        if let Some(level) = level {
            if level >= RiskLevel::High {
                b.factor(
                    "visit_priority",
                    level,
                    v.priority.clone().or(v.safety_floor.clone()),
                    vec![evidence("visit", v.id, v.service.clone(), v.occurred_at)],
                    v.occurred_at,
                );
            }
        }
        // Vitals only speak to acute safety while they are current.
        match &input.latest_vitals {
            Some(vit) if now - vit.recorded_at <= Duration::hours(ACUTE_VITALS_MAX_AGE_HOURS) => {
                let (priority, hits) = safety_floor(ArrivalKind::Scheduled, &[], &vit.vitals);
                if priority >= Priority::Urgent {
                    let rules = hits
                        .iter()
                        .map(|h| h.rule.clone())
                        .collect::<Vec<_>>()
                        .join(", ");
                    b.factor(
                        "abnormal_vitals",
                        priority_to_level(priority),
                        Some(rules),
                        vec![evidence(
                            "vital_signs",
                            vit.id,
                            "vital signs",
                            vit.recorded_at,
                        )],
                        vit.recorded_at,
                    );
                }
            }
            Some(vit) => b.stale(
                "vitals_stale",
                "vital_signs",
                Some(vit.id),
                vit.recorded_at,
                ACUTE_VITALS_MAX_AGE_HOURS / 24,
            ),
            None => b.missing("vitals_missing", "vital_signs"),
        }
    }
    b
}

fn chronic_complexity(input: &RiskInput) -> DomainBuilder {
    let now = input.now;
    let mut b = DomainBuilder::new(RiskDomain::ChronicComplexity);
    let active: Vec<&ConditionFact> = input
        .conditions
        .iter()
        .filter(|c| c.clinical_status == "active")
        .collect();
    let major: Vec<&ConditionFact> = active
        .iter()
        .copied()
        .filter(|c| is_major_chronic(&c.code))
        .collect();
    let unscheduled_recent = input
        .visits
        .iter()
        .filter(|v| {
            matches!(v.arrival_kind.as_str(), "walk_in" | "urgent")
                && days_between(now, v.occurred_at) <= UNSCHEDULED_WINDOW_DAYS
        })
        .count();
    if !major.is_empty() {
        let level = match major.len() {
            1 | 2 => RiskLevel::Moderate,
            3 => RiskLevel::High,
            _ if unscheduled_recent >= 1 => RiskLevel::Critical,
            _ => RiskLevel::High,
        };
        let detected = major.iter().map(|c| c.recorded_at).max().unwrap_or(now);
        b.factor(
            "multiple_chronic_conditions",
            level,
            Some(
                major
                    .iter()
                    .map(|c| c.display.clone())
                    .collect::<Vec<_>>()
                    .join("; "),
            ),
            major
                .iter()
                .map(|c| evidence("condition", c.id, c.display.clone(), c.recorded_at))
                .collect(),
            detected,
        );
    }
    let active_meds: Vec<&MedicationFact> = input
        .medications
        .iter()
        .filter(|m| m.status == "active")
        .collect();
    if active_meds.len() >= POLYPHARMACY_MODERATE {
        let level = if active_meds.len() >= POLYPHARMACY_HIGH {
            RiskLevel::High
        } else {
            RiskLevel::Moderate
        };
        let detected = active_meds
            .iter()
            .map(|m| m.recorded_at)
            .max()
            .unwrap_or(now);
        b.factor(
            "polypharmacy",
            level,
            Some(format!("{} active medications", active_meds.len())),
            active_meds
                .iter()
                .map(|m| evidence("medication", m.id, m.name.clone(), m.recorded_at))
                .collect(),
            detected,
        );
    }
    if active.is_empty() && input.encounters.is_empty() {
        b.missing("problem_list_missing", "condition");
    }
    b
}

fn medication_safety(input: &RiskInput) -> DomainBuilder {
    let now = input.now;
    let mut b = DomainBuilder::new(RiskDomain::MedicationAllergySafety);
    let active_meds: Vec<&MedicationFact> = input
        .medications
        .iter()
        .filter(|m| m.status == "active")
        .collect();
    for m in &active_meds {
        for a in &input.allergies {
            if medication_matches_allergy(&m.name, &a.substance) {
                let level = if a.criticality == "high" {
                    RiskLevel::Critical
                } else {
                    RiskLevel::High
                };
                b.factor(
                    "medication_allergy_conflict",
                    level,
                    Some(format!("{} / {}", m.name, a.substance)),
                    vec![
                        evidence("medication", m.id, m.name.clone(), m.recorded_at),
                        evidence("allergy", a.id, a.substance.clone(), a.recorded_at),
                    ],
                    m.recorded_at.max(a.recorded_at),
                );
            }
        }
    }
    let mut seen: Vec<String> = Vec::new();
    for m in &active_meds {
        let n = normalize(&m.name);
        if seen.contains(&n) {
            b.factor(
                "duplicate_medication",
                RiskLevel::Moderate,
                Some(m.name.clone()),
                vec![evidence("medication", m.id, m.name.clone(), m.recorded_at)],
                m.recorded_at,
            );
        } else {
            seen.push(n);
        }
    }
    if !active_meds.is_empty() && input.allergies.is_empty() {
        b.missing("allergy_status_unknown", "allergy");
        let detected = active_meds
            .iter()
            .map(|m| m.recorded_at)
            .max()
            .unwrap_or(now);
        b.factor(
            "allergy_status_unknown",
            RiskLevel::Moderate,
            None,
            active_meds
                .iter()
                .map(|m| evidence("medication", m.id, m.name.clone(), m.recorded_at))
                .collect(),
            detected,
        );
    }
    b
}

fn diagnostic_result(input: &RiskInput) -> DomainAssessment {
    let now = input.now;
    let mut b = DomainBuilder::new(RiskDomain::DiagnosticResult);
    for r in &input.results {
        let unreviewed = r.loop_state == "received";
        let open = r.loop_state != "closed";
        let label = match (&r.value, &r.unit) {
            (Some(v), Some(u)) => format!("{} {} {}", r.display, v, u),
            (Some(v), None) => format!("{} {}", r.display, v),
            _ => r.display.clone(),
        };
        let ev = vec![evidence(
            "observation",
            r.observation_id,
            label.clone(),
            r.effective_at,
        )];
        if r.critical && unreviewed {
            b.factor(
                "critical_result_unreviewed",
                RiskLevel::Critical,
                Some(label),
                ev,
                r.effective_at,
            );
        } else if r.critical && open {
            b.factor(
                "critical_result_open_loop",
                RiskLevel::High,
                Some(label),
                ev,
                r.effective_at,
            );
        } else if r.abnormal && unreviewed {
            b.factor(
                "abnormal_result_unreviewed",
                RiskLevel::High,
                Some(label),
                ev,
                r.effective_at,
            );
        } else if r.abnormal && open {
            b.factor(
                "abnormal_result_open_loop",
                RiskLevel::Moderate,
                Some(label),
                ev,
                r.effective_at,
            );
        }
    }
    let has_chronic = input
        .conditions
        .iter()
        .any(|c| c.clinical_status == "active" && is_major_chronic(&c.code));
    let newest = input.results.iter().map(|r| r.effective_at).max();
    match newest {
        None => {
            b.missing("laboratory_results_missing", "observation");
            let previous = input.previous.as_ref();
            let mut d = b.finish(RiskLevel::InsufficientData, now, previous);
            d.level = RiskLevel::InsufficientData;
            d.trend = RiskTrend::Unknown;
            d
        }
        Some(at) => {
            if has_chronic && days_between(now, at) > RESULT_STALE_DAYS {
                b.stale(
                    "laboratory_results_stale",
                    "observation",
                    None,
                    at,
                    RESULT_STALE_DAYS,
                );
            }
            b.finish(RiskLevel::Low, now, input.previous.as_ref())
        }
    }
}

fn latest_result<'a>(input: &'a RiskInput, loinc: &str) -> Option<&'a ResultFact> {
    input
        .results
        .iter()
        .filter(|r| r.code_loinc == loinc)
        .max_by_key(|r| r.effective_at)
}

/// (factor code, detail, evidence, detected_at, max age in days)
type PreventiveGap = (String, Option<String>, Vec<EvidenceRef>, DateTime<Utc>, i64);

fn preventive_care(input: &RiskInput) -> DomainBuilder {
    let now = input.now;
    let mut b = DomainBuilder::new(RiskDomain::PreventiveCare);
    let age = input.age_years();
    let active: Vec<&ConditionFact> = input
        .conditions
        .iter()
        .filter(|c| c.clinical_status == "active")
        .collect();
    let mut gaps: Vec<PreventiveGap> = Vec::new();

    let diabetes: Vec<&ConditionFact> = active
        .iter()
        .copied()
        .filter(|c| is_diabetes(&c.code))
        .collect();
    if !diabetes.is_empty() {
        let latest = latest_result(input, HBA1C_LOINC);
        let overdue = latest.is_none_or(|r| days_between(now, r.effective_at) > HBA1C_MAX_DAYS);
        if overdue {
            let mut ev: Vec<EvidenceRef> = diabetes
                .iter()
                .map(|c| evidence("condition", c.id, c.display.clone(), c.recorded_at))
                .collect();
            if let Some(r) = latest {
                ev.push(evidence(
                    "observation",
                    r.observation_id,
                    r.display.clone(),
                    r.effective_at,
                ));
                b.stale(
                    "hba1c_stale",
                    "observation",
                    Some(r.observation_id),
                    r.effective_at,
                    HBA1C_MAX_DAYS,
                );
            } else {
                b.missing("hba1c_missing", "observation");
            }
            gaps.push((
                "hba1c_overdue".into(),
                latest.map(|r| r.display.clone()),
                ev,
                latest.map(|r| r.effective_at).unwrap_or(now),
                HBA1C_MAX_DAYS,
            ));
        }
    }

    let renal: Vec<&ConditionFact> = active
        .iter()
        .copied()
        .filter(|c| is_renal_or_cardiac(&c.code))
        .collect();
    if !renal.is_empty() {
        let latest = latest_result(input, CREATININE_LOINC);
        let overdue =
            latest.is_none_or(|r| days_between(now, r.effective_at) > CREATININE_MAX_DAYS);
        if overdue {
            let mut ev: Vec<EvidenceRef> = renal
                .iter()
                .map(|c| evidence("condition", c.id, c.display.clone(), c.recorded_at))
                .collect();
            if let Some(r) = latest {
                ev.push(evidence(
                    "observation",
                    r.observation_id,
                    r.display.clone(),
                    r.effective_at,
                ));
                b.stale(
                    "creatinine_stale",
                    "observation",
                    Some(r.observation_id),
                    r.effective_at,
                    CREATININE_MAX_DAYS,
                );
            } else {
                b.missing("creatinine_missing", "observation");
            }
            gaps.push((
                "renal_function_overdue".into(),
                None,
                ev,
                latest.map(|r| r.effective_at).unwrap_or(now),
                CREATININE_MAX_DAYS,
            ));
        }
    }

    let hypertension = active.iter().any(|c| is_hypertension(&c.code));
    if hypertension || age >= BP_MIN_AGE {
        let bp = input
            .latest_vitals
            .as_ref()
            .filter(|v| v.vitals.systolic_mmhg.is_some());
        let overdue = bp.is_none_or(|v| days_between(now, v.recorded_at) > BP_MAX_DAYS);
        if overdue {
            let mut ev: Vec<EvidenceRef> = active
                .iter()
                .filter(|c| is_hypertension(&c.code))
                .map(|c| evidence("condition", c.id, c.display.clone(), c.recorded_at))
                .collect();
            if let Some(v) = bp {
                ev.push(evidence(
                    "vital_signs",
                    v.id,
                    "blood pressure",
                    v.recorded_at,
                ));
                b.stale(
                    "blood_pressure_stale",
                    "vital_signs",
                    Some(v.id),
                    v.recorded_at,
                    BP_MAX_DAYS,
                );
            } else {
                b.missing("blood_pressure_missing", "vital_signs");
            }
            gaps.push((
                "blood_pressure_overdue".into(),
                None,
                ev,
                bp.map(|v| v.recorded_at).unwrap_or(now),
                BP_MAX_DAYS,
            ));
        }
    }

    if age >= LIPID_MIN_AGE {
        let latest = latest_result(input, CHOLESTEROL_LOINC);
        let overdue = latest.is_none_or(|r| days_between(now, r.effective_at) > LIPID_MAX_DAYS);
        if overdue {
            let mut ev = Vec::new();
            if let Some(r) = latest {
                ev.push(evidence(
                    "observation",
                    r.observation_id,
                    r.display.clone(),
                    r.effective_at,
                ));
                b.stale(
                    "lipid_stale",
                    "observation",
                    Some(r.observation_id),
                    r.effective_at,
                    LIPID_MAX_DAYS,
                );
            } else {
                b.missing("lipid_missing", "observation");
            }
            gaps.push((
                "lipid_screening_overdue".into(),
                None,
                ev,
                latest.map(|r| r.effective_at).unwrap_or(now),
                LIPID_MAX_DAYS,
            ));
        }
    }

    let level = match gaps.len() {
        0 => RiskLevel::Low,
        1 => RiskLevel::Moderate,
        _ => RiskLevel::High,
    };
    for (code, detail, ev, at, _) in gaps {
        b.factor(&code, level, detail, ev, at);
    }
    b
}

fn access_utilization(input: &RiskInput) -> DomainBuilder {
    let now = input.now;
    let mut b = DomainBuilder::new(RiskDomain::AccessUtilization);
    let unscheduled: Vec<&VisitFact> = input
        .visits
        .iter()
        .filter(|v| {
            matches!(v.arrival_kind.as_str(), "walk_in" | "urgent")
                && days_between(now, v.occurred_at) <= UNSCHEDULED_WINDOW_DAYS
        })
        .collect();
    if unscheduled.len() >= 2 {
        let level = if unscheduled.len() >= 3 {
            RiskLevel::High
        } else {
            RiskLevel::Moderate
        };
        let detected = unscheduled
            .iter()
            .map(|v| v.occurred_at)
            .max()
            .unwrap_or(now);
        b.factor(
            "frequent_unscheduled_visits",
            level,
            Some(format!(
                "{} in {} days",
                unscheduled.len(),
                UNSCHEDULED_WINDOW_DAYS
            )),
            unscheduled
                .iter()
                .map(|v| evidence("visit", v.id, v.arrival_kind.clone(), v.occurred_at))
                .collect(),
            detected,
        );
    }
    let no_shows: Vec<&VisitFact> = input
        .visits
        .iter()
        .filter(|v| {
            v.status == "no_show" && days_between(now, v.occurred_at) <= NO_SHOW_WINDOW_DAYS
        })
        .collect();
    if no_shows.len() >= 2 {
        let detected = no_shows.iter().map(|v| v.occurred_at).max().unwrap_or(now);
        b.factor(
            "repeated_no_show",
            RiskLevel::Moderate,
            Some(format!(
                "{} in {} days",
                no_shows.len(),
                NO_SHOW_WINDOW_DAYS
            )),
            no_shows
                .iter()
                .map(|v| evidence("visit", v.id, v.status.clone(), v.occurred_at))
                .collect(),
            detected,
        );
    }
    let has_chronic = input
        .conditions
        .iter()
        .any(|c| c.clinical_status == "active" && is_major_chronic(&c.code));
    if has_chronic {
        let last_completed = input
            .encounters
            .iter()
            .filter(|e| e.status == "completed" && e.encounter_type == "consultation")
            .map(|e| e.completed_at.unwrap_or(e.started_at))
            .max();
        match last_completed {
            Some(at) if days_between(now, at) > FOLLOW_UP_MAX_AGE_DAYS => {
                let level = if days_between(now, at) > 2 * FOLLOW_UP_MAX_AGE_DAYS {
                    RiskLevel::High
                } else {
                    RiskLevel::Moderate
                };
                b.stale(
                    "follow_up_stale",
                    "encounter",
                    None,
                    at,
                    FOLLOW_UP_MAX_AGE_DAYS,
                );
                b.factor("no_recent_follow_up", level, None, Vec::new(), at);
            }
            Some(_) => {}
            None => {
                let open = input
                    .encounters
                    .iter()
                    .any(|e| e.status == "in_progress" && e.encounter_type == "consultation");
                if !open {
                    b.missing("consultation_missing", "encounter");
                    b.factor(
                        "no_recent_follow_up",
                        RiskLevel::Moderate,
                        None,
                        Vec::new(),
                        now,
                    );
                }
            }
        }
    }
    b
}

fn care_coordination(input: &RiskInput) -> DomainBuilder {
    let now = input.now;
    let mut b = DomainBuilder::new(RiskDomain::CareCoordination);
    let overdue: Vec<&TaskFact> = input
        .tasks
        .iter()
        .filter(|t| {
            t.status == "overdue" || (t.status == "open" && t.due_at.is_some_and(|d| d < now))
        })
        .collect();
    if !overdue.is_empty() {
        let oldest_days = overdue
            .iter()
            .filter_map(|t| t.due_at)
            .map(|d| days_between(now, d))
            .max()
            .unwrap_or(0);
        let level = if overdue.len() >= 3 || oldest_days > 30 {
            RiskLevel::High
        } else {
            RiskLevel::Moderate
        };
        let detected = overdue.iter().map(|t| t.created_at).max().unwrap_or(now);
        b.factor(
            "overdue_follow_up",
            level,
            Some(format!("{} overdue", overdue.len())),
            overdue
                .iter()
                .map(|t| evidence("follow_up_task", t.id, t.description.clone(), t.created_at))
                .collect(),
            detected,
        );
    }
    let pending: Vec<&ServiceRequestFact> = input
        .open_requests
        .iter()
        .filter(|r| {
            r.loop_state == "ordered" && days_between(now, r.created_at) > PENDING_RESULT_MAX_DAYS
        })
        .collect();
    if !pending.is_empty() {
        let detected = pending.iter().map(|r| r.created_at).max().unwrap_or(now);
        b.factor(
            "pending_result_overdue",
            RiskLevel::Moderate,
            Some(format!("{} awaiting result", pending.len())),
            pending
                .iter()
                .map(|r| evidence("service_request", r.id, r.display.clone(), r.created_at))
                .collect(),
            detected,
        );
    }
    let unhandled: Vec<&AlertFact> = input
        .alerts
        .iter()
        .filter(|a| {
            a.status == "open" && now - a.created_at > Duration::hours(ALERT_UNHANDLED_MAX_HOURS)
        })
        .collect();
    if !unhandled.is_empty() {
        let detected = unhandled.iter().map(|a| a.created_at).max().unwrap_or(now);
        b.factor(
            "unhandled_alert",
            RiskLevel::High,
            Some(format!(
                "{} open for more than {} h",
                unhandled.len(),
                ALERT_UNHANDLED_MAX_HOURS
            )),
            unhandled
                .iter()
                .map(|a| evidence("alert", a.id, a.message.clone(), a.created_at))
                .collect(),
            detected,
        );
    }
    let has_professional = input.care_team.iter().any(|c| c.has_professional);
    let needs_owner = !input.tasks.is_empty()
        || input
            .results
            .iter()
            .any(|r| r.loop_state != "closed" && (r.abnormal || r.critical))
        || input
            .conditions
            .iter()
            .filter(|c| c.clinical_status == "active" && is_major_chronic(&c.code))
            .count()
            >= 2;
    if needs_owner && !has_professional {
        b.missing("responsible_professional_missing", "care_team_assignment");
        b.factor(
            "no_responsible_professional",
            RiskLevel::Moderate,
            None,
            Vec::new(),
            now,
        );
    }
    b
}

// ---------------------------------------------------------------------------
// dMind risk summary contract (risk-summary.v1)
// ---------------------------------------------------------------------------

pub const RISK_SUMMARY_SCHEMA: &str = "risk-summary.v1";
pub const RISK_SUMMARY_PROMPT_VERSION: &str = "risk-summary-prompt.v1";

/// Professional follow-up categories a summary may suggest. Every suggestion
/// is a proposal for a professional; none is an order, prescription,
/// diagnosis or coverage decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FollowUpCategory {
    ReviewResult,
    MedicationReconciliation,
    ScheduleFollowUp,
    PreventiveScreening,
    CareTeamAssignment,
    CompleteRecord,
    DiscussWithPatient,
}

impl FollowUpCategory {
    pub fn as_str(self) -> &'static str {
        match self {
            FollowUpCategory::ReviewResult => "review_result",
            FollowUpCategory::MedicationReconciliation => "medication_reconciliation",
            FollowUpCategory::ScheduleFollowUp => "schedule_follow_up",
            FollowUpCategory::PreventiveScreening => "preventive_screening",
            FollowUpCategory::CareTeamAssignment => "care_team_assignment",
            FollowUpCategory::CompleteRecord => "complete_record",
            FollowUpCategory::DiscussWithPatient => "discuss_with_patient",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DomainExplanation {
    pub domain: RiskDomain,
    pub level: RiskLevel,
    /// Plain-language explanation of why this level was reached.
    pub summary: String,
    /// One reason per contributing factor, in the same order.
    pub reasons: Vec<String>,
    /// `record_type:record_id` references of the evidence used.
    pub cited_sources: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FollowUpSuggestion {
    pub category: FollowUpCategory,
    pub domain: RiskDomain,
    pub text: String,
    /// Always true: a suggestion becomes clinical work only after a
    /// professional confirms it.
    pub requires_confirmation: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RiskSummaryV1 {
    pub schema_version: String,
    /// Always true; rendered as an explicit "AI-generated" label.
    pub ai_generated: bool,
    pub rules_version: String,
    pub overall_level: RiskLevel,
    /// The deterministic floor the summary was aligned to.
    pub safety_floor: RiskLevel,
    /// Set when the provider proposed a lower level than the rules.
    pub raised_to_floor: bool,
    pub domains: Vec<DomainExplanation>,
    pub missing_information: Vec<String>,
    pub contradictions: Vec<String>,
    pub follow_up_suggestions: Vec<FollowUpSuggestion>,
    pub limitations: Vec<String>,
    pub cited_sources: Vec<String>,
    pub confidence: Confidence,
}

impl RiskSummaryV1 {
    /// Force every level back to the deterministic assessment. The provider's
    /// wording is kept; its levels are not trusted in either direction, and a
    /// lower proposal is recorded as `raised_to_floor`.
    pub fn align_to_deterministic(mut self, det: &RiskAssessment) -> Self {
        let mut raised = false;
        if self.overall_level < det.overall_level {
            raised = true;
        }
        self.overall_level = det.overall_level;
        self.safety_floor = det.safety_floor;
        for d in &mut self.domains {
            if let Some(x) = det.domain(d.domain) {
                if d.level < x.level {
                    raised = true;
                }
                d.level = x.level;
            }
        }
        self.raised_to_floor = raised;
        if raised {
            let note = "ai_level_raised_to_deterministic".to_string();
            if !self.limitations.contains(&note) {
                self.limitations.push(note);
            }
        }
        self
    }

    /// Structural validation against the deterministic assessment the
    /// summary explains. Anything that fails here is rejected as invalid
    /// provider output, never shown to a professional.
    pub fn validate(&self, det: &RiskAssessment) -> Result<(), String> {
        if self.schema_version != RISK_SUMMARY_SCHEMA {
            return Err(format!("unexpected schema {}", self.schema_version));
        }
        if !self.ai_generated {
            return Err("summary must be labelled ai_generated".into());
        }
        if self.rules_version != det.rules_version {
            return Err("rules_version mismatch".into());
        }
        if self.overall_level != det.overall_level || self.safety_floor != det.safety_floor {
            return Err("overall level differs from deterministic assessment".into());
        }
        if self.domains.len() != RiskDomain::ALL.len() {
            return Err("every risk domain must be explained exactly once".into());
        }
        for d in RiskDomain::ALL {
            let n = self.domains.iter().filter(|x| x.domain == d).count();
            if n != 1 {
                return Err(format!("domain {} explained {n} times", d.as_str()));
            }
        }
        for d in &self.domains {
            let Some(x) = det.domain(d.domain) else {
                return Err("unknown domain".into());
            };
            if d.level != x.level {
                return Err(format!("domain {} level differs", d.domain.as_str()));
            }
            if d.summary.trim().is_empty() {
                return Err(format!("domain {} has no explanation", d.domain.as_str()));
            }
            if !x.factors.is_empty() && d.cited_sources.is_empty() {
                return Err(format!("domain {} cites no sources", d.domain.as_str()));
            }
        }
        if self
            .follow_up_suggestions
            .iter()
            .any(|s| !s.requires_confirmation)
        {
            return Err("every follow-up suggestion requires confirmation".into());
        }
        if self
            .follow_up_suggestions
            .iter()
            .any(|s| s.text.trim().is_empty())
        {
            return Err("empty follow-up suggestion".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rust_decimal::Decimal;

    fn at(days_ago: i64) -> DateTime<Utc> {
        now() - Duration::days(days_ago)
    }

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn empty_input() -> RiskInput {
        RiskInput {
            now: now(),
            birth_date: NaiveDate::from_ymd_opt(1990, 5, 1).unwrap(),
            alerts: vec![],
            current_visit: None,
            visits: vec![],
            latest_vitals: None,
            conditions: vec![],
            medications: vec![],
            allergies: vec![],
            results: vec![],
            open_requests: vec![],
            encounters: vec![],
            tasks: vec![],
            care_team: vec![],
            previous: None,
        }
    }

    fn encounter(days_ago: i64) -> EncounterFact {
        EncounterFact {
            id: Uuid::now_v7(),
            status: "completed".into(),
            encounter_type: "consultation".into(),
            started_at: at(days_ago),
            completed_at: Some(at(days_ago)),
        }
    }

    fn result(
        code: &str,
        display: &str,
        critical: bool,
        abnormal: bool,
        state: &str,
        days_ago: i64,
    ) -> ResultFact {
        ResultFact {
            observation_id: Uuid::now_v7(),
            service_request_id: Uuid::now_v7(),
            code_loinc: code.into(),
            display: display.into(),
            value: Some("1".into()),
            unit: Some("u".into()),
            critical,
            abnormal,
            loop_state: state.into(),
            effective_at: at(days_ago),
        }
    }

    fn stable_input() -> RiskInput {
        let mut i = empty_input();
        i.encounters = vec![encounter(30)];
        i.results = vec![result("2345-7", "Glucose", false, false, "closed", 30)];
        i.care_team = vec![CareTeamFact {
            id: Uuid::now_v7(),
            function: "attending".into(),
            has_professional: true,
        }];
        i
    }

    #[test]
    fn empty_record_is_insufficient_everywhere() {
        let a = assess(&empty_input());
        assert_eq!(a.overall_level, RiskLevel::InsufficientData);
        assert_eq!(a.domains.len(), 7);
        assert!(a
            .domains
            .iter()
            .all(|d| d.level == RiskLevel::InsufficientData));
        assert!(a.domains.iter().all(|d| d.trend == RiskTrend::Unknown));
        assert!(a.domains.iter().all(|d| d
            .missing_data
            .iter()
            .any(|g| g.code == "no_clinical_record")));
        assert_eq!(a.rules_version, RISK_RULES_VERSION);
    }

    #[test]
    fn stable_patient_is_low_with_rules_version_on_every_domain() {
        let a = assess(&stable_input());
        assert_eq!(a.overall_level, RiskLevel::Low);
        for d in &a.domains {
            assert_eq!(d.rules_version, RISK_RULES_VERSION);
            assert_eq!(d.calculated_at, now());
            assert!(d.level <= RiskLevel::Low, "{:?}", d);
        }
        assert_eq!(a.trend, RiskTrend::Unknown);
    }

    #[test]
    fn assessment_is_deterministic_and_carries_no_numeric_score() {
        let i = stable_input();
        let a = assess(&i);
        let b = assess(&i);
        assert_eq!(a, b);
        let json = serde_json::to_string(&a).unwrap();
        assert!(!json.contains("\"score\""));
        assert!(!json.contains("\"points\""));
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert!(v["overall_level"].is_string());
        for d in v["domains"].as_array().unwrap() {
            assert!(d["level"].is_string());
        }
    }

    #[test]
    fn critical_unreviewed_result_drives_critical_with_evidence() {
        let mut i = stable_input();
        let r = result("2823-3", "Potassium", true, true, "received", 0);
        let obs = r.observation_id;
        i.results.push(r);
        let a = assess(&i);
        assert_eq!(a.overall_level, RiskLevel::Critical);
        assert_eq!(a.safety_floor, RiskLevel::Critical);
        let d = a.domain(RiskDomain::DiagnosticResult).unwrap();
        assert_eq!(d.level, RiskLevel::Critical);
        let f = d
            .factors
            .iter()
            .find(|f| f.code == "critical_result_unreviewed")
            .unwrap();
        assert_eq!(f.evidence[0].record_type, "observation");
        assert_eq!(f.evidence[0].record_id, obs);
        assert_eq!(d.detected_at, Some(f.detected_at));
    }

    #[test]
    fn reviewed_critical_result_drops_to_high_and_closed_to_low() {
        let mut i = stable_input();
        i.results
            .push(result("2823-3", "Potassium", true, true, "reviewed", 0));
        assert_eq!(
            assess(&i)
                .domain(RiskDomain::DiagnosticResult)
                .unwrap()
                .level,
            RiskLevel::High
        );
        i.results.last_mut().unwrap().loop_state = "closed".into();
        assert_eq!(
            assess(&i)
                .domain(RiskDomain::DiagnosticResult)
                .unwrap()
                .level,
            RiskLevel::Low
        );
    }

    #[test]
    fn medication_allergy_conflict_is_critical_for_high_criticality() {
        let mut i = stable_input();
        i.allergies.push(AllergyFact {
            id: Uuid::now_v7(),
            substance: "Penicillin".into(),
            criticality: "high".into(),
            recorded_at: at(400),
        });
        i.medications.push(MedicationFact {
            id: Uuid::now_v7(),
            name: "Amoxicillin 500 mg".into(),
            status: "active".into(),
            recorded_at: at(1),
        });
        let a = assess(&i);
        let d = a.domain(RiskDomain::MedicationAllergySafety).unwrap();
        assert_eq!(d.level, RiskLevel::Critical);
        let f = &d.factors[0];
        assert_eq!(f.code, "medication_allergy_conflict");
        assert_eq!(f.evidence.len(), 2);
        assert!(f.evidence.iter().any(|e| e.record_type == "allergy"));
        i.allergies[0].criticality = "low".into();
        assert_eq!(
            assess(&i)
                .domain(RiskDomain::MedicationAllergySafety)
                .unwrap()
                .level,
            RiskLevel::High
        );
    }

    #[test]
    fn inactive_medication_does_not_conflict() {
        let mut i = stable_input();
        i.allergies.push(AllergyFact {
            id: Uuid::now_v7(),
            substance: "penicillin".into(),
            criticality: "high".into(),
            recorded_at: at(400),
        });
        i.medications.push(MedicationFact {
            id: Uuid::now_v7(),
            name: "amoxicillin".into(),
            status: "stopped".into(),
            recorded_at: at(1),
        });
        assert_eq!(
            assess(&i)
                .domain(RiskDomain::MedicationAllergySafety)
                .unwrap()
                .level,
            RiskLevel::Low
        );
    }

    #[test]
    fn medications_without_allergy_record_flag_missing_data() {
        let mut i = stable_input();
        i.medications.push(MedicationFact {
            id: Uuid::now_v7(),
            name: "metformin".into(),
            status: "active".into(),
            recorded_at: at(1),
        });
        let d = assess(&i);
        let d = d.domain(RiskDomain::MedicationAllergySafety).unwrap();
        assert_eq!(d.level, RiskLevel::Moderate);
        assert!(d
            .missing_data
            .iter()
            .any(|g| g.code == "allergy_status_unknown"));
    }

    fn condition(code: &str, display: &str) -> ConditionFact {
        ConditionFact {
            id: Uuid::now_v7(),
            code: code.into(),
            display: display.into(),
            clinical_status: "active".into(),
            recorded_at: at(500),
        }
    }

    #[test]
    fn chronic_complexity_scales_with_major_conditions() {
        let mut i = stable_input();
        i.conditions.push(condition("E11", "Type 2 diabetes"));
        assert_eq!(
            assess(&i)
                .domain(RiskDomain::ChronicComplexity)
                .unwrap()
                .level,
            RiskLevel::Moderate
        );
        i.conditions.push(condition("N18.3", "CKD stage 3"));
        i.conditions.push(condition("I50.9", "Heart failure"));
        assert_eq!(
            assess(&i)
                .domain(RiskDomain::ChronicComplexity)
                .unwrap()
                .level,
            RiskLevel::High
        );
        i.conditions.push(condition("I10", "Hypertension"));
        i.visits.push(VisitFact {
            id: Uuid::now_v7(),
            status: "completed".into(),
            arrival_kind: "urgent".into(),
            service: "emergency".into(),
            priority: None,
            safety_floor: None,
            occurred_at: at(10),
        });
        assert_eq!(
            assess(&i)
                .domain(RiskDomain::ChronicComplexity)
                .unwrap()
                .level,
            RiskLevel::Critical
        );
        // Resolved conditions do not count.
        for c in &mut i.conditions {
            c.clinical_status = "resolved".into();
        }
        assert_eq!(
            assess(&i)
                .domain(RiskDomain::ChronicComplexity)
                .unwrap()
                .level,
            RiskLevel::Low
        );
    }

    #[test]
    fn preventive_gaps_count_and_record_stale_or_missing() {
        let mut i = stable_input();
        i.birth_date = NaiveDate::from_ymd_opt(1960, 1, 1).unwrap();
        i.conditions.push(condition("E11", "Type 2 diabetes"));
        // Old HbA1c: stale; no BP ever: missing; no lipid: missing.
        i.results
            .push(result(HBA1C_LOINC, "HbA1c", false, false, "closed", 400));
        let a = assess(&i);
        let d = a.domain(RiskDomain::PreventiveCare).unwrap();
        assert_eq!(d.level, RiskLevel::High);
        let codes: Vec<&str> = d.factors.iter().map(|f| f.code.as_str()).collect();
        assert!(codes.contains(&"hba1c_overdue"));
        assert!(codes.contains(&"blood_pressure_overdue"));
        assert!(codes.contains(&"lipid_screening_overdue"));
        assert!(codes.contains(&"renal_function_overdue"));
        assert!(d
            .stale_data
            .iter()
            .any(|g| g.code == "hba1c_stale" && g.max_age_days == Some(HBA1C_MAX_DAYS)));
        assert!(d
            .missing_data
            .iter()
            .any(|g| g.code == "blood_pressure_missing"));
        // A recent HbA1c, BP, lipid and creatinine close the gaps.
        i.results
            .push(result(HBA1C_LOINC, "HbA1c", false, false, "closed", 30));
        i.results.push(result(
            CHOLESTEROL_LOINC,
            "Cholesterol",
            false,
            false,
            "closed",
            30,
        ));
        i.results.push(result(
            CREATININE_LOINC,
            "Creatinine",
            false,
            false,
            "closed",
            30,
        ));
        i.latest_vitals = Some(VitalsFact {
            id: Uuid::now_v7(),
            recorded_at: at(30),
            vitals: TriageVitals {
                systolic_mmhg: Some(Decimal::from(120)),
                heart_rate_bpm: None,
                respiratory_rate_bpm: None,
                temperature_c: None,
                spo2_percent: None,
            },
        });
        let d = assess(&i);
        assert_eq!(
            d.domain(RiskDomain::PreventiveCare).unwrap().level,
            RiskLevel::Low
        );
    }

    #[test]
    fn acute_safety_uses_current_vitals_only() {
        let mut i = stable_input();
        i.current_visit = Some(VisitFact {
            id: Uuid::now_v7(),
            status: "arrived".into(),
            arrival_kind: "walk_in".into(),
            service: "general_medicine".into(),
            priority: None,
            safety_floor: None,
            occurred_at: now(),
        });
        let low_spo2 = TriageVitals {
            systolic_mmhg: None,
            heart_rate_bpm: None,
            respiratory_rate_bpm: None,
            temperature_c: None,
            spo2_percent: Some(Decimal::from(88)),
        };
        i.latest_vitals = Some(VitalsFact {
            id: Uuid::now_v7(),
            recorded_at: now() - Duration::hours(1),
            vitals: low_spo2.clone(),
        });
        let d = assess(&i);
        let d = d.domain(RiskDomain::AcuteSafety).unwrap();
        assert_eq!(d.level, RiskLevel::Critical);
        assert_eq!(d.factors[0].code, "abnormal_vitals");
        // The same vitals two days old are stale: reported, not applied.
        i.latest_vitals = Some(VitalsFact {
            id: Uuid::now_v7(),
            recorded_at: at(2),
            vitals: low_spo2,
        });
        let d = assess(&i);
        let d = d.domain(RiskDomain::AcuteSafety).unwrap();
        assert_eq!(d.level, RiskLevel::Low);
        assert_eq!(d.stale_data[0].code, "vitals_stale");
        // A visit with an immediate priority is critical regardless.
        i.current_visit.as_mut().unwrap().priority = Some("immediate".into());
        assert_eq!(
            assess(&i).domain(RiskDomain::AcuteSafety).unwrap().level,
            RiskLevel::Critical
        );
    }

    #[test]
    fn open_critical_alert_is_critical_and_unhandled_after_a_day() {
        let mut i = stable_input();
        i.alerts.push(AlertFact {
            id: Uuid::now_v7(),
            severity: "critical".into(),
            status: "open".into(),
            message: "Critical potassium".into(),
            created_at: at(2),
        });
        let a = assess(&i);
        assert_eq!(
            a.domain(RiskDomain::AcuteSafety).unwrap().level,
            RiskLevel::Critical
        );
        let c = a.domain(RiskDomain::CareCoordination).unwrap();
        assert!(c.factors.iter().any(|f| f.code == "unhandled_alert"));
        i.alerts[0].status = "acknowledged".into();
        assert_eq!(
            assess(&i).domain(RiskDomain::AcuteSafety).unwrap().level,
            RiskLevel::Low
        );
    }

    #[test]
    fn access_and_coordination_rules() {
        let mut i = stable_input();
        for d in [5, 20, 40] {
            i.visits.push(VisitFact {
                id: Uuid::now_v7(),
                status: "completed".into(),
                arrival_kind: "urgent".into(),
                service: "emergency".into(),
                priority: None,
                safety_floor: None,
                occurred_at: at(d),
            });
        }
        i.tasks.push(TaskFact {
            id: Uuid::now_v7(),
            description: "Repeat potassium".into(),
            status: "overdue".into(),
            priority: "urgent".into(),
            due_at: Some(at(45)),
            created_at: at(60),
        });
        i.care_team.clear();
        let a = assess(&i);
        assert_eq!(
            a.domain(RiskDomain::AccessUtilization).unwrap().level,
            RiskLevel::High
        );
        let c = a.domain(RiskDomain::CareCoordination).unwrap();
        assert_eq!(c.level, RiskLevel::High);
        assert!(c.factors.iter().any(|f| f.code == "overdue_follow_up"));
        assert!(c
            .factors
            .iter()
            .any(|f| f.code == "no_responsible_professional"));
        assert!(c
            .missing_data
            .iter()
            .any(|g| g.code == "responsible_professional_missing"));
    }

    #[test]
    fn chronic_patient_without_recent_consultation_is_stale() {
        let mut i = stable_input();
        i.conditions.push(condition("I50.9", "Heart failure"));
        i.encounters = vec![encounter(800)];
        let a = assess(&i);
        let d = a.domain(RiskDomain::AccessUtilization).unwrap();
        assert_eq!(d.level, RiskLevel::High);
        assert_eq!(d.stale_data[0].code, "follow_up_stale");
        let dx = a.domain(RiskDomain::DiagnosticResult).unwrap();
        // The only result is 30 days old; not stale.
        assert!(dx.stale_data.is_empty());
    }

    #[test]
    fn trend_compares_with_previous_assessment() {
        let mut i = stable_input();
        i.previous = Some(PreviousAssessment {
            calculated_at: at(30),
            levels: RiskDomain::ALL
                .iter()
                .map(|d| (*d, RiskLevel::Low))
                .collect(),
        });
        let a = assess(&i);
        assert_eq!(a.trend, RiskTrend::Stable);
        assert!(a.domains.iter().all(|d| d.trend == RiskTrend::Stable));
        i.conditions.push(condition("E11", "Type 2 diabetes"));
        i.conditions.push(condition("N18.3", "CKD"));
        i.conditions.push(condition("I50.9", "Heart failure"));
        let a = assess(&i);
        assert_eq!(
            a.domain(RiskDomain::ChronicComplexity).unwrap().trend,
            RiskTrend::Worsening
        );
        assert_eq!(a.trend, RiskTrend::Worsening);
        i.previous.as_mut().unwrap().levels = RiskDomain::ALL
            .iter()
            .map(|d| (*d, RiskLevel::Critical))
            .collect();
        let a = assess(&i);
        assert_eq!(a.trend, RiskTrend::Improving);
        // Insufficient data never yields a trend.
        i.previous.as_mut().unwrap().levels =
            vec![(RiskDomain::AcuteSafety, RiskLevel::InsufficientData)];
        assert_eq!(
            assess(&i).domain(RiskDomain::AcuteSafety).unwrap().trend,
            RiskTrend::Unknown
        );
    }

    #[test]
    fn diagnostic_domain_is_insufficient_without_any_result() {
        let mut i = stable_input();
        i.results.clear();
        let a = assess(&i);
        let d = a.domain(RiskDomain::DiagnosticResult).unwrap();
        assert_eq!(d.level, RiskLevel::InsufficientData);
        assert_eq!(d.missing_data[0].code, "laboratory_results_missing");
        // Insufficient domains do not lower nor raise the overall level.
        assert_eq!(a.overall_level, RiskLevel::Low);
    }

    fn summary_for(det: &RiskAssessment) -> RiskSummaryV1 {
        RiskSummaryV1 {
            schema_version: RISK_SUMMARY_SCHEMA.into(),
            ai_generated: true,
            rules_version: det.rules_version.clone(),
            overall_level: det.overall_level,
            safety_floor: det.safety_floor,
            raised_to_floor: false,
            domains: det
                .domains
                .iter()
                .map(|d| DomainExplanation {
                    domain: d.domain,
                    level: d.level,
                    summary: "explanation".into(),
                    reasons: d.factors.iter().map(|f| f.code.clone()).collect(),
                    cited_sources: d
                        .factors
                        .iter()
                        .flat_map(|f| {
                            f.evidence
                                .iter()
                                .map(|e| format!("{}:{}", e.record_type, e.record_id))
                        })
                        .collect(),
                })
                .collect(),
            missing_information: vec![],
            contradictions: vec![],
            follow_up_suggestions: vec![FollowUpSuggestion {
                category: FollowUpCategory::ReviewResult,
                domain: RiskDomain::DiagnosticResult,
                text: "Review the unreviewed result".into(),
                requires_confirmation: true,
            }],
            limitations: vec![],
            cited_sources: vec![],
            confidence: Confidence::Medium,
        }
    }

    fn critical_assessment() -> RiskAssessment {
        let mut i = stable_input();
        i.results
            .push(result("2823-3", "Potassium", true, true, "received", 0));
        assess(&i)
    }

    #[test]
    fn summary_cannot_lower_deterministic_critical_level() {
        let det = critical_assessment();
        let mut s = summary_for(&det);
        s.overall_level = RiskLevel::Low;
        for d in &mut s.domains {
            d.level = RiskLevel::Low;
        }
        assert!(s.validate(&det).is_err());
        let aligned = s.align_to_deterministic(&det);
        assert_eq!(aligned.overall_level, RiskLevel::Critical);
        assert_eq!(aligned.safety_floor, RiskLevel::Critical);
        assert!(aligned.raised_to_floor);
        assert!(aligned
            .limitations
            .contains(&"ai_level_raised_to_deterministic".to_string()));
        assert_eq!(
            aligned
                .domains
                .iter()
                .find(|d| d.domain == RiskDomain::DiagnosticResult)
                .unwrap()
                .level,
            RiskLevel::Critical
        );
        aligned.validate(&det).unwrap();
    }

    #[test]
    fn summary_validation_rejects_structural_defects() {
        let det = critical_assessment();
        let ok = summary_for(&det);
        ok.validate(&det).unwrap();

        let mut s = ok.clone();
        s.schema_version = "risk-summary.v0".into();
        assert!(s.validate(&det).is_err());

        let mut s = ok.clone();
        s.ai_generated = false;
        assert!(s.validate(&det).is_err());

        let mut s = ok.clone();
        s.domains.pop();
        assert!(s.validate(&det).is_err());

        let mut s = ok.clone();
        s.domains[0].domain = s.domains[1].domain;
        assert!(s.validate(&det).is_err());

        let mut s = ok.clone();
        let idx = s
            .domains
            .iter()
            .position(|d| d.domain == RiskDomain::DiagnosticResult)
            .unwrap();
        s.domains[idx].cited_sources.clear();
        assert!(s.validate(&det).is_err(), "factors without citations");

        let mut s = ok.clone();
        s.domains[0].summary = "  ".into();
        assert!(s.validate(&det).is_err());

        let mut s = ok.clone();
        s.follow_up_suggestions[0].requires_confirmation = false;
        assert!(s.validate(&det).is_err());

        let mut s = ok.clone();
        s.rules_version = "risk-rules.v0".into();
        assert!(s.validate(&det).is_err());
    }

    #[test]
    fn allergy_matching_is_conservative_and_family_aware() {
        assert!(medication_matches_allergy(
            "Amoxicillin 500 mg",
            "Penicillin"
        ));
        assert!(medication_matches_allergy("Ibuprofeno 400 mg", "NSAID"));
        assert!(medication_matches_allergy("Aspirin 100 mg", "aspirin"));
        assert!(!medication_matches_allergy(
            "Metformin 850 mg",
            "Penicillin"
        ));
        assert!(!medication_matches_allergy("Lisinopril", "latex"));
        assert!(!medication_matches_allergy("", "Penicillin"));
    }

    #[test]
    fn level_ordering_and_parsing_round_trip() {
        assert!(RiskLevel::Critical > RiskLevel::High);
        assert!(RiskLevel::High > RiskLevel::Moderate);
        assert!(RiskLevel::Moderate > RiskLevel::Low);
        assert!(RiskLevel::Low > RiskLevel::InsufficientData);
        for l in [
            RiskLevel::InsufficientData,
            RiskLevel::Low,
            RiskLevel::Moderate,
            RiskLevel::High,
            RiskLevel::Critical,
        ] {
            assert_eq!(RiskLevel::parse(l.as_str()), Some(l));
        }
        for d in RiskDomain::ALL {
            assert_eq!(RiskDomain::parse(d.as_str()), Some(d));
        }
        assert_eq!(RiskLevel::Critical.priority_rank(), 0);
        assert_eq!(RiskLevel::InsufficientData.priority_rank(), 4);
    }
}
