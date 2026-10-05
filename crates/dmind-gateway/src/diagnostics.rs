//! dMind diagnostic operations: request types, the deterministic offline
//! implementations used by the fixture provider, and the shared
//! parse-and-validate step every provider's raw JSON passes through.
//!
//! - `diagnostic-order-suggestion.v1` suggests orderables from a bounded
//!   candidate list WellOS supplies (one call per encounter snapshot);
//! - `diagnostic-result-synthesis.v1` summarizes a final report against
//!   prior results without touching the deterministic interpretation;
//! - `patient-result-explanation.v1` drafts an EN/ES plain-language
//!   explanation of a *reviewed* report version for clinician approval.
//!
//! None of them can create an order, invent an orderable, change a
//! criticality, release or notify; [`wellos_domain::diagnostics_ai`] rejects
//! any output that tries.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use wellos_domain::ai::{Confidence, ProviderInfo};
use wellos_domain::diagnostics::Interpretation;
use wellos_domain::diagnostics_ai::{
    ComponentStatement, DiagnosticOrderSuggestionV1, DiagnosticResultSynthesisV1, DuplicateWarning,
    ExplanationBounds, PatientResultExplanationV1, SuggestedOrderable, SynthesisBounds,
    ORDER_SUGGESTION_SCHEMA, PATIENT_EXPLANATION_SCHEMA, RESULT_SYNTHESIS_SCHEMA,
};

use crate::access::AccessResponse;
use crate::{hash_json, GatewayError};

pub type DiagnosticResponse<T> = AccessResponse<T>;

pub const ORDER_SUGGESTION_TEMPLATE: &str = "diagnostic-order-suggestion@1.0.0";
pub const RESULT_SYNTHESIS_TEMPLATE: &str = "diagnostic-result-synthesis@1.0.0";
pub const PATIENT_EXPLANATION_TEMPLATE: &str = "patient-result-explanation@1.0.0";

pub const ORDER_SUGGESTION_DETERMINISTIC_PROMPT_VERSION: &str =
    "diagnostic-order-suggestion-deterministic.v1";
pub const RESULT_SYNTHESIS_DETERMINISTIC_PROMPT_VERSION: &str =
    "diagnostic-result-synthesis-deterministic.v1";
pub const PATIENT_EXPLANATION_DETERMINISTIC_PROMPT_VERSION: &str =
    "patient-result-explanation-deterministic.v1";

pub const MAX_CANDIDATES: usize = 60;
pub const MAX_FACTS: usize = 120;
pub const MAX_COMPONENTS: usize = 200;
const MAX_SUGGESTIONS: usize = 5;

fn check_template(actual: &str, expected: &str) -> Result<(), GatewayError> {
    if actual != expected {
        return Err(GatewayError::PolicyDenied(format!(
            "unsupported template {actual}"
        )));
    }
    Ok(())
}

fn check_facts(facts: &[(String, String)]) -> Result<(), GatewayError> {
    if facts.len() > MAX_FACTS {
        return Err(GatewayError::InvalidOutput("too many facts".into()));
    }
    for (r, s) in facts {
        if r.trim().is_empty() || r.chars().count() > 120 {
            return Err(GatewayError::InvalidOutput(
                "fact reference is invalid".into(),
            ));
        }
        if s.chars().count() > 2_000 {
            return Err(GatewayError::InvalidOutput(
                "fact statement is oversized".into(),
            ));
        }
    }
    Ok(())
}

fn es(language: &str) -> bool {
    language.starts_with("es")
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .map(|c| match c {
            'á' => 'a',
            'é' => 'e',
            'í' => 'i',
            'ó' => 'o',
            'ú' | 'ü' => 'u',
            'ñ' => 'n',
            _ => c,
        })
        .collect()
}

// ---------------------------------------------------------------------------
// diagnostic-order-suggestion.v1
// ---------------------------------------------------------------------------

/// One orderable the model may suggest. Only entries supplied here can be
/// named in the output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateOrderable {
    pub orderable_id: Uuid,
    pub code: String,
    pub name_en: String,
    pub name_es: String,
    #[serde(default)]
    pub synonyms: Vec<String>,
    pub category_code: String,
    /// Deterministic keywords (from the catalog config) that indicate the
    /// orderable; used by the fixture provider and shown to real models.
    #[serde(default)]
    pub indications: Vec<String>,
    #[serde(default)]
    pub preparation: Option<String>,
    /// Set by WellOS when an equivalent order is recent or pending.
    #[serde(default)]
    pub recent_or_pending: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderSuggestionRequest {
    pub template: String,
    pub language: String,
    /// Policy-filtered encounter evidence as `(reference, statement)`:
    /// `note:<section>`, `obs:<id>`, `order:<id>`, `brief:<key>`,
    /// `vital:<id>`, `diagnosis:<id>`.
    pub facts: Vec<(String, String)>,
    pub candidates: Vec<CandidateOrderable>,
}

