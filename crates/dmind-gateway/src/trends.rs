//! Deterministic, offline dMind trend commentary for diagnostic history.
//!
//! The server computes the objective facts (values, reference-range flags,
//! direction) and passes them here; this module only turns those facts into
//! cautious, clearly-labelled assistive statements. It never adds
//! information that is not in the facts, never suggests a diagnosis, an
//! order or a treatment, and always lists the facts it used.

use serde::{Deserialize, Serialize};
use wellos_domain::ai::ProviderInfo;

/// Direction of the most recent results, computed by the caller from the
/// objective series (never by the model).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Direction {
    Rising,
    Falling,
    Stable,
    Insufficient,
    /// Results were recorded in units that cannot be converted to the
    /// series unit; no direction is computed over incomparable numbers.
    MixedUnits,
}

/// Objective facts for one analyte series.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SeriesFacts {
    /// LOINC code of the analyte.
    pub code: String,
    pub display: String,
    pub unit: String,
    /// `observation:<id>` references of the results considered, oldest first.
    pub observation_refs: Vec<String>,
    /// Rendered latest value (e.g. "134").
    pub latest_value: String,
    /// `Some("high")` / `Some("low")` when the latest result lies outside
    /// its reference range; `None` when inside or when no range exists.
    pub latest_abnormal: Option<String>,
    pub direction: Direction,
    /// Results in the series (excluding superseded rows).
    pub result_count: usize,
    /// Non-superseded results whose unit could not be converted to `unit`
    /// and were therefore left out of the direction calculation.
    #[serde(default)]
    pub incomparable_count: usize,
    /// Requests ordered but without a result yet.
    pub pending_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendStatement {
    pub code: String,
    pub text: String,
    /// Fact references used to produce this statement.
    pub facts: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TrendAnalysis {
    pub schema_version: String,
    pub provider: ProviderInfo,
    pub language: String,
    pub statements: Vec<TrendStatement>,
    pub limitations: Vec<String>,
}

pub const TREND_SCHEMA: &str = "diagnostic-trends.v1";

pub fn trend_info() -> ProviderInfo {
    ProviderInfo {
        provider: "dmind-fake".into(),
        model: "trend-commentary".into(),
        model_version: "0.1.0".into(),
    }
}

fn lang(language: &str) -> &'static str {
    if language.starts_with("es") {
        "es"
    } else {
        "en"
    }
}

