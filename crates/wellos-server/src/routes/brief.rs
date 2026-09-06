//! Patient Brief and diagnostic history for the consultation workspace.
//!
//! Both are read inside the workspace's consistent snapshot. Objective
//! measurements and calculations (values, reference-range flags, trend
//! direction) are computed here; the dMind commentary is a separate,
//! clearly-labelled assistive layer produced from those facts only.

use dmind_gateway::trends::{self, Direction, SeriesFacts};
use rust_decimal::Decimal;
use serde_json::{json, Value};
use sqlx::{PgConnection, Row};
use std::collections::BTreeMap;
use uuid::Uuid;
use wellos_domain::units::{convert, Quantity};

/// Follow-up task statuses that still require action. `completed` and
/// `superseded` tasks are history, never outstanding work. Shared by the
/// Patient Brief and the dashboard cockpit so both agree on what is pending.
pub(crate) const ACTIONABLE_TASK_STATUSES: &str = "('open','overdue')";

/// Ordering for outstanding tasks: overdue first, then urgent/high priority,
/// then soonest due. `alias` prefixes column names (e.g. `"t."`).
pub(crate) fn task_order(alias: &str) -> String {
    format!(
        "({alias}status = 'overdue') DESC, ({alias}priority IN ('urgent','high')) DESC, \
         {alias}due_at ASC NULLS LAST, {alias}created_at ASC"
    )
}

/// Parse a reference range rendered as `low-high` optionally followed by a
/// unit (e.g. `70-99 mg/dL`). Anything else is treated as not parseable.
pub(crate) fn parse_reference_range(range: &str) -> Option<(Decimal, Decimal)> {
    let numeric = range.split_whitespace().next()?;
    let (low, high) = numeric.split_once('-')?;
    let low: Decimal = low.trim().parse().ok()?;
    let high: Decimal = high.trim().parse().ok()?;
    (low <= high).then_some((low, high))
}

pub(crate) fn abnormal_flag(value: Decimal, range: Option<&str>) -> Option<&'static str> {
    let (low, high) = parse_reference_range(range?)?;
    if value < low {
        Some("low")
    } else if value > high {
        Some("high")
    } else {
        None
    }
}

/// Direction over the last three comparable, non-superseded results. Two
/// consecutive moves in the same direction are required for rising/falling;
/// otherwise the series is reported as stable. Values must already be
/// expressed in one unit; if any result of the series could not be
/// converted, no direction is computed at all.
pub(crate) fn direction(values: &[Decimal], incomparable: usize) -> Direction {
    if incomparable > 0 {
        return Direction::MixedUnits;
    }
    if values.len() < 2 {
        return Direction::Insufficient;
    }
    let tail: Vec<Decimal> = values.iter().rev().take(3).rev().copied().collect();
    let deltas: Vec<Decimal> = tail.windows(2).map(|w| w[1] - w[0]).collect();
    if deltas.iter().all(|d| *d > Decimal::ZERO) {
        Direction::Rising
    } else if deltas.iter().all(|d| *d < Decimal::ZERO) {
        Direction::Falling
    } else {
        Direction::Stable
    }
}

struct Series {
    display: String,
    /// Unit every comparable value is expressed in: the unit of the most
    /// recent non-superseded result, so the latest value and its reference
    /// range are always shown exactly as reported.
    unit: String,
    results: Vec<Value>,
    values: Vec<Decimal>,
    refs: Vec<String>,
    incomparable: usize,
    latest_value: Option<Decimal>,
    latest_range: Option<String>,
    pending: Vec<Value>,
}

impl Series {
    fn new(display: String, unit: String) -> Self {
        Series {
            display,
            unit,
            results: Vec::new(),
            values: Vec::new(),
            refs: Vec::new(),
            incomparable: 0,
            latest_value: None,
            latest_range: None,
            pending: Vec::new(),
        }
    }
}

/// Express `value unit` in the series unit for `code`. `None` when no exact
/// conversion is known: such a result is shown as reported but excluded from
/// the trend instead of being compared as a raw number.
pub(crate) fn normalize(
    value: Decimal,
    unit: &str,
    series_unit: &str,
    code: &str,
) -> Option<Decimal> {
    convert(
        &Quantity {
            value,
            unit: unit.to_string(),
        },
        series_unit,
        code,
    )
    .ok()
    .map(|q| q.value.round_dp(2).normalize())
}

