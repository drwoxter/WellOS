//! A2 triage assistance contract and the deterministic offline proposer.
//!
//! The proposal is advisory: it suggests an operational priority, a
//! destination service and a handoff summary, lists the facts it used, what is
//! missing or contradictory, and its limitations. The caller (server) owns the
//! deterministic safety floor and the human review gate; the provider never
//! diagnoses, assigns a professional, starts an encounter or notifies anyone.

use crate::{hash_json, GatewayError};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use wellos_domain::ai::Confidence;
use wellos_domain::triage::{
    is_service, ArrivalKind, Priority, TriageProposalV1, TriageVitals, TRIAGE_PROPOSAL_SCHEMA,
};

pub const TRIAGE_TEMPLATE: &str = "triage-proposal@1.0.0";

/// Policy-filtered triage facts. `facts` carries the (reference, statement)
/// pairs cited back in the proposal; the typed fields drive the heuristic.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageRequest {
    pub template: String,
    pub language: String,
    pub arrival_kind: ArrivalKind,
    pub age_years: Option<i32>,
    pub reason: Option<String>,
    pub concerns: Vec<String>,
    pub onset: Option<String>,
    pub red_flags: Vec<String>,
    pub vitals: TriageVitals,
    pub allergies: Vec<String>,
    pub requested_service: Option<String>,
    pub facts: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriageResponse {
    pub output: TriageProposalV1,
    pub model: String,
    pub model_version: String,
    pub route: String,
    pub input_hash: String,
}

pub fn triage_input_hash(req: &TriageRequest) -> String {
    hash_json(req)
}

fn concern_priority(code: &str) -> Priority {
    match code {
        "chest_pain" | "shortness_of_breath" => Priority::Urgent,
        "abdominal_pain" | "fever" | "vomiting" | "injury" | "dizziness" | "headache"
        | "mental_health" => Priority::Standard,
        "rash" | "urinary_symptoms" | "cough" | "medication_review" | "follow_up" => {
            Priority::NonUrgent
        }
        _ => Priority::Standard,
    }
}

fn label(code: &str, es: bool) -> String {
    let en = code.replace('_', " ");
    if !es {
        return en;
    }
    match code {
        "fever" => "fiebre",
        "cough" => "tos",
        "shortness_of_breath" => "dificultad respiratoria",
        "chest_pain" => "dolor torácico",
        "abdominal_pain" => "dolor abdominal",
        "headache" => "cefalea",
        "dizziness" => "mareo",
        "vomiting" => "vómitos",
        "injury" => "lesión",
        "rash" => "erupción cutánea",
        "urinary_symptoms" => "síntomas urinarios",
        "mental_health" => "salud mental",
        "medication_review" => "revisión de medicación",
        "follow_up" => "seguimiento",
        "other" => "otro",
        "airway_compromise" => "compromiso de vía aérea",
        "unresponsive" => "sin respuesta",
        "severe_bleeding" => "hemorragia grave",
        "stroke_signs" => "signos de ictus",
        "severe_breathing_difficulty" => "dificultad respiratoria grave",
        "anaphylaxis_signs" => "signos de anafilaxia",
        "severe_pain" => "dolor intenso",
        "suicidal_ideation" => "ideación suicida",
        "pregnancy_complication" => "complicación del embarazo",
        _ => return en,
    }
    .to_string()
}

