//! Versioned output contracts of the dMind Access agents.
//!
//! Every contract is *assistive*: the deterministic engines
//! ([`crate::matcher`], [`crate::recovery`], [`crate::capacity`]) remain
//! authoritative, and each `validate` here rejects any output that invents,
//! drops or reorders beyond what the supplied facts allow. The same
//! validators run for every provider, so the fixture provider is held to the
//! exact rules a real model is.

use std::collections::BTreeSet;

use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::access::{is_valid_code, WeeklyWindow};
use crate::ai::Confidence;

pub const ACCESS_INTENT_SCHEMA: &str = "access-intent.v1";
pub const APPOINTMENT_RANKING_SCHEMA: &str = "appointment-ranking.v1";
pub const CANCELLATION_RECOVERY_SCHEMA: &str = "cancellation-recovery.v1";
pub const CAPACITY_EXPLANATION_SCHEMA: &str = "capacity-explanation.v1";

const MAX_LIST: usize = 20;
const MAX_TEXT: usize = 600;

fn check_text(s: &str, name: &str) -> Result<(), String> {
    if s.trim().is_empty() {
        return Err(format!("{name} is empty"));
    }
    if s.chars().count() > MAX_TEXT {
        return Err(format!("{name} is oversized"));
    }
    Ok(())
}

