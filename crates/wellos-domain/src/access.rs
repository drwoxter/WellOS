//! dMind Access v1: the explicit, versioned state machines for access
//! requests, appointment offers and appointments, plus the pure scheduling
//! vocabulary shared by the matcher, the server and the fixtures.
//!
//! `Appointment` is the authoritative scheduling record; the operational
//! `Visit` (see [`crate::triage`]) is derived from it on confirmation. No
//! transition here is ever taken by an AI operation.

use chrono::{DateTime, Duration, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const ACCESS_REQUEST_MACHINE: &str = "access-request.v1";
pub const APPOINTMENT_OFFER_MACHINE: &str = "appointment-offer.v1";
pub const APPOINTMENT_MACHINE: &str = "appointment.v1";

// ---------------------------------------------------------------------------
// Access request
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessRequestStatus {
    Draft,
    Submitted,
    NeedsClinicalTriage,
    OptionsReady,
    Booked,
    Closed,
    Withdrawn,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AccessRequestTransition {
    Submit,
    /// Deterministic rules or a human decided clinical triage must come
    /// first. dMind may *suggest* it; only this transition, taken by the
    /// server on deterministic grounds or a human, records it.
    RouteToTriage,
    /// Triage finished (or staff decided scheduling can proceed).
    TriageCleared,
    OptionsGenerated,
    /// More information was supplied; options must be regenerated.
    Amend,
    Book,
    Close,
    Withdraw,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid transition {transition:?} from {machine} status {from}")]
pub struct InvalidTransition {
    pub machine: &'static str,
    pub from: String,
    pub transition: String,
}

impl AccessRequestStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Draft => "draft",
            Self::Submitted => "submitted",
            Self::NeedsClinicalTriage => "needs_clinical_triage",
            Self::OptionsReady => "options_ready",
            Self::Booked => "booked",
            Self::Closed => "closed",
            Self::Withdrawn => "withdrawn",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "draft" => Self::Draft,
            "submitted" => Self::Submitted,
            "needs_clinical_triage" => Self::NeedsClinicalTriage,
            "options_ready" => Self::OptionsReady,
            "booked" => Self::Booked,
            "closed" => Self::Closed,
            "withdrawn" => Self::Withdrawn,
            _ => return None,
        })
    }

    pub fn apply(self, t: AccessRequestTransition) -> Result<Self, InvalidTransition> {
        use AccessRequestStatus::*;
        use AccessRequestTransition::*;
        match (self, t) {
            (Draft, Submit) => Ok(Submitted),
            (Submitted | OptionsReady, RouteToTriage) => Ok(NeedsClinicalTriage),
            (NeedsClinicalTriage, TriageCleared) => Ok(Submitted),
            (Submitted | OptionsReady, OptionsGenerated) => Ok(OptionsReady),
            (Submitted | OptionsReady | NeedsClinicalTriage, Amend) => Ok(Submitted),
            (OptionsReady | Submitted, Book) => Ok(Booked),
            (Submitted | NeedsClinicalTriage | OptionsReady | Booked, Close) => Ok(Closed),
            (Draft | Submitted | NeedsClinicalTriage | OptionsReady, Withdraw) => Ok(Withdrawn),
            (from, transition) => Err(InvalidTransition {
                machine: ACCESS_REQUEST_MACHINE,
                from: from.as_str().to_string(),
                transition: format!("{transition:?}"),
            }),
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Booked | Self::Closed | Self::Withdrawn)
    }

    pub fn accepts_options(self) -> bool {
        matches!(self, Self::Submitted | Self::OptionsReady)
    }
}

// ---------------------------------------------------------------------------
// Appointment offer
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferStatus {
    Offered,
    Held,
    Accepted,
    Declined,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OfferTransition {
    Hold,
    /// The hold lapsed; the slot is released but the offer may be held
    /// again while the offer itself has not expired.
    ReleaseHold,
    Accept,
    Decline,
    Expire,
    Revoke,
}

impl OfferStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Offered => "offered",
            Self::Held => "held",
            Self::Accepted => "accepted",
            Self::Declined => "declined",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "offered" => Self::Offered,
            "held" => Self::Held,
            "accepted" => Self::Accepted,
            "declined" => Self::Declined,
            "expired" => Self::Expired,
            "revoked" => Self::Revoked,
            _ => return None,
        })
    }

    pub fn apply(self, t: OfferTransition) -> Result<Self, InvalidTransition> {
        use OfferStatus::*;
        use OfferTransition::*;
        match (self, t) {
            (Offered, Hold) => Ok(Held),
            (Held, ReleaseHold) => Ok(Offered),
            (Held, Accept) => Ok(Accepted),
            (Offered | Held, Decline) => Ok(Declined),
            (Offered | Held, Expire) => Ok(Expired),
            (Offered | Held, Revoke) => Ok(Revoked),
            (from, transition) => Err(InvalidTransition {
                machine: APPOINTMENT_OFFER_MACHINE,
                from: from.as_str().to_string(),
                transition: format!("{transition:?}"),
            }),
        }
    }

    pub fn is_live(self) -> bool {
        matches!(self, Self::Offered | Self::Held)
    }
}

