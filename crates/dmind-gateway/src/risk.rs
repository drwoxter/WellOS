//! dMind Risk Agent contract (`risk-summary.v1`) and the deterministic
//! offline summariser.
//!
//! The agent explains a deterministic [`RiskAssessment`]: it never computes
//! risk itself. Its output is a structured proposal — plain-language reasons
//! per domain, the records it cites, what is missing or contradictory and
//! follow-up categories for professional consideration. Every level in the
//! output is aligned to the deterministic assessment before validation, so a
//! provider can neither lower nor hide a critical signal, and every
//! suggestion carries `requires_confirmation: true`.

use crate::{hash_json, GatewayError, Usage};
use serde::{Deserialize, Serialize};
use wellos_domain::ai::Confidence;
use wellos_domain::risk::{
    DomainExplanation, FollowUpCategory, FollowUpSuggestion, RiskAssessment, RiskDomain, RiskLevel,
    RiskSummaryV1, RiskTrend, RISK_SUMMARY_SCHEMA,
};

pub const RISK_TEMPLATE: &str = "risk-summary@1.0.0";
/// Prompt version of the deterministic offline summariser.
pub const RISK_DETERMINISTIC_PROMPT_VERSION: &str = "risk-summary-deterministic.v1";

/// Policy-filtered input: the deterministic assessment plus the
/// (reference, statement) facts the summary may cite. No free-text notes.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskSummaryRequest {
    pub template: String,
    pub language: String,
    pub assessment: RiskAssessment,
    pub facts: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RiskSummaryResponse {
    pub output: RiskSummaryV1,
    pub model: String,
    pub model_version: String,
    pub route: String,
    pub prompt_version: String,
    pub input_hash: String,
    pub usage: Option<Usage>,
}

pub fn risk_input_hash(req: &RiskSummaryRequest) -> String {
    hash_json(req)
}

/// Parse raw provider JSON into a validated, floor-aligned summary. Used for
/// every provider (including the fake one) so that the same rejection rules
/// apply regardless of origin.
pub fn parse_summary(
    raw: &serde_json::Value,
    det: &RiskAssessment,
) -> Result<RiskSummaryV1, GatewayError> {
    let parsed: RiskSummaryV1 = serde_json::from_value(raw.clone()).map_err(|e| {
        GatewayError::InvalidOutput(format!("risk summary does not match schema: {e}"))
    })?;
    let aligned = parsed.align_to_deterministic(det);
    aligned.validate(det).map_err(GatewayError::InvalidOutput)?;
    Ok(aligned)
}

fn domain_name(d: RiskDomain, es: bool) -> &'static str {
    match (d, es) {
        (RiskDomain::AcuteSafety, false) => "acute safety",
        (RiskDomain::AcuteSafety, true) => "seguridad aguda",
        (RiskDomain::ChronicComplexity, false) => "chronic complexity",
        (RiskDomain::ChronicComplexity, true) => "complejidad crónica",
        (RiskDomain::MedicationAllergySafety, false) => "medication and allergy safety",
        (RiskDomain::MedicationAllergySafety, true) => "seguridad de medicación y alergias",
        (RiskDomain::DiagnosticResult, false) => "diagnostic results",
        (RiskDomain::DiagnosticResult, true) => "resultados diagnósticos",
        (RiskDomain::PreventiveCare, false) => "preventive care",
        (RiskDomain::PreventiveCare, true) => "atención preventiva",
        (RiskDomain::AccessUtilization, false) => "access and utilization",
        (RiskDomain::AccessUtilization, true) => "acceso y utilización",
        (RiskDomain::CareCoordination, false) => "care coordination",
        (RiskDomain::CareCoordination, true) => "coordinación de la atención",
    }
}

fn level_name(l: RiskLevel, es: bool) -> &'static str {
    match (l, es) {
        (RiskLevel::InsufficientData, false) => "insufficient data",
        (RiskLevel::InsufficientData, true) => "datos insuficientes",
        (RiskLevel::Low, false) => "low",
        (RiskLevel::Low, true) => "bajo",
        (RiskLevel::Moderate, false) => "moderate",
        (RiskLevel::Moderate, true) => "moderado",
        (RiskLevel::High, false) => "high",
        (RiskLevel::High, true) => "alto",
        (RiskLevel::Critical, false) => "critical",
        (RiskLevel::Critical, true) => "crítico",
    }
}

