//! `capacity-forecast.v1`: an explainable, deterministic demand/capacity
//! forecast per facility and service. Inputs are aggregated facts the
//! server assembles; the output states expected demand, available
//! capacity, the gap, a confidence band and the contributing factors, or
//! `insufficient_history` when the evidence cannot support a forecast.
//!
//! The forecast only *describes*. It never cancels care, removes
//! availability or changes priority; recommendations are advisory strings.

use chrono::{Datelike, Duration, NaiveDate, Weekday};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

pub const FORECAST_VERSION: &str = "capacity-forecast.v1";
/// Minimum number of historical weeks with any demand before a forecast is
/// attempted.
pub const MIN_HISTORY_WEEKS: usize = 4;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DailyHistory {
    pub date: NaiveDate,
    /// Access requests + booked appointments requested for that day.
    pub demand: i32,
    pub cancellations: i32,
    pub no_shows: i32,
    /// Mean lead time of cancellations in hours (None if none).
    pub cancellation_lead_hours: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct CalendarEffect {
    pub date: NaiveDate,
    pub name: String,
    pub kind: String,
    pub demand_multiplier: f64,
    pub capacity_multiplier: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct PlannedCapacity {
    pub date: NaiveDate,
    /// Bookable appointment slots after availability rules and exceptions.
    pub slots: i32,
    pub confirmed: i32,
    /// Slots removed by leave/sickness/closure exceptions that day.
    pub exception_slots_lost: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ForecastInputs {
    pub history: Vec<DailyHistory>,
    pub planned: Vec<PlannedCapacity>,
    pub calendar: Vec<CalendarEffect>,
    pub horizon_start: NaiveDate,
    pub horizon_end: NaiveDate,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DayForecast {
    pub date: NaiveDate,
    pub expected_demand: f64,
    pub available_capacity: f64,
    /// Positive = surplus capacity, negative = shortfall.
    pub gap: f64,
    pub demand_low: f64,
    pub demand_high: f64,
    pub factors: Vec<Factor>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Factor {
    pub code: String,
    pub effect: f64,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Forecast {
    Ready {
        version: String,
        days: Vec<DayForecast>,
        confidence: f64,
        history_weeks: usize,
        recommendations: Vec<String>,
        pressure_days: Vec<NaiveDate>,
    },
    InsufficientHistory {
        version: String,
        history_weeks: usize,
        required_weeks: usize,
        detail: String,
    },
}

fn weekday_index(w: Weekday) -> usize {
    w.number_from_monday() as usize - 1
}

pub fn forecast(inputs: &ForecastInputs) -> Forecast {
    let mut weeks = std::collections::BTreeSet::new();
    for h in &inputs.history {
        if h.demand > 0 || h.cancellations > 0 || h.no_shows > 0 {
            weeks.insert(h.date.iso_week());
        }
    }
    let history_weeks = weeks.len();
    if history_weeks < MIN_HISTORY_WEEKS {
        return Forecast::InsufficientHistory {
            version: FORECAST_VERSION.to_string(),
            history_weeks,
            required_weeks: MIN_HISTORY_WEEKS,
            detail: format!(
                "{history_weeks} week(s) of demand history available; {MIN_HISTORY_WEEKS} required"
            ),
        };
    }

    // Baseline demand per weekday (mean) and its dispersion, seasonality by
    // month relative to overall mean.
    let mut by_weekday: [Vec<f64>; 7] = Default::default();
    let mut by_month: BTreeMap<u32, Vec<f64>> = BTreeMap::new();
    let mut total_demand = 0.0;
    let mut total_cancel = 0.0;
    let mut total_noshow = 0.0;
    let mut lead_sum = 0.0;
    let mut lead_n = 0.0;
    for h in &inputs.history {
        by_weekday[weekday_index(h.date.weekday())].push(h.demand as f64);
        by_month
            .entry(h.date.month())
            .or_default()
            .push(h.demand as f64);
        total_demand += h.demand as f64;
        total_cancel += h.cancellations as f64;
        total_noshow += h.no_shows as f64;
        if let Some(l) = h.cancellation_lead_hours {
            lead_sum += l * h.cancellations as f64;
            lead_n += h.cancellations as f64;
        }
    }
    let overall_mean = if inputs.history.is_empty() {
        0.0
    } else {
        total_demand / inputs.history.len() as f64
    };
    let cancel_rate = if total_demand > 0.0 {
        total_cancel / total_demand
    } else {
        0.0
    };
    let noshow_rate = if total_demand > 0.0 {
        total_noshow / total_demand
    } else {
        0.0
    };
    let mean_lead = if lead_n > 0.0 { lead_sum / lead_n } else { 0.0 };

    let mean = |v: &[f64]| {
        if v.is_empty() {
            None
        } else {
            Some(v.iter().sum::<f64>() / v.len() as f64)
        }
    };
    let stddev = |v: &[f64], m: f64| {
        if v.len() < 2 {
            m.sqrt().max(1.0)
        } else {
            (v.iter().map(|x| (x - m).powi(2)).sum::<f64>() / (v.len() - 1) as f64).sqrt()
        }
    };

    let mut days = Vec::new();
    let mut pressure_days = Vec::new();
    let mut date = inputs.horizon_start;
    while date <= inputs.horizon_end {
        let wd = weekday_index(date.weekday());
        let base = mean(&by_weekday[wd]).unwrap_or(overall_mean);
        let sd = stddev(&by_weekday[wd], base);
        let mut factors = vec![Factor {
            code: "weekday_baseline".into(),
            effect: base,
            detail: format!(
                "mean demand on {} over {} observed day(s)",
                date.weekday(),
                by_weekday[wd].len()
            ),
        }];
        let mut expected = base;
        // Same-month seasonality from history, when we have any.
        if let Some(mm) = by_month.get(&date.month()).and_then(|v| mean(v)) {
            if overall_mean > 0.0 {
                let ratio = (mm / overall_mean).clamp(0.25, 4.0);
                expected *= ratio;
                factors.push(Factor {
                    code: "month_seasonality".into(),
                    effect: ratio,
                    detail: format!("month {} demand x{ratio:.2} vs overall mean", date.month()),
                });
            }
        }
        let mut capacity_mult = 1.0;
        for ev in inputs.calendar.iter().filter(|e| e.date == date) {
            expected *= ev.demand_multiplier;
            capacity_mult *= ev.capacity_multiplier;
            factors.push(Factor {
                code: format!("calendar:{}", ev.kind),
                effect: ev.demand_multiplier,
                detail: format!(
                    "{}: demand x{:.2}, capacity x{:.2}",
                    ev.name, ev.demand_multiplier, ev.capacity_multiplier
                ),
            });
        }
        let planned = inputs.planned.iter().find(|p| p.date == date);
        let slots = planned.map(|p| p.slots as f64).unwrap_or(0.0);
        let confirmed = planned.map(|p| p.confirmed as f64).unwrap_or(0.0);
        let lost = planned.map(|p| p.exception_slots_lost).unwrap_or(0);
        if lost > 0 {
            factors.push(Factor {
                code: "resource_exceptions".into(),
                effect: -(lost as f64),
                detail: format!("{lost} slot(s) removed by leave, sickness or closures"),
            });
        }
        // Expected reopenings from cancellations that will not be refilled.
        let expected_cancellations = confirmed * cancel_rate;
        if expected_cancellations > 0.0 {
            factors.push(Factor {
                code: "cancellation_rate".into(),
                effect: expected_cancellations,
                detail: format!(
                    "historical cancellation rate {:.0}% (mean lead {:.0}h)",
                    cancel_rate * 100.0,
                    mean_lead
                ),
            });
        }
        if noshow_rate > 0.0 {
            factors.push(Factor {
                code: "no_show_rate".into(),
                effect: noshow_rate,
                detail: format!(
                    "historical no-show rate {:.0}%; supports reminder intensity only",
                    noshow_rate * 100.0
                ),
            });
        }
        let available = ((slots * capacity_mult) - confirmed + expected_cancellations).max(0.0);
        let gap = available - expected;
        if gap < 0.0 && slots > 0.0 {
            pressure_days.push(date);
        }
        days.push(DayForecast {
            date,
            expected_demand: (expected * 10.0).round() / 10.0,
            available_capacity: (available * 10.0).round() / 10.0,
            gap: (gap * 10.0).round() / 10.0,
            demand_low: ((expected - 1.28 * sd).max(0.0) * 10.0).round() / 10.0,
            demand_high: ((expected + 1.28 * sd) * 10.0).round() / 10.0,
            factors,
        });
        date += Duration::days(1);
    }

    let confidence = ((history_weeks as f64 / 26.0).min(1.0) * 0.7 + 0.2).min(0.9);
    let mut recommendations = Vec::new();
    if !pressure_days.is_empty() {
        recommendations.push(format!(
            "expected shortfall on {} day(s); consider opening additional capacity or telehealth sessions",
            pressure_days.len()
        ));
    }
    if noshow_rate > 0.1 {
        recommendations.push(
            "no-show rate above 10%; consider an additional reminder and confirmation request"
                .into(),
        );
    }
    if cancel_rate > 0.15 {
        recommendations
            .push("cancellation rate above 15%; keep the waitlist enabled for this service".into());
    }
    Forecast::Ready {
        version: FORECAST_VERSION.to_string(),
        days,
        confidence: (confidence * 100.0).round() / 100.0,
        history_weeks,
        recommendations,
        pressure_days,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d(y: i32, m: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, day).unwrap()
    }

    fn history(weeks: usize, base: i32) -> Vec<DailyHistory> {
        let start = d(2026, 1, 5); // Monday
        (0..weeks * 7)
            .map(|i| {
                let date = start + Duration::days(i as i64);
                let weekend = date.weekday().number_from_monday() >= 6;
                DailyHistory {
                    date,
                    demand: if weekend { 0 } else { base + (i % 3) as i32 },
                    cancellations: if weekend { 0 } else { 1 },
                    no_shows: 0,
                    cancellation_lead_hours: Some(30.0),
                }
            })
            .collect()
    }

    #[test]
    fn insufficient_history_is_explicit() {
        let f = forecast(&ForecastInputs {
            history: history(2, 10),
            planned: vec![],
            calendar: vec![],
            horizon_start: d(2026, 3, 2),
            horizon_end: d(2026, 3, 8),
        });
        assert!(matches!(
            f,
            Forecast::InsufficientHistory {
                history_weeks: 2,
                required_weeks: 4,
                ..
            }
        ));
    }

    #[test]
    fn calendar_events_and_exceptions_drive_pressure() {
        let planned: Vec<PlannedCapacity> = (0..7)
            .map(|i| PlannedCapacity {
                date: d(2026, 8, 3) + Duration::days(i),
                slots: 12,
                confirmed: 6,
                exception_slots_lost: if i == 2 { 6 } else { 0 },
            })
            .collect();
        let f = forecast(&ForecastInputs {
            history: history(8, 8),
            planned,
            calendar: vec![CalendarEffect {
                date: d(2026, 8, 5),
                name: "Summer peak".into(),
                kind: "seasonal_period".into(),
                demand_multiplier: 1.8,
                capacity_multiplier: 1.0,
            }],
            horizon_start: d(2026, 8, 3),
            horizon_end: d(2026, 8, 9),
        });
        let Forecast::Ready {
            days,
            pressure_days,
            recommendations,
            ..
        } = f
        else {
            panic!("expected ready forecast");
        };
        let peak = days.iter().find(|x| x.date == d(2026, 8, 5)).unwrap();
        let normal = days.iter().find(|x| x.date == d(2026, 8, 4)).unwrap();
        assert!(peak.expected_demand > normal.expected_demand);
        assert!(peak
            .factors
            .iter()
            .any(|f| f.code == "calendar:seasonal_period"));
        assert!(peak.factors.iter().any(|f| f.code == "resource_exceptions"));
        assert!(pressure_days.contains(&d(2026, 8, 5)));
        assert!(!recommendations.is_empty());
        assert!(
            peak.demand_low <= peak.expected_demand && peak.expected_demand <= peak.demand_high
        );
    }
}
