//! Bounded, privacy-preserving RFC 5545 parsing and generation.
//!
//! [`busy_intervals`] reads an iCalendar file *in memory* and returns only
//! normalized busy intervals inside a horizon. Titles, descriptions,
//! attendees, locations, URLs and every other property are ignored and
//! never leave this function. Recurrence expansion, event count and the
//! horizon are bounded so a malformed or hostile calendar cannot exhaust
//! resources.
//!
//! [`appointment_event`] renders the minimal VEVENT a patient downloads for
//! a confirmed appointment.

use crate::matcher::{local_to_utc, Interval};
use chrono::{DateTime, Datelike, Duration, Months, NaiveDate, NaiveDateTime, NaiveTime, Utc};
use chrono_tz::Tz;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

pub const MAX_ICS_BYTES: usize = 1_000_000;
pub const MAX_EVENTS: usize = 2_000;
pub const MAX_OCCURRENCES_PER_EVENT: usize = 400;
pub const MAX_TOTAL_INTERVALS: usize = 5_000;
pub const MAX_HORIZON_DAYS: i64 = 180;
pub const MAX_LINES: usize = 200_000;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum IcsError {
    #[error("calendar exceeds {0} bytes")]
    TooLarge(usize),
    #[error("calendar has too many lines")]
    TooManyLines,
    #[error("calendar has more than {0} events")]
    TooManyEvents(usize),
    #[error("calendar produced more than {0} busy intervals in the horizon")]
    TooManyIntervals(usize),
    #[error("not an iCalendar file")]
    NotCalendar,
    #[error("invalid UTF-8")]
    InvalidUtf8,
    #[error("horizon must be at most {0} days")]
    HorizonTooLong(i64),
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct BusyImport {
    pub intervals: Vec<Interval>,
    pub time_zone: String,
    pub integrity_hash: String,
    pub events_seen: usize,
    pub events_skipped: usize,
}

struct Event {
    start: Option<NaiveDateTime>,
    start_tz: Option<Tz>,
    start_is_utc: bool,
    all_day: bool,
    end: Option<NaiveDateTime>,
    end_tz: Option<Tz>,
    end_is_utc: bool,
    duration: Option<Duration>,
    rrule: Option<String>,
    exdates: Vec<NaiveDateTime>,
    transparent: bool,
    cancelled: bool,
}

impl Event {
    fn new() -> Self {
        Self {
            start: None,
            start_tz: None,
            start_is_utc: false,
            all_day: false,
            end: None,
            end_tz: None,
            end_is_utc: false,
            duration: None,
            rrule: None,
            exdates: Vec::new(),
            transparent: false,
            cancelled: false,
        }
    }
}

fn unfold(text: &str) -> Result<Vec<String>, IcsError> {
    let mut lines: Vec<String> = Vec::new();
    for (n, raw) in text.split('\n').enumerate() {
        if n > MAX_LINES {
            return Err(IcsError::TooManyLines);
        }
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        if line.starts_with(' ') || line.starts_with('\t') {
            if let Some(last) = lines.last_mut() {
                last.push_str(&line[1..]);
                continue;
            }
        }
        lines.push(line.to_string());
    }
    Ok(lines)
}

type Params = Vec<(String, String)>;

fn split_property(line: &str) -> Option<(String, Params, String)> {
    let (head, value) = line.split_once(':')?;
    let mut parts = head.split(';');
    let name = parts.next()?.to_ascii_uppercase();
    let params = parts
        .filter_map(|p| {
            let (k, v) = p.split_once('=')?;
            Some((k.to_ascii_uppercase(), v.trim_matches('"').to_string()))
        })
        .collect();
    Some((name, params, value.to_string()))
}

fn parse_dt(value: &str) -> Option<(NaiveDateTime, bool, bool)> {
    let v = value.trim();
    if v.len() == 8 {
        let d = NaiveDate::parse_from_str(v, "%Y%m%d").ok()?;
        return Some((d.and_time(NaiveTime::MIN), false, true));
    }
    let (body, utc) = match v.strip_suffix('Z') {
        Some(b) => (b, true),
        None => (v, false),
    };
    let dt = NaiveDateTime::parse_from_str(body, "%Y%m%dT%H%M%S").ok()?;
    Some((dt, utc, false))
}

fn parse_duration(v: &str) -> Option<Duration> {
    // P[n]W | P[n]DT[n]H[n]M[n]S — sign ignored (negative durations are
    // not meaningful for busy time).
    let v = v.trim().trim_start_matches(['+', '-']);
    let body = v.strip_prefix('P')?;
    let mut total = Duration::zero();
    let mut num = String::new();
    let mut in_time = false;
    for ch in body.chars() {
        match ch {
            'T' => in_time = true,
            c if c.is_ascii_digit() => {
                if num.len() > 6 {
                    return None;
                }
                num.push(c)
            }
            unit => {
                let n: i64 = num.parse().ok()?;
                num.clear();
                total += match (unit, in_time) {
                    ('W', false) => Duration::weeks(n),
                    ('D', false) => Duration::days(n),
                    ('H', true) => Duration::hours(n),
                    ('M', true) => Duration::minutes(n),
                    ('S', true) => Duration::seconds(n),
                    _ => return None,
                };
            }
        }
    }
    Some(total)
}

fn to_utc(dt: NaiveDateTime, is_utc: bool, tz: Option<Tz>, default_tz: Tz) -> DateTime<Utc> {
    if is_utc {
        return DateTime::<Utc>::from_naive_utc_and_offset(dt, Utc);
    }
    let zone = tz.unwrap_or(default_tz);
    local_to_utc(zone, dt.date(), dt.time())
}

struct Rrule {
    freq: String,
    interval: u32,
    count: Option<usize>,
    until: Option<NaiveDateTime>,
    until_utc: bool,
    byday: Vec<chrono::Weekday>,
}

fn parse_rrule(v: &str) -> Option<Rrule> {
    let mut r = Rrule {
        freq: String::new(),
        interval: 1,
        count: None,
        until: None,
        until_utc: false,
        byday: Vec::new(),
    };
    for part in v.split(';') {
        let (k, val) = part.split_once('=')?;
        match k.to_ascii_uppercase().as_str() {
            "FREQ" => r.freq = val.to_ascii_uppercase(),
            "INTERVAL" => r.interval = val.parse::<u32>().ok()?.clamp(1, 366),
            "COUNT" => r.count = Some(val.parse::<usize>().ok()?),
            "UNTIL" => {
                let (dt, utc, _) = parse_dt(val)?;
                r.until = Some(dt);
                r.until_utc = utc;
            }
            "BYDAY" => {
                for d in val.split(',') {
                    // Strip ordinal prefixes like 1MO / -1FR (unsupported;
                    // treated as the plain weekday).
                    let code =
                        d.trim_start_matches(|c: char| c.is_ascii_digit() || c == '-' || c == '+');
                    let wd = match code {
                        "MO" => chrono::Weekday::Mon,
                        "TU" => chrono::Weekday::Tue,
                        "WE" => chrono::Weekday::Wed,
                        "TH" => chrono::Weekday::Thu,
                        "FR" => chrono::Weekday::Fri,
                        "SA" => chrono::Weekday::Sat,
                        "SU" => chrono::Weekday::Sun,
                        _ => return None,
                    };
                    r.byday.push(wd);
                }
            }
            _ => {}
        }
    }
    if r.freq.is_empty() {
        return None;
    }
    Some(r)
}

fn advance(start: NaiveDateTime, freq: &str, interval: u32, n: u32) -> Option<NaiveDateTime> {
    let steps = interval.checked_mul(n)?;
    match freq {
        "DAILY" => start.checked_add_signed(Duration::days(steps as i64)),
        "WEEKLY" => start.checked_add_signed(Duration::weeks(steps as i64)),
        "MONTHLY" => start.checked_add_months(Months::new(steps)),
        "YEARLY" => start.checked_add_months(Months::new(steps.checked_mul(12)?)),
        _ => None,
    }
}

/// Parse `bytes` and return busy intervals inside `[horizon_start,
/// horizon_end)`. `default_tz` resolves floating times and TZIDs that do
/// not name an IANA zone.
pub fn busy_intervals(
    bytes: &[u8],
    default_tz: &str,
    horizon_start: DateTime<Utc>,
    horizon_end: DateTime<Utc>,
) -> Result<BusyImport, IcsError> {
    if bytes.len() > MAX_ICS_BYTES {
        return Err(IcsError::TooLarge(MAX_ICS_BYTES));
    }
    if horizon_end - horizon_start > Duration::days(MAX_HORIZON_DAYS) {
        return Err(IcsError::HorizonTooLong(MAX_HORIZON_DAYS));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| IcsError::InvalidUtf8)?;
    let default: Tz = default_tz.parse().unwrap_or(chrono_tz::UTC);
    let lines = unfold(text)?;
    if !lines
        .iter()
        .any(|l| l.trim().eq_ignore_ascii_case("BEGIN:VCALENDAR"))
    {
        return Err(IcsError::NotCalendar);
    }

    let horizon = Interval::new(horizon_start, horizon_end);
    let mut intervals: Vec<Interval> = Vec::new();
    let mut events_seen = 0usize;
    let mut events_skipped = 0usize;
    let mut current: Option<Event> = None;
    let mut depth_other = 0usize; // inside VALARM/VTIMEZONE etc.
    for line in &lines {
        let Some((name, params, value)) = split_property(line) else {
            continue;
        };
        match name.as_str() {
            "BEGIN" => {
                if value.eq_ignore_ascii_case("VEVENT") && current.is_none() {
                    events_seen += 1;
                    if events_seen > MAX_EVENTS {
                        return Err(IcsError::TooManyEvents(MAX_EVENTS));
                    }
                    current = Some(Event::new());
                } else if current.is_some() {
                    depth_other += 1;
                }
                continue;
            }
            "END" => {
                if depth_other > 0 {
                    depth_other -= 1;
                    continue;
                }
                if value.eq_ignore_ascii_case("VEVENT") {
                    if let Some(ev) = current.take() {
                        if !expand(&ev, default, &horizon, &mut intervals) {
                            events_skipped += 1;
                        }
                        if intervals.len() > MAX_TOTAL_INTERVALS {
                            return Err(IcsError::TooManyIntervals(MAX_TOTAL_INTERVALS));
                        }
                    }
                }
                continue;
            }
            _ => {}
        }
        if depth_other > 0 {
            continue;
        }
        let Some(ev) = current.as_mut() else {
            continue;
        };
        let tzid = params
            .iter()
            .find(|(k, _)| k == "TZID")
            .and_then(|(_, v)| v.parse::<Tz>().ok());
        match name.as_str() {
            "DTSTART" => {
                if let Some((dt, utc, all_day)) = parse_dt(&value) {
                    ev.start = Some(dt);
                    ev.start_is_utc = utc;
                    ev.start_tz = tzid;
                    ev.all_day = all_day;
                }
            }
            "DTEND" => {
                if let Some((dt, utc, _)) = parse_dt(&value) {
                    ev.end = Some(dt);
                    ev.end_is_utc = utc;
                    ev.end_tz = tzid;
                }
            }
            "DURATION" => ev.duration = parse_duration(&value),
            "RRULE" => ev.rrule = Some(value),
            "EXDATE" => {
                for v in value.split(',') {
                    if let Some((dt, _, _)) = parse_dt(v) {
                        ev.exdates.push(dt);
                    }
                }
            }
            "TRANSP" => ev.transparent = value.trim().eq_ignore_ascii_case("TRANSPARENT"),
            "STATUS" => ev.cancelled = value.trim().eq_ignore_ascii_case("CANCELLED"),
            // Every other property (SUMMARY, DESCRIPTION, LOCATION,
            // ATTENDEE, ORGANIZER, URL, ATTACH, ...) is deliberately dropped.
            _ => {}
        }
    }

    intervals.sort_by_key(|i| (i.start, i.end));
    let mut merged: Vec<Interval> = Vec::with_capacity(intervals.len());
    for i in intervals {
        match merged.last_mut() {
            Some(last) if i.start <= last.end => {
                if i.end > last.end {
                    last.end = i.end;
                }
            }
            _ => merged.push(i),
        }
    }
    Ok(BusyImport {
        intervals: merged,
        time_zone: default.name().to_string(),
        integrity_hash: hex::encode(Sha256::digest(bytes)),
        events_seen,
        events_skipped,
    })
}

/// Expand one event into `out`; returns false if it contributed nothing.
fn expand(ev: &Event, default: Tz, horizon: &Interval, out: &mut Vec<Interval>) -> bool {
    if ev.transparent || ev.cancelled {
        return false;
    }
    let Some(start) = ev.start else {
        return false;
    };
    let length = if let Some(end) = ev.end {
        let s = to_utc(start, ev.start_is_utc, ev.start_tz, default);
        let e = to_utc(end, ev.end_is_utc, ev.end_tz.or(ev.start_tz), default);
        e - s
    } else if let Some(d) = ev.duration {
        d
    } else if ev.all_day {
        Duration::days(1)
    } else {
        Duration::zero()
    };
    if length <= Duration::zero() || length > Duration::days(MAX_HORIZON_DAYS) {
        return false;
    }
    let mut added = false;
    let mut emit =
        |occurrence_start: NaiveDateTime, out: &mut Vec<Interval>| {
            if ev.exdates.iter().any(|x| {
                x == &occurrence_start || (ev.all_day && x.date() == occurrence_start.date())
            }) {
                return;
            }
            let s = to_utc(occurrence_start, ev.start_is_utc, ev.start_tz, default);
            let i = Interval::new(s, s + length);
            if i.overlaps(horizon) {
                out.push(Interval::new(
                    i.start.max(horizon.start),
                    i.end.min(horizon.end),
                ));
                added = true;
            }
        };
    match ev.rrule.as_deref().and_then(parse_rrule) {
        None => emit(start, out),
        Some(rule) => {
            let until_utc = rule
                .until
                .map(|u| to_utc(u, rule.until_utc, ev.start_tz, default));
            let max_count = rule.count.unwrap_or(usize::MAX);
            let mut produced = 0usize;
            let mut n = 0u32;
            'outer: while produced < max_count && (n as usize) < MAX_OCCURRENCES_PER_EVENT {
                let Some(base) = advance(start, &rule.freq, rule.interval, n) else {
                    break;
                };
                n += 1;
                let occurrences: Vec<NaiveDateTime> =
                    if rule.freq == "WEEKLY" && !rule.byday.is_empty() {
                        // Expand the week containing `base` to the requested weekdays.
                        let week_start = base.date()
                            - Duration::days(base.weekday().num_days_from_monday() as i64);
                        rule.byday
                            .iter()
                            .map(|wd| {
                                (week_start + Duration::days(wd.num_days_from_monday() as i64))
                                    .and_time(base.time())
                            })
                            .filter(|d| *d >= start)
                            .collect()
                    } else {
                        vec![base]
                    };
                for occ in occurrences {
                    let occ_utc = to_utc(occ, ev.start_is_utc, ev.start_tz, default);
                    if until_utc.is_some_and(|u| occ_utc > u) {
                        break 'outer;
                    }
                    if occ_utc >= horizon.end {
                        break 'outer;
                    }
                    produced += 1;
                    emit(occ, out);
                    if produced >= max_count {
                        break 'outer;
                    }
                }
            }
        }
    }
    added
}