/// Grouped diagnostic history for one patient with deterministic dMind
/// commentary. `language` selects the commentary language only.
pub(crate) async fn diagnostic_history(
    tx: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
    language: &str,
) -> Result<Value, sqlx::Error> {
    let rows = sqlx::query(
        "SELECT o.id, o.code_loinc, sr.display, o.value_num, o.unit, o.reference_range,
                o.status, o.effective_at, o.received_at, sr.id AS service_request_id,
                sr.loop_state,
                EXISTS (SELECT 1 FROM observations x WHERE x.amends = o.id) AS superseded,
                EXISTS (SELECT 1 FROM rule_evaluations re
                        WHERE re.observation_id = o.id
                          AND re.outcome->>'outcome' = 'critical') AS critical
         FROM observations o JOIN service_requests sr ON sr.id = o.service_request_id
         WHERE o.tenant_id = $1 AND o.patient_id = $2
         ORDER BY o.effective_at ASC, o.received_at ASC",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *tx)
    .await?;
    let pending_rows = sqlx::query(
        "SELECT sr.id, sr.code_loinc, sr.display, sr.created_at, sr.loop_state
         FROM service_requests sr
         WHERE sr.tenant_id = $1 AND sr.patient_id = $2
           AND NOT EXISTS (SELECT 1 FROM observations o WHERE o.service_request_id = sr.id)
         ORDER BY sr.created_at DESC",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *tx)
    .await?;

    // Series unit per analyte: the unit of the newest non-superseded result
    // (falling back to the newest row of any kind). Rows are oldest-first, so
    // a later row overrides an earlier one.
    let mut series_units: BTreeMap<String, (String, bool)> = BTreeMap::new();
    for r in &rows {
        let code: String = r.get("code_loinc");
        let unit: String = r.get("unit");
        let superseded: bool = r.get("superseded");
        match series_units.get(&code) {
            Some((_, true)) if superseded => {}
            _ => {
                series_units.insert(code, (unit, !superseded));
            }
        }
    }

    let mut groups: BTreeMap<String, Series> = BTreeMap::new();
    for r in &rows {
        let code: String = r.get("code_loinc");
        let value: Decimal = r.get("value_num");
        let unit: String = r.get("unit");
        let range: Option<String> = r.get("reference_range");
        let superseded: bool = r.get("superseded");
        let id: Uuid = r.get("id");
        let flag = abnormal_flag(value, range.as_deref());
        let series_unit = series_units
            .get(&code)
            .map(|(u, _)| u.clone())
            .unwrap_or_else(|| unit.clone());
        let normalized = normalize(value, &unit, &series_unit, &code);
        let entry = groups
            .entry(code)
            .or_insert_with(|| Series::new(r.get("display"), series_unit.clone()));
        entry.results.push(json!({
            "id": id,
            "service_request_id": r.get::<Uuid,_>("service_request_id"),
            "value": value,
            "unit": unit,
            "normalized_value": normalized,
            "comparable": normalized.is_some(),
            "reference_range": range,
            "abnormal": flag,
            "critical": r.get::<bool,_>("critical"),
            "status": r.get::<String,_>("status"),
            "superseded": superseded,
            "loop_state": r.get::<String,_>("loop_state"),
            "effective_at": r.get::<chrono::DateTime<chrono::Utc>,_>("effective_at"),
            "received_at": r.get::<chrono::DateTime<chrono::Utc>,_>("received_at"),
        }));
        if !superseded {
            match normalized {
                Some(v) => {
                    entry.values.push(v);
                    entry.refs.push(format!("observation:{id}"));
                    entry.latest_value = Some(v);
                    entry.latest_range = range;
                }
                None => entry.incomparable += 1,
            }
        }
    }
    for r in &pending_rows {
        let code: String = r.get("code_loinc");
        let entry = groups
            .entry(code)
            .or_insert_with(|| Series::new(r.get("display"), String::new()));
        entry.pending.push(json!({
            "id": r.get::<Uuid,_>("id"),
            "display": r.get::<String,_>("display"),
            "loop_state": r.get::<String,_>("loop_state"),
            "created_at": r.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
        }));
    }

    let mut facts = Vec::new();
    let mut tests = Vec::new();
    for (code, s) in &groups {
        let dir = direction(&s.values, s.incomparable);
        let latest_abnormal = s
            .latest_value
            .and_then(|v| abnormal_flag(v, s.latest_range.as_deref()));
        facts.push(SeriesFacts {
            code: code.clone(),
            display: s.display.clone(),
            unit: s.unit.clone(),
            observation_refs: s.refs.clone(),
            latest_value: s
                .latest_value
                .map(|v| v.normalize().to_string())
                .unwrap_or_default(),
            latest_abnormal: latest_abnormal.map(str::to_string),
            direction: dir,
            result_count: s.values.len() + s.incomparable,
            incomparable_count: s.incomparable,
            pending_count: s.pending.len(),
        });
        tests.push(json!({
            "code": code,
            "display": s.display,
            "unit": s.unit,
            "reference_range": s.latest_range,
            "results": s.results,
            "pending": s.pending,
            "pending_count": s.pending.len(),
            "latest_value": s.latest_value,
            "latest_abnormal": latest_abnormal,
            "direction": dir,
            "result_count": s.values.len() + s.incomparable,
            "incomparable_count": s.incomparable,
        }));
    }
    let analysis = trends::analyze(&facts, language);
    Ok(json!({
        "tests": tests,
        "analysis": serde_json::to_value(&analysis).unwrap_or(Value::Null),
        "generated_at": chrono::Utc::now(),
    }))
}

/// Cross-encounter context the clinician needs before talking to the
/// patient: recent signed notes, outstanding follow-up, recent abnormal
/// results. Allergies, medications, problems, alerts and vitals are already
/// part of the workspace payload and are not duplicated here.
pub(crate) async fn patient_brief(
    tx: &mut PgConnection,
    tenant_id: Uuid,
    patient_id: Uuid,
    encounter_id: Uuid,
) -> Result<Value, sqlx::Error> {
    let recent_notes = sqlx::query(
        "SELECT e.id AS encounter_id, e.started_at, n.signed_at, n.reason_for_encounter,
                n.assessment, n.plan, u.display_name AS signed_by
         FROM encounter_notes n
         JOIN encounters e ON e.id = n.encounter_id
         LEFT JOIN users u ON u.id = n.signed_by
         WHERE n.tenant_id = $1 AND n.patient_id = $2 AND n.status = 'signed'
           AND n.encounter_id <> $3
         ORDER BY n.signed_at DESC LIMIT 3",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .bind(encounter_id)
    .fetch_all(&mut *tx)
    .await?
    .iter()
    .map(|r| {
        json!({
            "encounter_id": r.get::<Uuid,_>("encounter_id"),
            "started_at": r.get::<chrono::DateTime<chrono::Utc>,_>("started_at"),
            "signed_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("signed_at"),
            "signed_by": r.get::<Option<String>,_>("signed_by"),
            "reason_for_encounter": r.get::<Option<String>,_>("reason_for_encounter"),
            "assessment": r.get::<Option<String>,_>("assessment"),
            "plan": r.get::<Option<String>,_>("plan"),
        })
    })
    .collect::<Vec<_>>();

    let open_tasks = sqlx::query(&format!(
        "SELECT id, description, priority, status, due_at, service_request_id
         FROM follow_up_tasks
         WHERE tenant_id = $1 AND patient_id = $2 AND status IN {ACTIONABLE_TASK_STATUSES}
         ORDER BY {} LIMIT 10",
        task_order("")
    ))
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *tx)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "description": r.get::<String,_>("description"),
            "priority": r.get::<String,_>("priority"),
            "status": r.get::<String,_>("status"),
            "due_at": r.get::<Option<chrono::DateTime<chrono::Utc>>,_>("due_at"),
            "service_request_id": r.get::<Uuid,_>("service_request_id"),
        })
    })
    .collect::<Vec<_>>();

    let open_requests = sqlx::query(
        "SELECT id, display, loop_state, created_at, encounter_id
         FROM service_requests
         WHERE tenant_id = $1 AND patient_id = $2 AND loop_state <> 'closed'
         ORDER BY created_at DESC LIMIT 10",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *tx)
    .await?
    .iter()
    .map(|r| {
        json!({
            "id": r.get::<Uuid,_>("id"),
            "display": r.get::<String,_>("display"),
            "loop_state": r.get::<String,_>("loop_state"),
            "created_at": r.get::<chrono::DateTime<chrono::Utc>,_>("created_at"),
            "this_encounter": r.get::<Uuid,_>("encounter_id") == encounter_id,
        })
    })
    .collect::<Vec<_>>();

    // Recent results outside their reference range (latest non-superseded
    // row per request), for the brief's "recent abnormal results" list.
    let abnormal = sqlx::query(
        "SELECT o.id, o.code_loinc, sr.display, o.value_num, o.unit, o.reference_range,
                o.effective_at, sr.id AS service_request_id,
                EXISTS (SELECT 1 FROM rule_evaluations re
                        WHERE re.observation_id = o.id
                          AND re.outcome->>'outcome' = 'critical') AS critical
         FROM observations o JOIN service_requests sr ON sr.id = o.service_request_id
         WHERE o.tenant_id = $1 AND o.patient_id = $2
           AND NOT EXISTS (SELECT 1 FROM observations x WHERE x.amends = o.id)
         ORDER BY o.effective_at DESC LIMIT 20",
    )
    .bind(tenant_id)
    .bind(patient_id)
    .fetch_all(&mut *tx)
    .await?
    .iter()
    .filter_map(|r| {
        let value: Decimal = r.get("value_num");
        let range: Option<String> = r.get("reference_range");
        let flag = abnormal_flag(value, range.as_deref())?;
        Some(json!({
            "id": r.get::<Uuid,_>("id"),
            "service_request_id": r.get::<Uuid,_>("service_request_id"),
            "code": r.get::<String,_>("code_loinc"),
            "display": r.get::<String,_>("display"),
            "value": value,
            "unit": r.get::<String,_>("unit"),
            "reference_range": range,
            "abnormal": flag,
            "critical": r.get::<bool,_>("critical"),
            "effective_at": r.get::<chrono::DateTime<chrono::Utc>,_>("effective_at"),
        }))
    })
    .take(5)
    .collect::<Vec<_>>();

    Ok(json!({
        "recent_notes": recent_notes,
        "open_tasks": open_tasks,
        "open_requests": open_requests,
        "recent_abnormal": abnormal,
        "generated_at": chrono::Utc::now(),
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(s: &str) -> Decimal {
        s.parse().unwrap()
    }

    #[test]
    fn parses_reference_ranges_with_units() {
        assert_eq!(
            parse_reference_range("70-99 mg/dL"),
            Some((d("70"), d("99")))
        );
        assert_eq!(
            parse_reference_range("3.5-5.1 mmol/L"),
            Some((d("3.5"), d("5.1")))
        );
        assert_eq!(parse_reference_range("<5"), None);
        assert_eq!(parse_reference_range("99-70"), None);
    }

    #[test]
    fn flags_high_low_and_within() {
        assert_eq!(abnormal_flag(d("134"), Some("70-99 mg/dL")), Some("high"));
        assert_eq!(abnormal_flag(d("38"), Some("70-99 mg/dL")), Some("low"));
        assert_eq!(abnormal_flag(d("85"), Some("70-99 mg/dL")), None);
        assert_eq!(abnormal_flag(d("85"), None), None);
    }

    #[test]
    fn direction_uses_last_three_results() {
        assert_eq!(direction(&[d("118")], 0), Direction::Insufficient);
        assert_eq!(
            direction(&[d("118"), d("126"), d("134")], 0),
            Direction::Rising
        );
        assert_eq!(
            direction(&[d("5"), d("4.5"), d("4.1")], 0),
            Direction::Falling
        );
        assert_eq!(
            direction(&[d("5"), d("4.5"), d("4.7")], 0),
            Direction::Stable
        );
        // Older history does not mask the recent direction.
        assert_eq!(
            direction(&[d("200"), d("118"), d("126"), d("134")], 0),
            Direction::Rising
        );
        // One incomparable result suppresses the trend entirely.
        assert_eq!(
            direction(&[d("118"), d("126"), d("134")], 1),
            Direction::MixedUnits
        );
    }

    #[test]
    fn normalizes_known_conversions_and_refuses_unknown_ones() {
        // Glucose 6.1 mmol/L expressed in the series unit mg/dL.
        let mgdl = normalize(d("6.1"), "mmol/L", "mg/dL", "2345-7").unwrap();
        assert!(mgdl > d("109") && mgdl < d("110.5"), "{mgdl}");
        // Identity conversion keeps the value.
        assert_eq!(
            normalize(d("134"), "mg/dL", "mg/dL", "2345-7"),
            Some(d("134"))
        );
        // Potassium mmol/L <-> meq/L is 1:1.
        assert_eq!(
            normalize(d("5.9"), "meq/L", "mmol/L", "2823-3"),
            Some(d("5.9"))
        );
        // Unknown units are never compared as raw numbers.
        assert_eq!(normalize(d("7.7"), "furlongs", "mmol/L", "2823-3"), None);
        assert_eq!(normalize(d("100"), "mg/dL", "mmol/L", "2823-3"), None);
    }
}