impl OrderSuggestionRequest {
    pub fn check(&self) -> Result<(), GatewayError> {
        check_template(&self.template, ORDER_SUGGESTION_TEMPLATE)?;
        check_facts(&self.facts)?;
        if self.candidates.is_empty() || self.candidates.len() > MAX_CANDIDATES {
            return Err(GatewayError::InvalidOutput(
                "candidate list is empty or oversized".into(),
            ));
        }
        Ok(())
    }

    pub fn candidate_ids(&self) -> Vec<Uuid> {
        self.candidates.iter().map(|c| c.orderable_id).collect()
    }

    pub fn fact_refs(&self) -> Vec<String> {
        self.facts
            .iter()
            .map(|(r, _)| r.clone())
            .chain(
                self.candidates
                    .iter()
                    .map(|c| format!("catalog:{}", c.code)),
            )
            .collect()
    }
}

pub fn parse_suggestion(
    raw: &Value,
    req: &OrderSuggestionRequest,
) -> Result<DiagnosticOrderSuggestionV1, GatewayError> {
    let mut raw = raw.clone();
    if let Some(obj) = raw.as_object_mut() {
        obj.insert(
            "schema_version".into(),
            Value::from(ORDER_SUGGESTION_SCHEMA),
        );
    }
    let out: DiagnosticOrderSuggestionV1 = serde_json::from_value(raw)
        .map_err(|e| GatewayError::InvalidOutput(format!("suggestion schema: {e}")))?;
    out.validate(&req.candidate_ids(), &req.fact_refs())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(out)
}

fn keyword_hits(text: &str, c: &CandidateOrderable) -> usize {
    let mut hits = 0;
    let words = c
        .indications
        .iter()
        .chain(c.synonyms.iter())
        .chain([&c.name_en, &c.name_es])
        .map(|w| normalize(w))
        .filter(|w| w.chars().count() >= 4);
    for w in words {
        if text.contains(&w) {
            hits += 1;
        }
    }
    hits
}

pub fn deterministic_suggestion(
    req: &OrderSuggestionRequest,
    provider: ProviderInfo,
) -> Result<DiagnosticResponse<DiagnosticOrderSuggestionV1>, GatewayError> {
    req.check()?;
    let spanish = es(&req.language);
    let mut scored: Vec<(usize, &CandidateOrderable, Vec<String>)> = Vec::new();
    for c in &req.candidates {
        let mut matched_refs = Vec::new();
        let mut total = 0;
        for (r, s) in &req.facts {
            let hits = keyword_hits(&normalize(s), c);
            if hits > 0 {
                total += hits;
                matched_refs.push(r.clone());
            }
        }
        if total > 0 {
            scored.push((total, c, matched_refs));
        }
    }
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.code.cmp(&b.1.code)));
    let mut cited = Vec::new();
    let mut suggestions = Vec::new();
    let mut duplicate_warnings = Vec::new();
    for (hits, c, refs) in scored.iter().take(MAX_SUGGESTIONS) {
        let name = if spanish { &c.name_es } else { &c.name_en };
        let rationale = if spanish {
            format!(
                "{name} coincide con {hits} elemento(s) de la evidencia de la consulta ({}).",
                refs.join(", ")
            )
        } else {
            format!(
                "{name} matches {hits} item(s) in the consultation evidence ({}).",
                refs.join(", ")
            )
        };
        let mut sources = refs.clone();
        sources.push(format!("catalog:{}", c.code));
        for s in &sources {
            if !cited.contains(s) {
                cited.push(s.clone());
            }
        }
        suggestions.push(SuggestedOrderable {
            orderable_id: c.orderable_id,
            rationale,
            cited_sources: sources.clone(),
            proposed_timing: Some("routine".into()),
            preparation_note: c.preparation.clone(),
        });
        if c.recent_or_pending {
            duplicate_warnings.push(DuplicateWarning {
                orderable_id: c.orderable_id,
                reason: if spanish {
                    format!("{name} ya tiene una solicitud reciente o pendiente.")
                } else {
                    format!("{name} already has a recent or pending request.")
                },
                cited_sources: vec![format!("catalog:{}", c.code)],
            });
        }
    }
    let mut missing_information = Vec::new();
    if req.facts.is_empty() {
        missing_information.push(if spanish {
            "No hay evidencia de la consulta disponible; documente el motivo de consulta."
                .to_string()
        } else {
            "No consultation evidence is available; document the presenting problem.".to_string()
        });
    }
    if suggestions.is_empty() {
        missing_information.push(if spanish {
            "Ningún elemento del catálogo coincide con la evidencia documentada.".to_string()
        } else {
            "No catalog item matches the documented evidence.".to_string()
        });
    }
    let limitations = vec![
        if spanish {
            "Sugerencia determinista de desarrollo basada en coincidencia de palabras clave; no es una recomendación clínica.".to_string()
        } else {
            "Deterministic development suggestion based on keyword matching; not a clinical recommendation.".to_string()
        },
        if spanish {
            "El profesional decide qué solicitar, la urgencia y los requisitos; dMind no crea órdenes.".to_string()
        } else {
            "The clinician decides what to request, its urgency and prerequisites; dMind creates no orders.".to_string()
        },
    ];
    let output = DiagnosticOrderSuggestionV1 {
        schema_version: ORDER_SUGGESTION_SCHEMA.into(),
        suggestions,
        duplicate_warnings,
        missing_information,
        cited_sources: cited,
        confidence: Confidence::Low,
        limitations,
    };
    output
        .validate(&req.candidate_ids(), &req.fact_refs())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(DiagnosticResponse {
        output,
        provider,
        prompt_version: ORDER_SUGGESTION_DETERMINISTIC_PROMPT_VERSION.into(),
        input_hash: hash_json(req),
        usage: None,
    })
}

