//! Care Operations v1: pure, deterministic operational rules.
//!
//! Versioned state machines for care episodes, placement requests,
//! placements, coordination tasks, perioperative cases and escalation
//! timers; the deterministic placement matcher (`care-placement.v1`), the
//! theatre conflict engine (`theatre-conflicts.v1`), workforce coverage
//! evaluation (`workforce-coverage.v1`) and the operational forecast
//! baseline (`care-operations-forecast.v1`).
//!
//! Nothing here performs I/O or consults a model. Every hard constraint is
//! evaluated before any ranking, and the outputs only *describe*: placing,
//! transferring, discharging, scheduling and rostering remain explicit human
//! actions in the server layer.

use chrono::{DateTime, Datelike, Duration, NaiveDate, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub const EPISODE_MACHINE: &str = "care-episode.v1";
pub const PLACEMENT_REQUEST_MACHINE: &str = "placement-request.v1";
pub const PLACEMENT_MACHINE: &str = "placement.v1";
pub const POSITION_MACHINE: &str = "position-state.v1";
pub const TASK_MACHINE: &str = "coordination-task.v1";
pub const PERIOPERATIVE_MACHINE: &str = "perioperative-case.v1";
pub const ESCALATION_MACHINE: &str = "escalation.v1";
pub const PLACEMENT_MATCHER_VERSION: &str = "care-placement.v1";
pub const THEATRE_CONFLICT_VERSION: &str = "theatre-conflicts.v1";
pub const COVERAGE_VERSION: &str = "workforce-coverage.v1";
pub const FORECAST_VERSION: &str = "care-operations-forecast.v1";
/// Minimum number of distinct days with any arrivals before an operational
/// forecast is attempted.
pub const FORECAST_MIN_HISTORY_DAYS: usize = 14;
/// Default time-limited hold.
pub const DEFAULT_HOLD_MINUTES: i64 = 30;
pub const MAX_HOLD_MINUTES: i64 = 240;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid transition {transition} from {machine} status {from}")]
pub struct InvalidTransition {
    pub machine: &'static str,
    pub from: String,
    pub transition: String,
}

fn invalid(machine: &'static str, from: &str, t: impl std::fmt::Debug) -> InvalidTransition {
    InvalidTransition {
        machine,
        from: from.to_string(),
        transition: format!("{t:?}"),
    }
}

pub fn hash_facts(v: &Value) -> String {
    let canonical = canonical_json(v);
    let mut h = Sha256::new();
    h.update(canonical.as_bytes());
    format!("sha256:{:x}", h.finalize())
}

fn canonical_json(v: &Value) -> String {
    match v {
        Value::Object(m) => {
            let mut keys: Vec<&String> = m.keys().collect();
            keys.sort();
            let parts: Vec<String> = keys
                .into_iter()
                .map(|k| {
                    format!(
                        "{}:{}",
                        serde_json::to_string(k).unwrap(),
                        canonical_json(&m[k])
                    )
                })
                .collect();
            format!("{{{}}}", parts.join(","))
        }
        Value::Array(a) => format!(
            "[{}]",
            a.iter().map(canonical_json).collect::<Vec<_>>().join(",")
        ),
        other => other.to_string(),
    }
}

// ---------------------------------------------------------------------------
// Care episode
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeStatus {
    Planned,
    Active,
    DischargePreparation,
    DischargeReady,
    Completed,
    Cancelled,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EpisodeTransition {
    Activate,
    StartDischargePreparation,
    MarkDischargeReady,
    ReopenPreparation,
    Complete,
    Cancel,
    MarkError,
}

impl EpisodeStatus {
    pub const ALL: [EpisodeStatus; 7] = [
        Self::Planned,
        Self::Active,
        Self::DischargePreparation,
        Self::DischargeReady,
        Self::Completed,
        Self::Cancelled,
        Self::Error,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Active => "active",
            Self::DischargePreparation => "discharge_preparation",
            Self::DischargeReady => "discharge_ready",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Error => "error",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Error)
    }

    pub fn is_open(self) -> bool {
        !self.is_terminal()
    }

    pub fn apply(self, t: EpisodeTransition) -> Result<Self, InvalidTransition> {
        use EpisodeStatus::*;
        use EpisodeTransition::*;
        match (self, t) {
            (Planned, Activate) => Ok(Active),
            (Active, StartDischargePreparation) => Ok(DischargePreparation),
            (DischargePreparation, MarkDischargeReady) => Ok(DischargeReady),
            (DischargeReady, ReopenPreparation) => Ok(DischargePreparation),
            // Ambulatory/virtual episodes may complete directly from active;
            // completion is an operational close, never a clinical judgement.
            (Active | DischargePreparation | DischargeReady, Complete) => Ok(Completed),
            (Planned | Active | DischargePreparation | DischargeReady, Cancel) => Ok(Cancelled),
            (Planned | Active | DischargePreparation | DischargeReady, MarkError) => Ok(Error),
            (from, t) => Err(invalid(EPISODE_MACHINE, from.as_str(), t)),
        }
    }

    pub fn allowed_transitions(self) -> Vec<EpisodeTransition> {
        use EpisodeTransition::*;
        [
            Activate,
            StartDischargePreparation,
            MarkDischargeReady,
            ReopenPreparation,
            Complete,
            Cancel,
            MarkError,
        ]
        .into_iter()
        .filter(|t| self.apply(*t).is_ok())
        .collect()
    }
}

impl EpisodeTransition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Activate => "activate",
            Self::StartDischargePreparation => "start_discharge_preparation",
            Self::MarkDischargeReady => "mark_discharge_ready",
            Self::ReopenPreparation => "reopen_preparation",
            Self::Complete => "complete",
            Self::Cancel => "cancel",
            Self::MarkError => "mark_error",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Activate,
            Self::StartDischargePreparation,
            Self::MarkDischargeReady,
            Self::ReopenPreparation,
            Self::Complete,
            Self::Cancel,
            Self::MarkError,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }
}

/// `discharge_ready` is operational readiness only. The guard enforces that
/// every open discharge barrier is resolved; it never asserts medical fitness.
pub fn discharge_ready_blockers(open_barriers: usize, open_blocking_tasks: usize) -> Vec<String> {
    let mut out = Vec::new();
    if open_barriers > 0 {
        out.push(format!("{open_barriers} open discharge barrier(s)"));
    }
    if open_blocking_tasks > 0 {
        out.push(format!(
            "{open_blocking_tasks} open discharge-preparation task(s)"
        ));
    }
    out
}

// ---------------------------------------------------------------------------
// Placement request / placement / position state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementRequestStatus {
    Requested,
    CandidatesReady,
    Held,
    Placed,
    Cancelled,
    Expired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementRequestTransition {
    CandidatesGenerated,
    Hold,
    ReleaseHold,
    Confirm,
    Cancel,
    Expire,
}