fn reason_text(code: &str, detail: Option<&str>, es: bool) -> String {
    let base = match (code, es) {
        ("open_critical_alert", false) => "an open safety alert is recorded",
        ("open_critical_alert", true) => "hay una alerta de seguridad abierta",
        ("visit_priority", false) => "the current visit was triaged as urgent or immediate",
        ("visit_priority", true) => "la visita actual fue clasificada como urgente o inmediata",
        ("abnormal_vitals", false) => {
            "the most recent vital signs breach a deterministic safety rule"
        }
        ("abnormal_vitals", true) => {
            "las constantes vitales más recientes incumplen una regla determinista de seguridad"
        }
        ("multiple_chronic_conditions", false) => "major chronic conditions are active",
        ("multiple_chronic_conditions", true) => "hay enfermedades crónicas mayores activas",
        ("polypharmacy", false) => "many medications are active at the same time",
        ("polypharmacy", true) => "hay muchos medicamentos activos a la vez",
        ("medication_allergy_conflict", false) => "an active medication matches a recorded allergy",
        ("medication_allergy_conflict", true) => {
            "un medicamento activo coincide con una alergia registrada"
        }
        ("duplicate_medication", false) => "the same medication is recorded twice as active",
        ("duplicate_medication", true) => "el mismo medicamento consta dos veces como activo",
        ("allergy_status_unknown", false) => {
            "medications are active but no allergy status is recorded"
        }
        ("allergy_status_unknown", true) => {
            "hay medicación activa pero no consta el estado de alergias"
        }
        ("critical_result_unreviewed", false) => "a critical result has not been reviewed yet",
        ("critical_result_unreviewed", true) => "un resultado crítico aún no ha sido revisado",
        ("critical_result_open_loop", false) => {
            "a critical result was reviewed but its loop is still open"
        }
        ("critical_result_open_loop", true) => {
            "un resultado crítico fue revisado pero su ciclo sigue abierto"
        }
        ("abnormal_result_unreviewed", false) => "an abnormal result is awaiting review",
        ("abnormal_result_unreviewed", true) => "un resultado anormal está pendiente de revisión",
        ("abnormal_result_open_loop", false) => {
            "an abnormal result was reviewed but its loop is still open"
        }
        ("abnormal_result_open_loop", true) => {
            "un resultado anormal fue revisado pero su ciclo sigue abierto"
        }
        ("hba1c_overdue", false) => "no HbA1c is recorded within the expected interval",
        ("hba1c_overdue", true) => "no consta una HbA1c dentro del intervalo esperado",
        ("renal_function_overdue", false) => {
            "no renal function test is recorded within the expected interval"
        }
        ("renal_function_overdue", true) => {
            "no consta una prueba de función renal dentro del intervalo esperado"
        }
        ("blood_pressure_overdue", false) => {
            "no blood pressure is recorded within the expected interval"
        }
        ("blood_pressure_overdue", true) => {
            "no consta una tensión arterial dentro del intervalo esperado"
        }
        ("lipid_screening_overdue", false) => {
            "no lipid screening is recorded within the expected interval"
        }
        ("lipid_screening_overdue", true) => {
            "no consta un cribado lipídico dentro del intervalo esperado"
        }
        ("frequent_unscheduled_visits", false) => "several unscheduled visits occurred recently",
        ("frequent_unscheduled_visits", true) => {
            "ha habido varias visitas no programadas recientes"
        }
        ("repeated_no_show", false) => "several appointments were missed",
        ("repeated_no_show", true) => "se han perdido varias citas",
        ("no_recent_follow_up", false) => {
            "no recent consultation is recorded despite chronic conditions"
        }
        ("no_recent_follow_up", true) => {
            "no consta una consulta reciente a pesar de enfermedades crónicas"
        }
        ("overdue_follow_up", false) => "follow-up tasks are overdue",
        ("overdue_follow_up", true) => "hay tareas de seguimiento vencidas",
        ("pending_result_overdue", false) => "an ordered test has had no result for a long time",
        ("pending_result_overdue", true) => {
            "una prueba solicitada lleva mucho tiempo sin resultado"
        }
        ("unhandled_alert", false) => "an alert has stayed open for more than a day",
        ("unhandled_alert", true) => "una alerta lleva abierta más de un día",
        ("no_responsible_professional", false) => "no responsible professional is assigned",
        ("no_responsible_professional", true) => "no hay un profesional responsable asignado",
        (other, _) => other,
    };
    match detail {
        Some(d) if !d.trim().is_empty() => format!("{base} ({d})"),
        _ => base.to_string(),
    }
}