// ---------------------------------------------------------------------------
// diagnostic-result-synthesis.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SynthesisComponent {
    /// `component:<observation id>`.
    pub component_ref: String,
    pub display: String,
    pub value_text: String,
    pub interpretation: Interpretation,
    #[serde(default)]
    pub reference_range: Option<String>,
    /// Prior value of the same component (`(reference, value text)`).
    #[serde(default)]
    pub prior: Option<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultSynthesisRequest {
    pub template: String,
    pub language: String,
    /// `report:<id>`.
    pub report_ref: String,
    pub report_display: String,
    pub report_status: String,
    pub criticality: Interpretation,
    #[serde(default)]
    pub conclusion: Option<String>,
    pub components: Vec<SynthesisComponent>,
    /// Additional policy-filtered facts (prior reports, trends, context).
    #[serde(default)]
    pub facts: Vec<(String, String)>,
}

impl ResultSynthesisRequest {
    pub fn check(&self) -> Result<(), GatewayError> {
        check_template(&self.template, RESULT_SYNTHESIS_TEMPLATE)?;
        check_facts(&self.facts)?;
        if !self.report_ref.starts_with("report:") {
            return Err(GatewayError::InvalidOutput("report_ref is invalid".into()));
        }
        if self.components.len() > MAX_COMPONENTS {
            return Err(GatewayError::InvalidOutput("too many components".into()));
        }
        Ok(())
    }

    pub fn bounds(&self) -> SynthesisBounds {
        let mut fact_refs = vec![self.report_ref.clone()];
        for c in &self.components {
            fact_refs.push(c.component_ref.clone());
            if let Some((r, _)) = &c.prior {
                fact_refs.push(r.clone());
            }
        }
        fact_refs.extend(self.facts.iter().map(|(r, _)| r.clone()));
        SynthesisBounds {
            report_ref: self.report_ref.clone(),
            criticality: self.criticality,
            components: self
                .components
                .iter()
                .map(|c| (c.component_ref.clone(), c.interpretation))
                .collect(),
            fact_refs,
        }
    }
}

pub fn parse_synthesis(
    raw: &Value,
    req: &ResultSynthesisRequest,
) -> Result<DiagnosticResultSynthesisV1, GatewayError> {
    let mut raw = raw.clone();
    if let Some(obj) = raw.as_object_mut() {
        obj.insert(
            "schema_version".into(),
            Value::from(RESULT_SYNTHESIS_SCHEMA),
        );
    }
    let out: DiagnosticResultSynthesisV1 = serde_json::from_value(raw)
        .map_err(|e| GatewayError::InvalidOutput(format!("synthesis schema: {e}")))?;
    out.validate(&req.bounds())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(out)
}