fn ics_escape(s: &str) -> String {
    s.replace('\\', "\\\\")
        .replace(';', "\\;")
        .replace(',', "\\,")
        .replace('\n', "\\n")
}

fn fmt_utc(t: DateTime<Utc>) -> String {
    t.format("%Y%m%dT%H%M%SZ").to_string()
}

/// Minimal VEVENT for a confirmed appointment. Contains only what the
/// patient needs to see it in their own calendar: no diagnosis, no free
/// text beyond the localized service name and the facility.
pub fn appointment_event(
    uid: &str,
    starts_at: DateTime<Utc>,
    ends_at: DateTime<Utc>,
    summary: &str,
    location: &str,
    description: &str,
    now: DateTime<Utc>,
) -> String {
    let lines = [
        "BEGIN:VCALENDAR".to_string(),
        "VERSION:2.0".to_string(),
        "PRODID:-//WellOS//dMind Access v1//EN".to_string(),
        "METHOD:PUBLISH".to_string(),
        "BEGIN:VEVENT".to_string(),
        format!("UID:{}", ics_escape(uid)),
        format!("DTSTAMP:{}", fmt_utc(now)),
        format!("DTSTART:{}", fmt_utc(starts_at)),
        format!("DTEND:{}", fmt_utc(ends_at)),
        format!("SUMMARY:{}", ics_escape(summary)),
        format!("LOCATION:{}", ics_escape(location)),
        format!("DESCRIPTION:{}", ics_escape(description)),
        "STATUS:CONFIRMED".to_string(),
        "TRANSP:OPAQUE".to_string(),
        "END:VEVENT".to_string(),
        "END:VCALENDAR".to_string(),
    ];
    let mut out = String::new();
    for l in lines {
        // RFC 5545 folding at 75 octets.
        let mut first = true;
        let mut cur = String::new();
        for ch in l.chars() {
            if cur.len() + ch.len_utf8() > if first { 75 } else { 74 } {
                out.push_str(&cur);
                out.push_str("\r\n");
                cur = " ".to_string();
                first = false;
            }
            cur.push(ch);
        }
        out.push_str(&cur);
        out.push_str("\r\n");
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s).unwrap().with_timezone(&Utc)
    }

    const SAMPLE: &str = "BEGIN:VCALENDAR\r\nVERSION:2.0\r\nPRODID:test\r\nBEGIN:VEVENT\r\nUID:1\r\nSUMMARY:Secret therapy session with Dr X\r\nDESCRIPTION:Talk about\r\n  the diagnosis\r\nATTENDEE:mailto:someone@example.com\r\nLOCATION:Hidden clinic\r\nDTSTART;TZID=Europe/Madrid:20260601T090000\r\nDTEND;TZID=Europe/Madrid:20260601T100000\r\nRRULE:FREQ=WEEKLY;COUNT=3\r\nEXDATE;TZID=Europe/Madrid:20260608T090000\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:2\r\nDTSTART:20260603T120000Z\r\nDURATION:PT30M\r\nTRANSP:TRANSPARENT\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nUID:3\r\nDTSTART;VALUE=DATE:20260610\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";

    #[test]
    fn keeps_only_busy_intervals() {
        let out = busy_intervals(
            SAMPLE.as_bytes(),
            "Europe/Madrid",
            t("2026-06-01T00:00:00Z"),
            t("2026-07-01T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(out.events_seen, 3);
        // Weekly x3 minus one EXDATE = 2, transparent skipped, all-day = 1.
        assert_eq!(out.intervals.len(), 3);
        assert_eq!(out.intervals[0].start, t("2026-06-01T07:00:00Z"));
        assert_eq!(out.intervals[0].end, t("2026-06-01T08:00:00Z"));
        assert_eq!(out.intervals[1].start, t("2026-06-09T22:00:00Z"));
        assert_eq!(out.intervals[1].end, t("2026-06-10T22:00:00Z"));
        assert_eq!(out.intervals[2].start, t("2026-06-15T07:00:00Z"));
        let json = serde_json::to_string(&out).unwrap();
        for leaked in [
            "Secret",
            "diagnosis",
            "someone@example.com",
            "Hidden clinic",
        ] {
            assert!(!json.contains(leaked), "{leaked} leaked");
        }
        assert_eq!(out.integrity_hash.len(), 64);
    }

    #[test]
    fn bounds_are_enforced() {
        let big = vec![b'A'; MAX_ICS_BYTES + 1];
        assert_eq!(
            busy_intervals(
                &big,
                "UTC",
                t("2026-06-01T00:00:00Z"),
                t("2026-06-02T00:00:00Z")
            ),
            Err(IcsError::TooLarge(MAX_ICS_BYTES))
        );
        assert_eq!(
            busy_intervals(
                b"hello",
                "UTC",
                t("2026-06-01T00:00:00Z"),
                t("2026-06-02T00:00:00Z")
            ),
            Err(IcsError::NotCalendar)
        );
        assert_eq!(
            busy_intervals(
                SAMPLE.as_bytes(),
                "UTC",
                t("2026-01-01T00:00:00Z"),
                t("2027-01-01T00:00:00Z")
            ),
            Err(IcsError::HorizonTooLong(MAX_HORIZON_DAYS))
        );
        let mut many = String::from("BEGIN:VCALENDAR\r\n");
        for i in 0..(MAX_EVENTS + 1) {
            many.push_str(&format!(
                "BEGIN:VEVENT\r\nUID:{i}\r\nDTSTART:20260601T000000Z\r\nDURATION:PT1H\r\nEND:VEVENT\r\n"
            ));
        }
        many.push_str("END:VCALENDAR\r\n");
        assert_eq!(
            busy_intervals(
                many.as_bytes(),
                "UTC",
                t("2026-06-01T00:00:00Z"),
                t("2026-06-02T00:00:00Z")
            ),
            Err(IcsError::TooManyEvents(MAX_EVENTS))
        );
        // Unbounded hostile recurrence is capped per event and by horizon.
        let hostile = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nDTSTART:20260601T000000Z\r\nDURATION:PT1M\r\nRRULE:FREQ=DAILY;INTERVAL=1\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let out = busy_intervals(
            hostile.as_bytes(),
            "UTC",
            t("2026-06-01T00:00:00Z"),
            t("2026-11-01T00:00:00Z"),
        )
        .unwrap();
        assert!(out.intervals.len() <= MAX_OCCURRENCES_PER_EVENT);
        assert!(out.intervals.len() >= 150);
    }

    #[test]
    fn malformed_lines_are_ignored_not_fatal() {
        let junk = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nDTSTART:not-a-date\r\nRRULE:FREQ=BOGUS\r\nDURATION:P99999999D\r\nEND:VEVENT\r\nBEGIN:VEVENT\r\nDTSTART:20260601T100000Z\r\nDTEND:20260601T090000Z\r\nEND:VEVENT\r\nGARBAGE LINE WITHOUT COLON\r\nEND:VCALENDAR\r\n";
        let out = busy_intervals(
            junk.as_bytes(),
            "UTC",
            t("2026-06-01T00:00:00Z"),
            t("2026-06-02T00:00:00Z"),
        )
        .unwrap();
        assert!(out.intervals.is_empty());
        assert_eq!(out.events_skipped, 2);
    }

    #[test]
    fn weekly_byday_and_until() {
        let ics = "BEGIN:VCALENDAR\r\nBEGIN:VEVENT\r\nDTSTART:20260601T080000Z\r\nDTEND:20260601T090000Z\r\nRRULE:FREQ=WEEKLY;BYDAY=MO,WE;UNTIL=20260610T000000Z\r\nEND:VEVENT\r\nEND:VCALENDAR\r\n";
        let out = busy_intervals(
            ics.as_bytes(),
            "UTC",
            t("2026-06-01T00:00:00Z"),
            t("2026-07-01T00:00:00Z"),
        )
        .unwrap();
        let starts: Vec<_> = out.intervals.iter().map(|i| i.start.to_rfc3339()).collect();
        assert_eq!(
            starts,
            vec![
                "2026-06-01T08:00:00+00:00",
                "2026-06-03T08:00:00+00:00",
                "2026-06-08T08:00:00+00:00"
            ]
        );
    }

    #[test]
    fn appointment_event_is_minimal_and_folded() {
        let ics = appointment_event(
            "appt-1@wellos",
            t("2026-06-01T08:00:00Z"),
            t("2026-06-01T08:20:00Z"),
            "Consulta de medicina general; con acentos",
            "Centro de Salud, Ibiza",
            "Bring your identification and arrive ten minutes early. Please avoid eating for two hours before the appointment.",
            t("2026-05-20T10:00:00Z"),
        );
        assert!(ics.contains("DTSTART:20260601T080000Z\r\n"));
        assert!(ics.contains("SUMMARY:Consulta de medicina general\\; con acentos"));
        assert!(ics.lines().all(|l| l.len() <= 75));
        assert!(ics.contains("\r\n ")); // folded continuation present
        let parsed = busy_intervals(
            ics.as_bytes(),
            "UTC",
            t("2026-06-01T00:00:00Z"),
            t("2026-06-02T00:00:00Z"),
        )
        .unwrap();
        assert_eq!(parsed.intervals.len(), 1);
    }
}