fn gap_text(code: &str, stale: bool, es: bool) -> String {
    let what = match code {
        c if c.starts_with("vitals") => {
            if es {
                "constantes vitales"
            } else {
                "vital signs"
            }
        }
        c if c.starts_with("hba1c") => "HbA1c",
        c if c.starts_with("creatinine") => {
            if es {
                "función renal"
            } else {
                "renal function"
            }
        }
        c if c.starts_with("blood_pressure") => {
            if es {
                "tensión arterial"
            } else {
                "blood pressure"
            }
        }
        c if c.starts_with("lipid") => {
            if es {
                "perfil lipídico"
            } else {
                "lipid screening"
            }
        }
        c if c.starts_with("laboratory_results") => {
            if es {
                "resultados de laboratorio"
            } else {
                "laboratory results"
            }
        }
        c if c.starts_with("allergy") => {
            if es {
                "estado de alergias"
            } else {
                "allergy status"
            }
        }
        c if c.starts_with("problem_list") => {
            if es {
                "lista de problemas"
            } else {
                "problem list"
            }
        }
        c if c.starts_with("follow_up") | c.starts_with("consultation") => {
            if es {
                "consulta de seguimiento"
            } else {
                "follow-up consultation"
            }
        }
        c if c.starts_with("responsible_professional") => {
            if es {
                "profesional responsable"
            } else {
                "responsible professional"
            }
        }
        "no_clinical_record" => {
            if es {
                "registro clínico"
            } else {
                "clinical record"
            }
        }
        other => other,
    };
    match (stale, es) {
        (true, false) => format!("{what} is outdated"),
        (true, true) => format!("{what}: dato desactualizado"),
        (false, false) => format!("{what} is not recorded"),
        (false, true) => format!("{what}: no registrado"),
    }
}

fn suggestion_for(code: &str, domain: RiskDomain, es: bool) -> Option<FollowUpSuggestion> {
    let (category, en, esx) = match code {
        "critical_result_unreviewed" | "abnormal_result_unreviewed" => (
            FollowUpCategory::ReviewResult,
            "Review the pending result and close its loop",
            "Revisar el resultado pendiente y cerrar su ciclo",
        ),
        "critical_result_open_loop" | "abnormal_result_open_loop" | "pending_result_overdue" => (
            FollowUpCategory::ReviewResult,
            "Complete the open result loop",
            "Completar el ciclo de resultado abierto",
        ),
        "medication_allergy_conflict"
        | "duplicate_medication"
        | "allergy_status_unknown"
        | "polypharmacy" => (
            FollowUpCategory::MedicationReconciliation,
            "Reconcile medications and allergies with the patient",
            "Conciliar la medicación y las alergias con el paciente",
        ),
        "hba1c_overdue"
        | "renal_function_overdue"
        | "blood_pressure_overdue"
        | "lipid_screening_overdue" => (
            FollowUpCategory::PreventiveScreening,
            "Consider the overdue preventive measure",
            "Valorar la medida preventiva pendiente",
        ),
        "no_recent_follow_up"
        | "frequent_unscheduled_visits"
        | "repeated_no_show"
        | "multiple_chronic_conditions" => (
            FollowUpCategory::ScheduleFollowUp,
            "Consider scheduling a follow-up consultation",
            "Valorar programar una consulta de seguimiento",
        ),
        "no_responsible_professional" => (
            FollowUpCategory::CareTeamAssignment,
            "Assign a responsible professional",
            "Asignar un profesional responsable",
        ),
        "overdue_follow_up" | "unhandled_alert" | "open_critical_alert" => (
            FollowUpCategory::ReviewResult,
            "Address the overdue task or open alert",
            "Atender la tarea vencida o la alerta abierta",
        ),
        "visit_priority" | "abnormal_vitals" => (
            FollowUpCategory::DiscussWithPatient,
            "Reassess the patient in person",
            "Reevaluar al paciente presencialmente",
        ),
        _ => return None,
    };
    Some(FollowUpSuggestion {
        category,
        domain,
        text: if es { esx.to_string() } else { en.to_string() },
        requires_confirmation: true,
    })
}

