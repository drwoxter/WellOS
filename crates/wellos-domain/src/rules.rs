//! Versioned deterministic critical-result rules.
//!
//! An LLM may explain a rule but never evaluates it. Rules are pure functions
//! over normalized quantities. If units cannot be safely normalized the rule
//! refuses to evaluate and reports a data-quality condition.

use crate::units::{convert, Quantity, UnitError};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriticalRule {
    /// Stable rule identifier, e.g. "critical.potassium".
    pub rule_id: String,
    /// Semantic version of the rule definition.
    pub version: String,
    /// LOINC code of the analyte this rule applies to.
    pub analyte_loinc: String,
    /// Canonical UCUM unit thresholds are expressed in.
    pub canonical_unit: String,
    pub critical_low: Option<Decimal>,
    pub critical_high: Option<Decimal>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum RuleOutcome {
    /// Value evaluated and is within non-critical range.
    NotCritical { normalized: Quantity },
    /// Value evaluated and breaches a critical threshold.
    Critical {
        normalized: Quantity,
        breached: Breach,
    },
    /// Unit could not be safely normalized; evaluation refused.
    UnitMismatch { reason: String },
    /// No rule applies to this analyte.
    NotApplicable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Breach {
    CriticalLow,
    CriticalHigh,
}

impl CriticalRule {
    pub fn evaluate(&self, analyte_loinc: &str, observed: &Quantity) -> RuleOutcome {
        if analyte_loinc != self.analyte_loinc {
            return RuleOutcome::NotApplicable;
        }
        let normalized = match convert(observed, &self.canonical_unit, analyte_loinc) {
            Ok(q) => q,
            Err(UnitError::UnknownConversion { .. }) => {
                return RuleOutcome::UnitMismatch {
                    reason: format!(
                        "cannot safely convert '{}' to canonical unit '{}'",
                        observed.unit, self.canonical_unit
                    ),
                }
            }
        };
        if let Some(low) = self.critical_low {
            if normalized.value < low {
                return RuleOutcome::Critical {
                    normalized,
                    breached: Breach::CriticalLow,
                };
            }
        }
        if let Some(high) = self.critical_high {
            if normalized.value > high {
                return RuleOutcome::Critical {
                    normalized,
                    breached: Breach::CriticalHigh,
                };
            }
        }
        RuleOutcome::NotCritical { normalized }
    }
}

/// The versioned rule set shipped with this development baseline.
pub fn baseline_rules() -> Vec<CriticalRule> {
    vec![
        CriticalRule {
            rule_id: "critical.potassium".into(),
            version: "1.0.0".into(),
            analyte_loinc: "2823-3".into(),
            canonical_unit: "mmol/L".into(),
            critical_low: Some(Decimal::new(25, 1)),  // 2.5
            critical_high: Some(Decimal::new(65, 1)), // 6.5
        },
        CriticalRule {
            rule_id: "critical.glucose".into(),
            version: "1.0.0".into(),
            analyte_loinc: "2345-7".into(),
            canonical_unit: "mg/dL".into(),
            critical_low: Some(Decimal::new(40, 0)),
            critical_high: Some(Decimal::new(500, 0)),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn potassium_rule() -> CriticalRule {
        baseline_rules().into_iter().next().unwrap()
    }

    fn q(v: &str, unit: &str) -> Quantity {
        Quantity {
            value: v.parse().unwrap(),
            unit: unit.into(),
        }
    }

    #[test]
    fn normal_value_is_not_critical() {
        let out = potassium_rule().evaluate("2823-3", &q("4.1", "mmol/L"));
        assert!(matches!(out, RuleOutcome::NotCritical { .. }));
    }

    #[test]
    fn high_value_is_critical_high() {
        let out = potassium_rule().evaluate("2823-3", &q("7.1", "mmol/L"));
        assert!(matches!(
            out,
            RuleOutcome::Critical {
                breached: Breach::CriticalHigh,
                ..
            }
        ));
    }

    #[test]
    fn low_value_is_critical_low() {
        let out = potassium_rule().evaluate("2823-3", &q("2.0", "mmol/L"));
        assert!(matches!(
            out,
            RuleOutcome::Critical {
                breached: Breach::CriticalLow,
                ..
            }
        ));
    }

    #[test]
    fn equivalent_unit_is_converted() {
        let out = potassium_rule().evaluate("2823-3", &q("7.1", "meq/L"));
        assert!(matches!(out, RuleOutcome::Critical { .. }));
    }

    #[test]
    fn unknown_unit_refuses_evaluation() {
        let out = potassium_rule().evaluate("2823-3", &q("7.1", "g/L"));
        assert!(matches!(out, RuleOutcome::UnitMismatch { .. }));
    }

    #[test]
    fn other_analyte_is_not_applicable() {
        let out = potassium_rule().evaluate("2345-7", &q("100", "mg/dL"));
        assert_eq!(out, RuleOutcome::NotApplicable);
    }
}