fn interp_word(i: Interpretation, spanish: bool) -> &'static str {
    match (i, spanish) {
        (Interpretation::Critical, false) => "critical",
        (Interpretation::Critical, true) => "crítico",
        (Interpretation::Abnormal, false) => "outside the reference range",
        (Interpretation::Abnormal, true) => "fuera del rango de referencia",
        (Interpretation::Normal, false) => "within the reference range",
        (Interpretation::Normal, true) => "dentro del rango de referencia",
        (Interpretation::Unknown, false) => "not interpreted by a deterministic rule",
        (Interpretation::Unknown, true) => "sin interpretación determinista",
    }
}

pub fn deterministic_synthesis(
    req: &ResultSynthesisRequest,
    provider: ProviderInfo,
) -> Result<DiagnosticResponse<DiagnosticResultSynthesisV1>, GatewayError> {
    req.check()?;
    let spanish = es(&req.language);
    let mut components = Vec::new();
    let mut changes = Vec::new();
    let mut cited = vec![req.report_ref.clone()];
    for c in &req.components {
        let mut sources = vec![c.component_ref.clone()];
        let change = c.prior.as_ref().map(|(r, v)| {
            sources.push(r.clone());
            let s = if spanish {
                format!("Valor previo {v} ({r}).")
            } else {
                format!("Prior value {v} ({r}).")
            };
            changes.push(format!("{}: {} → {}", c.display, v, c.value_text));
            s
        });
        let statement = if spanish {
            format!(
                "{}: {} — {}{}.",
                c.display,
                c.value_text,
                interp_word(c.interpretation, true),
                c.reference_range
                    .as_ref()
                    .map(|r| format!(" (referencia {r})"))
                    .unwrap_or_default()
            )
        } else {
            format!(
                "{}: {} — {}{}.",
                c.display,
                c.value_text,
                interp_word(c.interpretation, false),
                c.reference_range
                    .as_ref()
                    .map(|r| format!(" (reference {r})"))
                    .unwrap_or_default()
            )
        };
        for s in &sources {
            if !cited.contains(s) {
                cited.push(s.clone());
            }
        }
        components.push(ComponentStatement {
            component_ref: c.component_ref.clone(),
            interpretation: c.interpretation,
            statement,
            change_from_prior: change,
            cited_sources: sources,
        });
    }
    let n_abn = req
        .components
        .iter()
        .filter(|c| c.interpretation >= Interpretation::Abnormal)
        .count();
    let summary = if spanish {
        format!(
            "Informe {} ({}) con criticidad determinista «{}»: {} de {} componente(s) fuera de rango.{}",
            req.report_display,
            req.report_status,
            interp_word(req.criticality, true),
            n_abn,
            req.components.len(),
            req.conclusion
                .as_ref()
                .map(|c| format!(" Conclusión del informe: {c}"))
                .unwrap_or_default()
        )
    } else {
        format!(
            "Report {} ({}) with deterministic criticality \"{}\": {} of {} component(s) outside range.{}",
            req.report_display,
            req.report_status,
            interp_word(req.criticality, false),
            n_abn,
            req.components.len(),
            req.conclusion
                .as_ref()
                .map(|c| format!(" Report conclusion: {c}"))
                .unwrap_or_default()
        )
    };
    let missing_information = if req.components.is_empty() && req.conclusion.is_none() {
        vec![if spanish {
            "El informe no contiene componentes ni conclusión.".to_string()
        } else {
            "The report contains no components and no conclusion.".to_string()
        }]
    } else {
        vec![]
    };
    let output = DiagnosticResultSynthesisV1 {
        schema_version: RESULT_SYNTHESIS_SCHEMA.into(),
        report_ref: req.report_ref.clone(),
        summary,
        criticality: req.criticality,
        components,
        changes,
        contradictions: vec![],
        missing_information,
        cited_sources: cited,
        confidence: Confidence::Low,
        limitations: vec![if spanish {
            "Síntesis determinista de desarrollo; no sustituye la revisión profesional ni modifica el informe.".to_string()
        } else {
            "Deterministic development synthesis; it does not replace professional review and does not modify the report.".to_string()
        }],
    };
    output
        .validate(&req.bounds())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(DiagnosticResponse {
        output,
        provider,
        prompt_version: RESULT_SYNTHESIS_DETERMINISTIC_PROMPT_VERSION.into(),
        input_hash: hash_json(req),
        usage: None,
    })
}