/// Deterministic summary from the typed assessment. Pure and offline;
/// identical inputs always yield identical output.
pub fn summarize(req: &RiskSummaryRequest) -> Result<RiskSummaryResponse, GatewayError> {
    if req.template != RISK_TEMPLATE {
        return Err(GatewayError::PolicyDenied(format!(
            "unsupported template {}",
            req.template
        )));
    }
    let es = req.language == "es";
    let det = &req.assessment;
    let mut domains = Vec::with_capacity(det.domains.len());
    let mut missing: Vec<String> = Vec::new();
    let mut suggestions: Vec<FollowUpSuggestion> = Vec::new();
    for d in &det.domains {
        let name = domain_name(d.domain, es);
        let level = level_name(d.level, es);
        let reasons: Vec<String> = d
            .factors
            .iter()
            .map(|f| reason_text(&f.code, f.detail.as_deref(), es))
            .collect();
        let summary = if d.level == RiskLevel::InsufficientData {
            if es {
                format!("No hay datos suficientes para valorar {name}.")
            } else {
                format!("There is not enough recorded data to assess {name}.")
            }
        } else if reasons.is_empty() {
            if es {
                format!("Riesgo {level} en {name}: no se detectó ningún factor determinista.")
            } else {
                format!("{level} risk in {name}: no deterministic factor was detected.")
            }
        } else if es {
            format!("Riesgo {level} en {name} porque {}.", reasons.join("; "))
        } else {
            format!("{level} risk in {name} because {}.", reasons.join("; "))
        };
        let trend = match (d.trend, es) {
            (RiskTrend::Worsening, false) => {
                Some(" This domain is worsening compared with the previous assessment.")
            }
            (RiskTrend::Worsening, true) => {
                Some(" Este dominio empeora respecto a la valoración anterior.")
            }
            (RiskTrend::Improving, false) => {
                Some(" This domain is improving compared with the previous assessment.")
            }
            (RiskTrend::Improving, true) => {
                Some(" Este dominio mejora respecto a la valoración anterior.")
            }
            _ => None,
        };
        let summary = capitalize(&format!("{summary}{}", trend.unwrap_or("")));
        let mut cited: Vec<String> = d
            .factors
            .iter()
            .flat_map(|f| {
                f.evidence
                    .iter()
                    .map(|e| format!("{}:{}", e.record_type, e.record_id))
            })
            .collect();
        cited.dedup();
        if !d.factors.is_empty() && cited.is_empty() {
            // Factors derived from absence (e.g. no follow-up) cite the
            // deterministic rule itself so the provenance is never empty.
            cited.push(format!("rule:{}:{}", d.rules_version, d.domain.as_str()));
        }
        for g in &d.missing_data {
            missing.push(gap_text(&g.code, false, es));
        }
        for g in &d.stale_data {
            missing.push(gap_text(&g.code, true, es));
        }
        for f in &d.factors {
            if let Some(s) = suggestion_for(&f.code, d.domain, es) {
                if !suggestions.iter().any(|x| x.text == s.text) {
                    suggestions.push(s);
                }
            }
        }
        domains.push(DomainExplanation {
            domain: d.domain,
            level: d.level,
            summary,
            reasons,
            cited_sources: cited,
        });
    }
    missing.dedup();
    if det.overall_level == RiskLevel::InsufficientData {
        suggestions.push(FollowUpSuggestion {
            category: FollowUpCategory::CompleteRecord,
            domain: RiskDomain::CareCoordination,
            text: if es {
                "Completar el registro clínico antes de valorar el riesgo".into()
            } else {
                "Complete the clinical record before assessing risk".into()
            },
            requires_confirmation: true,
        });
    }

    // Contradictions the rules can detect from the facts alone.
    let mut contradictions: Vec<String> = Vec::new();
    if let Some(med) = det.domain(RiskDomain::MedicationAllergySafety) {
        if med
            .factors
            .iter()
            .any(|f| f.code == "medication_allergy_conflict")
        {
            contradictions.push(if es {
                "Un medicamento activo contradice una alergia registrada; confirmar cuál de los dos registros es correcto.".into()
            } else {
                "An active medication contradicts a recorded allergy; confirm which record is correct.".into()
            });
        }
    }

    let confidence = match det.overall_level {
        RiskLevel::InsufficientData => Confidence::Low,
        _ if !missing.is_empty() => Confidence::Medium,
        _ => Confidence::High,
    };
    let limitations = vec![
        if es {
            "Contenido generado por IA a partir de reglas deterministas; no es un diagnóstico ni una recomendación de tratamiento.".to_string()
        } else {
            "AI-generated from deterministic rules; not a diagnosis or a treatment recommendation."
                .to_string()
        },
        if es {
            "Las sugerencias requieren confirmación profesional antes de crear cualquier tarea u orden.".to_string()
        } else {
            "Suggestions require professional confirmation before any task or order is created."
                .to_string()
        },
        if es {
            "No evalúa elegibilidad, cobertura, prima ni autorización de seguros.".to_string()
        } else {
            "Does not assess insurance eligibility, coverage, premium or authorization.".to_string()
        },
    ];
    let cited_sources: Vec<String> = req.facts.iter().map(|(r, _)| r.clone()).collect();

    let output = RiskSummaryV1 {
        schema_version: RISK_SUMMARY_SCHEMA.into(),
        ai_generated: true,
        rules_version: det.rules_version.clone(),
        overall_level: det.overall_level,
        safety_floor: det.safety_floor,
        raised_to_floor: false,
        domains,
        missing_information: missing,
        contradictions,
        follow_up_suggestions: suggestions,
        limitations,
        cited_sources,
        confidence,
    };
    let raw = serde_json::to_value(&output).expect("serializable");
    let output = parse_summary(&raw, det)?;
    Ok(RiskSummaryResponse {
        output,
        model: crate::FIXTURE_MODEL.into(),
        model_version: crate::FIXTURE_MODEL_VERSION.into(),
        route: crate::FIXTURE_ROUTE.into(),
        prompt_version: RISK_DETERMINISTIC_PROMPT_VERSION.into(),
        input_hash: risk_input_hash(req),
        usage: None,
    })
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Duration, NaiveDate, Utc};
    use uuid::Uuid;
    use wellos_domain::risk::{assess, EncounterFact, ResultFact, RiskInput};

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-09-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn input(critical: bool) -> RiskInput {
        let mut results = vec![ResultFact {
            observation_id: Uuid::now_v7(),
            service_request_id: Uuid::now_v7(),
            code_loinc: "2345-7".into(),
            display: "Glucose".into(),
            value: Some("95".into()),
            unit: Some("mg/dL".into()),
            critical: false,
            abnormal: false,
            loop_state: "closed".into(),
            effective_at: now() - Duration::days(20),
        }];
        if critical {
            results.push(ResultFact {
                observation_id: Uuid::now_v7(),
                service_request_id: Uuid::now_v7(),
                code_loinc: "2823-3".into(),
                display: "Potassium".into(),
                value: Some("7.1".into()),
                unit: Some("mmol/L".into()),
                critical: true,
                abnormal: true,
                loop_state: "received".into(),
                effective_at: now() - Duration::hours(2),
            });
        }
        RiskInput {
            now: now(),
            birth_date: NaiveDate::from_ymd_opt(1980, 1, 1).unwrap(),
            alerts: vec![],
            current_visit: None,
            visits: vec![],
            latest_vitals: None,
            conditions: vec![],
            medications: vec![],
            allergies: vec![],
            results,
            open_requests: vec![],
            encounters: vec![EncounterFact {
                id: Uuid::now_v7(),
                status: "completed".into(),
                encounter_type: "consultation".into(),
                started_at: now() - Duration::days(20),
                completed_at: Some(now() - Duration::days(20)),
            }],
            tasks: vec![],
            care_team: vec![],
            previous: None,
        }
    }

    fn req(lang: &str, critical: bool) -> RiskSummaryRequest {
        let assessment = assess(&input(critical));
        RiskSummaryRequest {
            template: RISK_TEMPLATE.into(),
            language: lang.into(),
            facts: vec![("patient:synthetic".into(), "synthetic".into())],
            assessment,
        }
    }

    #[test]
    fn deterministic_and_labelled_ai_generated() {
        let r = req("en", true);
        let a = summarize(&r).unwrap();
        let b = summarize(&r).unwrap();
        assert_eq!(a.output, b.output);
        assert_eq!(a.input_hash, b.input_hash);
        assert!(a.output.ai_generated);
        assert_eq!(a.output.schema_version, RISK_SUMMARY_SCHEMA);
        assert_eq!(a.output.domains.len(), 7);
        assert_eq!(a.model, crate::FIXTURE_MODEL);
        assert!(a
            .output
            .follow_up_suggestions
            .iter()
            .all(|s| s.requires_confirmation));
    }

    #[test]
    fn explains_critical_domain_with_citations_and_suggestion() {
        let r = summarize(&req("en", true)).unwrap();
        let dx = r
            .output
            .domains
            .iter()
            .find(|d| d.domain == RiskDomain::DiagnosticResult)
            .unwrap();
        assert_eq!(dx.level, RiskLevel::Critical);
        assert!(dx
            .summary
            .contains("Critical risk in diagnostic results because"));
        assert!(dx.summary.contains("Potassium 7.1 mmol/L"));
        assert!(dx.cited_sources[0].starts_with("observation:"));
        assert!(r
            .output
            .follow_up_suggestions
            .iter()
            .any(|s| s.category == FollowUpCategory::ReviewResult));
        assert_eq!(r.output.overall_level, RiskLevel::Critical);
        assert!(!r.output.raised_to_floor);
    }

    #[test]
    fn spanish_output_and_missing_information() {
        let r = summarize(&req("es", false)).unwrap();
        let dx = r
            .output
            .domains
            .iter()
            .find(|d| d.domain == RiskDomain::DiagnosticResult)
            .unwrap();
        assert!(dx
            .summary
            .starts_with("Riesgo bajo en resultados diagnósticos"));
        assert!(r
            .output
            .limitations
            .iter()
            .any(|l| l.contains("generado por IA")));
    }

    #[test]
    fn rejects_unknown_template() {
        let mut r = req("en", false);
        r.template = "risk-summary@9.9.9".into();
        assert!(matches!(summarize(&r), Err(GatewayError::PolicyDenied(_))));
    }

    #[test]
    fn parse_rejects_malformed_and_lowered_output() {
        let det = assess(&input(true));
        let bad = serde_json::json!({"schema_version": "risk-summary.v1", "domains": "nope"});
        assert!(matches!(
            parse_summary(&bad, &det),
            Err(GatewayError::InvalidOutput(_))
        ));

        let good = summarize(&req("en", true)).unwrap().output;
        let mut lowered = serde_json::to_value(&good).unwrap();
        lowered["overall_level"] = serde_json::json!("low");
        for d in lowered["domains"].as_array_mut().unwrap() {
            d["level"] = serde_json::json!("low");
        }
        let parsed = parse_summary(&lowered, &det).unwrap();
        assert_eq!(parsed.overall_level, RiskLevel::Critical);
        assert!(parsed.raised_to_floor);

        let mut unlabelled = serde_json::to_value(&good).unwrap();
        unlabelled["ai_generated"] = serde_json::json!(false);
        assert!(parse_summary(&unlabelled, &det).is_err());

        let mut missing_domain = serde_json::to_value(&good).unwrap();
        missing_domain["domains"].as_array_mut().unwrap().pop();
        assert!(parse_summary(&missing_domain, &det).is_err());

        let mut no_confirm = serde_json::to_value(&good).unwrap();
        no_confirm["follow_up_suggestions"][0]["requires_confirmation"] = serde_json::json!(false);
        assert!(parse_summary(&no_confirm, &det).is_err());
    }

    #[test]
    fn input_hash_changes_with_assessment() {
        let ra = req("en", true);
        let a = risk_input_hash(&ra);
        let b = risk_input_hash(&req("en", false));
        assert_ne!(a, b);
        assert_eq!(a, risk_input_hash(&ra));
        let mut es = ra.clone();
        es.language = "es".into();
        assert_ne!(a, risk_input_hash(&es));
    }
}
