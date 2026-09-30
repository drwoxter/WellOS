//! Cancellation recovery: deterministic eligibility and fairness ordering of
//! consented waitlist entries for a freed slot. dMind's
//! `cancellation-recovery.v1` may only reorder within the floors computed
//! here; [`enforce_floors`] rejects any AI ordering that demotes a
//! higher-urgency or longer-waiting patient below what fairness allows.

use crate::access::{Urgency, WeeklyWindow};
use crate::matcher::{local_to_utc, Interval};
use chrono::{DateTime, Datelike, Duration, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const RECOVERY_VERSION: &str = "cancellation-recovery-eligibility.v1";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WaitlistFact {
    pub entry_id: Uuid,
    pub patient_id: Uuid,
    pub urgency: Urgency,
    pub joined_at: DateTime<Utc>,
    pub facility_ids: Vec<Uuid>,
    pub modality_codes: Vec<String>,
    pub acceptable_windows: Vec<WeeklyWindow>,
    pub earliest: Option<DateTime<Utc>>,
    pub latest: Option<DateTime<Utc>>,
    pub min_notice_hours: i32,
    pub time_zone: String,
    pub busy_intervals: Vec<Interval>,
    pub offers_declined: i32,
    /// Only patients who consented to waitlist offers are ever passed in;
    /// this mirrors the persisted consent for defence in depth.
    pub consented: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FreedSlot {
    pub facility_id: Uuid,
    pub modality_code: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EligibleEntry {
    pub entry_id: Uuid,
    pub patient_id: Uuid,
    pub urgency: Urgency,
    pub waited_hours: i64,
    /// Deterministic rank, 1 = first to be offered.
    pub rank: usize,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EligibilityOutput {
    pub version: String,
    pub eligible: Vec<EligibleEntry>,
    pub excluded: Vec<(Uuid, String)>,
}

/// Compute eligibility and the fairness order: urgency first, then longest
/// wait, then fewest declined offers, then entry id for stability.
pub fn eligible_entries(
    now: DateTime<Utc>,
    slot: &FreedSlot,
    entries: &[WaitlistFact],
) -> EligibilityOutput {
    let mut eligible = Vec::new();
    let mut excluded = Vec::new();
    let appt = Interval::new(slot.starts_at, slot.ends_at);
    for e in entries {
        if !e.consented {
            excluded.push((e.entry_id, "no_consent".into()));
            continue;
        }
        if !e.facility_ids.is_empty() && !e.facility_ids.contains(&slot.facility_id) {
            excluded.push((e.entry_id, "facility_not_acceptable".into()));
            continue;
        }
        if !e.modality_codes.is_empty() && !e.modality_codes.contains(&slot.modality_code) {
            excluded.push((e.entry_id, "modality_not_acceptable".into()));
            continue;
        }
        if slot.starts_at - now < Duration::hours(e.min_notice_hours.max(0) as i64) {
            excluded.push((e.entry_id, "insufficient_notice".into()));
            continue;
        }
        if e.earliest.is_some_and(|x| slot.starts_at < x)
            || e.latest.is_some_and(|x| slot.ends_at > x)
        {
            excluded.push((e.entry_id, "outside_acceptable_dates".into()));
            continue;
        }
        if e.busy_intervals.iter().any(|b| b.overlaps(&appt)) {
            excluded.push((e.entry_id, "calendar_conflict".into()));
            continue;
        }
        if !e.acceptable_windows.is_empty() {
            let tz: Tz = e.time_zone.parse().unwrap_or(chrono_tz::UTC);
            let date = slot.starts_at.with_timezone(&tz).date_naive();
            let weekday = date.weekday().number_from_monday() as u8;
            let inside = e.acceptable_windows.iter().any(|w| {
                w.weekday == weekday
                    && Interval::new(
                        local_to_utc(tz, date, w.start),
                        local_to_utc(tz, date, w.end),
                    )
                    .contains(&appt)
            });
            if !inside {
                excluded.push((e.entry_id, "outside_acceptable_windows".into()));
                continue;
            }
        }
        let waited_hours = (now - e.joined_at).num_hours().max(0);
        let mut reasons = vec![format!("urgency:{}", e.urgency.as_str())];
        reasons.push(format!("waited_hours:{waited_hours}"));
        eligible.push(EligibleEntry {
            entry_id: e.entry_id,
            patient_id: e.patient_id,
            urgency: e.urgency,
            waited_hours,
            rank: 0,
            reasons,
        });
    }
    let declined = |id: Uuid| {
        entries
            .iter()
            .find(|e| e.entry_id == id)
            .map(|e| e.offers_declined)
            .unwrap_or(0)
    };
    eligible.sort_by(|a, b| {
        b.urgency
            .rank()
            .cmp(&a.urgency.rank())
            .then(b.waited_hours.cmp(&a.waited_hours))
            .then(declined(a.entry_id).cmp(&declined(b.entry_id)))
            .then(a.entry_id.cmp(&b.entry_id))
    });
    for (i, e) in eligible.iter_mut().enumerate() {
        e.rank = i + 1;
    }
    EligibilityOutput {
        version: RECOVERY_VERSION.to_string(),
        eligible,
        excluded,
    }
}

/// Validate an AI-proposed ordering against the deterministic floors:
/// it must be a permutation of the eligible ids, no patient may be placed
/// below one of strictly lower urgency, and among equal urgency a patient
/// may only be demoted below someone who has waited at least
/// `max_wait_demotion_hours` less than them (i.e. AI may break near-ties on
/// convenience but cannot skip long waiters). Returns the accepted ordering
/// or the reason it was refused.
pub fn enforce_floors(
    deterministic: &[EligibleEntry],
    proposed: &[Uuid],
    max_wait_demotion_hours: i64,
) -> Result<Vec<Uuid>, String> {
    if proposed.len() != deterministic.len() {
        return Err("proposed ordering must contain every eligible entry exactly once".into());
    }
    let mut seen = std::collections::BTreeSet::new();
    for id in proposed {
        if !deterministic.iter().any(|e| e.entry_id == *id) {
            return Err(format!("unknown entry {id} in proposed ordering"));
        }
        if !seen.insert(*id) {
            return Err(format!("entry {id} repeated in proposed ordering"));
        }
    }
    let lookup = |id: Uuid| deterministic.iter().find(|e| e.entry_id == id).unwrap();
    for (i, a) in proposed.iter().enumerate() {
        let ea = lookup(*a);
        for b in &proposed[i + 1..] {
            let eb = lookup(*b);
            if eb.urgency.rank() > ea.urgency.rank() {
                return Err(format!(
                    "entry {b} ({}) cannot rank below {a} ({})",
                    eb.urgency.as_str(),
                    ea.urgency.as_str()
                ));
            }
            if eb.urgency == ea.urgency
                && eb.waited_hours - ea.waited_hours > max_wait_demotion_hours
            {
                return Err(format!(
                    "entry {b} waited {}h longer than {a}; fairness floor exceeded",
                    eb.waited_hours - ea.waited_hours
                ));
            }
        }
    }
    Ok(proposed.to_vec())
}

// ---------------------------------------------------------------------------
// Waitlist entry state machine
// ---------------------------------------------------------------------------

/// Lifecycle of one waitlist entry. `Offered` is the transient state while
/// a recovery offer is live; the entry returns to `Active` when the offer
/// is declined, expires or is revoked, and becomes `Fulfilled` on booking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitlistStatus {
    Active,
    Paused,
    Offered,
    Fulfilled,
    Left,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WaitlistTransition {
    Pause,
    Resume,
    Offer,
    OfferClosed,
    Fulfil,
    Leave,
}

impl WaitlistStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Offered => "offered",
            Self::Fulfilled => "fulfilled",
            Self::Left => "left",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "active" => Self::Active,
            "paused" => Self::Paused,
            "offered" => Self::Offered,
            "fulfilled" => Self::Fulfilled,
            "left" => Self::Left,
            _ => return None,
        })
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Fulfilled | Self::Left)
    }

    pub fn apply(self, t: WaitlistTransition) -> Result<Self, String> {
        use WaitlistStatus::*;
        use WaitlistTransition::*;
        Ok(match (self, t) {
            (Active, Pause) => Paused,
            (Paused, Resume) => Active,
            (Active, Offer) => Offered,
            (Offered, OfferClosed) => Active,
            (Offered, Fulfil) => Fulfilled,
            (Active | Paused | Offered, Leave) => Left,
            (from, t) => {
                return Err(format!(
                    "waitlist entry in status {} cannot {:?}",
                    from.as_str(),
                    t
                ))
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn entry(n: u128, urgency: Urgency, joined: &str) -> WaitlistFact {
        WaitlistFact {
            entry_id: Uuid::from_u128(n),
            patient_id: Uuid::from_u128(100 + n),
            urgency,
            joined_at: t(joined),
            facility_ids: vec![],
            modality_codes: vec![],
            acceptable_windows: vec![],
            earliest: None,
            latest: None,
            min_notice_hours: 1,
            time_zone: "Europe/Madrid".into(),
            busy_intervals: vec![],
            offers_declined: 0,
            consented: true,
        }
    }

    fn slot() -> FreedSlot {
        FreedSlot {
            facility_id: Uuid::from_u128(1),
            modality_code: "in_person".into(),
            starts_at: t("2026-06-10T09:00:00Z"),
            ends_at: t("2026-06-10T09:20:00Z"),
        }
    }

    #[test]
    fn urgency_then_waiting_time() {
        let now = t("2026-06-09T09:00:00Z");
        let entries = vec![
            entry(1, Urgency::Routine, "2026-05-01T00:00:00Z"),
            entry(2, Urgency::Priority, "2026-06-01T00:00:00Z"),
            entry(3, Urgency::Routine, "2026-06-05T00:00:00Z"),
        ];
        let out = eligible_entries(now, &slot(), &entries);
        let ids: Vec<u128> = out.eligible.iter().map(|e| e.entry_id.as_u128()).collect();
        assert_eq!(ids, vec![2, 1, 3]);
        assert_eq!(out.eligible[0].rank, 1);
    }

    #[test]
    fn excludes_by_consent_notice_calendar_and_windows() {
        let now = t("2026-06-10T07:00:00Z");
        let mut a = entry(1, Urgency::Routine, "2026-05-01T00:00:00Z");
        a.consented = false;
        let mut b = entry(2, Urgency::Routine, "2026-05-01T00:00:00Z");
        b.min_notice_hours = 3;
        let mut c = entry(3, Urgency::Routine, "2026-05-01T00:00:00Z");
        c.busy_intervals.push(Interval::new(
            t("2026-06-10T09:10:00Z"),
            t("2026-06-10T10:00:00Z"),
        ));
        let mut d = entry(4, Urgency::Routine, "2026-05-01T00:00:00Z");
        d.acceptable_windows.push(WeeklyWindow {
            weekday: 6,
            start: chrono::NaiveTime::from_hms_opt(8, 0, 0).unwrap(),
            end: chrono::NaiveTime::from_hms_opt(12, 0, 0).unwrap(),
        });
        let out = eligible_entries(now, &slot(), &[a, b, c, d]);
        assert!(out.eligible.is_empty(), "{:?}", out);
        let reasons: Vec<&str> = out.excluded.iter().map(|(_, r)| r.as_str()).collect();
        assert_eq!(
            reasons,
            vec![
                "no_consent",
                "insufficient_notice",
                "calendar_conflict",
                "outside_acceptable_windows"
            ]
        );
    }

    #[test]
    fn floors_reject_demotion_of_urgent_or_long_waiters() {
        let now = t("2026-06-09T09:00:00Z");
        let entries = vec![
            entry(1, Urgency::Routine, "2026-05-01T00:00:00Z"),
            entry(2, Urgency::Priority, "2026-06-01T00:00:00Z"),
            entry(3, Urgency::Routine, "2026-06-08T00:00:00Z"),
        ];
        let out = eligible_entries(now, &slot(), &entries);
        let id = |n: u128| Uuid::from_u128(n);
        assert!(enforce_floors(&out.eligible, &[id(1), id(2), id(3)], 48).is_err());
        assert!(enforce_floors(&out.eligible, &[id(2), id(3), id(1)], 48).is_err());
        assert!(enforce_floors(&out.eligible, &[id(2), id(1), id(3)], 48).is_ok());
        assert!(enforce_floors(&out.eligible, &[id(2), id(1)], 48).is_err());
        assert!(enforce_floors(&out.eligible, &[id(2), id(1), id(1)], 48).is_err());
    }

    #[test]
    fn waitlist_state_machine() {
        use WaitlistStatus::*;
        use WaitlistTransition::*;
        assert_eq!(Active.apply(Pause), Ok(Paused));
        assert_eq!(Paused.apply(Resume), Ok(Active));
        assert_eq!(Active.apply(Offer), Ok(Offered));
        assert_eq!(Offered.apply(OfferClosed), Ok(Active));
        assert_eq!(Offered.apply(Fulfil), Ok(Fulfilled));
        assert_eq!(Paused.apply(Leave), Ok(Left));
        assert!(Paused.apply(Offer).is_err());
        assert!(Fulfilled.apply(Resume).is_err());
        assert!(Left.apply(Leave).is_err());
        for s in [Active, Paused, Offered, Fulfilled, Left] {
            assert_eq!(WaitlistStatus::parse(s.as_str()), Some(s));
        }
    }
}