/// Produce assistive commentary from objective series facts. Deterministic:
/// identical facts always yield identical text.
pub fn analyze(series: &[SeriesFacts], language: &str) -> TrendAnalysis {
    let l = lang(language);
    let mut statements = Vec::new();
    for s in series {
        if s.result_count == 0 {
            if s.pending_count > 0 {
                statements.push(TrendStatement {
                    code: s.code.clone(),
                    text: match l {
                        "es" => format!(
                            "{}: sin resultados registrados; {} solicitud(es) pendiente(s).",
                            s.display, s.pending_count
                        ),
                        _ => format!(
                            "{}: no recorded results; {} request(s) pending.",
                            s.display, s.pending_count
                        ),
                    },
                    facts: Vec::new(),
                });
            }
            continue;
        }
        let abnormal = match (l, s.latest_abnormal.as_deref()) {
            ("es", Some("high")) => " (por encima del rango de referencia)",
            ("es", Some("low")) => " (por debajo del rango de referencia)",
            ("es", _) => "",
            (_, Some("high")) => " (above the reference range)",
            (_, Some("low")) => " (below the reference range)",
            _ => "",
        };
        let direction = match (l, s.direction) {
            ("es", Direction::Rising) => "en ascenso en los resultados recientes",
            ("es", Direction::Falling) => "en descenso en los resultados recientes",
            ("es", Direction::Stable) => "estable en los resultados recientes",
            ("es", Direction::Insufficient) => "menos de tres resultados; sin tendencia calculable",
            ("es", Direction::MixedUnits) => {
                "tendencia no calculada: resultados en unidades no convertibles"
            }
            (_, Direction::Rising) => "rising across recent results",
            (_, Direction::Falling) => "falling across recent results",
            (_, Direction::Stable) => "stable across recent results",
            (_, Direction::Insufficient) => "fewer than three results; no trend can be calculated",
            (_, Direction::MixedUnits) => "trend not calculated: results in non-convertible units",
        };
        let incomparable = if s.incomparable_count > 0 {
            match l {
                "es" => format!(
                    " {} resultado(s) en otra unidad no comparable con {}.",
                    s.incomparable_count, s.unit
                ),
                _ => format!(
                    " {} result(s) in another unit not comparable with {}.",
                    s.incomparable_count, s.unit
                ),
            }
        } else {
            String::new()
        };
        let pending = if s.pending_count > 0 {
            match l {
                "es" => format!(" {} solicitud(es) pendiente(s).", s.pending_count),
                _ => format!(" {} request(s) pending.", s.pending_count),
            }
        } else {
            String::new()
        };
        let text = match l {
            "es" => format!(
                "{}: último valor {} {}{}; {} ({} resultado(s)).{}{}",
                s.display,
                s.latest_value,
                s.unit,
                abnormal,
                direction,
                s.result_count,
                incomparable,
                pending
            ),
            _ => format!(
                "{}: latest {} {}{}; {} ({} result(s)).{}{}",
                s.display,
                s.latest_value,
                s.unit,
                abnormal,
                direction,
                s.result_count,
                incomparable,
                pending
            ),
        };
        statements.push(TrendStatement {
            code: s.code.clone(),
            text,
            facts: s.observation_refs.clone(),
        });
    }
    let limitations = match l {
        "es" => vec![
            "Comentario asistencial generado de forma determinista a partir de los resultados registrados; no es un diagnóstico ni una recomendación.".to_string(),
            "La dirección de la tendencia se calcula sobre los últimos resultados no reemplazados; no considera medicación ni contexto clínico.".to_string(),
            "Debe ser revisado por el profesional responsable.".to_string(),
        ],
        _ => vec![
            "Assistive commentary generated deterministically from recorded results; not a diagnosis or a recommendation.".to_string(),
            "Trend direction is computed over the latest non-superseded results; medication and clinical context are not considered.".to_string(),
            "Must be reviewed by the responsible clinician.".to_string(),
        ],
    };
    TrendAnalysis {
        schema_version: TREND_SCHEMA.into(),
        provider: trend_info(),
        language: l.into(),
        statements,
        limitations,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn glucose(direction: Direction, abnormal: Option<&str>) -> SeriesFacts {
        SeriesFacts {
            code: "2345-7".into(),
            display: "Glucose".into(),
            unit: "mg/dL".into(),
            observation_refs: vec!["observation:a".into(), "observation:b".into()],
            latest_value: "134".into(),
            latest_abnormal: abnormal.map(str::to_string),
            direction,
            result_count: 2,
            incomparable_count: 0,
            pending_count: 1,
        }
    }

    #[test]
    fn mixed_units_are_stated_instead_of_a_direction() {
        let mut s = glucose(Direction::MixedUnits, None);
        s.incomparable_count = 1;
        let a = analyze(&[s.clone()], "en");
        let text = &a.statements[0].text;
        assert!(text.contains("trend not calculated"), "{text}");
        assert!(text.contains("1 result(s) in another unit not comparable with mg/dL"));
        for banned in ["rising", "falling", "stable"] {
            assert!(!text.contains(banned), "{text}");
        }
        let es = analyze(&[s], "es");
        assert!(es.statements[0].text.contains("unidades no convertibles"));
    }

    #[test]
    fn deterministic_and_cites_facts() {
        let a = analyze(&[glucose(Direction::Rising, Some("high"))], "en");
        let b = analyze(&[glucose(Direction::Rising, Some("high"))], "en");
        assert_eq!(
            serde_json::to_string(&a).unwrap(),
            serde_json::to_string(&b).unwrap()
        );
        assert_eq!(a.statements.len(), 1);
        assert_eq!(a.statements[0].facts.len(), 2);
        assert!(a.statements[0].text.contains("rising"));
        assert!(a.statements[0].text.contains("above the reference range"));
        assert!(a.statements[0].text.contains("1 request(s) pending"));
        assert!(!a.limitations.is_empty());
    }

    #[test]
    fn spanish_output_and_no_recommendations() {
        let a = analyze(&[glucose(Direction::Falling, Some("low"))], "es");
        let text = &a.statements[0].text;
        assert!(text.contains("en descenso"));
        assert!(text.contains("por debajo"));
        for banned in ["prescribe", "diagnos", "recomend", "recommend", "order"] {
            assert!(
                !text.to_lowercase().contains(banned),
                "statement must not contain {banned}: {text}"
            );
        }
    }

    #[test]
    fn pending_only_series_is_reported_without_facts() {
        let mut s = glucose(Direction::Insufficient, None);
        s.result_count = 0;
        s.observation_refs.clear();
        let a = analyze(&[s], "en");
        assert_eq!(a.statements.len(), 1);
        assert!(a.statements[0].text.contains("no recorded results"));
        assert!(a.statements[0].facts.is_empty());
    }
}