fn check_list(items: &[String], name: &str) -> Result<(), String> {
    if items.len() > MAX_LIST {
        return Err(format!("{name} has too many items"));
    }
    for i in items {
        check_text(i, name)?;
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

// ---------------------------------------------------------------------------
// access-intent.v1
// ---------------------------------------------------------------------------

/// Catalog vocabulary the intent operation may choose from. Only codes in
/// these lists are accepted back; nothing else can be "understood".
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct IntentVocabulary {
    pub services: Vec<String>,
    pub specialties: Vec<String>,
    pub modalities: Vec<String>,
    pub accessibility: Vec<String>,
    /// Facility codes (never internal ids) the patient may name.
    pub facilities: Vec<String>,
}

/// Structured scheduling constraints extracted from free text. Never a
/// clinical judgement: urgency is not part of the output, only a flag that
/// clinical triage should look at the request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AccessIntentV1 {
    pub schema_version: String,
    pub service_code: Option<String>,
    pub specialty_code: Option<String>,
    pub modality_codes: Vec<String>,
    pub facility_codes: Vec<String>,
    pub accessibility_codes: Vec<String>,
    pub preferred_windows: Vec<WeeklyWindow>,
    pub earliest_date: Option<NaiveDate>,
    pub latest_date: Option<NaiveDate>,
    pub language: Option<String>,
    pub continuity_requested: bool,
    pub transport_requested: bool,
    /// Questions the patient still has to answer before matching.
    pub missing_information: Vec<String>,
    /// True when the request should be seen by clinical triage before any
    /// appointment is offered. The deterministic keyword floor can only
    /// raise this flag; the model can never clear it.
    pub clinical_triage_suggested: bool,
    pub triage_reasons: Vec<String>,
    pub cited_sources: Vec<String>,
    pub confidence: Confidence,
    pub limitations: Vec<String>,
}

impl AccessIntentV1 {
    /// References an intent may cite: the request text itself and every
    /// vocabulary entry.
    pub fn citable(vocab: &IntentVocabulary) -> Vec<String> {
        let mut out = vec!["request:text".to_string()];
        for (kind, codes) in [
            ("service", &vocab.services),
            ("specialty", &vocab.specialties),
            ("modality", &vocab.modalities),
            ("accessibility", &vocab.accessibility),
            ("facility", &vocab.facilities),
        ] {
            for c in codes {
                out.push(format!("catalog:{kind}:{c}"));
            }
        }
        out
    }

    /// Applies the deterministic triage floor and validates against the
    /// supplied vocabulary.
    pub fn finalize(
        mut self,
        vocab: &IntentVocabulary,
        floor_reason: Option<&str>,
    ) -> Result<Self, String> {
        self.schema_version = ACCESS_INTENT_SCHEMA.into();
        if let Some(reason) = floor_reason {
            self.clinical_triage_suggested = true;
            if !self.triage_reasons.iter().any(|r| r == reason) {
                self.triage_reasons.insert(0, reason.to_string());
            }
        }
        self.validate(vocab)?;
        Ok(self)
    }

    pub fn validate(&self, vocab: &IntentVocabulary) -> Result<(), String> {
        if self.schema_version != ACCESS_INTENT_SCHEMA {
            return Err("unexpected schema_version".into());
        }
        if let Some(s) = &self.service_code {
            if !vocab.services.contains(s) {
                return Err("service_code is not in the supplied catalog".into());
            }
        }
        if let Some(s) = &self.specialty_code {
            if !vocab.specialties.contains(s) {
                return Err("specialty_code is not in the supplied catalog".into());
            }
        }
        for (codes, allowed, name) in [
            (&self.modality_codes, &vocab.modalities, "modality_codes"),
            (
                &self.accessibility_codes,
                &vocab.accessibility,
                "accessibility_codes",
            ),
            (&self.facility_codes, &vocab.facilities, "facility_codes"),
        ] {
            if codes.len() > MAX_LIST {
                return Err(format!("{name} has too many items"));
            }
            for c in codes {
                if !allowed.contains(c) {
                    return Err(format!(
                        "{name} contains a code not in the supplied catalog"
                    ));
                }
            }
        }
        if self.preferred_windows.len() > MAX_LIST {
            return Err("preferred_windows has too many items".into());
        }
        for w in &self.preferred_windows {
            w.validate()?;
        }
        if let (Some(a), Some(b)) = (self.earliest_date, self.latest_date) {
            if b < a {
                return Err("latest_date precedes earliest_date".into());
            }
        }
        if let Some(l) = &self.language {
            if l.is_empty()
                || l.len() > 16
                || !l.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
            {
                return Err("language must be a BCP-47 tag".into());
            }
        }
        check_list(&self.missing_information, "missing_information")?;
        check_list(&self.triage_reasons, "triage_reasons")?;
        check_list(&self.limitations, "limitations")?;
        if self.clinical_triage_suggested && self.triage_reasons.is_empty() {
            return Err("clinical_triage_suggested requires a reason".into());
        }
        let citable = Self::citable(vocab);
        let allowed: BTreeSet<&str> = citable.iter().map(String::as_str).collect();
        check_subset(&self.cited_sources, &allowed, "access intent")?;
        if self.cited_sources.is_empty() {
            return Err("access intent cites no source".into());
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// appointment-ranking.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RankedCandidate {
    pub candidate_id: String,
    /// 1 = best.
    pub rank: u32,
    pub explanation: String,
    pub cited_sources: Vec<String>,
}

/// A permutation of the deterministic candidates with explanations. It
/// can neither add, drop nor alter a candidate.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AppointmentRankingV1 {
    pub schema_version: String,
    pub ranked: Vec<RankedCandidate>,
    pub overall_note: Option<String>,
    pub cited_sources: Vec<String>,
    pub confidence: Confidence,
    pub limitations: Vec<String>,
}

impl AppointmentRankingV1 {
    /// Ordered candidate ids in the proposed ranking.
    pub fn order(&self) -> Vec<String> {
        let mut r = self.ranked.clone();
        r.sort_by_key(|c| c.rank);
        r.into_iter().map(|c| c.candidate_id).collect()
    }

    /// `candidate_ids` are the deterministic candidates; `fact_refs` the
    /// additional facts supplied to the model.
    pub fn validate(&self, candidate_ids: &[String], fact_refs: &[String]) -> Result<(), String> {
        if self.schema_version != APPOINTMENT_RANKING_SCHEMA {
            return Err("unexpected schema_version".into());
        }
        if self.ranked.len() != candidate_ids.len() {
            return Err("ranking must contain every supplied candidate exactly once".into());
        }
        let mut seen_ids = BTreeSet::new();
        let mut seen_ranks = BTreeSet::new();
        let allowed: BTreeSet<&str> = candidate_ids
            .iter()
            .map(String::as_str)
            .chain(fact_refs.iter().map(String::as_str))
            .collect();
        for c in &self.ranked {
            if !candidate_ids.contains(&c.candidate_id) {
                return Err("ranking references a candidate that was not supplied".into());
            }
            if !seen_ids.insert(c.candidate_id.as_str()) {
                return Err("ranking repeats a candidate".into());
            }
            if c.rank == 0 || c.rank as usize > candidate_ids.len() || !seen_ranks.insert(c.rank) {
                return Err("ranks must be a permutation of 1..=n".into());
            }
            check_text(&c.explanation, "explanation")?;
            check_subset(&c.cited_sources, &allowed, "ranking explanation")?;
            if !c.cited_sources.iter().any(|s| s == &c.candidate_id) {
                return Err("each explanation must cite its own candidate id".into());
            }
        }
        if let Some(n) = &self.overall_note {
            check_text(n, "overall_note")?;
        }
        check_subset(&self.cited_sources, &allowed, "ranking")?;
        check_list(&self.limitations, "limitations")?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// cancellation-recovery.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RecoveryExplanation {
    pub entry_id: Uuid,
    pub explanation: String,
    pub cited_sources: Vec<String>,
}

/// Proposed order in which eligible waitlist entries are offered a freed
/// slot. The caller runs [`crate::recovery::enforce_floors`] on
/// `ordered_entry_ids` before accepting it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CancellationRecoveryV1 {
    pub schema_version: String,
    pub ordered_entry_ids: Vec<Uuid>,
    pub explanations: Vec<RecoveryExplanation>,
    pub cited_sources: Vec<String>,
    pub confidence: Confidence,
    pub limitations: Vec<String>,
}

impl CancellationRecoveryV1 {
    pub fn validate(&self, entry_ids: &[Uuid], fact_refs: &[String]) -> Result<(), String> {
        if self.schema_version != CANCELLATION_RECOVERY_SCHEMA {
            return Err("unexpected schema_version".into());
        }
        if self.ordered_entry_ids.len() != entry_ids.len() {
            return Err("ordering must contain every eligible entry exactly once".into());
        }
        let mut seen = BTreeSet::new();
        for id in &self.ordered_entry_ids {
            if !entry_ids.contains(id) {
                return Err("ordering references an entry that was not supplied".into());
            }
            if !seen.insert(*id) {
                return Err("ordering repeats an entry".into());
            }
        }
        let entry_refs: Vec<String> = entry_ids.iter().map(|id| format!("entry:{id}")).collect();
        let allowed: BTreeSet<&str> = entry_refs
            .iter()
            .map(String::as_str)
            .chain(fact_refs.iter().map(String::as_str))
            .collect();
        if self.explanations.len() > entry_ids.len() {
            return Err("more explanations than entries".into());
        }
        for e in &self.explanations {
            if !entry_ids.contains(&e.entry_id) {
                return Err("explanation references an entry that was not supplied".into());
            }
            check_text(&e.explanation, "explanation")?;
            check_subset(&e.cited_sources, &allowed, "recovery explanation")?;
        }
        check_subset(&self.cited_sources, &allowed, "recovery")?;
        check_list(&self.limitations, "limitations")?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// capacity-explanation.v1
// ---------------------------------------------------------------------------

pub const CAPACITY_RECOMMENDATION_CATEGORIES: &[&str] = &[
    "open_capacity",
    "adjust_reminders",
    "review_staffing",
    "monitor",
    "collect_more_history",
];

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PressurePoint {
    pub date: NaiveDate,
    pub explanation: String,
    pub cited_sources: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CapacityRecommendation {
    pub category: String,
    pub text: String,
    /// Always true: a forecast explanation never acts on schedules.
    pub requires_confirmation: bool,
}

/// Plain-language explanation of a deterministic capacity forecast.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CapacityExplanationV1 {
    pub schema_version: String,
    pub summary: String,
    pub pressure_points: Vec<PressurePoint>,
    pub recommendations: Vec<CapacityRecommendation>,
    pub cited_sources: Vec<String>,
    pub confidence: Confidence,
    pub limitations: Vec<String>,
}

impl CapacityExplanationV1 {
    /// `dates` are the forecast days; `fact_refs` the forecast/day/factor
    /// references supplied to the model.
    pub fn validate(&self, dates: &[NaiveDate], fact_refs: &[String]) -> Result<(), String> {
        if self.schema_version != CAPACITY_EXPLANATION_SCHEMA {
            return Err("unexpected schema_version".into());
        }
        check_text(&self.summary, "summary")?;
        if self.summary.chars().count() > 4 * MAX_TEXT {
            return Err("summary is oversized".into());
        }
        let allowed: BTreeSet<&str> = fact_refs.iter().map(String::as_str).collect();
        if self.pressure_points.len() > MAX_LIST {
            return Err("too many pressure points".into());
        }
        for p in &self.pressure_points {
            if !dates.contains(&p.date) {
                return Err("pressure point names a date outside the forecast".into());
            }
            check_text(&p.explanation, "pressure explanation")?;
            check_subset(&p.cited_sources, &allowed, "pressure point")?;
            if p.cited_sources.is_empty() {
                return Err("pressure point cites no forecast evidence".into());
            }
        }
        if self.recommendations.len() > MAX_LIST {
            return Err("too many recommendations".into());
        }
        for r in &self.recommendations {
            if !CAPACITY_RECOMMENDATION_CATEGORIES.contains(&r.category.as_str())
                || !is_valid_code(&r.category)
            {
                return Err("unknown recommendation category".into());
            }
            if !r.requires_confirmation {
                return Err("recommendations must require confirmation".into());
            }
            check_text(&r.text, "recommendation")?;
        }
        check_subset(&self.cited_sources, &allowed, "capacity explanation")?;
        if self.cited_sources.is_empty() {
            return Err("capacity explanation cites no source".into());
        }
        check_list(&self.limitations, "limitations")?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveTime;

    fn vocab() -> IntentVocabulary {
        IntentVocabulary {
            services: vec!["dermatology_consult".into()],
            specialties: vec!["dermatology".into()],
            modalities: vec!["in_person".into(), "video".into()],
            accessibility: vec!["wheelchair".into()],
            facilities: vec!["north".into()],
        }
    }

    fn intent() -> AccessIntentV1 {
        AccessIntentV1 {
            schema_version: ACCESS_INTENT_SCHEMA.into(),
            service_code: Some("dermatology_consult".into()),
            specialty_code: None,
            modality_codes: vec!["video".into()],
            facility_codes: vec![],
            accessibility_codes: vec![],
            preferred_windows: vec![WeeklyWindow {
                weekday: 2,
                start: NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
                end: NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
            }],
            earliest_date: None,
            latest_date: None,
            language: Some("es".into()),
            continuity_requested: false,
            transport_requested: false,
            missing_information: vec![],
            clinical_triage_suggested: false,
            triage_reasons: vec![],
            cited_sources: vec!["request:text".into()],
            confidence: Confidence::Medium,
            limitations: vec![],
        }
    }

    #[test]
    fn intent_rejects_unknown_codes_and_floor_cannot_be_cleared() {
        let v = vocab();
        assert!(intent().validate(&v).is_ok());
        let mut bad = intent();
        bad.service_code = Some("neurosurgery".into());
        assert!(bad.validate(&v).is_err());
        let mut bad = intent();
        bad.cited_sources = vec!["catalog:service:made_up".into()];
        assert!(bad.validate(&v).is_err());
        let out = intent()
            .finalize(&v, Some("possible_acute_symptom"))
            .unwrap();
        assert!(out.clinical_triage_suggested);
        assert_eq!(
            out.triage_reasons,
            vec!["possible_acute_symptom".to_string()]
        );
    }

    fn ranking(ids: &[&str]) -> AppointmentRankingV1 {
        AppointmentRankingV1 {
            schema_version: APPOINTMENT_RANKING_SCHEMA.into(),
            ranked: ids
                .iter()
                .enumerate()
                .map(|(i, id)| RankedCandidate {
                    candidate_id: id.to_string(),
                    rank: i as u32 + 1,
                    explanation: "closest to preferred window".into(),
                    cited_sources: vec![id.to_string()],
                })
                .collect(),
            overall_note: None,
            cited_sources: vec![],
            confidence: Confidence::High,
            limitations: vec![],
        }
    }

    #[test]
    fn ranking_must_be_a_permutation_of_supplied_candidates() {
        let ids = vec!["c1".to_string(), "c2".to_string()];
        assert!(ranking(&["c2", "c1"]).validate(&ids, &[]).is_ok());
        assert!(ranking(&["c1"]).validate(&ids, &[]).is_err());
        assert!(ranking(&["c1", "c3"]).validate(&ids, &[]).is_err());
        assert!(ranking(&["c1", "c1"]).validate(&ids, &[]).is_err());
        let mut r = ranking(&["c1", "c2"]);
        r.ranked[1].rank = 1;
        assert!(r.validate(&ids, &[]).is_err());
        let mut r = ranking(&["c1", "c2"]);
        r.ranked[0].cited_sources = vec!["fact:x".into()];
        assert!(r.validate(&ids, &[]).is_err());
    }

    #[test]
    fn recovery_order_must_cover_every_entry_once() {
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let ok = CancellationRecoveryV1 {
            schema_version: CANCELLATION_RECOVERY_SCHEMA.into(),
            ordered_entry_ids: vec![b, a],
            explanations: vec![RecoveryExplanation {
                entry_id: b,
                explanation: "longest wait".into(),
                cited_sources: vec![format!("entry:{b}")],
            }],
            cited_sources: vec![],
            confidence: Confidence::Medium,
            limitations: vec![],
        };
        assert!(ok.validate(&[a, b], &[]).is_ok());
        let mut bad = ok.clone();
        bad.ordered_entry_ids = vec![a, Uuid::from_u128(9)];
        assert!(bad.validate(&[a, b], &[]).is_err());
        let mut bad = ok.clone();
        bad.ordered_entry_ids = vec![a];
        assert!(bad.validate(&[a, b], &[]).is_err());
    }

    #[test]
    fn capacity_explanation_is_bound_to_forecast_dates_and_categories() {
        let d = NaiveDate::from_ymd_opt(2026, 8, 3).unwrap();
        let refs = vec![
            format!("forecast:day:{d}"),
            "forecast:factor:seasonal".into(),
        ];
        let ok = CapacityExplanationV1 {
            schema_version: CAPACITY_EXPLANATION_SCHEMA.into(),
            summary: "Demand exceeds capacity in early August.".into(),
            pressure_points: vec![PressurePoint {
                date: d,
                explanation: "seasonal demand".into(),
                cited_sources: vec![refs[0].clone()],
            }],
            recommendations: vec![CapacityRecommendation {
                category: "open_capacity".into(),
                text: "Consider an extra clinic".into(),
                requires_confirmation: true,
            }],
            cited_sources: vec![refs[1].clone()],
            confidence: Confidence::Medium,
            limitations: vec![],
        };
        assert!(ok.validate(&[d], &refs).is_ok());
        let mut bad = ok.clone();
        bad.pressure_points[0].date = d.succ_opt().unwrap();
        assert!(bad.validate(&[d], &refs).is_err());
        let mut bad = ok.clone();
        bad.recommendations[0].category = "cancel_clinic".into();
        assert!(bad.validate(&[d], &refs).is_err());
        let mut bad = ok;
        bad.recommendations[0].requires_confirmation = false;
        assert!(bad.validate(&[d], &refs).is_err());
    }
}
