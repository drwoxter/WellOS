//! Minimal UCUM-oriented unit normalization for laboratory quantities.
//!
//! Only conversions that are exactly known are performed. If a conversion is
//! not known, evaluation refuses the comparison instead of guessing — an
//! unknown unit must surface as a data-quality issue, never a silent repair.

use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Quantity {
    pub value: Decimal,
    /// UCUM unit code, e.g. "mmol/L", "mg/dL".
    pub unit: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UnitError {
    #[error("no known conversion from '{from}' to '{to}' for analyte '{analyte}'")]
    UnknownConversion {
        from: String,
        to: String,
        analyte: String,
    },
}

/// Convert `q` to `target_unit` for a given analyte (LOINC code).
///
/// Analyte-specific mass<->substance conversions require molar mass, so the
/// table is keyed by analyte where needed. Identity conversions are always
/// allowed.
pub fn convert(
    q: &Quantity,
    target_unit: &str,
    analyte_loinc: &str,
) -> Result<Quantity, UnitError> {
    if q.unit == target_unit {
        return Ok(q.clone());
    }
    let factor = conversion_factor(&q.unit, target_unit, analyte_loinc).ok_or_else(|| {
        UnitError::UnknownConversion {
            from: q.unit.clone(),
            to: target_unit.to_string(),
            analyte: analyte_loinc.to_string(),
        }
    })?;
    Ok(Quantity {
        value: q.value * factor,
        unit: target_unit.to_string(),
    })
}

fn conversion_factor(from: &str, to: &str, analyte_loinc: &str) -> Option<Decimal> {
    use rust_decimal::prelude::FromPrimitive;
    // Exact, versioned conversion table. Extend deliberately; never infer.
    let f = match (analyte_loinc, from, to) {
        // Potassium [Moles/volume] in Serum (2823-3): mmol/L <-> mEq/L (monovalent, 1:1)
        ("2823-3", "mmol/L", "meq/L") | ("2823-3", "meq/L", "mmol/L") => 1.0,
        // Glucose (2345-7): mg/dL -> mmol/L (molar mass 180.156 g/mol)
        ("2345-7", "mg/dL", "mmol/L") => 0.055_51,
        ("2345-7", "mmol/L", "mg/dL") => 18.016,
        _ => return None,
    };
    Decimal::from_f64(f)
}