// ---------------------------------------------------------------------------
// Appointment
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppointmentStatus {
    Confirmed,
    /// Superseded by a new appointment; `rescheduled_to` points forward.
    Rescheduled,
    Cancelled,
    Fulfilled,
    NoShow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AppointmentTransition {
    Reschedule,
    Cancel,
    Fulfil,
    MarkNoShow,
}

impl AppointmentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Confirmed => "confirmed",
            Self::Rescheduled => "rescheduled",
            Self::Cancelled => "cancelled",
            Self::Fulfilled => "fulfilled",
            Self::NoShow => "no_show",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "confirmed" => Self::Confirmed,
            "rescheduled" => Self::Rescheduled,
            "cancelled" => Self::Cancelled,
            "fulfilled" => Self::Fulfilled,
            "no_show" => Self::NoShow,
            _ => return None,
        })
    }

    pub fn apply(self, t: AppointmentTransition) -> Result<Self, InvalidTransition> {
        use AppointmentStatus::*;
        use AppointmentTransition::*;
        match (self, t) {
            (Confirmed, Reschedule) => Ok(Rescheduled),
            (Confirmed, Cancel) => Ok(Cancelled),
            (Confirmed, Fulfil) => Ok(Fulfilled),
            (Confirmed, MarkNoShow) => Ok(NoShow),
            (from, transition) => Err(InvalidTransition {
                machine: APPOINTMENT_MACHINE,
                from: from.as_str().to_string(),
                transition: format!("{transition:?}"),
            }),
        }
    }

    /// Whether the appointment still occupies its resources.
    pub fn occupies_slot(self) -> bool {
        matches!(self, Self::Confirmed)
    }
}

// ---------------------------------------------------------------------------
// Urgency, reasons and cancellation policy
// ---------------------------------------------------------------------------

/// Operational urgency of an access request or waitlist entry. It is only
/// ever set by deterministic rules or a human; AI operations receive it as
/// a fact and cannot lower it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Hash)]
#[serde(rename_all = "snake_case")]
pub enum Urgency {
    Routine,
    Priority,
    Urgent,
}

impl Urgency {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Routine => "routine",
            Self::Priority => "priority",
            Self::Urgent => "urgent",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "routine" => Self::Routine,
            "priority" => Self::Priority,
            "urgent" => Self::Urgent,
            _ => return None,
        })
    }

    /// Rank used by fairness ordering: higher is seen first.
    pub fn rank(self) -> u8 {
        match self {
            Self::Routine => 0,
            Self::Priority => 1,
            Self::Urgent => 2,
        }
    }
}

pub const CANCELLATION_REASONS: &[&str] = &[
    "patient_request",
    "patient_unwell",
    "patient_unavailable",
    "clinical_decision",
    "professional_unavailable",
    "resource_unavailable",
    "duplicate",
    "administrative",
    "no_longer_needed",
    "other",
];

pub fn is_cancellation_reason(code: &str) -> bool {
    CANCELLATION_REASONS.contains(&code)
}

/// Whether a patient-initiated cancellation or reschedule is inside the
/// tenant's policy window. Staff may override with a mandatory reason.
pub fn within_patient_window(
    now: DateTime<Utc>,
    starts_at: DateTime<Utc>,
    window_hours: i64,
) -> bool {
    starts_at - now >= Duration::hours(window_hours)
}

/// Quiet hours in the recipient's local time; a window may cross midnight.
pub fn in_quiet_hours(local: NaiveTime, start: NaiveTime, end: NaiveTime) -> bool {
    if start == end {
        return false;
    }
    if start < end {
        local >= start && local < end
    } else {
        local >= start || local < end
    }
}