// ---------------------------------------------------------------------------
// patient-result-explanation.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PatientExplanationRequest {
    pub template: String,
    pub report_ref: String,
    pub report_version: i64,
    pub report_display: String,
    pub criticality: Interpretation,
    /// Reviewer's clinical assessment and disposition (the professional
    /// review the explanation must stay consistent with).
    pub review_summary: String,
    /// Facts explained: `report:<id>`, `component:<id>`, `review:<id>`.
    pub facts: Vec<(String, String)>,
}

impl PatientExplanationRequest {
    pub fn check(&self) -> Result<(), GatewayError> {
        check_template(&self.template, PATIENT_EXPLANATION_TEMPLATE)?;
        check_facts(&self.facts)?;
        if !self.report_ref.starts_with("report:") || self.report_version < 1 {
            return Err(GatewayError::InvalidOutput(
                "report binding is invalid".into(),
            ));
        }
        if self.review_summary.trim().is_empty() {
            return Err(GatewayError::PolicyDenied(
                "patient explanation requires a professional review".into(),
            ));
        }
        Ok(())
    }

    pub fn bounds(&self) -> ExplanationBounds {
        let mut fact_refs = vec![self.report_ref.clone()];
        fact_refs.extend(self.facts.iter().map(|(r, _)| r.clone()));
        ExplanationBounds {
            report_ref: self.report_ref.clone(),
            report_version: self.report_version,
            criticality: self.criticality,
            fact_refs,
        }
    }
}

pub fn parse_explanation(
    raw: &Value,
    req: &PatientExplanationRequest,
) -> Result<PatientResultExplanationV1, GatewayError> {
    let mut raw = raw.clone();
    if let Some(obj) = raw.as_object_mut() {
        obj.insert(
            "schema_version".into(),
            Value::from(PATIENT_EXPLANATION_SCHEMA),
        );
        obj.insert("report_ref".into(), Value::from(req.report_ref.clone()));
        obj.insert("report_version".into(), Value::from(req.report_version));
    }
    let out: PatientResultExplanationV1 = serde_json::from_value(raw)
        .map_err(|e| GatewayError::InvalidOutput(format!("explanation schema: {e}")))?;
    out.validate(&req.bounds())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(out)
}

