//! `access-matcher.v1`: the deterministic, provider-independent candidate
//! generator behind "Find the best appointment".
//!
//! Hard constraints decide *feasibility*; only feasible options are ever
//! surfaced or handed to dMind. Soft factors produce a transparent score
//! whose full decomposition is persisted with every candidate. Nothing here
//! performs I/O: the server assembles [`MatchFacts`] from the database and
//! passes them in.
//!
//! No-show history is *supportive only*: it can add reminder suggestions and
//! never lowers a score, denies a slot or reorders against the patient.

use crate::access::{CandidateResource, ServiceConfig, Urgency, WeeklyWindow};
use chrono::{DateTime, Datelike, Duration, NaiveDate, NaiveTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use uuid::Uuid;

pub const MATCHER_VERSION: &str = "access-matcher.v1";
pub const TRAVEL_ESTIMATE_PROVENANCE: &str = "haversine-urban-estimate.v1";

/// Assumed door-to-door average speed for the travel estimate, km/h. The
/// provenance label makes the crudeness explicit to staff and patients.
const TRAVEL_SPEED_KMH: f64 = 30.0;
const TRAVEL_FIXED_MINUTES: f64 = 10.0;
/// Upper bound on generated slots before scoring, protecting against
/// pathological availability configurations.
const MAX_RAW_SLOTS: usize = 5_000;
/// Diversity cap: at most this many candidates per primary resource and
/// day, so one wide-open diary does not crowd out every alternative.
const MAX_PER_RESOURCE_DAY: usize = 3;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Interval {
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

impl Interval {
    pub fn new(start: DateTime<Utc>, end: DateTime<Utc>) -> Self {
        Self { start, end }
    }

    pub fn overlaps(&self, other: &Interval) -> bool {
        self.start < other.end && other.start < self.end
    }

    pub fn contains(&self, other: &Interval) -> bool {
        self.start <= other.start && other.end <= self.end
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct AvailabilityRule {
    pub weekday: u8,
    pub start: NaiveTime,
    pub end: NaiveTime,
    /// `available` or `break`.
    pub kind: String,
    pub capacity: Option<i32>,
    pub effective_from: Option<NaiveDate>,
    pub effective_to: Option<NaiveDate>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceException {
    pub kind: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub capacity_delta: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ExistingBooking {
    pub slot_index: i32,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceServiceFact {
    pub service_code: String,
    pub duration_minutes: Option<i32>,
    pub prep_minutes: i32,
    pub cleanup_minutes: i32,
    pub modality_codes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct FacilityFact {
    pub facility_id: Uuid,
    pub time_zone: String,
    pub opening_hours: Vec<WeeklyWindow>,
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ResourceFact {
    pub resource_id: Uuid,
    pub facility_id: Uuid,
    pub resource_type_code: String,
    pub name: String,
    pub user_id: Option<Uuid>,
    pub languages: Vec<String>,
    pub accessibility_codes: Vec<String>,
    pub capacity: i32,
    pub time_zone: String,
    pub services: Vec<ResourceServiceFact>,
    pub rules: Vec<AvailabilityRule>,
    pub exceptions: Vec<ResourceException>,
    pub bookings: Vec<ExistingBooking>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PatientFacts {
    pub age_years: Option<i32>,
    pub language: Option<String>,
    pub accessibility_needs: Vec<String>,
    pub time_zone: String,
    pub busy_intervals: Vec<Interval>,
    pub available_windows: Vec<WeeklyWindow>,
    pub unavailable_windows: Vec<WeeklyWindow>,
    pub care_team_user_ids: Vec<Uuid>,
    /// One-time coordinates (never persisted by the caller).
    pub origin: Option<(f64, f64)>,
    pub kept_appointments: i32,
    pub no_shows: i32,
    pub has_referral: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RequestConstraints {
    pub service_code: String,
    pub service: ServiceConfig,
    pub modality_codes: Vec<String>,
    pub facility_ids: Vec<Uuid>,
    pub earliest: Option<DateTime<Utc>>,
    pub latest: Option<DateTime<Utc>>,
    pub preferred_windows: Vec<WeeklyWindow>,
    pub max_travel_minutes: Option<i32>,
    pub continuity_required: bool,
    pub urgency: Urgency,
    pub requested_at: DateTime<Utc>,
    pub waitlist_position: Option<(i32, i32)>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PolicyFacts {
    pub min_notice_hours: i32,
    pub horizon_days: i32,
    pub max_candidates: usize,
    /// Demand multiplier per local date from the tenant's operational
    /// calendar (1.0 = baseline).
    pub demand_by_date: BTreeMap<NaiveDate, f64>,
    /// Newly freed slots (cancellations) worth filling.
    pub cancellation_gaps: Vec<Interval>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MatchFacts {
    pub now: DateTime<Utc>,
    pub request: RequestConstraints,
    pub patient: PatientFacts,
    pub facilities: Vec<FacilityFact>,
    pub resources: Vec<ResourceFact>,
    pub policy: PolicyFacts,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ScoreFactor {
    pub code: String,
    pub weight: f64,
    /// Normalized 0..=1 contribution before weighting.
    pub value: f64,
    pub points: f64,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct TravelEstimate {
    pub distance_km: f64,
    pub minutes: f64,
    pub provenance: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct BookingPlan {
    pub resource_id: Uuid,
    pub role: String,
    pub slot_index: i32,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Candidate {
    pub candidate_id: String,
    pub facility_id: Uuid,
    pub modality_code: String,
    pub starts_at: DateTime<Utc>,
    pub ends_at: DateTime<Utc>,
    pub resources: Vec<CandidateResource>,
    pub bookings: Vec<BookingPlan>,
    pub score: f64,
    pub factors: Vec<ScoreFactor>,
    pub travel: Option<TravelEstimate>,
    pub reasons: Vec<String>,
    pub supportive_actions: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MatchOutput {
    pub matcher_version: String,
    pub candidates: Vec<Candidate>,
    /// Hard-constraint rejections by reason code, for staff transparency.
    pub rejected: BTreeMap<String, u64>,
    pub window_start: DateTime<Utc>,
    pub window_end: DateTime<Utc>,
    pub facts_hash: String,
}

fn parse_tz(name: &str) -> Tz {
    name.parse().unwrap_or(chrono_tz::UTC)
}

/// Local wall time to UTC. Across a DST gap the wall time does not exist
/// and the instant one hour later is used; across a fold the earlier
/// instant is used. Both choices are deterministic and documented.
pub fn local_to_utc(tz: Tz, date: NaiveDate, time: NaiveTime) -> DateTime<Utc> {
    let naive = date.and_time(time);
    match tz.from_local_datetime(&naive) {
        chrono::LocalResult::Single(t) => t.with_timezone(&Utc),
        chrono::LocalResult::Ambiguous(a, _) => a.with_timezone(&Utc),
        chrono::LocalResult::None => {
            let shifted = naive + Duration::hours(1);
            tz.from_local_datetime(&shifted)
                .earliest()
                .map(|t| t.with_timezone(&Utc))
                .unwrap_or_else(|| Utc.from_utc_datetime(&naive))
        }
    }
}

fn windows_on(tz: Tz, date: NaiveDate, windows: &[WeeklyWindow]) -> Vec<Interval> {
    let weekday = date.weekday().number_from_monday() as u8;
    windows
        .iter()
        .filter(|w| w.weekday == weekday)
        .map(|w| {
            Interval::new(
                local_to_utc(tz, date, w.start),
                local_to_utc(tz, date, w.end),
            )
        })
        .filter(|i| i.end > i.start)
        .collect()
}

fn subtract(intervals: Vec<Interval>, cut: &Interval) -> Vec<Interval> {
    let mut out = Vec::new();
    for i in intervals {
        if !i.overlaps(cut) {
            out.push(i);
            continue;
        }
        if i.start < cut.start {
            out.push(Interval::new(i.start, cut.start));
        }
        if cut.end < i.end {
            out.push(Interval::new(cut.end, i.end));
        }
    }
    out
}

fn intersect(a: &[Interval], b: &[Interval]) -> Vec<Interval> {
    let mut out = Vec::new();
    for x in a {
        for y in b {
            let s = x.start.max(y.start);
            let e = x.end.min(y.end);
            if e > s {
                out.push(Interval::new(s, e));
            }
        }
    }
    out
}

/// Haversine great-circle distance in km.
pub fn haversine_km(a: (f64, f64), b: (f64, f64)) -> f64 {
    let (lat1, lon1) = (a.0.to_radians(), a.1.to_radians());
    let (lat2, lon2) = (b.0.to_radians(), b.1.to_radians());
    let dlat = lat2 - lat1;
    let dlon = lon2 - lon1;
    let h = (dlat / 2.0).sin().powi(2) + lat1.cos() * lat2.cos() * (dlon / 2.0).sin().powi(2);
    2.0 * 6371.0 * h.sqrt().asin()
}

pub fn travel_estimate(origin: (f64, f64), facility: (f64, f64)) -> TravelEstimate {
    let distance_km = haversine_km(origin, facility);
    TravelEstimate {
        distance_km: (distance_km * 10.0).round() / 10.0,
        minutes: (TRAVEL_FIXED_MINUTES + distance_km / TRAVEL_SPEED_KMH * 60.0).round(),
        provenance: TRAVEL_ESTIMATE_PROVENANCE.to_string(),
    }
}

fn remote_modality(code: &str) -> bool {
    matches!(code, "telehealth" | "phone" | "video" | "home_visit")
}

/// Available capacity intervals of a resource on `date` (resource local),
/// each with the number of parallel slots open.
fn resource_capacity_on(
    res: &ResourceFact,
    facility: Option<&FacilityFact>,
    date: NaiveDate,
) -> Vec<(Interval, i32)> {
    let tz = parse_tz(&res.time_zone);
    let weekday = date.weekday().number_from_monday() as u8;
    let applies = |r: &AvailabilityRule| {
        r.weekday == weekday
            && r.effective_from.is_none_or(|d| d <= date)
            && r.effective_to.is_none_or(|d| d >= date)
    };
    let mut open: Vec<(Interval, i32)> = res
        .rules
        .iter()
        .filter(|r| r.kind == "available" && applies(r))
        .map(|r| {
            (
                Interval::new(
                    local_to_utc(tz, date, r.start),
                    local_to_utc(tz, date, r.end),
                ),
                r.capacity.unwrap_or(res.capacity).max(1),
            )
        })
        .filter(|(i, _)| i.end > i.start)
        .collect();
    for brk in res.rules.iter().filter(|r| r.kind == "break" && applies(r)) {
        let cut = Interval::new(
            local_to_utc(tz, date, brk.start),
            local_to_utc(tz, date, brk.end),
        );
        open = open
            .into_iter()
            .flat_map(|(i, c)| subtract(vec![i], &cut).into_iter().map(move |x| (x, c)))
            .collect();
    }
    if let Some(f) = facility {
        if !f.opening_hours.is_empty() {
            let hours = windows_on(parse_tz(&f.time_zone), date, &f.opening_hours);
            open = open
                .into_iter()
                .flat_map(|(i, c)| intersect(&[i], &hours).into_iter().map(move |x| (x, c)))
                .collect();
        }
    }
    for ex in &res.exceptions {
        let cut = Interval::new(ex.start, ex.end);
        match ex.kind.as_str() {
            "extra_capacity" => {
                open = open
                    .into_iter()
                    .map(|(i, c)| {
                        if i.overlaps(&cut) {
                            (i, (c + ex.capacity_delta).max(1))
                        } else {
                            (i, c)
                        }
                    })
                    .collect();
            }
            _ => {
                open = open
                    .into_iter()
                    .flat_map(|(i, c)| subtract(vec![i], &cut).into_iter().map(move |x| (x, c)))
                    .collect();
            }
        }
    }
    open
}

fn free_slot_index(res: &ResourceFact, capacity: i32, interval: &Interval) -> Option<i32> {
    (0..capacity).find(|idx| {
        !res.bookings
            .iter()
            .any(|b| b.slot_index == *idx && Interval::new(b.start, b.end).overlaps(interval))
    })
}

/// Whether a required (non-primary) resource can be occupied over
/// `interval`: it must be open (rules, or facility hours when it has no
/// rules) and have a free slot.
fn required_resource_slot(
    res: &ResourceFact,
    facility: Option<&FacilityFact>,
    interval: &Interval,
) -> Option<i32> {
    let tz = parse_tz(&res.time_zone);
    let date = interval.start.with_timezone(&tz).date_naive();
    let dates = [date - Duration::days(1), date, date + Duration::days(1)];
    let open: Vec<(Interval, i32)> = if res.rules.is_empty() {
        dates
            .iter()
            .flat_map(|d| match facility {
                Some(f) if !f.opening_hours.is_empty() => {
                    windows_on(parse_tz(&f.time_zone), *d, &f.opening_hours)
                }
                _ => vec![Interval::new(
                    local_to_utc(tz, *d, NaiveTime::MIN),
                    local_to_utc(tz, *d + Duration::days(1), NaiveTime::MIN),
                )],
            })
            .flat_map(|i| {
                let capacity = res.capacity.max(1);
                let mut cur = vec![i];
                for ex in &res.exceptions {
                    if ex.kind != "extra_capacity" {
                        cur = subtract(cur, &Interval::new(ex.start, ex.end));
                    }
                }
                cur.into_iter().map(move |x| (x, capacity))
            })
            .collect()
    } else {
        dates
            .iter()
            .flat_map(|d| resource_capacity_on(res, facility, *d))
            .collect()
    };
    let (_, capacity) = open.iter().find(|(i, _)| i.contains(interval))?;
    free_slot_index(res, *capacity, interval)
}

fn reject(rejected: &mut BTreeMap<String, u64>, reason: &str) {
    *rejected.entry(reason.to_string()).or_insert(0) += 1;
}

fn facts_hash(facts: &MatchFacts) -> String {
    let mut h = Sha256::new();
    h.update(MATCHER_VERSION.as_bytes());
    h.update(serde_json::to_vec(facts).unwrap_or_default());
    hex::encode(h.finalize())
}

fn candidate_id(resource: Uuid, start: DateTime<Utc>, facts_hash: &str) -> String {
    let mut h = Sha256::new();
    h.update(facts_hash.as_bytes());
    h.update(resource.as_bytes());
    h.update(start.timestamp().to_be_bytes());
    format!("cand_{}", &hex::encode(h.finalize())[..16])
}

/// Generate and score feasible candidates.
pub fn find_candidates(facts: &MatchFacts) -> MatchOutput {
    let hash = facts_hash(facts);
    let req = &facts.request;
    let policy = &facts.policy;
    let mut rejected = BTreeMap::new();

    let min_notice = Duration::hours(policy.min_notice_hours.max(0) as i64);
    let service_lead = Duration::hours(req.service.min_lead_hours.unwrap_or(0).max(0) as i64);
    let mut window_start = facts.now + min_notice.max(service_lead);
    if let Some(e) = req.earliest {
        window_start = window_start.max(e);
    }
    let mut window_end = facts.now + Duration::days(policy.horizon_days.clamp(1, 365) as i64);
    if let Some(l) = req.latest {
        window_end = window_end.min(l);
    }
    let empty = |reason: &str, rejected: &mut BTreeMap<String, u64>| {
        reject(rejected, reason);
    };

    let mut out = MatchOutput {
        matcher_version: MATCHER_VERSION.to_string(),
        candidates: Vec::new(),
        rejected: BTreeMap::new(),
        window_start,
        window_end,
        facts_hash: hash.clone(),
    };
    if window_end <= window_start {
        empty("empty_window", &mut rejected);
        out.rejected = rejected;
        return out;
    }

    // Explicit, tenant-configured restrictions only.
    if let Some(age) = facts.patient.age_years {
        if req.service.min_age_years.is_some_and(|m| age < m)
            || req.service.max_age_years.is_some_and(|m| age > m)
        {
            empty("service_age_restriction", &mut rejected);
            out.rejected = rejected;
            return out;
        }
    }
    if req.service.requires_referral && !facts.patient.has_referral {
        empty("referral_required", &mut rejected);
        out.rejected = rejected;
        return out;
    }

    let facility_of = |id: Uuid| facts.facilities.iter().find(|f| f.facility_id == id);
    let required_types = &req.service.required_resource_types;

    let mut raw: Vec<Candidate> = Vec::new();
    for res in &facts.resources {
        let Some(svc) = res
            .services
            .iter()
            .find(|s| s.service_code == req.service_code)
        else {
            continue; // not a primary deliverer of this service
        };
        if !req.facility_ids.is_empty() && !req.facility_ids.contains(&res.facility_id) {
            reject(&mut rejected, "facility_not_requested");
            continue;
        }
        let modalities: Vec<String> = {
            let base: Vec<String> = if !svc.modality_codes.is_empty() {
                svc.modality_codes.clone()
            } else if !req.service.modality_codes.is_empty() {
                req.service.modality_codes.clone()
            } else {
                vec!["in_person".to_string()]
            };
            if req.modality_codes.is_empty() {
                base
            } else {
                base.into_iter()
                    .filter(|m| req.modality_codes.contains(m))
                    .collect()
            }
        };
        if modalities.is_empty() {
            reject(&mut rejected, "modality_not_available");
            continue;
        }
        if let Some(lang) = &facts.patient.language {
            if !res.languages.is_empty() && !res.languages.iter().any(|l| l == lang) {
                reject(&mut rejected, "language_not_supported");
                continue;
            }
        }
        if facts
            .patient
            .accessibility_needs
            .iter()
            .any(|n| !res.accessibility_codes.contains(n))
            && !modalities.iter().all(|m| remote_modality(m))
        {
            reject(&mut rejected, "accessibility_not_met");
            continue;
        }
        if req.continuity_required
            && !res
                .user_id
                .is_some_and(|u| facts.patient.care_team_user_ids.contains(&u))
        {
            reject(&mut rejected, "continuity_not_met");
            continue;
        }
        let facility = facility_of(res.facility_id);
        let travel = match (facts.patient.origin, facility) {
            (Some(o), Some(f)) if f.latitude.is_some() && f.longitude.is_some() => Some(
                travel_estimate(o, (f.latitude.unwrap_or(0.0), f.longitude.unwrap_or(0.0))),
            ),
            _ => None,
        };
        let modality = modalities[0].clone();
        let remote = remote_modality(&modality);
        if let (Some(max), Some(t)) = (req.max_travel_minutes, &travel) {
            if !remote && t.minutes > max as f64 {
                reject(&mut rejected, "travel_too_long");
                continue;
            }
        }

        let duration = Duration::minutes(
            svc.duration_minutes
                .unwrap_or(req.service.duration_minutes)
                .clamp(5, 720) as i64,
        );
        let prep =
            Duration::minutes((svc.prep_minutes.max(req.service.prep_minutes)).max(0) as i64);
        let cleanup =
            Duration::minutes((svc.cleanup_minutes.max(req.service.cleanup_minutes)).max(0) as i64);
        let occupancy = prep + duration + cleanup;
        let tz = parse_tz(&res.time_zone);
        let patient_tz = parse_tz(&facts.patient.time_zone);

        let mut date = window_start.with_timezone(&tz).date_naive() - Duration::days(1);
        let last = window_end.with_timezone(&tz).date_naive() + Duration::days(1);
        let mut per_resource = 0usize;
        while date <= last && raw.len() < MAX_RAW_SLOTS {
            for (open, capacity) in resource_capacity_on(res, facility, date) {
                let mut slot_start = open.start;
                while slot_start + occupancy <= open.end {
                    let booking = Interval::new(slot_start, slot_start + occupancy);
                    let appt = Interval::new(slot_start + prep, slot_start + prep + duration);
                    slot_start += occupancy;
                    if appt.start < window_start || appt.end > window_end {
                        continue;
                    }
                    let Some(slot_index) = free_slot_index(res, capacity, &booking) else {
                        reject(&mut rejected, "resource_booked");
                        continue;
                    };
                    if facts
                        .patient
                        .busy_intervals
                        .iter()
                        .any(|b| b.overlaps(&appt))
                    {
                        reject(&mut rejected, "patient_calendar_conflict");
                        continue;
                    }
                    let local_date = appt.start.with_timezone(&patient_tz).date_naive();
                    if !facts.patient.available_windows.is_empty() {
                        let avail =
                            windows_on(patient_tz, local_date, &facts.patient.available_windows);
                        if !avail.iter().any(|w| w.contains(&appt)) {
                            reject(&mut rejected, "outside_patient_availability");
                            continue;
                        }
                    }
                    if windows_on(patient_tz, local_date, &facts.patient.unavailable_windows)
                        .iter()
                        .any(|w| w.overlaps(&appt))
                    {
                        reject(&mut rejected, "patient_unavailable");
                        continue;
                    }
                    // Required companion resources at the same facility.
                    let mut bookings = vec![BookingPlan {
                        resource_id: res.resource_id,
                        role: "primary".into(),
                        slot_index,
                        start: booking.start,
                        end: booking.end,
                    }];
                    let mut ok = true;
                    for rt in required_types {
                        // The primary deliverer satisfies its own type.
                        if *rt == res.resource_type_code {
                            continue;
                        }
                        let found = facts.resources.iter().find_map(|other| {
                            if other.facility_id != res.facility_id
                                || &other.resource_type_code != rt
                                || other.resource_id == res.resource_id
                                || bookings.iter().any(|b| b.resource_id == other.resource_id)
                            {
                                return None;
                            }
                            required_resource_slot(other, facility, &booking)
                                .map(|idx| (other.resource_id, idx))
                        });
                        match found {
                            Some((rid, idx)) => bookings.push(BookingPlan {
                                resource_id: rid,
                                role: rt.clone(),
                                slot_index: idx,
                                start: booking.start,
                                end: booking.end,
                            }),
                            None => {
                                ok = false;
                                break;
                            }
                        }
                    }
                    if !ok {
                        reject(&mut rejected, "required_resource_unavailable");
                        continue;
                    }
                    let (score, factors, reasons, supportive) =
                        score_candidate(facts, res, &appt, travel.as_ref(), &modality);
                    raw.push(Candidate {
                        candidate_id: candidate_id(res.resource_id, appt.start, &hash),
                        facility_id: res.facility_id,
                        modality_code: modality.clone(),
                        starts_at: appt.start,
                        ends_at: appt.end,
                        resources: bookings
                            .iter()
                            .map(|b| CandidateResource {
                                resource_id: b.resource_id,
                                role: b.role.clone(),
                                slot_index: b.slot_index,
                            })
                            .collect(),
                        bookings,
                        score,
                        factors,
                        travel: travel.clone(),
                        reasons,
                        supportive_actions: supportive,
                    });
                    per_resource += 1;
                    if raw.len() >= MAX_RAW_SLOTS {
                        break;
                    }
                }
            }
            date += Duration::days(1);
        }
        if per_resource == 0 {
            reject(&mut rejected, "no_open_slot_for_resource");
        }
    }

    raw.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.starts_at.cmp(&b.starts_at))
            .then(a.candidate_id.cmp(&b.candidate_id))
    });
    let mut per_res: BTreeMap<(Uuid, NaiveDate), usize> = BTreeMap::new();
    let mut chosen = Vec::new();
    for c in raw {
        let primary = c.bookings[0].resource_id;
        let n = per_res
            .entry((primary, c.starts_at.date_naive()))
            .or_insert(0);
        if *n >= MAX_PER_RESOURCE_DAY {
            continue;
        }
        *n += 1;
        chosen.push(c);
        if chosen.len() >= policy.max_candidates.clamp(1, 20) {
            break;
        }
    }
    out.candidates = chosen;
    out.rejected = rejected;
    out
}

fn score_candidate(
    facts: &MatchFacts,
    res: &ResourceFact,
    appt: &Interval,
    travel: Option<&TravelEstimate>,
    modality: &str,
) -> (f64, Vec<ScoreFactor>, Vec<String>, Vec<String>) {
    let req = &facts.request;
    let mut factors = Vec::new();
    let mut reasons = Vec::new();
    let mut supportive = Vec::new();
    let mut push = |code: &'static str, weight: f64, value: f64, detail: String| {
        let value = value.clamp(0.0, 1.0);
        factors.push(ScoreFactor {
            code: code.to_string(),
            weight,
            value,
            points: (weight * value * 100.0).round() / 100.0,
            detail,
        });
    };

    // Clinical urgency (deterministic/human) favours soonness.
    let hours_until = (appt.start - facts.now).num_minutes() as f64 / 60.0;
    let (urg_weight, half_life_h) = match req.urgency {
        Urgency::Urgent => (35.0, 12.0),
        Urgency::Priority => (25.0, 72.0),
        Urgency::Routine => (15.0, 24.0 * 14.0),
    };
    let soonness = 1.0 / (1.0 + hours_until / half_life_h);
    push(
        "urgency_soonness",
        urg_weight,
        soonness,
        format!(
            "{} request; {hours_until:.0}h until slot",
            req.urgency.as_str()
        ),
    );
    if req.urgency != Urgency::Routine && hours_until <= half_life_h {
        reasons.push("early_slot_for_urgency".into());
    }

    // Waiting time already accrued by this patient.
    let waited_h = (facts.now - req.requested_at).num_minutes().max(0) as f64 / 60.0;
    push(
        "waiting_time",
        10.0,
        (waited_h / (24.0 * 14.0)).min(1.0),
        format!("waiting {waited_h:.0}h since request"),
    );

    // Preferred dates/times.
    let patient_tz = parse_tz(&facts.patient.time_zone);
    let local_date = appt.start.with_timezone(&patient_tz).date_naive();
    let preferred = !req.preferred_windows.is_empty()
        && windows_on(patient_tz, local_date, &req.preferred_windows)
            .iter()
            .any(|w| w.contains(appt));
    push(
        "preferred_time",
        15.0,
        if req.preferred_windows.is_empty() {
            0.5
        } else if preferred {
            1.0
        } else {
            0.0
        },
        if preferred {
            "inside a preferred window".into()
        } else {
            "outside preferred windows".into()
        },
    );
    if preferred {
        reasons.push("matches_preferred_time".into());
    }

    // Travel burden.
    let remote = remote_modality(modality);
    let travel_value = match travel {
        _ if remote => 1.0,
        Some(t) => 1.0 - (t.minutes / 120.0).min(1.0),
        None => 0.5,
    };
    push(
        "travel_burden",
        10.0,
        travel_value,
        match travel {
            _ if remote => "no travel (remote modality)".into(),
            Some(t) => format!(
                "~{:.0} min, {:.1} km ({})",
                t.minutes, t.distance_km, t.provenance
            ),
            None => "travel unknown".into(),
        },
    );
    if remote || travel.is_some_and(|t| t.minutes <= 20.0) {
        reasons.push("low_travel_burden".into());
    }

    // Continuity of care.
    let continuity = res
        .user_id
        .is_some_and(|u| facts.patient.care_team_user_ids.contains(&u));
    push(
        "continuity",
        10.0,
        if continuity { 1.0 } else { 0.0 },
        if continuity {
            "existing care-team member".into()
        } else {
            "new professional".into()
        },
    );
    if continuity {
        reasons.push("continuity_of_care".into());
    }

    // Utilization: fill days that already have activity (adjacent bookings).
    let adjacent = res.bookings.iter().any(|b| {
        (b.end - appt.start).num_minutes().abs() <= 60
            || (appt.end - b.start).num_minutes().abs() <= 60
    });
    push(
        "utilization",
        5.0,
        if adjacent { 1.0 } else { 0.3 },
        if adjacent {
            "adjacent to existing bookings".into()
        } else {
            "opens a new block".into()
        },
    );

    // Cancellation gap filling.
    let gap = facts
        .policy
        .cancellation_gaps
        .iter()
        .any(|g| g.contains(appt));
    push(
        "gap_fill",
        5.0,
        if gap { 1.0 } else { 0.0 },
        if gap {
            "fills a cancelled slot".into()
        } else {
            "not a cancellation gap".into()
        },
    );
    if gap {
        reasons.push("fills_cancellation_gap".into());
    }

    // Fair waitlist position (1 = first).
    let fairness = match req.waitlist_position {
        Some((pos, total)) if total > 0 => 1.0 - ((pos - 1).max(0) as f64 / total as f64),
        _ => 0.5,
    };
    push(
        "waitlist_fairness",
        5.0,
        fairness,
        match req.waitlist_position {
            Some((p, t)) => format!("waitlist position {p} of {t}"),
            None => "not on a waitlist".into(),
        },
    );

    // Seasonal demand: prefer lower-pressure days.
    let res_tz = parse_tz(&res.time_zone);
    let res_date = appt.start.with_timezone(&res_tz).date_naive();
    let demand = facts
        .policy
        .demand_by_date
        .get(&res_date)
        .copied()
        .unwrap_or(1.0);
    push(
        "seasonal_demand",
        5.0,
        (2.0 - demand).clamp(0.0, 1.0),
        format!("expected demand x{demand:.2}"),
    );

    // Confirmation history: supportive only. Never negative.
    if facts.patient.no_shows > 0 {
        supportive.push("extra_reminder".into());
        supportive.push("confirmation_request".into());
        if preferred || remote {
            supportive.push("convenient_option".into());
        }
    }
    push(
        "confirmation_support",
        0.0,
        0.0,
        format!(
            "{} kept / {} missed; history only adds reminders",
            facts.patient.kept_appointments, facts.patient.no_shows
        ),
    );

    let total: f64 = factors.iter().map(|f| f.points).sum();
    (
        (total * 100.0).round() / 100.0,
        factors,
        reasons,
        supportive,
    )
}

/// Whether a UTC instant falls on a given local hour (used by tests and the
/// fixtures to reason about DST).
pub fn local_hour(tz_name: &str, at: DateTime<Utc>) -> u32 {
    at.with_timezone(&parse_tz(tz_name)).hour()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    fn time(h: u32, m: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(h, m, 0).unwrap()
    }

    fn base_facts() -> MatchFacts {
        let fac = Uuid::from_u128(1);
        let doc = Uuid::from_u128(2);
        let doc_user = Uuid::from_u128(20);
        MatchFacts {
            now: t("2026-03-23T08:00:00Z"),
            request: RequestConstraints {
                service_code: "general_medicine".into(),
                service: ServiceConfig {
                    duration_minutes: 20,
                    ..Default::default()
                },
                modality_codes: vec![],
                facility_ids: vec![],
                earliest: None,
                latest: None,
                preferred_windows: vec![],
                max_travel_minutes: None,
                continuity_required: false,
                urgency: Urgency::Routine,
                requested_at: t("2026-03-23T08:00:00Z"),
                waitlist_position: None,
            },
            patient: PatientFacts {
                age_years: Some(40),
                language: None,
                accessibility_needs: vec![],
                time_zone: "Europe/Madrid".into(),
                busy_intervals: vec![],
                available_windows: vec![],
                unavailable_windows: vec![],
                care_team_user_ids: vec![],
                origin: None,
                kept_appointments: 3,
                no_shows: 0,
                has_referral: false,
            },
            facilities: vec![FacilityFact {
                facility_id: fac,
                time_zone: "Europe/Madrid".into(),
                opening_hours: vec![],
                latitude: Some(38.9067),
                longitude: Some(1.4206),
            }],
            resources: vec![ResourceFact {
                resource_id: doc,
                facility_id: fac,
                resource_type_code: "professional".into(),
                name: "Dr Test".into(),
                user_id: Some(doc_user),
                languages: vec!["es".into(), "en".into()],
                accessibility_codes: vec![],
                capacity: 1,
                time_zone: "Europe/Madrid".into(),
                services: vec![ResourceServiceFact {
                    service_code: "general_medicine".into(),
                    duration_minutes: None,
                    prep_minutes: 0,
                    cleanup_minutes: 0,
                    modality_codes: vec!["in_person".into()],
                }],
                rules: (1..=5)
                    .map(|wd| AvailabilityRule {
                        weekday: wd,
                        start: time(9, 0),
                        end: time(11, 0),
                        kind: "available".into(),
                        capacity: None,
                        effective_from: None,
                        effective_to: None,
                    })
                    .collect(),
                exceptions: vec![],
                bookings: vec![],
            }],
            policy: PolicyFacts {
                min_notice_hours: 2,
                horizon_days: 7,
                max_candidates: 8,
                demand_by_date: BTreeMap::new(),
                cancellation_gaps: vec![],
            },
        }
    }

    #[test]
    fn generates_only_open_slots_and_is_deterministic() {
        let facts = base_facts();
        let a = find_candidates(&facts);
        let b = find_candidates(&facts);
        assert_eq!(a, b);
        assert!(!a.candidates.is_empty());
        for c in &a.candidates {
            let h = local_hour("Europe/Madrid", c.starts_at);
            assert!((9..11).contains(&h), "slot at local hour {h}");
            assert!(c.starts_at >= facts.now + Duration::hours(2));
            assert_eq!(c.resources[0].resource_id, Uuid::from_u128(2));
        }
    }

    #[test]
    fn dst_transition_keeps_local_wall_time() {
        // Europe/Madrid springs forward on 2026-03-29.
        let mut facts = base_facts();
        facts.now = t("2026-03-26T05:00:00Z");
        facts.policy.horizon_days = 6;
        facts.policy.max_candidates = 20;
        let out = find_candidates(&facts);
        let before: Vec<_> = out
            .candidates
            .iter()
            .filter(|c| c.starts_at < t("2026-03-29T00:00:00Z"))
            .collect();
        let after: Vec<_> = out
            .candidates
            .iter()
            .filter(|c| c.starts_at >= t("2026-03-30T00:00:00Z"))
            .collect();
        assert!(!before.is_empty() && !after.is_empty());
        for c in before.iter().chain(after.iter()) {
            assert_eq!(local_hour("Europe/Madrid", c.starts_at), 9, "{c:?}");
        }
        // UTC offsets differ across the transition.
        assert_eq!(before[0].starts_at.hour(), 8);
        assert_eq!(after[0].starts_at.hour(), 7);
    }

    #[test]
    fn excludes_booked_slots_calendar_conflicts_and_exceptions() {
        let mut facts = base_facts();
        let day = |h, m| Utc.with_ymd_and_hms(2026, 3, 24, h, m, 0).unwrap();
        // 2026-03-24 is CET (UTC+1): local 09:00 = 08:00Z.
        facts.resources[0].bookings.push(ExistingBooking {
            slot_index: 0,
            start: day(8, 0),
            end: day(8, 20),
        });
        facts
            .patient
            .busy_intervals
            .push(Interval::new(day(8, 20), day(8, 40)));
        facts.resources[0].exceptions.push(ResourceException {
            kind: "leave".into(),
            start: t("2026-03-25T00:00:00Z"),
            end: t("2026-03-26T00:00:00Z"),
            capacity_delta: 0,
        });
        facts.policy.max_candidates = 20;
        let out = find_candidates(&facts);
        assert!(out.candidates.iter().all(|c| c.starts_at != day(8, 0)));
        assert!(out.candidates.iter().all(|c| c.starts_at != day(8, 20)));
        assert!(out.candidates.iter().any(|c| c.starts_at == day(8, 40)));
        assert!(out
            .candidates
            .iter()
            .all(|c| c.starts_at.date_naive() != NaiveDate::from_ymd_opt(2026, 3, 25).unwrap()));
        assert!(out.rejected.contains_key("resource_booked"));
        assert!(out.rejected.contains_key("patient_calendar_conflict"));
    }

    #[test]
    fn required_resource_combination_is_hard() {
        let mut facts = base_facts();
        facts.request.service.required_resource_types = vec!["dental_chair".into()];
        let out = find_candidates(&facts);
        assert!(out.candidates.is_empty());
        assert!(out.rejected.contains_key("required_resource_unavailable"));
        facts.resources.push(ResourceFact {
            resource_id: Uuid::from_u128(3),
            facility_id: Uuid::from_u128(1),
            resource_type_code: "dental_chair".into(),
            name: "Chair 1".into(),
            user_id: None,
            languages: vec![],
            accessibility_codes: vec![],
            capacity: 1,
            time_zone: "Europe/Madrid".into(),
            services: vec![],
            rules: vec![],
            exceptions: vec![],
            bookings: vec![],
        });
        let out = find_candidates(&facts);
        assert!(!out.candidates.is_empty());
        assert_eq!(out.candidates[0].bookings.len(), 2);
        assert_eq!(out.candidates[0].bookings[1].role, "dental_chair");
    }

    #[test]
    fn primary_resource_satisfies_its_own_required_type() {
        let mut facts = base_facts();
        facts.request.service.required_resource_types = vec!["professional".into()];
        let out = find_candidates(&facts);
        assert!(!out.candidates.is_empty());
        assert!(out
            .candidates
            .iter()
            .all(|c| c.bookings.len() == 1 && c.bookings[0].role == "primary"));
        assert!(!out.rejected.contains_key("required_resource_unavailable"));
    }

    #[test]
    fn explicit_age_restriction_and_language_are_hard() {
        let mut facts = base_facts();
        facts.request.service.min_age_years = Some(65);
        assert!(find_candidates(&facts).candidates.is_empty());
        let mut facts = base_facts();
        facts.patient.language = Some("de".into());
        let out = find_candidates(&facts);
        assert!(out.candidates.is_empty());
        assert!(out.rejected.contains_key("language_not_supported"));
    }

    #[test]
    fn no_show_history_never_lowers_score() {
        let clean = base_facts();
        let mut history = base_facts();
        history.patient.no_shows = 4;
        let a = find_candidates(&clean);
        let b = find_candidates(&history);
        assert_eq!(a.candidates.len(), b.candidates.len());
        for (x, y) in a.candidates.iter().zip(b.candidates.iter()) {
            assert_eq!(x.score, y.score);
            assert!(y.supportive_actions.contains(&"extra_reminder".to_string()));
        }
    }

    #[test]
    fn urgency_and_continuity_reorder_transparently() {
        let mut facts = base_facts();
        facts.patient.care_team_user_ids = vec![Uuid::from_u128(20)];
        facts.request.urgency = Urgency::Urgent;
        let out = find_candidates(&facts);
        let first = &out.candidates[0];
        assert!(first.reasons.contains(&"continuity_of_care".to_string()));
        assert!(first
            .factors
            .iter()
            .any(|f| f.code == "urgency_soonness" && f.value > 0.3));
        let total: f64 = first.factors.iter().map(|f| f.points).sum();
        assert!((total - first.score).abs() < 0.05);
        // earliest slot ranks first under urgency
        assert!(out
            .candidates
            .iter()
            .all(|c| c.starts_at >= first.starts_at));
    }

    #[test]
    fn travel_estimate_has_provenance() {
        let e = travel_estimate((38.9067, 1.4206), (38.98, 1.30));
        assert_eq!(e.provenance, TRAVEL_ESTIMATE_PROVENANCE.to_string());
        assert!(e.distance_km > 10.0 && e.distance_km < 15.0);
    }
}