/// Deterministic keyword rule deciding whether an access request must be
/// routed to clinical triage *before* scheduling. This is the authoritative
/// safety floor: dMind's `access-intent.v1` may additionally suggest triage
/// but can never clear a request these rules flagged.
pub fn requires_clinical_triage(free_text: Option<&str>, urgency: Urgency) -> Option<&'static str> {
    if urgency == Urgency::Urgent {
        return Some("urgent_request");
    }
    let text = free_text?.to_lowercase();
    const RED_FLAGS: &[(&str, &str)] = &[
        ("chest pain", "possible_acute_symptom"),
        ("dolor en el pecho", "possible_acute_symptom"),
        ("dolor de pecho", "possible_acute_symptom"),
        ("can't breathe", "possible_acute_symptom"),
        ("cannot breathe", "possible_acute_symptom"),
        ("shortness of breath", "possible_acute_symptom"),
        ("no puedo respirar", "possible_acute_symptom"),
        ("falta de aire", "possible_acute_symptom"),
        ("unconscious", "possible_acute_symptom"),
        ("inconsciente", "possible_acute_symptom"),
        ("severe bleeding", "possible_acute_symptom"),
        ("sangrado abundante", "possible_acute_symptom"),
        ("stroke", "possible_acute_symptom"),
        ("ictus", "possible_acute_symptom"),
        ("suicid", "mental_health_crisis"),
        ("overdose", "possible_acute_symptom"),
        ("sobredosis", "possible_acute_symptom"),
        ("seizure", "possible_acute_symptom"),
        ("convulsi", "possible_acute_symptom"),
        ("anaphyla", "possible_acute_symptom"),
        ("anafila", "possible_acute_symptom"),
    ];
    RED_FLAGS
        .iter()
        .find(|(needle, _)| text.contains(needle))
        .map(|(_, reason)| *reason)
}

// ---------------------------------------------------------------------------
// Shared vocabulary
// ---------------------------------------------------------------------------

/// Closed vocabulary of catalog *kinds*; the entries within each kind are
/// open and tenant-configurable.
pub const CATALOG_KINDS: &[&str] = &[
    "clinical_service",
    "specialty",
    "profession",
    "modality",
    "resource_type",
    "accessibility_capability",
    "location",
    "transport_resource",
    "diagnostic_orderable",
];

pub fn is_catalog_kind(kind: &str) -> bool {
    CATALOG_KINDS.contains(&kind)
}

/// Stable code grammar for catalog entries.
pub fn is_valid_code(code: &str) -> bool {
    let bytes = code.as_bytes();
    if bytes.is_empty() || bytes.len() > 64 {
        return false;
    }
    if !bytes[0].is_ascii_lowercase() && !bytes[0].is_ascii_digit() {
        return false;
    }
    bytes
        .iter()
        .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'.' | b'-'))
}

/// Kind-specific configuration of a `clinical_service` catalog entry.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq)]
pub struct ServiceConfig {
    #[serde(default = "default_duration")]
    pub duration_minutes: i32,
    #[serde(default)]
    pub prep_minutes: i32,
    #[serde(default)]
    pub cleanup_minutes: i32,
    #[serde(default)]
    pub modality_codes: Vec<String>,
    /// Resource types that must all be booked alongside the primary
    /// professional/team (e.g. `dental_chair`).
    #[serde(default)]
    pub required_resource_types: Vec<String>,
    /// Explicit, tenant-configured age bounds. Absent = no restriction.
    #[serde(default)]
    pub min_age_years: Option<i32>,
    #[serde(default)]
    pub max_age_years: Option<i32>,
    #[serde(default)]
    pub requires_referral: bool,
    /// Patient-facing preparation instructions, localized.
    #[serde(default)]
    pub preparation_en: Option<String>,
    #[serde(default)]
    pub preparation_es: Option<String>,
    /// Whether the tenant requires an explicit patient confirmation before
    /// the appointment is considered kept.
    #[serde(default)]
    pub patient_confirmation_required: Option<bool>,
    /// Earliest lead time in hours a patient may self-book this service.
    #[serde(default)]
    pub min_lead_hours: Option<i32>,
}

fn default_duration() -> i32 {
    20
}

impl ServiceConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(5..=720).contains(&self.duration_minutes) {
            return Err("duration_minutes must be between 5 and 720".into());
        }
        if !(0..=240).contains(&self.prep_minutes) || !(0..=240).contains(&self.cleanup_minutes) {
            return Err("buffers must be between 0 and 240 minutes".into());
        }
        if let (Some(min), Some(max)) = (self.min_age_years, self.max_age_years) {
            if min > max {
                return Err("min_age_years must not exceed max_age_years".into());
            }
        }
        for age in [self.min_age_years, self.max_age_years]
            .into_iter()
            .flatten()
        {
            if !(0..=150).contains(&age) {
                return Err("age restrictions must be between 0 and 150 years".into());
            }
        }
        for code in self
            .modality_codes
            .iter()
            .chain(self.required_resource_types.iter())
        {
            if !is_valid_code(code) {
                return Err(format!("invalid code {code:?}"));
            }
        }
        Ok(())
    }
}