impl PlacementRequestStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::CandidatesReady => "candidates_ready",
            Self::Held => "held",
            Self::Placed => "placed",
            Self::Cancelled => "cancelled",
            Self::Expired => "expired",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Requested,
            Self::CandidatesReady,
            Self::Held,
            Self::Placed,
            Self::Cancelled,
            Self::Expired,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }

    pub fn is_open(self) -> bool {
        matches!(self, Self::Requested | Self::CandidatesReady | Self::Held)
    }

    pub fn apply(self, t: PlacementRequestTransition) -> Result<Self, InvalidTransition> {
        use PlacementRequestStatus::*;
        use PlacementRequestTransition::*;
        match (self, t) {
            (Requested | CandidatesReady, CandidatesGenerated) => Ok(CandidatesReady),
            (CandidatesReady, Hold) => Ok(Held),
            (Held, ReleaseHold) => Ok(CandidatesReady),
            // Confirmation is allowed from a hold or straight from a fresh
            // candidate set (the hold is optional).
            (CandidatesReady | Held, Confirm) => Ok(Placed),
            (Requested | CandidatesReady | Held, Cancel) => Ok(Cancelled),
            (Requested | CandidatesReady | Held, Expire) => Ok(Expired),
            (from, t) => Err(invalid(PLACEMENT_REQUEST_MACHINE, from.as_str(), t)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementStatus {
    Held,
    Occupied,
    Released,
    Expired,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementTransition {
    Confirm,
    Release,
    Expire,
    Revoke,
}

impl PlacementStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Held => "held",
            Self::Occupied => "occupied",
            Self::Released => "released",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Held,
            Self::Occupied,
            Self::Released,
            Self::Expired,
            Self::Revoked,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }

    pub fn is_live(self) -> bool {
        matches!(self, Self::Held | Self::Occupied)
    }

    pub fn apply(self, t: PlacementTransition) -> Result<Self, InvalidTransition> {
        use PlacementStatus::*;
        use PlacementTransition::*;
        match (self, t) {
            (Held, Confirm) => Ok(Occupied),
            (Occupied, Release) => Ok(Released),
            (Held, Expire) => Ok(Expired),
            (Held, Revoke) => Ok(Revoked),
            (from, t) => Err(invalid(PLACEMENT_MACHINE, from.as_str(), t)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PositionState {
    Available,
    Held,
    Occupied,
    Cleaning,
    Blocked,
    OutOfService,
}

impl PositionState {
    pub const ALL: [PositionState; 6] = [
        Self::Available,
        Self::Held,
        Self::Occupied,
        Self::Cleaning,
        Self::Blocked,
        Self::OutOfService,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::Held => "held",
            Self::Occupied => "occupied",
            Self::Cleaning => "cleaning",
            Self::Blocked => "blocked",
            Self::OutOfService => "out_of_service",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|v| v.as_str() == s)
    }

    /// Only an available position can be held or occupied by the matcher.
    pub fn accepts_placement(self) -> bool {
        matches!(self, Self::Available)
    }

    /// Explicit position state graph (`position-state.v1`).
    pub fn can_transition(self, to: PositionState) -> bool {
        use PositionState::*;
        matches!(
            (self, to),
            (Available, Held)
                | (Available, Occupied)
                | (Available, Blocked)
                | (Available, OutOfService)
                | (Available, Cleaning)
                | (Held, Available)
                | (Held, Occupied)
                | (Occupied, Cleaning)
                | (Occupied, Available)
                | (Cleaning, Available)
                | (Cleaning, Blocked)
                | (Cleaning, OutOfService)
                | (Blocked, Available)
                | (Blocked, OutOfService)
                | (OutOfService, Available)
                | (OutOfService, Blocked)
        )
    }
}

// ---------------------------------------------------------------------------
// Coordination tasks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Open,
    InProgress,
    Blocked,
    HandedOff,
    Completed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskTransition {
    Start,
    Block,
    Unblock,
    HandOff,
    Accept,
    Complete,
    Cancel,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::InProgress => "in_progress",
            Self::Blocked => "blocked",
            Self::HandedOff => "handed_off",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Open,
            Self::InProgress,
            Self::Blocked,
            Self::HandedOff,
            Self::Completed,
            Self::Cancelled,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }

    pub fn is_open(self) -> bool {
        matches!(
            self,
            Self::Open | Self::InProgress | Self::Blocked | Self::HandedOff
        )
    }

    pub fn apply(self, t: TaskTransition) -> Result<Self, InvalidTransition> {
        use TaskStatus::*;
        use TaskTransition::*;
        match (self, t) {
            (Open | HandedOff, Start) => Ok(InProgress),
            (Open | InProgress | HandedOff, Block) => Ok(Blocked),
            (Blocked, Unblock) => Ok(Open),
            (Open | InProgress | Blocked, HandOff) => Ok(HandedOff),
            (HandedOff, Accept) => Ok(InProgress),
            (Open | InProgress | HandedOff, Complete) => Ok(Completed),
            (Open | InProgress | Blocked | HandedOff, Cancel) => Ok(Cancelled),
            (from, t) => Err(invalid(TASK_MACHINE, from.as_str(), t)),
        }
    }
}

impl TaskTransition {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "start" => Self::Start,
            "block" => Self::Block,
            "unblock" => Self::Unblock,
            "hand_off" => Self::HandOff,
            "accept" => Self::Accept,
            "complete" => Self::Complete,
            "cancel" => Self::Cancel,
            _ => return None,
        })
    }
}

/// A task may only complete once every dependency is completed (or
/// cancelled); returns the identifiers still blocking it.
pub fn unmet_dependencies(deps: &[(Uuid, TaskStatus)]) -> Vec<Uuid> {
    deps.iter()
        .filter(|(_, s)| !matches!(s, TaskStatus::Completed | TaskStatus::Cancelled))
        .map(|(id, _)| *id)
        .collect()
}

// ---------------------------------------------------------------------------
// Perioperative case
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseStatus {
    Requested,
    ReadyForScheduling,
    Scheduled,
    InTheatre,
    Completed,
    Delayed,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CaseTransition {
    MarkReady,
    Schedule,
    Reschedule,
    Start,
    Complete,
    Delay,
    Unschedule,
    Cancel,
}

impl CaseStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Requested => "requested",
            Self::ReadyForScheduling => "ready_for_scheduling",
            Self::Scheduled => "scheduled",
            Self::InTheatre => "in_theatre",
            Self::Completed => "completed",
            Self::Delayed => "delayed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Requested,
            Self::ReadyForScheduling,
            Self::Scheduled,
            Self::InTheatre,
            Self::Completed,
            Self::Delayed,
            Self::Cancelled,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled)
    }

    pub fn holds_theatre(self) -> bool {
        matches!(self, Self::Scheduled | Self::InTheatre | Self::Delayed)
    }

    pub fn apply(self, t: CaseTransition) -> Result<Self, InvalidTransition> {
        use CaseStatus::*;
        use CaseTransition::*;
        match (self, t) {
            (Requested, MarkReady) => Ok(ReadyForScheduling),
            (ReadyForScheduling | Delayed, Schedule) => Ok(Scheduled),
            (Scheduled | Delayed, Reschedule) => Ok(Scheduled),
            (Scheduled, Start) => Ok(InTheatre),
            (InTheatre, Complete) => Ok(Completed),
            (Scheduled, Delay) => Ok(Delayed),
            (Scheduled | Delayed, Unschedule) => Ok(ReadyForScheduling),
            (Requested | ReadyForScheduling | Scheduled | Delayed, Cancel) => Ok(Cancelled),
            (from, t) => Err(invalid(PERIOPERATIVE_MACHINE, from.as_str(), t)),
        }
    }
}

impl CaseTransition {
    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "mark_ready" => Self::MarkReady,
            "schedule" => Self::Schedule,
            "reschedule" => Self::Reschedule,
            "start" => Self::Start,
            "complete" => Self::Complete,
            "delay" => Self::Delay,
            "unschedule" => Self::Unschedule,
            "cancel" => Self::Cancel,
            _ => return None,
        })
    }
}

// ---------------------------------------------------------------------------
// Escalation timers
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EscalationStatus {
    Armed,
    Escalated,
    Acknowledged,
    Resolved,
    Cancelled,
}

impl EscalationStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Armed => "armed",
            Self::Escalated => "escalated",
            Self::Acknowledged => "acknowledged",
            Self::Resolved => "resolved",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        [
            Self::Armed,
            Self::Escalated,
            Self::Acknowledged,
            Self::Resolved,
            Self::Cancelled,
        ]
        .into_iter()
        .find(|v| v.as_str() == s)
    }

    pub fn can_transition(self, to: EscalationStatus) -> bool {
        use EscalationStatus::*;
        matches!(
            (self, to),
            (Armed, Escalated)
                | (Armed, Resolved)
                | (Armed, Cancelled)
                | (Escalated, Acknowledged)
                | (Escalated, Resolved)
                | (Acknowledged, Resolved)
        )
    }
}

/// A timer fires once `now >= due_at`; the caller escalates through the
/// existing internal alert channel. Pure so it is testable at DST edges.
pub fn escalation_due(due_at: DateTime<Utc>, now: DateTime<Utc>) -> bool {
    now >= due_at
}

/// Default thresholds (minutes) by escalation kind and operational priority.
/// Tenants override these through policy; the table is a floor, not a
/// clinical judgement, and never changes triage priority.
pub fn default_escalation_minutes(kind: &str, priority: &str) -> Option<i64> {
    let base = match kind {
        "waiting_triage" => 15,
        "waiting_clinician" => 60,
        "boarding" => 240,
        "placement_delay" => 120,
        "discharge_delay" => 180,
        "transport_delay" => 45,
        _ => return None,
    };
    let factor = match priority {
        "emergency" => 0.25,
        "urgent" => 0.5,
        "priority" => 0.75,
        _ => 1.0,
    };
    Some(((base as f64) * factor).ceil().max(1.0) as i64)
}