/// Deterministic proposal from typed facts. Pure and offline; identical
/// inputs always yield identical output.
pub fn propose(req: &TriageRequest) -> Result<TriageResponse, GatewayError> {
    if req.template != TRIAGE_TEMPLATE {
        return Err(GatewayError::PolicyDenied(format!(
            "unsupported template {}",
            req.template
        )));
    }
    let es = req.language == "es";
    let mut rationale: Vec<String> = Vec::new();
    let mut important: Vec<String> = Vec::new();
    let mut missing: Vec<String> = Vec::new();
    let mut contradictions: Vec<String> = Vec::new();

    let mut priority = req
        .concerns
        .iter()
        .map(|c| concern_priority(c))
        .max()
        .unwrap_or(Priority::Standard);
    if !req.concerns.is_empty() {
        let names = req
            .concerns
            .iter()
            .map(|c| label(c, es))
            .collect::<Vec<_>>()
            .join(", ");
        rationale.push(if es {
            format!("Motivos estructurados registrados: {names}.")
        } else {
            format!("Structured concerns recorded: {names}.")
        });
    }
    if !req.red_flags.is_empty() {
        priority = priority.max(Priority::Urgent);
        let names = req
            .red_flags
            .iter()
            .map(|c| label(c, es))
            .collect::<Vec<_>>()
            .join(", ");
        important.push(if es {
            format!("Señales de alarma: {names}")
        } else {
            format!("Red flags: {names}")
        });
        rationale.push(if es {
            "Las señales de alarma explícitas elevan la prioridad operativa.".into()
        } else {
            "Explicit red flags raise the operational priority.".into()
        });
    }
    if req.arrival_kind == ArrivalKind::Urgent {
        priority = priority.max(Priority::Urgent);
        rationale.push(if es {
            "Llegada registrada como urgente.".into()
        } else {
            "Arrival was registered as urgent.".into()
        });
    }

    match &req.reason {
        Some(r) if !r.trim().is_empty() => important.push(if es {
            format!("Motivo de consulta: {}", r.trim())
        } else {
            format!("Reason for attendance: {}", r.trim())
        }),
        _ => missing.push(if es {
            "Falta el motivo de consulta.".into()
        } else {
            "Reason for attendance is missing.".into()
        }),
    }
    match &req.onset {
        Some(o) if !o.trim().is_empty() => important.push(if es {
            format!("Inicio/duración: {}", o.trim())
        } else {
            format!("Onset/duration: {}", o.trim())
        }),
        _ => missing.push(if es {
            "No se registró inicio ni duración.".into()
        } else {
            "Onset or duration was not recorded.".into()
        }),
    }

    let v = &req.vitals;
    let has_vitals = v.systolic_mmhg.is_some()
        || v.heart_rate_bpm.is_some()
        || v.respiratory_rate_bpm.is_some()
        || v.temperature_c.is_some()
        || v.spo2_percent.is_some();
    if !has_vitals {
        missing.push(if es {
            "No hay constantes vitales registradas en el triaje.".into()
        } else {
            "No vital signs recorded at triage.".into()
        });
    } else {
        let mut parts: Vec<String> = Vec::new();
        if let Some(x) = v.systolic_mmhg {
            parts.push(format!("SBP {x} mmHg"));
        }
        if let Some(x) = v.heart_rate_bpm {
            parts.push(format!("HR {x} bpm"));
        }
        if let Some(x) = v.respiratory_rate_bpm {
            parts.push(format!("RR {x}/min"));
        }
        if let Some(x) = v.temperature_c {
            parts.push(format!("T {x} °C"));
        }
        if let Some(x) = v.spo2_percent {
            parts.push(format!("SpO2 {x}%"));
        }
        important.push(if es {
            format!("Constantes: {}", parts.join(", "))
        } else {
            format!("Vitals: {}", parts.join(", "))
        });
    }
    if req.concerns.iter().any(|c| c == "fever") {
        if let Some(t) = v.temperature_c {
            if t < Decimal::new(375, 1) {
                contradictions.push(if es {
                    format!("Se refiere fiebre pero la temperatura registrada es {t} °C.")
                } else {
                    format!("Fever is reported but the recorded temperature is {t} °C.")
                });
            }
        }
    }
    if !req.allergies.is_empty() {
        important.push(if es {
            format!("Alergias registradas: {}", req.allergies.join(", "))
        } else {
            format!("Recorded allergies: {}", req.allergies.join(", "))
        });
    }

    let proposed_service = match req.requested_service.as_deref() {
        Some(s) if is_service(s) => s.to_string(),
        _ if req.arrival_kind == ArrivalKind::Remote => "telehealth".to_string(),
        _ if priority >= Priority::Urgent => "emergency".to_string(),
        _ => "general_medicine".to_string(),
    };
    rationale.push(if es {
        format!(
            "Servicio propuesto: {} (solicitud registrada o inferencia por tipo de llegada y prioridad).",
            proposed_service.replace('_', " ")
        )
    } else {
        format!(
            "Proposed service: {} (from the recorded request, or arrival kind and priority).",
            proposed_service.replace('_', " ")
        )
    });

    let confidence = if req.reason.as_deref().unwrap_or("").trim().is_empty() {
        Confidence::Low
    } else if has_vitals && !req.concerns.is_empty() {
        Confidence::High
    } else {
        Confidence::Medium
    };

    let age = req.age_years.map(|a| {
        if es {
            format!("{a} años")
        } else {
            format!("{a} y")
        }
    });
    let handoff_summary = if es {
        format!(
            "Paciente{} llegado como {}. {}{} Prioridad operativa propuesta: {}. Propuesta generada por dMind; requiere decisión del profesional de triaje.",
            age.map(|a| format!(" ({a})")).unwrap_or_default(),
            match req.arrival_kind {
                ArrivalKind::Scheduled => "cita programada",
                ArrivalKind::WalkIn => "sin cita",
                ArrivalKind::Urgent => "llegada urgente",
                ArrivalKind::Remote => "teleconsulta",
            },
            important.join(". "),
            if important.is_empty() { "" } else { "." },
            priority.as_str().replace('_', " ")
        )
    } else {
        format!(
            "Patient{} arrived as {}. {}{} Proposed operational priority: {}. Generated by dMind; requires the triage professional's decision.",
            age.map(|a| format!(" ({a})")).unwrap_or_default(),
            match req.arrival_kind {
                ArrivalKind::Scheduled => "a scheduled appointment",
                ArrivalKind::WalkIn => "a walk-in",
                ArrivalKind::Urgent => "an urgent arrival",
                ArrivalKind::Remote => "a teleconsultation",
            },
            important.join(". "),
            if important.is_empty() { "" } else { "." },
            priority.as_str().replace('_', " ")
        )
    };

    let limitations = if es {
        vec![
            "Generado por un proveedor determinista de desarrollo; no es una valoración clínica ni un diagnóstico.".to_string(),
            "Propone solo prioridad operativa y destino; nunca asigna profesionales, inicia consultas ni notifica pacientes.".to_string(),
            "Las reglas deterministas de seguridad y las señales de alarma explícitas prevalecen sobre esta propuesta.".to_string(),
        ]
    } else {
        vec![
            "Generated by a deterministic development provider; not a clinical assessment or diagnosis.".to_string(),
            "Proposes operational priority and destination only; never assigns professionals, starts consultations or notifies patients.".to_string(),
            "Deterministic safety rules and explicit red flags take precedence over this proposal.".to_string(),
        ]
    };

    let output = TriageProposalV1 {
        schema_version: TRIAGE_PROPOSAL_SCHEMA.into(),
        proposed_priority: priority,
        safety_floor: Priority::NonUrgent,
        raised_to_floor: false,
        proposed_service,
        important_facts: important,
        missing_information: missing,
        contradictions,
        handoff_summary,
        rationale,
        confidence,
        limitations,
        cited_sources: req.facts.iter().map(|(r, _)| r.clone()).collect(),
    };
    output.validate().map_err(GatewayError::InvalidOutput)?;
    Ok(TriageResponse {
        output,
        model: "dmind-fake-triage".into(),
        model_version: "0.1.0".into(),
        route: "local-fake".into(),
        input_hash: triage_input_hash(req),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req() -> TriageRequest {
        TriageRequest {
            template: TRIAGE_TEMPLATE.into(),
            language: "en".into(),
            arrival_kind: ArrivalKind::WalkIn,
            age_years: Some(44),
            reason: Some("Cough for three days".into()),
            concerns: vec!["cough".into(), "fever".into()],
            onset: Some("3 days".into()),
            red_flags: vec![],
            vitals: TriageVitals {
                temperature_c: Some(Decimal::new(368, 1)),
                spo2_percent: Some(Decimal::from(97)),
                ..Default::default()
            },
            allergies: vec!["Penicillin".into()],
            requested_service: None,
            facts: vec![("visit:reason".into(), "Cough for three days".into())],
        }
    }

    #[test]
    fn deterministic_and_cites_sources() {
        let a = propose(&req()).unwrap();
        let b = propose(&req()).unwrap();
        assert_eq!(a.output, b.output);
        assert_eq!(a.input_hash, b.input_hash);
        assert_eq!(a.output.cited_sources, vec!["visit:reason".to_string()]);
        assert_eq!(a.output.proposed_priority, Priority::Standard);
        assert_eq!(a.output.proposed_service, "general_medicine");
        assert_eq!(a.output.confidence, Confidence::High);
        assert!(!a.output.limitations.is_empty());
    }

    #[test]
    fn flags_fever_contradiction_and_missing_information() {
        let a = propose(&req()).unwrap();
        assert_eq!(a.output.contradictions.len(), 1);
        let mut r = req();
        r.onset = None;
        r.vitals = TriageVitals::default();
        let a = propose(&r).unwrap();
        assert_eq!(a.output.missing_information.len(), 2);
        assert_eq!(a.output.confidence, Confidence::Medium);
        assert!(a.output.contradictions.is_empty());
    }

    #[test]
    fn red_flags_and_urgent_arrival_raise_priority_and_route_to_emergency() {
        let mut r = req();
        r.concerns = vec!["rash".into()];
        r.red_flags = vec!["chest_pain".into()];
        let a = propose(&r).unwrap();
        assert_eq!(a.output.proposed_priority, Priority::Urgent);
        assert_eq!(a.output.proposed_service, "emergency");
        let mut r = req();
        r.concerns = vec!["rash".into()];
        r.arrival_kind = ArrivalKind::Urgent;
        assert_eq!(
            propose(&r).unwrap().output.proposed_priority,
            Priority::Urgent
        );
    }

    #[test]
    fn remote_arrival_routes_to_telehealth_unless_requested() {
        let mut r = req();
        r.arrival_kind = ArrivalKind::Remote;
        assert_eq!(propose(&r).unwrap().output.proposed_service, "telehealth");
        r.requested_service = Some("nursing".into());
        assert_eq!(propose(&r).unwrap().output.proposed_service, "nursing");
    }

    #[test]
    fn spanish_output_is_localized() {
        let mut r = req();
        r.language = "es".into();
        let a = propose(&r).unwrap();
        assert!(a.output.handoff_summary.starts_with("Paciente"));
        assert!(a.output.limitations[0].starts_with("Generado"));
    }

    #[test]
    fn unsupported_template_is_denied() {
        let mut r = req();
        r.template = "other@1".into();
        assert!(matches!(propose(&r), Err(GatewayError::PolicyDenied(_))));
    }
}