/// A localized recurring window, in the owner's local time zone.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct WeeklyWindow {
    /// ISO weekday 1 (Monday) .. 7 (Sunday).
    pub weekday: u8,
    pub start: NaiveTime,
    pub end: NaiveTime,
}

impl WeeklyWindow {
    pub fn validate(&self) -> Result<(), String> {
        if !(1..=7).contains(&self.weekday) {
            return Err("weekday must be 1..=7".into());
        }
        if self.end <= self.start {
            return Err("window end must be after start".into());
        }
        Ok(())
    }
}

/// A resource occupying a candidate slot.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CandidateResource {
    pub resource_id: Uuid,
    pub role: String,
    pub slot_index: i32,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn access_request_machine_is_explicit() {
        use AccessRequestStatus::*;
        use AccessRequestTransition::*;
        assert_eq!(Draft.apply(Submit), Ok(Submitted));
        assert_eq!(Submitted.apply(RouteToTriage), Ok(NeedsClinicalTriage));
        assert_eq!(NeedsClinicalTriage.apply(TriageCleared), Ok(Submitted));
        assert_eq!(Submitted.apply(OptionsGenerated), Ok(OptionsReady));
        assert_eq!(OptionsReady.apply(Book), Ok(Booked));
        assert!(Booked.apply(Book).is_err());
        assert!(Withdrawn.apply(Submit).is_err());
        assert!(Draft.apply(Book).is_err());
        assert!(Booked.apply(Withdraw).is_err());
    }

    #[test]
    fn offer_machine_holds_and_releases() {
        use OfferStatus::*;
        use OfferTransition::*;
        assert_eq!(Offered.apply(Hold), Ok(Held));
        assert_eq!(Held.apply(ReleaseHold), Ok(Offered));
        assert_eq!(Held.apply(Accept), Ok(Accepted));
        assert!(Offered.apply(Accept).is_err(), "accept requires a hold");
        assert!(Accepted.apply(Decline).is_err());
        assert!(Expired.apply(Hold).is_err());
    }

    #[test]
    fn appointment_machine_only_moves_from_confirmed() {
        use AppointmentStatus::*;
        use AppointmentTransition::*;
        assert_eq!(Confirmed.apply(Reschedule), Ok(Rescheduled));
        assert_eq!(Confirmed.apply(Cancel), Ok(Cancelled));
        assert!(Cancelled.apply(Fulfil).is_err());
        assert!(Rescheduled.apply(Cancel).is_err());
        assert!(NoShow.apply(Reschedule).is_err());
    }

    #[test]
    fn quiet_hours_cross_midnight() {
        let t = |h: u32| NaiveTime::from_hms_opt(h, 0, 0).unwrap();
        assert!(in_quiet_hours(t(22), t(21), t(8)));
        assert!(in_quiet_hours(t(3), t(21), t(8)));
        assert!(!in_quiet_hours(t(12), t(21), t(8)));
        assert!(in_quiet_hours(t(13), t(12), t(14)));
        assert!(!in_quiet_hours(t(13), t(13), t(13)));
    }

    #[test]
    fn red_flags_route_to_triage_in_both_languages() {
        assert_eq!(
            requires_clinical_triage(Some("Tengo dolor en el pecho desde ayer"), Urgency::Routine),
            Some("possible_acute_symptom")
        );
        assert_eq!(
            requires_clinical_triage(Some("routine check-up for my knee"), Urgency::Routine),
            None
        );
        assert_eq!(
            requires_clinical_triage(None, Urgency::Urgent),
            Some("urgent_request")
        );
    }

    #[test]
    fn code_grammar() {
        assert!(is_valid_code("hyperbaric_medicine"));
        assert!(is_valid_code("ct.head-contrast"));
        assert!(!is_valid_code("Cardiology"));
        assert!(!is_valid_code(""));
        assert!(!is_valid_code("_x"));
    }

    #[test]
    fn service_config_validation() {
        let mut c = ServiceConfig {
            duration_minutes: 30,
            ..Default::default()
        };
        assert!(c.validate().is_ok());
        c.min_age_years = Some(18);
        c.max_age_years = Some(12);
        assert!(c.validate().is_err());
        c.max_age_years = None;
        c.duration_minutes = 2;
        assert!(c.validate().is_err());
    }
}