// ---------------------------------------------------------------------------
// Deterministic placement matcher (care-placement.v1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Default)]
pub struct PlacementRequirements {
    pub service_code: Option<String>,
    #[serde(default)]
    pub specialty_codes: Vec<String>,
    pub age_group_code: Option<String>,
    #[serde(default)]
    pub isolation_codes: Vec<String>,
    #[serde(default)]
    pub accessibility_codes: Vec<String>,
    #[serde(default)]
    pub equipment_codes: Vec<String>,
    /// Required room compatibility (`single`, `same_sex`, `cohort_isolation`).
    pub room_policy: Option<String>,
    /// Administrative sex marker used only for `same_sex` room policy.
    pub sex: Option<String>,
    #[serde(default)]
    pub preferred_unit_ids: Vec<Uuid>,
    /// Profession codes that must be covered on the unit's current shift.
    #[serde(default)]
    pub required_staffed_professions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PositionFact {
    pub location_id: Uuid,
    pub facility_id: Uuid,
    pub unit_id: Option<Uuid>,
    pub room_id: Option<Uuid>,
    pub unit_name: String,
    pub room_name: Option<String>,
    pub position_name: String,
    pub type_code: String,
    pub state: PositionState,
    pub active: bool,
    /// Hierarchy operational status: any ancestor closed/maintenance closes it.
    pub hierarchy_open: bool,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
    /// Union of position/room/unit specialty and service-line codes.
    pub specialty_codes: Vec<String>,
    pub service_line_codes: Vec<String>,
    pub age_group_codes: Vec<String>,
    pub isolation_codes: Vec<String>,
    pub accessibility_codes: Vec<String>,
    pub equipment_codes: Vec<String>,
    pub room_policy: String,
    /// Administrative sex markers of patients currently occupying the same room.
    pub room_occupant_sexes: Vec<String>,
    pub room_occupant_count: usize,
    pub room_capacity: usize,
    /// Live hold or occupancy on this position (any status in held/occupied).
    pub live_placement: bool,
    /// Active closure/maintenance/cleaning/out-of-service period covering now.
    pub blocking_period: Option<String>,
    /// Profession codes staffed on the unit's current shift.
    pub staffed_professions: Vec<String>,
    /// Unit occupancy ratio 0..=1 (soft factor only).
    pub unit_occupancy: f64,
    pub cleaning_minutes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlacementFacts {
    pub tenant_id: Uuid,
    pub facility_id: Uuid,
    pub facility_closed: bool,
    pub local_date: NaiveDate,
    pub requirements: PlacementRequirements,
    pub positions: Vec<PositionFact>,
    /// Position the episode currently occupies (excluded for transfers).
    pub current_location_id: Option<Uuid>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlacementScoreFactor {
    pub code: String,
    pub weight: f64,
    pub value: f64,
    pub points: f64,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlacementCandidate {
    pub location_id: Uuid,
    pub unit_id: Option<Uuid>,
    pub label: String,
    pub feasible: bool,
    pub rank: Option<usize>,
    pub score: Option<f64>,
    pub factors: Vec<PlacementScoreFactor>,
    pub rejection_codes: Vec<String>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlacementMatch {
    pub matcher_version: String,
    pub facts_hash: String,
    pub feasible: Vec<PlacementCandidate>,
    pub rejected: Vec<PlacementCandidate>,
    pub rejected_by_code: BTreeMap<String, u64>,
}

fn contains_all(have: &[String], need: &[String]) -> Vec<String> {
    need.iter()
        .filter(|n| !have.iter().any(|h| h == *n))
        .cloned()
        .collect()
}

/// Hard constraints for one position. Empty vector means feasible.
pub fn placement_rejections(facts: &PlacementFacts, p: &PositionFact) -> Vec<String> {
    let r = &facts.requirements;
    let mut out = Vec::new();
    if p.facility_id != facts.facility_id {
        out.push("facility_scope".into());
    }
    if facts.facility_closed {
        out.push("facility_closed".into());
    }
    if !p.active || !p.hierarchy_open {
        out.push("location_closed".into());
    }
    if let Some(from) = p.effective_from {
        if facts.local_date < from {
            out.push("not_yet_effective".into());
        }
    }
    if let Some(to) = p.effective_to {
        if facts.local_date > to {
            out.push("no_longer_effective".into());
        }
    }
    if Some(p.location_id) == facts.current_location_id {
        out.push("current_position".into());
    }
    if let Some(period) = &p.blocking_period {
        out.push(format!("period_{period}"));
    }
    if !p.state.accepts_placement() {
        out.push(format!("state_{}", p.state.as_str()));
    }
    if p.live_placement {
        out.push("overlapping_placement".into());
    }
    if let Some(service) = &r.service_code {
        if !p.service_line_codes.is_empty() && !p.service_line_codes.contains(service) {
            out.push("service_capability".into());
        }
    }
    if !r.specialty_codes.is_empty()
        && !p.specialty_codes.is_empty()
        && !r
            .specialty_codes
            .iter()
            .any(|s| p.specialty_codes.contains(s))
    {
        out.push("specialty_capability".into());
    }
    if let Some(age) = &r.age_group_code {
        if !p.age_group_codes.is_empty() && !p.age_group_codes.contains(age) {
            out.push("age_group_capability".into());
        }
    }
    if !contains_all(&p.isolation_codes, &r.isolation_codes).is_empty() {
        out.push("isolation_capability".into());
    }
    if !contains_all(&p.accessibility_codes, &r.accessibility_codes).is_empty() {
        out.push("accessibility_capability".into());
    }
    if !contains_all(&p.equipment_codes, &r.equipment_codes).is_empty() {
        out.push("equipment_capability".into());
    }
    match r.room_policy.as_deref() {
        Some("single") => {
            if p.room_capacity > 1 || p.room_occupant_count > 0 {
                out.push("room_policy_single".into());
            }
        }
        Some("same_sex") => {
            if let Some(sex) = &r.sex {
                if p.room_occupant_sexes.iter().any(|s| s != sex) {
                    out.push("room_policy_same_sex".into());
                }
            } else {
                out.push("room_policy_same_sex_unknown".into());
            }
        }
        Some("cohort_isolation")
            if p.room_policy != "cohort_isolation" && p.room_occupant_count > 0 =>
        {
            out.push("room_policy_cohort".into());
        }
        _ => {}
    }
    // A room configured same_sex rejects a mixed occupant regardless of request.
    if p.room_policy == "same_sex" {
        match &r.sex {
            Some(sex) if p.room_occupant_sexes.iter().all(|s| s == sex) => {}
            Some(_) => out.push("room_policy_same_sex".into()),
            None if p.room_occupant_count > 0 => out.push("room_policy_same_sex_unknown".into()),
            None => {}
        }
    }
    if p.room_policy == "single" && p.room_occupant_count > 0 {
        out.push("room_full".into());
    }
    if !contains_all(&p.staffed_professions, &r.required_staffed_professions).is_empty() {
        out.push("staffed_capability".into());
    }
    out.sort();
    out.dedup();
    out
}

fn facts_value(facts: &PlacementFacts) -> Value {
    serde_json::to_value(facts).unwrap_or(Value::Null)
}

/// Deterministic candidate generation: hard constraints first, then a
/// transparent score. Ties resolve by position label so the order is stable.
pub fn match_placements(facts: &PlacementFacts) -> PlacementMatch {
    let r = &facts.requirements;
    let mut feasible = Vec::new();
    let mut rejected = Vec::new();
    let mut by_code: BTreeMap<String, u64> = BTreeMap::new();
    for p in &facts.positions {
        let label = match &p.room_name {
            Some(room) => format!("{} / {} / {}", p.unit_name, room, p.position_name),
            None => format!("{} / {}", p.unit_name, p.position_name),
        };
        let codes = placement_rejections(facts, p);
        if !codes.is_empty() {
            for c in &codes {
                *by_code.entry(c.clone()).or_insert(0) += 1;
            }
            rejected.push(PlacementCandidate {
                location_id: p.location_id,
                unit_id: p.unit_id,
                label,
                feasible: false,
                rank: None,
                score: None,
                factors: vec![],
                rejection_codes: codes,
                reasons: vec![],
            });
            continue;
        }
        let mut factors = Vec::new();
        let mut reasons = Vec::new();
        // Specialty/service fit (0.35): exact specialty match scores full.
        let specialty_fit = if r.specialty_codes.is_empty() {
            0.5
        } else if r
            .specialty_codes
            .iter()
            .any(|s| p.specialty_codes.contains(s))
        {
            1.0
        } else {
            0.2
        };
        factors.push(PlacementScoreFactor {
            code: "specialty_fit".into(),
            weight: 0.35,
            value: specialty_fit,
            points: 0.35 * specialty_fit,
            detail: if specialty_fit >= 1.0 {
                "unit specialty matches the episode".into()
            } else {
                "unit accepts the episode without a dedicated specialty match".into()
            },
        });
        if specialty_fit >= 1.0 {
            reasons.push("specialty_match".into());
        }
        // Preferred unit (0.2).
        let preferred = p
            .unit_id
            .map(|u| r.preferred_unit_ids.contains(&u))
            .unwrap_or(false);
        factors.push(PlacementScoreFactor {
            code: "preferred_unit".into(),
            weight: 0.2,
            value: if preferred { 1.0 } else { 0.0 },
            points: if preferred { 0.2 } else { 0.0 },
            detail: if preferred {
                "unit preferred by the requesting team".into()
            } else {
                "not a preferred unit".into()
            },
        });
        if preferred {
            reasons.push("preferred_unit".into());
        }
        // Unit pressure (0.25): lower occupancy scores higher.
        let pressure_value = (1.0 - p.unit_occupancy).clamp(0.0, 1.0);
        factors.push(PlacementScoreFactor {
            code: "unit_pressure".into(),
            weight: 0.25,
            value: pressure_value,
            points: 0.25 * pressure_value,
            detail: format!("unit occupancy {:.0}%", p.unit_occupancy * 100.0),
        });
        // Capability surplus (0.1): avoid consuming scarce isolation/equipment
        // positions when the request does not need them.
        let surplus = (p
            .isolation_codes
            .len()
            .saturating_sub(r.isolation_codes.len())
            + p.equipment_codes
                .len()
                .saturating_sub(r.equipment_codes.len())) as f64;
        let surplus_value = (1.0 - surplus / 4.0).clamp(0.0, 1.0);
        factors.push(PlacementScoreFactor {
            code: "capability_fit".into(),
            weight: 0.1,
            value: surplus_value,
            points: 0.1 * surplus_value,
            detail: if surplus == 0.0 {
                "capabilities match the request exactly".into()
            } else {
                format!("{surplus} scarce capabilit(ies) not required by this request")
            },
        });
        if surplus == 0.0 {
            reasons.push("exact_capability_fit".into());
        }
        // Turnaround (0.1): shorter cleaning configuration ranks slightly higher.
        let turnaround_value = (1.0 - (p.cleaning_minutes as f64 / 120.0)).clamp(0.0, 1.0);
        factors.push(PlacementScoreFactor {
            code: "turnaround".into(),
            weight: 0.1,
            value: turnaround_value,
            points: 0.1 * turnaround_value,
            detail: format!("{} min configured turnaround", p.cleaning_minutes),
        });
        let score: f64 = factors.iter().map(|f| f.points).sum();
        feasible.push(PlacementCandidate {
            location_id: p.location_id,
            unit_id: p.unit_id,
            label,
            feasible: true,
            rank: None,
            score: Some((score * 1000.0).round() / 1000.0),
            factors,
            rejection_codes: vec![],
            reasons,
        });
    }
    feasible.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.label.cmp(&b.label))
    });
    for (i, c) in feasible.iter_mut().enumerate() {
        c.rank = Some(i + 1);
    }
    rejected.sort_by(|a, b| a.label.cmp(&b.label));
    PlacementMatch {
        matcher_version: PLACEMENT_MATCHER_VERSION.into(),
        facts_hash: hash_facts(&facts_value(facts)),
        feasible,
        rejected,
        rejected_by_code: by_code,
    }
}

/// Hold expiry bounded by policy.
pub fn hold_expiry(now: DateTime<Utc>, minutes: Option<i64>) -> DateTime<Utc> {
    let m = minutes
        .unwrap_or(DEFAULT_HOLD_MINUTES)
        .clamp(1, MAX_HOLD_MINUTES);
    now + Duration::minutes(m)
}

// ---------------------------------------------------------------------------
// Theatre conflict engine (theatre-conflicts.v1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Interval {
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

impl Interval {
    pub fn overlaps(&self, other: &Interval) -> bool {
        self.starts_at < other.ends_at && other.starts_at < self.ends_at
    }
}

/// Prerequisite states that do not block readiness or scheduling.
pub fn prerequisite_satisfied(status: &str) -> bool {
    matches!(status, "met" | "waived" | "not_required")
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Prerequisite {
    pub code: String,
    /// `pending` | `met` | `waived` | `not_required` | `failed`
    pub status: String,
    pub evidence_ref: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TheatreCaseFacts {
    pub case_id: Uuid,
    pub case_version: i64,
    pub expected_case_version: i64,
    pub theatre_location_id: Uuid,
    pub theatre_version: i64,
    pub expected_theatre_version: i64,
    pub theatre_open: bool,
    pub facility_closed: bool,
    pub proposed: Interval,
    pub turnaround_minutes: i64,
    pub minimum_turnaround_minutes: i64,
    pub session: Option<Interval>,
    /// Other scheduled cases in the same theatre (with their turnaround).
    pub theatre_cases: Vec<(Uuid, Interval, i64)>,
    /// Team members required and their other commitments.
    pub team_user_ids: Vec<Uuid>,
    pub team_commitments: Vec<(Uuid, Interval)>,
    pub required_equipment_codes: Vec<String>,
    pub theatre_equipment_codes: Vec<String>,
    pub prerequisites: Vec<Prerequisite>,
    pub required_results_complete: bool,
    pub destination_required: bool,
    pub destination_feasible_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TheatreConflict {
    pub code: String,
    pub detail: String,
    pub blocking: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TheatreCheck {
    pub engine_version: String,
    pub feasible: bool,
    pub conflicts: Vec<TheatreConflict>,
    pub facts_hash: String,
}

pub fn check_theatre(facts: &TheatreCaseFacts) -> TheatreCheck {
    let mut c = Vec::new();
    let push = |c: &mut Vec<TheatreConflict>, code: &str, detail: String| {
        c.push(TheatreConflict {
            code: code.into(),
            detail,
            blocking: true,
        })
    };
    if facts.case_version != facts.expected_case_version {
        push(
            &mut c,
            "stale_case_version",
            format!(
                "case version {} differs from expected {}",
                facts.case_version, facts.expected_case_version
            ),
        );
    }
    if facts.theatre_version != facts.expected_theatre_version {
        push(
            &mut c,
            "stale_resource_version",
            "theatre configuration changed since the plan was prepared".into(),
        );
    }
    if facts.facility_closed {
        push(
            &mut c,
            "facility_closed",
            "the facility is closed during the proposed time".into(),
        );
    }
    if !facts.theatre_open {
        push(
            &mut c,
            "theatre_unavailable",
            "the theatre is closed, in maintenance or out of service".into(),
        );
    }
    if facts.proposed.ends_at <= facts.proposed.starts_at {
        push(&mut c, "invalid_interval", "end must be after start".into());
    }
    if let Some(s) = &facts.session {
        if facts.proposed.starts_at < s.starts_at || facts.proposed.ends_at > s.ends_at {
            push(
                &mut c,
                "outside_session",
                "the case does not fit inside the scheduling session".into(),
            );
        }
    }
    if facts.turnaround_minutes < facts.minimum_turnaround_minutes {
        push(
            &mut c,
            "unrealistic_turnaround",
            format!(
                "{} min turnaround is below the configured minimum of {} min",
                facts.turnaround_minutes, facts.minimum_turnaround_minutes
            ),
        );
    }
    for (other_id, iv, other_turnaround) in &facts.theatre_cases {
        if *other_id == facts.case_id {
            continue;
        }
        let padded = Interval {
            starts_at: iv.starts_at - Duration::minutes(facts.turnaround_minutes),
            ends_at: iv.ends_at + Duration::minutes(*other_turnaround),
        };
        if padded.overlaps(&facts.proposed) {
            push(
                &mut c,
                "theatre_overlap",
                format!("overlaps case {other_id} including turnaround"),
            );
        }
    }
    for (user, iv) in &facts.team_commitments {
        if facts.team_user_ids.contains(user) && iv.overlaps(&facts.proposed) {
            push(
                &mut c,
                "team_overlap",
                format!("team member {user} is committed elsewhere"),
            );
        }
    }
    let missing = contains_all(
        &facts.theatre_equipment_codes,
        &facts.required_equipment_codes,
    );
    if !missing.is_empty() {
        push(
            &mut c,
            "missing_equipment",
            format!("theatre lacks: {}", missing.join(", ")),
        );
    }
    for p in &facts.prerequisites {
        if !prerequisite_satisfied(&p.status) {
            push(
                &mut c,
                "missing_prerequisite",
                format!("prerequisite {} is {}", p.code, p.status),
            );
        }
    }
    if !facts.required_results_complete {
        push(
            &mut c,
            "missing_required_results",
            "required diagnostic results or documents are not final".into(),
        );
    }
    if facts.destination_required && facts.destination_feasible_count == 0 {
        push(
            &mut c,
            "no_post_procedure_placement",
            "no feasible post-procedure placement is available".into(),
        );
    }
    let mut codes: Vec<&str> = c.iter().map(|x| x.code.as_str()).collect();
    codes.dedup();
    TheatreCheck {
        engine_version: THEATRE_CONFLICT_VERSION.into(),
        feasible: c.is_empty(),
        facts_hash: hash_facts(&serde_json::to_value(facts).unwrap_or(Value::Null)),
        conflicts: c,
    }
}

// ---------------------------------------------------------------------------
// Workforce coverage (workforce-coverage.v1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CoverageRequirement {
    pub profession_code: String,
    pub capability_codes: Vec<String>,
    pub specialty_codes: Vec<String>,
    pub minimum_count: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct StaffFact {
    pub assignment_id: Uuid,
    pub user_id: Option<Uuid>,
    pub profession_code: String,
    pub capability_codes: Vec<String>,
    pub specialty_codes: Vec<String>,
    pub role_in_shift: String,
    pub status: String,
    pub absent: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CoverageGap {
    pub profession_code: String,
    pub capability_codes: Vec<String>,
    pub specialty_codes: Vec<String>,
    pub required: i64,
    pub present: i64,
    pub on_call: i64,
    pub shortfall: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CoverageReport {
    pub engine_version: String,
    pub gaps: Vec<CoverageGap>,
    pub covered: bool,
    pub facts_hash: String,
}

fn staff_matches(s: &StaffFact, r: &CoverageRequirement) -> bool {
    s.profession_code == r.profession_code
        && contains_all(&s.capability_codes, &r.capability_codes).is_empty()
        && (r.specialty_codes.is_empty()
            || r.specialty_codes
                .iter()
                .any(|c| s.specialty_codes.contains(c)))
}

/// Distinct people (by `user_id`; assignments without a user count once each)
/// in `role` who satisfy the requirement. Duplicate assignment rows for the
/// same person never inflate coverage.
fn distinct_staff(staff: &[StaffFact], r: &CoverageRequirement, role: &str) -> i64 {
    let mut users = std::collections::BTreeSet::new();
    let mut anonymous = 0i64;
    for s in staff.iter().filter(|s| {
        !s.absent
            && s.role_in_shift == role
            && matches!(s.status.as_str(), "planned" | "confirmed")
            && staff_matches(s, r)
    }) {
        match s.user_id {
            Some(u) => {
                users.insert(u);
            }
            None => anonymous += 1,
        }
    }
    users.len() as i64 + anonymous
}

pub fn evaluate_coverage(reqs: &[CoverageRequirement], staff: &[StaffFact]) -> CoverageReport {
    let mut gaps = Vec::new();
    for r in reqs {
        let present = distinct_staff(staff, r, "assigned");
        let on_call = distinct_staff(staff, r, "on_call");
        if present < r.minimum_count {
            gaps.push(CoverageGap {
                profession_code: r.profession_code.clone(),
                capability_codes: r.capability_codes.clone(),
                specialty_codes: r.specialty_codes.clone(),
                required: r.minimum_count,
                present,
                on_call,
                shortfall: r.minimum_count - present,
            });
        }
    }
    let facts = serde_json::json!({"requirements": reqs, "staff": staff});
    CoverageReport {
        engine_version: COVERAGE_VERSION.into(),
        covered: gaps.is_empty(),
        gaps,
        facts_hash: hash_facts(&facts),
    }
}

/// Feasible reallocation suggestions: only staff whose profession and
/// capabilities satisfy the gap, who are present, not absent and not already
/// counted against the requirement (floats and on-call first). Never
/// executes anything.
pub fn reallocation_candidates<'a>(gap: &CoverageGap, pool: &'a [StaffFact]) -> Vec<&'a StaffFact> {
    let req = CoverageRequirement {
        profession_code: gap.profession_code.clone(),
        capability_codes: gap.capability_codes.clone(),
        specialty_codes: gap.specialty_codes.clone(),
        minimum_count: gap.required,
    };
    let mut out: Vec<&StaffFact> = pool
        .iter()
        .filter(|s| !s.absent && s.status != "cancelled" && staff_matches(s, &req))
        .filter(|s| s.role_in_shift == "float" || s.role_in_shift == "on_call")
        .collect();
    out.sort_by(|a, b| {
        let w = |s: &StaffFact| if s.role_in_shift == "float" { 0 } else { 1 };
        w(a).cmp(&w(b))
            .then_with(|| a.assignment_id.cmp(&b.assignment_id))
    });
    out
}

// ---------------------------------------------------------------------------
// Operational forecast baseline (care-operations-forecast.v1)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DailyOpsHistory {
    pub date: NaiveDate,
    pub arrivals: i32,
    pub admissions: i32,
    pub discharges: i32,
    pub placement_requests: i32,
    pub theatre_cases: i32,
    pub transport_requests: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpsCalendarEffect {
    pub date: NaiveDate,
    pub name: String,
    pub kind: String,
    pub demand_multiplier: f64,
    pub capacity_multiplier: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpsForecastInputs {
    pub history: Vec<DailyOpsHistory>,
    pub calendar: Vec<OpsCalendarEffect>,
    pub horizon_start: NaiveDate,
    pub horizon_days: usize,
    pub total_positions: i32,
    pub occupied_positions: i32,
    pub blocked_positions: i32,
    /// Discharges already in preparation/ready (confirmed signal).
    pub discharge_pipeline: i32,
    /// Confirmed theatre cases per horizon day.
    pub scheduled_theatre_cases: BTreeMap<NaiveDate, i32>,
    /// Configured staffing gaps per horizon day (deterministic coverage).
    pub staffing_gaps: BTreeMap<NaiveDate, i32>,
    pub theatre_sessions_capacity: BTreeMap<NaiveDate, i32>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpsDayForecast {
    pub date: NaiveDate,
    pub expected_arrivals: f64,
    pub arrivals_low: f64,
    pub arrivals_high: f64,
    pub expected_admissions: f64,
    pub expected_discharges: f64,
    pub expected_placement_demand: f64,
    pub expected_occupancy: f64,
    pub occupancy_pressure: f64,
    pub theatre_demand: f64,
    pub theatre_utilisation: Option<f64>,
    pub transport_demand: f64,
    pub staffing_gaps: i32,
    pub factors: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum OpsForecast {
    Ready {
        version: String,
        days: Vec<OpsDayForecast>,
        confidence: f64,
        history_days: usize,
        pressure_dates: Vec<NaiveDate>,
        notes: Vec<String>,
    },
    InsufficientHistory {
        version: String,
        history_days: usize,
        required_days: usize,
        detail: String,
    },
}

fn mean(v: &[f64]) -> f64 {
    if v.is_empty() {
        0.0
    } else {
        v.iter().sum::<f64>() / v.len() as f64
    }
}

fn stddev(v: &[f64], m: f64) -> f64 {
    if v.len() < 2 {
        return 0.0;
    }
    (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt()
}

pub fn forecast_operations(inputs: &OpsForecastInputs) -> OpsForecast {
    let active_days: BTreeSet<NaiveDate> = inputs
        .history
        .iter()
        .filter(|h| h.arrivals > 0 || h.admissions > 0 || h.discharges > 0)
        .map(|h| h.date)
        .collect();
    let history_days = active_days.len();
    if history_days < FORECAST_MIN_HISTORY_DAYS {
        return OpsForecast::InsufficientHistory {
            version: FORECAST_VERSION.into(),
            history_days,
            required_days: FORECAST_MIN_HISTORY_DAYS,
            detail: format!(
                "{history_days} day(s) of operational history available; {FORECAST_MIN_HISTORY_DAYS} required"
            ),
        };
    }
    // Weekday baselines.
    let mut by_weekday: Vec<Vec<&DailyOpsHistory>> = vec![Vec::new(); 7];
    for h in &inputs.history {
        by_weekday[h.date.weekday().num_days_from_monday() as usize].push(h);
    }
    let all_arrivals: Vec<f64> = inputs.history.iter().map(|h| h.arrivals as f64).collect();
    let overall_arrivals = mean(&all_arrivals);
    let overall_sd = stddev(&all_arrivals, overall_arrivals);
    let admit_ratio = {
        let a: f64 = inputs.history.iter().map(|h| h.admissions as f64).sum();
        let r: f64 = all_arrivals.iter().sum();
        if r > 0.0 {
            (a / r).clamp(0.0, 1.0)
        } else {
            0.0
        }
    };
    let discharge_mean = mean(
        &inputs
            .history
            .iter()
            .map(|h| h.discharges as f64)
            .collect::<Vec<_>>(),
    );
    let placement_ratio = {
        let p: f64 = inputs
            .history
            .iter()
            .map(|h| h.placement_requests as f64)
            .sum();
        let a: f64 = inputs.history.iter().map(|h| h.admissions as f64).sum();
        if a > 0.0 {
            p / a
        } else {
            1.0
        }
    };
    let transport_mean = mean(
        &inputs
            .history
            .iter()
            .map(|h| h.transport_requests as f64)
            .collect::<Vec<_>>(),
    );
    let theatre_mean = mean(
        &inputs
            .history
            .iter()
            .map(|h| h.theatre_cases as f64)
            .collect::<Vec<_>>(),
    );
    // Seasonal factor by month relative to overall mean (seasonal/summer pressure).
    let mut month_sum: BTreeMap<u32, (f64, usize)> = BTreeMap::new();
    for h in &inputs.history {
        let e = month_sum.entry(h.date.month()).or_insert((0.0, 0));
        e.0 += h.arrivals as f64;
        e.1 += 1;
    }
    let seasonal = |month: u32| -> Option<f64> {
        month_sum.get(&month).and_then(|(sum, n)| {
            if *n >= 7 && overall_arrivals > 0.0 {
                Some(((sum / *n as f64) / overall_arrivals).clamp(0.5, 2.0))
            } else {
                None
            }
        })
    };
    let mut days = Vec::new();
    let mut pressure_dates = Vec::new();
    let mut occupancy = inputs.occupied_positions as f64;
    let usable = (inputs.total_positions - inputs.blocked_positions).max(0) as f64;
    let mut pipeline = inputs.discharge_pipeline as f64;
    for i in 0..inputs.horizon_days {
        let date = inputs.horizon_start + Duration::days(i as i64);
        let wd = date.weekday().num_days_from_monday() as usize;
        let wd_hist: Vec<f64> = by_weekday[wd].iter().map(|h| h.arrivals as f64).collect();
        let base = if wd_hist.len() >= 2 {
            mean(&wd_hist)
        } else {
            overall_arrivals
        };
        let mut factors = Vec::new();
        let mut demand_mult = 1.0;
        let mut capacity_mult = 1.0;
        if let Some(s) = seasonal(date.month()) {
            if (s - 1.0).abs() > 0.05 {
                demand_mult *= s;
                factors.push(format!("seasonal_{:+.0}%", (s - 1.0) * 100.0));
            }
        }
        for c in inputs.calendar.iter().filter(|c| c.date == date) {
            demand_mult *= c.demand_multiplier;
            capacity_mult *= c.capacity_multiplier;
            factors.push(format!("{}:{}", c.kind, c.name));
        }
        let expected_arrivals = base * demand_mult;
        let sd = if wd_hist.len() >= 2 {
            stddev(&wd_hist, base)
        } else {
            overall_sd
        };
        let expected_admissions = expected_arrivals * admit_ratio;
        let expected_discharges = if i == 0 {
            (discharge_mean + pipeline * 0.5).max(discharge_mean)
        } else {
            discharge_mean
        };
        pipeline *= 0.5;
        occupancy = (occupancy + expected_admissions - expected_discharges).max(0.0);
        let effective_capacity = usable * capacity_mult;
        let occupancy_pressure = if effective_capacity > 0.0 {
            occupancy / effective_capacity
        } else {
            1.0
        };
        if occupancy_pressure >= 0.9 {
            pressure_dates.push(date);
            factors.push("occupancy_pressure".into());
        }
        let scheduled = *inputs.scheduled_theatre_cases.get(&date).unwrap_or(&0) as f64;
        let theatre_demand = scheduled.max(theatre_mean * demand_mult.min(1.2));
        let theatre_utilisation = inputs
            .theatre_sessions_capacity
            .get(&date)
            .filter(|c| **c > 0)
            .map(|c| (scheduled / *c as f64).clamp(0.0, 2.0));
        let gaps = *inputs.staffing_gaps.get(&date).unwrap_or(&0);
        if gaps > 0 {
            factors.push(format!("staffing_gaps:{gaps}"));
        }
        days.push(OpsDayForecast {
            date,
            expected_arrivals: round1(expected_arrivals),
            arrivals_low: round1((expected_arrivals - 1.28 * sd).max(0.0)),
            arrivals_high: round1(expected_arrivals + 1.28 * sd),
            expected_admissions: round1(expected_admissions),
            expected_discharges: round1(expected_discharges),
            expected_placement_demand: round1(expected_admissions * placement_ratio),
            expected_occupancy: round1(occupancy),
            occupancy_pressure: round3(occupancy_pressure),
            theatre_demand: round1(theatre_demand),
            theatre_utilisation: theatre_utilisation.map(round3),
            transport_demand: round1(transport_mean * demand_mult),
            staffing_gaps: gaps,
            factors,
        });
    }
    // Confidence grows with history and falls with dispersion; capped so the
    // output never implies precision the evidence cannot support.
    let dispersion = if overall_arrivals > 0.0 {
        (overall_sd / overall_arrivals).min(1.0)
    } else {
        1.0
    };
    let confidence =
        (0.35 + 0.4 * (history_days as f64 / 56.0).min(1.0) - 0.3 * dispersion).clamp(0.1, 0.8);
    let mut notes = vec![
        "aggregate operational baseline; no patient-level prediction".to_string(),
        format!("{history_days} day(s) of synthetic operational history"),
    ];
    if inputs.horizon_days > 1 {
        notes.push("occupancy carries forward day to day; uncertainty widens with horizon".into());
    }
    OpsForecast::Ready {
        version: FORECAST_VERSION.into(),
        days,
        confidence: round3(confidence),
        history_days,
        pressure_dates,
        notes,
    }
}

fn round1(v: f64) -> f64 {
    (v * 10.0).round() / 10.0
}

fn round3(v: f64) -> f64 {
    (v * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn position(id: u128) -> PositionFact {
        PositionFact {
            location_id: Uuid::from_u128(id),
            facility_id: Uuid::from_u128(1),
            unit_id: Some(Uuid::from_u128(100)),
            room_id: Some(Uuid::from_u128(200)),
            unit_name: "Medical ward".into(),
            room_name: Some("Room 1".into()),
            position_name: format!("Bed {id}"),
            type_code: "bed".into(),
            state: PositionState::Available,
            active: true,
            hierarchy_open: true,
            effective_from: None,
            effective_to: None,
            specialty_codes: vec!["internal_medicine".into()],
            service_line_codes: vec!["medical".into()],
            age_group_codes: vec!["adult".into()],
            isolation_codes: vec![],
            accessibility_codes: vec![],
            equipment_codes: vec![],
            room_policy: "mixed".into(),
            room_occupant_sexes: vec![],
            room_occupant_count: 0,
            room_capacity: 2,
            live_placement: false,
            blocking_period: None,
            staffed_professions: vec!["nurse".into(), "physician".into()],
            unit_occupancy: 0.5,
            cleaning_minutes: 30,
        }
    }

    fn facts(positions: Vec<PositionFact>, req: PlacementRequirements) -> PlacementFacts {
        PlacementFacts {
            tenant_id: Uuid::from_u128(9),
            facility_id: Uuid::from_u128(1),
            facility_closed: false,
            local_date: NaiveDate::from_ymd_opt(2026, 10, 4).unwrap(),
            requirements: req,
            positions,
            current_location_id: None,
        }
    }

    #[test]
    fn episode_machine_guards_paths() {
        use EpisodeStatus::*;
        use EpisodeTransition::*;
        assert_eq!(Planned.apply(Activate), Ok(Active));
        assert_eq!(
            Active.apply(StartDischargePreparation),
            Ok(DischargePreparation)
        );
        assert_eq!(
            DischargePreparation.apply(MarkDischargeReady),
            Ok(DischargeReady)
        );
        assert_eq!(DischargeReady.apply(Complete), Ok(Completed));
        assert!(Planned.apply(MarkDischargeReady).is_err());
        assert!(Completed.apply(Activate).is_err());
        assert!(Cancelled.apply(Complete).is_err());
        assert!(Planned.apply(Complete).is_err());
        for s in EpisodeStatus::ALL {
            assert_eq!(EpisodeStatus::parse(s.as_str()), Some(s));
        }
    }

    #[test]
    fn placement_request_and_placement_machines() {
        use PlacementRequestStatus as R;
        use PlacementRequestTransition as T;
        assert_eq!(
            R::Requested.apply(T::CandidatesGenerated),
            Ok(R::CandidatesReady)
        );
        assert_eq!(R::CandidatesReady.apply(T::Hold), Ok(R::Held));
        assert_eq!(R::Held.apply(T::ReleaseHold), Ok(R::CandidatesReady));
        assert_eq!(R::Held.apply(T::Confirm), Ok(R::Placed));
        assert_eq!(R::CandidatesReady.apply(T::Confirm), Ok(R::Placed));
        assert!(R::Requested.apply(T::Hold).is_err());
        assert!(R::Placed.apply(T::Cancel).is_err());
        assert_eq!(
            PlacementStatus::Held.apply(PlacementTransition::Confirm),
            Ok(PlacementStatus::Occupied)
        );
        assert_eq!(
            PlacementStatus::Occupied.apply(PlacementTransition::Release),
            Ok(PlacementStatus::Released)
        );
        assert!(PlacementStatus::Occupied
            .apply(PlacementTransition::Expire)
            .is_err());
        assert!(PlacementStatus::Released
            .apply(PlacementTransition::Confirm)
            .is_err());
    }

    #[test]
    fn position_state_graph() {
        use PositionState::*;
        assert!(Available.can_transition(Held));
        assert!(Held.can_transition(Occupied));
        assert!(Occupied.can_transition(Cleaning));
        assert!(Cleaning.can_transition(Available));
        assert!(!Occupied.can_transition(Held));
        assert!(!Cleaning.can_transition(Occupied));
        assert!(!OutOfService.can_transition(Occupied));
        assert!(Available.accepts_placement() && !Cleaning.accepts_placement());
    }

    #[test]
    fn task_and_case_machines() {
        assert_eq!(
            TaskStatus::Open.apply(TaskTransition::HandOff),
            Ok(TaskStatus::HandedOff)
        );
        assert_eq!(
            TaskStatus::HandedOff.apply(TaskTransition::Accept),
            Ok(TaskStatus::InProgress)
        );
        assert!(TaskStatus::Completed.apply(TaskTransition::Start).is_err());
        assert!(TaskStatus::Blocked.apply(TaskTransition::Complete).is_err());
        let deps = vec![
            (Uuid::from_u128(1), TaskStatus::Completed),
            (Uuid::from_u128(2), TaskStatus::Open),
            (Uuid::from_u128(3), TaskStatus::Cancelled),
        ];
        assert_eq!(unmet_dependencies(&deps), vec![Uuid::from_u128(2)]);

        assert_eq!(
            CaseStatus::Requested.apply(CaseTransition::MarkReady),
            Ok(CaseStatus::ReadyForScheduling)
        );
        assert_eq!(
            CaseStatus::ReadyForScheduling.apply(CaseTransition::Schedule),
            Ok(CaseStatus::Scheduled)
        );
        assert_eq!(
            CaseStatus::Scheduled.apply(CaseTransition::Delay),
            Ok(CaseStatus::Delayed)
        );
        assert_eq!(
            CaseStatus::Delayed.apply(CaseTransition::Schedule),
            Ok(CaseStatus::Scheduled)
        );
        assert!(CaseStatus::Requested
            .apply(CaseTransition::Schedule)
            .is_err());
        assert!(CaseStatus::Completed.apply(CaseTransition::Cancel).is_err());
        assert!(CaseStatus::InTheatre.apply(CaseTransition::Cancel).is_err());
    }

    #[test]
    fn escalation_defaults_scale_with_priority_and_fire_at_due() {
        assert_eq!(default_escalation_minutes("boarding", "routine"), Some(240));
        assert_eq!(
            default_escalation_minutes("boarding", "emergency"),
            Some(60)
        );
        assert_eq!(
            default_escalation_minutes("waiting_triage", "urgent"),
            Some(8)
        );
        assert_eq!(default_escalation_minutes("unknown", "routine"), None);
        let due = Utc.with_ymd_and_hms(2026, 3, 29, 1, 30, 0).unwrap();
        assert!(!escalation_due(due, due - Duration::seconds(1)));
        assert!(escalation_due(due, due));
        assert!(EscalationStatus::Armed.can_transition(EscalationStatus::Escalated));
        assert!(!EscalationStatus::Resolved.can_transition(EscalationStatus::Escalated));
    }

    #[test]
    fn matcher_rejects_every_hard_constraint() {
        let mut closed = position(1);
        closed.hierarchy_open = false;
        let mut occupied = position(2);
        occupied.state = PositionState::Occupied;
        occupied.live_placement = true;
        let mut cleaning = position(3);
        cleaning.state = PositionState::Cleaning;
        let mut paediatric = position(4);
        paediatric.age_group_codes = vec!["paediatric".into()];
        let mut no_isolation = position(5);
        no_isolation.isolation_codes = vec![];
        let mut other_facility = position(6);
        other_facility.facility_id = Uuid::from_u128(77);
        let mut expired = position(7);
        expired.effective_to = Some(NaiveDate::from_ymd_opt(2026, 1, 1).unwrap());
        let mut unstaffed = position(8);
        unstaffed.staffed_professions = vec!["physician".into()];
        let mut maintenance = position(9);
        maintenance.blocking_period = Some("maintenance".into());
        let mut ok = position(10);
        ok.isolation_codes = vec!["airborne".into()];
        let req = PlacementRequirements {
            age_group_code: Some("adult".into()),
            isolation_codes: vec!["airborne".into()],
            required_staffed_professions: vec!["nurse".into()],
            ..Default::default()
        };
        let out = match_placements(&facts(
            vec![
                closed,
                occupied,
                cleaning,
                paediatric,
                no_isolation,
                other_facility,
                expired,
                unstaffed,
                maintenance,
                ok,
            ],
            req,
        ));
        assert_eq!(out.feasible.len(), 1);
        assert_eq!(out.feasible[0].location_id, Uuid::from_u128(10));
        assert_eq!(out.feasible[0].rank, Some(1));
        let codes: BTreeSet<String> = out.rejected_by_code.keys().cloned().collect();
        for c in [
            "location_closed",
            "overlapping_placement",
            "state_occupied",
            "state_cleaning",
            "age_group_capability",
            "isolation_capability",
            "facility_scope",
            "no_longer_effective",
            "staffed_capability",
            "period_maintenance",
        ] {
            assert!(codes.contains(c), "missing {c} in {codes:?}");
        }
        assert_eq!(out.matcher_version, PLACEMENT_MATCHER_VERSION);
        assert!(out.facts_hash.starts_with("sha256:"));
    }

    #[test]
    fn matcher_room_policy_and_accessibility_and_equipment() {
        let mut mixed_room = position(1);
        mixed_room.room_occupant_sexes = vec!["male".into()];
        mixed_room.room_occupant_count = 1;
        let mut same_sex_room = position(2);
        same_sex_room.room_policy = "same_sex".into();
        same_sex_room.room_occupant_sexes = vec!["female".into()];
        same_sex_room.room_occupant_count = 1;
        let mut single = position(3);
        single.room_capacity = 1;
        single.room_policy = "single".into();
        let mut no_ramp = position(4);
        no_ramp.room_capacity = 1;
        let req = PlacementRequirements {
            room_policy: Some("single".into()),
            sex: Some("female".into()),
            accessibility_codes: vec!["wheelchair".into()],
            equipment_codes: vec!["telemetry".into()],
            ..Default::default()
        };
        let mut good = single.clone();
        good.location_id = Uuid::from_u128(5);
        good.accessibility_codes = vec!["wheelchair".into()];
        good.equipment_codes = vec!["telemetry".into()];
        let out = match_placements(&facts(
            vec![mixed_room, same_sex_room, single, no_ramp, good],
            req,
        ));
        assert_eq!(out.feasible.len(), 1);
        assert_eq!(out.feasible[0].location_id, Uuid::from_u128(5));
        assert!(out.rejected_by_code.contains_key("room_policy_single"));
        assert!(out
            .rejected_by_code
            .contains_key("accessibility_capability"));
        assert!(out.rejected_by_code.contains_key("equipment_capability"));

        // Same-sex room with a different-sex occupant rejects a male request.
        let mut ss = position(6);
        ss.room_policy = "same_sex".into();
        ss.room_occupant_sexes = vec!["female".into()];
        ss.room_occupant_count = 1;
        let out = match_placements(&facts(
            vec![ss],
            PlacementRequirements {
                sex: Some("male".into()),
                ..Default::default()
            },
        ));
        assert!(out.feasible.is_empty());
        assert!(out.rejected_by_code.contains_key("room_policy_same_sex"));
    }

    #[test]
    fn matcher_ranks_deterministically_and_hash_is_stable() {
        let mut busy = position(1);
        busy.unit_occupancy = 0.95;
        let mut calm = position(2);
        calm.unit_occupancy = 0.3;
        let mut preferred = position(3);
        preferred.unit_id = Some(Uuid::from_u128(101));
        preferred.unit_occupancy = 0.95;
        let req = PlacementRequirements {
            specialty_codes: vec!["internal_medicine".into()],
            preferred_unit_ids: vec![Uuid::from_u128(101)],
            ..Default::default()
        };
        let f = facts(vec![busy, calm, preferred], req);
        let a = match_placements(&f);
        let b = match_placements(&f);
        assert_eq!(a, b);
        assert_eq!(a.feasible[0].location_id, Uuid::from_u128(3));
        assert_eq!(a.feasible[1].location_id, Uuid::from_u128(2));
        assert!(a.feasible.iter().all(|c| !c.factors.is_empty()));
        let total: f64 = a.feasible[0].factors.iter().map(|x| x.points).sum();
        assert!((total - a.feasible[0].score.unwrap()).abs() < 0.001);
    }

    #[test]
    fn facility_closed_rejects_everything() {
        let mut f = facts(
            vec![position(1), position(2)],
            PlacementRequirements::default(),
        );
        f.facility_closed = true;
        let out = match_placements(&f);
        assert!(out.feasible.is_empty());
        assert_eq!(out.rejected_by_code.get("facility_closed"), Some(&2));
    }

    #[test]
    fn hold_expiry_is_bounded() {
        let now = Utc.with_ymd_and_hms(2026, 10, 4, 10, 0, 0).unwrap();
        assert_eq!(
            hold_expiry(now, None),
            now + Duration::minutes(DEFAULT_HOLD_MINUTES)
        );
        assert_eq!(
            hold_expiry(now, Some(10_000)),
            now + Duration::minutes(MAX_HOLD_MINUTES)
        );
        assert_eq!(hold_expiry(now, Some(0)), now + Duration::minutes(1));
    }

    fn theatre_facts() -> TheatreCaseFacts {
        let start = Utc.with_ymd_and_hms(2026, 10, 5, 8, 0, 0).unwrap();
        TheatreCaseFacts {
            case_id: Uuid::from_u128(1),
            case_version: 3,
            expected_case_version: 3,
            theatre_location_id: Uuid::from_u128(50),
            theatre_version: 1,
            expected_theatre_version: 1,
            theatre_open: true,
            facility_closed: false,
            proposed: Interval {
                starts_at: start,
                ends_at: start + Duration::minutes(90),
            },
            turnaround_minutes: 30,
            minimum_turnaround_minutes: 20,
            session: Some(Interval {
                starts_at: start - Duration::hours(1),
                ends_at: start + Duration::hours(6),
            }),
            theatre_cases: vec![],
            team_user_ids: vec![Uuid::from_u128(7)],
            team_commitments: vec![],
            required_equipment_codes: vec!["laparoscope".into()],
            theatre_equipment_codes: vec!["laparoscope".into()],
            prerequisites: vec![Prerequisite {
                code: "anaesthetic_review".into(),
                status: "met".into(),
                evidence_ref: None,
            }],
            required_results_complete: true,
            destination_required: true,
            destination_feasible_count: 2,
        }
    }

    #[test]
    fn theatre_engine_detects_each_conflict() {
        let ok = check_theatre(&theatre_facts());
        assert!(ok.feasible, "{:?}", ok.conflicts);
        let mut f = theatre_facts();
        f.expected_case_version = 4;
        f.theatre_open = false;
        f.facility_closed = true;
        f.turnaround_minutes = 5;
        f.theatre_cases = vec![(
            Uuid::from_u128(2),
            Interval {
                starts_at: f.proposed.ends_at - Duration::minutes(10),
                ends_at: f.proposed.ends_at + Duration::hours(1),
            },
            30,
        )];
        f.team_commitments = vec![(Uuid::from_u128(7), f.proposed.clone())];
        f.theatre_equipment_codes = vec![];
        f.prerequisites[0].status = "pending".into();
        f.required_results_complete = false;
        f.destination_feasible_count = 0;
        f.session = Some(Interval {
            starts_at: f.proposed.starts_at + Duration::minutes(5),
            ends_at: f.proposed.ends_at,
        });
        let out = check_theatre(&f);
        assert!(!out.feasible);
        let codes: BTreeSet<&str> = out.conflicts.iter().map(|c| c.code.as_str()).collect();
        for c in [
            "stale_case_version",
            "theatre_unavailable",
            "facility_closed",
            "unrealistic_turnaround",
            "theatre_overlap",
            "team_overlap",
            "missing_equipment",
            "missing_prerequisite",
            "missing_required_results",
            "no_post_procedure_placement",
            "outside_session",
        ] {
            assert!(codes.contains(c), "missing {c}");
        }
        assert_eq!(out.engine_version, THEATRE_CONFLICT_VERSION);
    }

    #[test]
    fn theatre_overlap_respects_turnaround_padding() {
        let mut f = theatre_facts();
        // Next case starts exactly 30 min after ours ends: OK with 30 min turnaround.
        f.theatre_cases = vec![(
            Uuid::from_u128(2),
            Interval {
                starts_at: f.proposed.ends_at + Duration::minutes(30),
                ends_at: f.proposed.ends_at + Duration::hours(2),
            },
            30,
        )];
        assert!(check_theatre(&f).feasible);
        f.theatre_cases[0].1.starts_at = f.proposed.ends_at + Duration::minutes(29);
        assert!(!check_theatre(&f).feasible);
    }

    #[test]
    fn waived_and_not_required_prerequisites_do_not_block_scheduling() {
        let mut f = theatre_facts();
        f.prerequisites = vec![
            Prerequisite {
                code: "anaesthetic_review".into(),
                status: "waived".into(),
                evidence_ref: Some("consultant waiver".into()),
            },
            Prerequisite {
                code: "blood_products".into(),
                status: "not_required".into(),
                evidence_ref: None,
            },
        ];
        let r = check_theatre(&f);
        assert!(
            r.conflicts.iter().all(|c| c.code != "missing_prerequisite"),
            "{r:?}"
        );
        f.prerequisites[0].status = "pending".into();
        assert!(check_theatre(&f)
            .conflicts
            .iter()
            .any(|c| c.code == "missing_prerequisite" && c.blocking));
    }

    fn staff(id: u128, profession: &str, caps: &[&str], role: &str) -> StaffFact {
        StaffFact {
            assignment_id: Uuid::from_u128(id),
            user_id: Some(Uuid::from_u128(id)),
            profession_code: profession.into(),
            capability_codes: caps.iter().map(|c| c.to_string()).collect(),
            specialty_codes: vec![],
            role_in_shift: role.into(),
            status: "confirmed".into(),
            absent: false,
        }
    }

    #[test]
    fn coverage_counts_distinct_people_not_assignment_rows() {
        let reqs = vec![CoverageRequirement {
            profession_code: "nurse".into(),
            capability_codes: vec![],
            specialty_codes: vec![],
            minimum_count: 2,
        }];
        let mut duplicate = staff(1, "nurse", &[], "assigned");
        duplicate.assignment_id = Uuid::from_u128(99);
        let r = evaluate_coverage(&reqs, &[staff(1, "nurse", &[], "assigned"), duplicate]);
        assert!(!r.covered, "{r:?}");
        assert_eq!(r.gaps[0].present, 1);
        assert_eq!(r.gaps[0].shortfall, 1);
        let r = evaluate_coverage(
            &reqs,
            &[
                staff(1, "nurse", &[], "assigned"),
                staff(2, "nurse", &[], "assigned"),
            ],
        );
        assert!(r.covered, "{r:?}");
        let mut unnamed_a = staff(1, "nurse", &[], "assigned");
        unnamed_a.user_id = None;
        let mut unnamed_b = staff(2, "nurse", &[], "assigned");
        unnamed_b.user_id = None;
        assert!(evaluate_coverage(&reqs, &[unnamed_a, unnamed_b]).covered);
    }

    #[test]
    fn coverage_gaps_respect_profession_and_capability() {
        let reqs = vec![
            CoverageRequirement {
                profession_code: "nurse".into(),
                capability_codes: vec!["paediatric".into()],
                specialty_codes: vec![],
                minimum_count: 2,
            },
            CoverageRequirement {
                profession_code: "physiotherapist".into(),
                capability_codes: vec![],
                specialty_codes: vec![],
                minimum_count: 1,
            },
            CoverageRequirement {
                profession_code: "midwife".into(),
                capability_codes: vec![],
                specialty_codes: vec![],
                minimum_count: 1,
            },
        ];
        let mut absent = staff(4, "midwife", &[], "assigned");
        absent.absent = true;
        let staff_list = vec![
            staff(1, "nurse", &["paediatric"], "assigned"),
            staff(2, "nurse", &[], "assigned"),
            staff(3, "physiotherapist", &[], "assigned"),
            absent,
            staff(5, "nurse", &["paediatric"], "on_call"),
            staff(6, "nurse", &["paediatric"], "float"),
        ];
        let r = evaluate_coverage(&reqs, &staff_list);
        assert!(!r.covered);
        assert_eq!(r.gaps.len(), 2);
        let nurse_gap = r
            .gaps
            .iter()
            .find(|g| g.profession_code == "nurse")
            .unwrap();
        assert_eq!(
            (nurse_gap.present, nurse_gap.on_call, nurse_gap.shortfall),
            (1, 1, 1)
        );
        let midwife_gap = r
            .gaps
            .iter()
            .find(|g| g.profession_code == "midwife")
            .unwrap();
        assert_eq!(midwife_gap.present, 0);
        let candidates = reallocation_candidates(nurse_gap, &staff_list);
        // Only capability-matching float/on-call nurses, float first; the
        // unqualified nurse is never suggested.
        assert_eq!(
            candidates
                .iter()
                .map(|s| s.assignment_id)
                .collect::<Vec<_>>(),
            vec![Uuid::from_u128(6), Uuid::from_u128(5)]
        );
        assert!(reallocation_candidates(midwife_gap, &staff_list).is_empty());
    }

    fn history(days: usize, base: i32) -> Vec<DailyOpsHistory> {
        let start = NaiveDate::from_ymd_opt(2026, 8, 1).unwrap();
        (0..days)
            .map(|i| {
                let date = start + Duration::days(i as i64);
                let weekend = date.weekday().num_days_from_monday() >= 5;
                DailyOpsHistory {
                    date,
                    arrivals: if weekend { base / 2 } else { base },
                    admissions: base / 4,
                    discharges: base / 4,
                    placement_requests: base / 4,
                    theatre_cases: 3,
                    transport_requests: 2,
                }
            })
            .collect()
    }

    fn inputs(h: Vec<DailyOpsHistory>) -> OpsForecastInputs {
        OpsForecastInputs {
            history: h,
            calendar: vec![],
            horizon_start: NaiveDate::from_ymd_opt(2026, 10, 5).unwrap(),
            horizon_days: 7,
            total_positions: 40,
            occupied_positions: 30,
            blocked_positions: 2,
            discharge_pipeline: 4,
            scheduled_theatre_cases: BTreeMap::new(),
            staffing_gaps: BTreeMap::new(),
            theatre_sessions_capacity: BTreeMap::new(),
        }
    }

    #[test]
    fn forecast_requires_history() {
        match forecast_operations(&inputs(history(10, 20))) {
            OpsForecast::InsufficientHistory {
                history_days,
                required_days,
                ..
            } => {
                assert_eq!(history_days, 10);
                assert_eq!(required_days, FORECAST_MIN_HISTORY_DAYS);
            }
            other => panic!("expected insufficient history, got {other:?}"),
        }
    }

    #[test]
    fn forecast_is_deterministic_bounded_and_reflects_calendar_and_season() {
        let mut inp = inputs(history(60, 20));
        // Summer pressure: August weekdays higher than September.
        for h in inp.history.iter_mut() {
            if h.date.month() == 8 {
                h.arrivals = (h.arrivals as f64 * 1.5) as i32;
            }
        }
        inp.calendar.push(OpsCalendarEffect {
            date: inp.horizon_start,
            name: "closure".into(),
            kind: "closure".into(),
            demand_multiplier: 1.0,
            capacity_multiplier: 0.0,
        });
        inp.staffing_gaps
            .insert(inp.horizon_start + Duration::days(1), 2);
        let a = forecast_operations(&inp);
        let b = forecast_operations(&inp);
        assert_eq!(a, b);
        match a {
            OpsForecast::Ready {
                days,
                confidence,
                pressure_dates,
                ..
            } => {
                assert_eq!(days.len(), 7);
                assert!(confidence > 0.0 && confidence <= 0.8);
                for d in &days {
                    assert!(
                        d.arrivals_low <= d.expected_arrivals
                            && d.expected_arrivals <= d.arrivals_high
                    );
                    assert!(d.expected_occupancy >= 0.0);
                }
                // Closed facility day is full pressure.
                assert!(days[0].occupancy_pressure >= 1.0);
                assert!(pressure_dates.contains(&days[0].date));
                assert!(days[0].factors.iter().any(|f| f.starts_with("closure:")));
                assert_eq!(days[1].staffing_gaps, 2);
                // Weekend baselines are lower than weekday ones.
                let weekday = days
                    .iter()
                    .find(|d| d.date.weekday().num_days_from_monday() < 5)
                    .unwrap();
                let weekend = days
                    .iter()
                    .find(|d| d.date.weekday().num_days_from_monday() >= 5)
                    .unwrap();
                assert!(weekend.expected_arrivals < weekday.expected_arrivals);
            }
            other => panic!("expected ready, got {other:?}"),
        }
    }

    #[test]
    fn canonical_hash_ignores_key_order() {
        let a = serde_json::json!({"b": 1, "a": [1, {"y": 2, "x": 1}]});
        let b = serde_json::json!({"a": [1, {"x": 1, "y": 2}], "b": 1});
        assert_eq!(hash_facts(&a), hash_facts(&b));
        assert_ne!(hash_facts(&a), hash_facts(&serde_json::json!({"a": 1})));
    }
}
