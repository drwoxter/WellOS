//! Registry of governed dMind operations, derived from the gateway's
//! [`Operation::ALL`] and the server's governance scopes rather than kept as
//! a separate list. Coverage tests fail when a registered operation has no
//! evaluation cases.

use dmind_gateway::Operation;

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct RegisteredOperation {
    pub operation: Operation,
    pub key: &'static str,
    pub output_schema: &'static str,
    /// Governance artifact types (`aigov::ReuseScope`) that execute this
    /// operation; `encounter_summary` shares `result_summary`'s contract.
    pub artifact_types: Vec<&'static str>,
    /// Which part of the platform the operation serves.
    pub family: &'static str,
    /// Operations whose output a patient may eventually see after clinician
    /// approval get stricter language and claim checks.
    pub patient_facing: bool,
    /// Whether a deterministic baseline exists for fallback comparison.
    pub deterministic_baseline: bool,
}

pub fn registry() -> Vec<RegisteredOperation> {
    Operation::ALL
        .iter()
        .map(|&operation| {
            let artifact_types = crate::aigov::ARTIFACT_TYPES
                .iter()
                .filter(|(_, op)| *op == operation)
                .map(|(t, _)| *t)
                .collect::<Vec<_>>();
            let (family, patient_facing) = match operation {
                Operation::ResultSummary => ("results", false),
                Operation::TriageProposal => ("access", false),
                Operation::RiskSummary => ("risk", false),
                Operation::NoteDraft => ("scribe", false),
                Operation::AccessIntent
                | Operation::AppointmentRanking
                | Operation::CancellationRecovery
                | Operation::CapacityExplanation => ("access", false),
                Operation::DiagnosticOrderSuggestion | Operation::DiagnosticResultSynthesis => {
                    ("diagnostics", false)
                }
                Operation::PatientResultExplanation => ("diagnostics", true),
            };
            RegisteredOperation {
                operation,
                key: operation.as_str(),
                output_schema: operation.output_schema(),
                artifact_types,
                family,
                patient_facing,
                deterministic_baseline: true,
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_operation_is_registered_with_a_governance_scope() {
        let reg = registry();
        assert_eq!(reg.len(), Operation::ALL.len());
        for r in &reg {
            assert!(
                !r.artifact_types.is_empty(),
                "{} has no aigov::ReuseScope artifact type",
                r.key
            );
        }
        // Every governance artifact type maps to a registered operation.
        for (t, op) in crate::aigov::ARTIFACT_TYPES {
            assert!(reg.iter().any(|r| r.operation == *op), "{t} unregistered");
        }
    }
}