pub fn deterministic_explanation(
    req: &PatientExplanationRequest,
    provider: ProviderInfo,
) -> Result<DiagnosticResponse<PatientResultExplanationV1>, GatewayError> {
    req.check()?;
    let (en_state, es_state) = match req.criticality {
        Interpretation::Critical => (
            "Your result includes a value that your care team treats as urgent",
            "Su resultado incluye un valor que su equipo asistencial trata como urgente",
        ),
        Interpretation::Abnormal => (
            "Your result includes a value outside the usual range",
            "Su resultado incluye un valor fuera del rango habitual",
        ),
        Interpretation::Normal => (
            "Your result values are within the usual range",
            "Los valores de su resultado están dentro del rango habitual",
        ),
        Interpretation::Unknown => (
            "Your result has been reviewed by your clinician",
            "Su resultado ha sido revisado por su profesional",
        ),
    };
    let explanation_en = format!(
        "{en_state}. This is the {} report your clinician reviewed. The reviewer noted: \"{}\". This explanation describes the result; it is not a diagnosis.",
        req.report_display,
        req.review_summary.trim()
    );
    let explanation_es = format!(
        "{es_state}. Se trata del informe de {} que revisó su profesional. La persona revisora indicó: «{}». Esta explicación describe el resultado; no es un diagnóstico.",
        req.report_display,
        req.review_summary.trim()
    );
    let mut cited = vec![req.report_ref.clone()];
    cited.extend(req.facts.iter().map(|(r, _)| r.clone()));
    cited.truncate(20);
    let output = PatientResultExplanationV1 {
        schema_version: PATIENT_EXPLANATION_SCHEMA.into(),
        report_ref: req.report_ref.clone(),
        report_version: req.report_version,
        explanation_en,
        explanation_es,
        cited_sources: cited,
        next_steps_en: "Your clinician will discuss what this means for you and any next steps. Contact your care team if you have questions.".into(),
        next_steps_es: "Su profesional comentará con usted qué significa y los siguientes pasos. Contacte con su equipo asistencial si tiene dudas.".into(),
        confidence: Confidence::Low,
        limitations: vec![
            "Deterministic development draft; a clinician must approve or edit it before release.".into(),
        ],
    };
    output
        .validate(&req.bounds())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(DiagnosticResponse {
        output,
        provider,
        prompt_version: PATIENT_EXPLANATION_DETERMINISTIC_PROMPT_VERSION.into(),
        input_hash: hash_json(req),
        usage: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider() -> ProviderInfo {
        ProviderInfo {
            provider: "test".into(),
            model: "m".into(),
            model_version: "1".into(),
        }
    }

    fn candidate(code: &str, name: &str, indications: &[&str]) -> CandidateOrderable {
        CandidateOrderable {
            orderable_id: Uuid::now_v7(),
            code: code.into(),
            name_en: name.into(),
            name_es: name.into(),
            synonyms: vec![],
            category_code: "laboratory".into(),
            indications: indications.iter().map(|s| s.to_string()).collect(),
            preparation: None,
            recent_or_pending: false,
        }
    }

    #[test]
    fn suggestion_only_names_supplied_candidates_and_cites_facts() {
        let req = OrderSuggestionRequest {
            template: ORDER_SUGGESTION_TEMPLATE.into(),
            language: "en".into(),
            facts: vec![(
                "note:assessment".into(),
                "Fatigue and polyuria, suspect hyperglycaemia".into(),
            )],
            candidates: vec![
                candidate("glucose", "Glucose", &["hyperglycaemia", "polyuria"]),
                candidate("cxr", "Chest radiograph", &["cough", "dyspnoea"]),
            ],
        };
        let out = deterministic_suggestion(&req, provider()).unwrap().output;
        assert_eq!(out.suggestions.len(), 1);
        assert_eq!(
            out.suggestions[0].orderable_id,
            req.candidates[0].orderable_id
        );
        assert!(out.suggestions[0]
            .cited_sources
            .contains(&"note:assessment".to_string()));
        // A raw provider answer naming an unknown orderable is rejected.
        let raw = serde_json::json!({
            "suggestions": [{"orderable_id": Uuid::now_v7(), "rationale": "x", "cited_sources": ["note:assessment"]}],
            "cited_sources": [], "confidence": "low", "limitations": ["l"]
        });
        assert!(matches!(
            parse_suggestion(&raw, &req),
            Err(GatewayError::InvalidOutput(_))
        ));
    }

    #[test]
    fn synthesis_keeps_interpretation_and_explanation_binds_version() {
        let req = ResultSynthesisRequest {
            template: RESULT_SYNTHESIS_TEMPLATE.into(),
            language: "es".into(),
            report_ref: "report:r1".into(),
            report_display: "Panel metabólico".into(),
            report_status: "final".into(),
            criticality: Interpretation::Critical,
            conclusion: None,
            components: vec![SynthesisComponent {
                component_ref: "component:c1".into(),
                display: "Potasio".into(),
                value_text: "7.1 mmol/L".into(),
                interpretation: Interpretation::Critical,
                reference_range: Some("3.5-5.1".into()),
                prior: Some(("obs:p1".into(), "4.2 mmol/L".into())),
            }],
            facts: vec![],
        };
        let out = deterministic_synthesis(&req, provider()).unwrap().output;
        assert_eq!(out.criticality, Interpretation::Critical);
        assert_eq!(out.components[0].interpretation, Interpretation::Critical);
        assert!(out.summary.contains("crítico"));
        let mut raw = serde_json::to_value(&out).unwrap();
        raw["criticality"] = Value::from("normal");
        assert!(parse_synthesis(&raw, &req).is_err());

        let ereq = PatientExplanationRequest {
            template: PATIENT_EXPLANATION_TEMPLATE.into(),
            report_ref: "report:r1".into(),
            report_version: 2,
            report_display: "metabolic panel".into(),
            criticality: Interpretation::Abnormal,
            review_summary: "Mild hyperkalaemia, repeat in one week".into(),
            facts: vec![("review:v1".into(), "reviewed".into())],
        };
        let e = deterministic_explanation(&ereq, provider()).unwrap().output;
        assert_eq!(e.report_version, 2);
        assert!(!e.explanation_es.is_empty());
        let mut no_review = ereq.clone();
        no_review.review_summary = "  ".into();
        assert!(matches!(
            deterministic_explanation(&no_review, provider()),
            Err(GatewayError::PolicyDenied(_))
        ));
    }
}
