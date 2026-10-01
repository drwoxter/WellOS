//! dMind Access operations: request/response types, the deterministic
//! offline implementations used by the fixture provider, and the shared
//! parse-and-validate step every provider's raw JSON passes through.
//!
//! - `access-intent.v1` structures free text into scheduling constraints;
//! - `appointment-ranking.v1` reorders/explains matcher candidates;
//! - `cancellation-recovery.v1` orders eligible waitlist entries;
//! - `capacity-explanation.v1` explains a deterministic forecast.
//!
//! None of them can create a candidate, entry, date or code that was not
//! supplied; the validators in [`wellos_domain::access_ai`] reject it.

use chrono::{Datelike, NaiveDate, NaiveTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use uuid::Uuid;
use wellos_domain::access::{requires_clinical_triage, Urgency, WeeklyWindow};
use wellos_domain::access_ai::{
    AccessIntentV1, AppointmentRankingV1, CancellationRecoveryV1, CapacityExplanationV1,
    CapacityRecommendation, IntentVocabulary, PressurePoint, RankedCandidate, RecoveryExplanation,
    ACCESS_INTENT_SCHEMA, APPOINTMENT_RANKING_SCHEMA, CANCELLATION_RECOVERY_SCHEMA,
    CAPACITY_EXPLANATION_SCHEMA,
};
use wellos_domain::ai::{Confidence, ProviderInfo};
use wellos_domain::capacity::Forecast;
use wellos_domain::recovery::EligibleEntry;

use crate::{hash_json, GatewayError, Usage};

pub const ACCESS_INTENT_TEMPLATE: &str = "access-intent@1.0.0";
pub const APPOINTMENT_RANKING_TEMPLATE: &str = "appointment-ranking@1.0.0";
pub const CANCELLATION_RECOVERY_TEMPLATE: &str = "cancellation-recovery@1.0.0";
pub const CAPACITY_EXPLANATION_TEMPLATE: &str = "capacity-explanation@1.0.0";

pub const ACCESS_INTENT_DETERMINISTIC_PROMPT_VERSION: &str = "access-intent-deterministic.v1";
pub const APPOINTMENT_RANKING_DETERMINISTIC_PROMPT_VERSION: &str =
    "appointment-ranking-deterministic.v1";
pub const CANCELLATION_RECOVERY_DETERMINISTIC_PROMPT_VERSION: &str =
    "cancellation-recovery-deterministic.v1";
pub const CAPACITY_EXPLANATION_DETERMINISTIC_PROMPT_VERSION: &str =
    "capacity-explanation-deterministic.v1";

/// Longest free text accepted by the intent operation.
pub const MAX_INTENT_TEXT_CHARS: usize = 2_000;
/// Most candidates ever sent to one ranking call (the matcher reduces to
/// this bound deterministically first).
pub const MAX_RANKING_CANDIDATES: usize = 20;
pub const MAX_RECOVERY_ENTRIES: usize = 50;

/// Provider-independent response envelope shared by the four operations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessResponse<T> {
    pub output: T,
    pub provider: ProviderInfo,
    pub prompt_version: String,
    pub input_hash: String,
    pub usage: Option<Usage>,
}

// ---------------------------------------------------------------------------
// access-intent.v1
// ---------------------------------------------------------------------------

/// One vocabulary entry with the words that may name it.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct VocabularyTerm {
    pub code: String,
    pub name_en: String,
    pub name_es: String,
    #[serde(default)]
    pub synonyms: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AccessIntentRequest {
    pub template: String,
    pub language: String,
    pub free_text: String,
    /// Urgency already set by deterministic rules or staff; the model may
    /// not change it and it feeds the triage floor.
    pub urgency: Urgency,
    /// Structured fields the request already carries (`service`,
    /// `modality`, `preferred_windows`, `facility`, `language`,
    /// `accessibility`): the model must not ask for them again.
    #[serde(default)]
    pub already_known: Vec<String>,
    pub services: Vec<VocabularyTerm>,
    pub specialties: Vec<VocabularyTerm>,
    pub modalities: Vec<VocabularyTerm>,
    pub accessibility: Vec<VocabularyTerm>,
    pub facilities: Vec<VocabularyTerm>,
}

impl AccessIntentRequest {
    pub fn knows(&self, field: &str) -> bool {
        self.already_known.iter().any(|f| f == field)
    }

    pub fn vocabulary(&self) -> IntentVocabulary {
        let codes = |v: &[VocabularyTerm]| v.iter().map(|t| t.code.clone()).collect();
        IntentVocabulary {
            services: codes(&self.services),
            specialties: codes(&self.specialties),
            modalities: codes(&self.modalities),
            accessibility: codes(&self.accessibility),
            facilities: codes(&self.facilities),
        }
    }

    pub fn floor_reason(&self) -> Option<&'static str> {
        requires_clinical_triage(Some(&self.free_text), self.urgency)
    }

    pub fn check(&self) -> Result<(), GatewayError> {
        if self.template != ACCESS_INTENT_TEMPLATE {
            return Err(GatewayError::PolicyDenied(format!(
                "unsupported template {}",
                self.template
            )));
        }
        if self.free_text.trim().is_empty() {
            return Err(GatewayError::InvalidOutput("request text is empty".into()));
        }
        if self.free_text.chars().count() > MAX_INTENT_TEXT_CHARS {
            return Err(GatewayError::InvalidOutput(
                "request text is oversized".into(),
            ));
        }
        Ok(())
    }
}

/// Parse raw provider JSON into a floor-applied, vocabulary-validated intent.
pub fn parse_intent(
    raw: &Value,
    req: &AccessIntentRequest,
) -> Result<AccessIntentV1, GatewayError> {
    let mut raw = raw.clone();
    if let Some(obj) = raw.as_object_mut() {
        obj.insert("schema_version".into(), Value::from(ACCESS_INTENT_SCHEMA));
    }
    let parsed: AccessIntentV1 = serde_json::from_value(raw).map_err(|e| {
        GatewayError::InvalidOutput(format!("access intent does not match schema: {e}"))
    })?;
    parsed
        .finalize(&req.vocabulary(), req.floor_reason())
        .map_err(GatewayError::InvalidOutput)
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

fn term_matches(text: &str, term: &VocabularyTerm) -> bool {
    std::iter::once(term.name_en.as_str())
        .chain(std::iter::once(term.name_es.as_str()))
        .chain(term.synonyms.iter().map(String::as_str))
        .filter(|w| w.len() >= 3)
        .any(|w| text.contains(&normalize(w)))
}

fn hm(h: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(h, 0, 0).expect("valid hour")
}

/// Deterministic keyword-based intent extraction. Pure and offline.
pub fn deterministic_intent(
    req: &AccessIntentRequest,
    provider: ProviderInfo,
) -> Result<AccessResponse<AccessIntentV1>, GatewayError> {
    req.check()?;
    let es = req.language.starts_with("es");
    let text = normalize(&req.free_text);
    let mut cited = vec!["request:text".to_string()];
    let pick = |terms: &[VocabularyTerm], kind: &str, cited: &mut Vec<String>| -> Vec<String> {
        let mut out = Vec::new();
        for t in terms {
            if term_matches(&text, t) {
                out.push(t.code.clone());
                cited.push(format!("catalog:{kind}:{}", t.code));
            }
        }
        out
    };
    let services = pick(&req.services, "service", &mut cited);
    let specialties = pick(&req.specialties, "specialty", &mut cited);
    let modality_codes = pick(&req.modalities, "modality", &mut cited);
    let accessibility_codes = pick(&req.accessibility, "accessibility", &mut cited);
    let facility_codes = pick(&req.facilities, "facility", &mut cited);

    let mut preferred_windows = Vec::new();
    let weekdays: &[(&[&str], u8)] = &[
        (&["monday", "lunes"], 1),
        (&["tuesday", "martes"], 2),
        (&["wednesday", "miercoles"], 3),
        (&["thursday", "jueves"], 4),
        (&["friday", "viernes"], 5),
        (&["saturday", "sabado"], 6),
        (&["sunday", "domingo"], 7),
    ];
    let morning = ["morning", "manana", "por la manana"]
        .iter()
        .any(|w| text.contains(w));
    let afternoon = [
        "afternoon",
        "tarde",
        "evening",
        "after work",
        "despues del trabajo",
    ]
    .iter()
    .any(|w| text.contains(w));
    let (start, end) = match (morning, afternoon) {
        (true, false) => (hm(8), hm(13)),
        (false, true) => (hm(15), hm(20)),
        _ => (hm(8), hm(20)),
    };
    let mut named_days: Vec<u8> = weekdays
        .iter()
        .filter(|(words, _)| words.iter().any(|w| text.contains(w)))
        .map(|(_, d)| *d)
        .collect();
    if named_days.is_empty() && (morning || afternoon) {
        named_days = (1..=5).collect();
    }
    for d in named_days {
        preferred_windows.push(WeeklyWindow {
            weekday: d,
            start,
            end,
        });
    }

    let continuity_requested = [
        "same doctor",
        "my doctor",
        "usual doctor",
        "mismo medico",
        "mi medico",
        "misma doctora",
        "mi doctora",
        "same nurse",
        "misma enfermera",
    ]
    .iter()
    .any(|w| text.contains(w));
    let transport_requested = [
        "transport",
        "transporte",
        "ambulance",
        "ambulancia",
        "pick me up",
        "recogerme",
        "cannot travel",
        "no puedo desplazarme",
    ]
    .iter()
    .any(|w| text.contains(w));
    let language = if text.contains("in spanish") || text.contains("en espanol") {
        Some("es".to_string())
    } else if text.contains("in english") || text.contains("en ingles") {
        Some("en".to_string())
    } else {
        None
    };

    let mut missing = Vec::new();
    if services.is_empty() && specialties.is_empty() && !req.knows("service") {
        missing.push(if es {
            "¿Qué tipo de consulta o servicio necesita?".to_string()
        } else {
            "Which service or type of consultation do you need?".to_string()
        });
    }
    if preferred_windows.is_empty() && !req.knows("preferred_windows") {
        missing.push(if es {
            "¿Qué días u horas le vienen mejor?".to_string()
        } else {
            "Which days or times suit you best?".to_string()
        });
    }
    if modality_codes.is_empty() && req.modalities.len() > 1 && !req.knows("modality") {
        missing.push(if es {
            "¿Prefiere consulta presencial, telefónica o por vídeo?".to_string()
        } else {
            "Do you prefer in-person, phone or video?".to_string()
        });
    }

    let floor = req.floor_reason();
    let confidence = match (
        services.len() + specialties.len(),
        preferred_windows.is_empty(),
    ) {
        (0, _) => Confidence::Low,
        (1, false) => Confidence::High,
        _ => Confidence::Medium,
    };
    let limitations = vec![if es {
        "Interpretación determinista por palabras clave; no evalúa síntomas ni urgencia clínica."
            .to_string()
    } else {
        "Deterministic keyword interpretation; does not assess symptoms or clinical urgency."
            .to_string()
    }];
    let output = AccessIntentV1 {
        schema_version: ACCESS_INTENT_SCHEMA.into(),
        service_code: services.first().cloned(),
        specialty_code: specialties.first().cloned(),
        modality_codes,
        facility_codes,
        accessibility_codes,
        preferred_windows,
        earliest_date: None,
        latest_date: None,
        language,
        continuity_requested,
        transport_requested,
        missing_information: missing,
        clinical_triage_suggested: floor.is_some(),
        triage_reasons: floor.map(|r| vec![r.to_string()]).unwrap_or_default(),
        cited_sources: cited,
        confidence,
        limitations,
    }
    .finalize(&req.vocabulary(), floor)
    .map_err(GatewayError::InvalidOutput)?;
    Ok(AccessResponse {
        output,
        provider,
        prompt_version: ACCESS_INTENT_DETERMINISTIC_PROMPT_VERSION.into(),
        input_hash: hash_json(req),
        usage: None,
    })
}

// ---------------------------------------------------------------------------
// appointment-ranking.v1
// ---------------------------------------------------------------------------

/// A deterministic candidate reduced to the facts a ranking may talk about.
/// Labels are already patient-appropriate (no internal ids besides the
/// candidate id).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RankingCandidate {
    pub candidate_id: String,
    pub starts_at: chrono::DateTime<Utc>,
    pub ends_at: chrono::DateTime<Utc>,
    pub facility_label: String,
    pub modality_code: String,
    pub resource_labels: Vec<String>,
    pub score: f64,
    /// (factor code, points, detail) from the matcher decomposition.
    pub factors: Vec<(String, f64, String)>,
    pub travel_minutes: Option<f64>,
    pub reasons: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RankingRequest {
    pub template: String,
    pub language: String,
    /// (reference, statement) facts about the request/preferences.
    pub facts: Vec<(String, String)>,
    /// Candidates in deterministic order (best first).
    pub candidates: Vec<RankingCandidate>,
}

impl RankingRequest {
    pub fn candidate_ids(&self) -> Vec<String> {
        self.candidates
            .iter()
            .map(|c| c.candidate_id.clone())
            .collect()
    }

    pub fn fact_refs(&self) -> Vec<String> {
        self.facts.iter().map(|(r, _)| r.clone()).collect()
    }

    pub fn check(&self) -> Result<(), GatewayError> {
        if self.template != APPOINTMENT_RANKING_TEMPLATE {
            return Err(GatewayError::PolicyDenied(format!(
                "unsupported template {}",
                self.template
            )));
        }
        if self.candidates.is_empty() {
            return Err(GatewayError::InvalidOutput("no candidates supplied".into()));
        }
        if self.candidates.len() > MAX_RANKING_CANDIDATES {
            return Err(GatewayError::PolicyDenied(format!(
                "at most {MAX_RANKING_CANDIDATES} candidates per ranking call"
            )));
        }
        Ok(())
    }
}

pub fn parse_ranking(
    raw: &Value,
    req: &RankingRequest,
) -> Result<AppointmentRankingV1, GatewayError> {
    let mut raw = raw.clone();
    if let Some(obj) = raw.as_object_mut() {
        obj.insert(
            "schema_version".into(),
            Value::from(APPOINTMENT_RANKING_SCHEMA),
        );
    }
    let parsed: AppointmentRankingV1 = serde_json::from_value(raw)
        .map_err(|e| GatewayError::InvalidOutput(format!("ranking does not match schema: {e}")))?;
    parsed
        .validate(&req.candidate_ids(), &req.fact_refs())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(parsed)
}

fn factor_phrase(code: &str, es: bool) -> &'static str {
    match (code, es) {
        ("soonness", false) => "an early date",
        ("soonness", true) => "una fecha temprana",
        ("waiting_time", false) => "the time already waited",
        ("waiting_time", true) => "el tiempo ya esperado",
        ("preferred_time", false) => "a preferred day and time",
        ("preferred_time", true) => "un día y hora preferidos",
        ("travel", false) => "a short journey",
        ("travel", true) => "un desplazamiento corto",
        ("continuity", false) => "continuity with the usual care team",
        ("continuity", true) => "continuidad con el equipo habitual",
        ("utilization", false) => "good use of available capacity",
        ("utilization", true) => "buen uso de la capacidad disponible",
        ("cancellation_gap", false) => "a slot freed by a cancellation",
        ("cancellation_gap", true) => "un hueco liberado por una cancelación",
        ("waitlist_fairness", false) => "a fair waitlist position",
        ("waitlist_fairness", true) => "una posición justa en la lista de espera",
        ("seasonal_demand", false) => "lower seasonal pressure",
        ("seasonal_demand", true) => "menor presión estacional",
        ("confirmation_support", false) => "a time easier to keep",
        ("confirmation_support", true) => "una hora más fácil de cumplir",
        (_, false) => "the deterministic score",
        (_, true) => "la puntuación determinista",
    }
}

/// Deterministic ranking: preserves the matcher order and explains each
/// candidate from its top score factors.
pub fn deterministic_ranking(
    req: &RankingRequest,
    provider: ProviderInfo,
) -> Result<AccessResponse<AppointmentRankingV1>, GatewayError> {
    req.check()?;
    let es = req.language.starts_with("es");
    let mut ordered = req.candidates.clone();
    ordered.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then(a.starts_at.cmp(&b.starts_at))
            .then(a.candidate_id.cmp(&b.candidate_id))
    });
    let ranked = ordered
        .iter()
        .enumerate()
        .map(|(i, c)| {
            let mut top: Vec<&(String, f64, String)> =
                c.factors.iter().filter(|f| f.1 > 0.0).collect();
            top.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
            let phrases: Vec<&str> = top
                .iter()
                .take(3)
                .map(|f| factor_phrase(&f.0, es))
                .collect();
            let explanation = if phrases.is_empty() {
                if es {
                    "Opción válida según las reglas deterministas.".to_string()
                } else {
                    "Valid option under the deterministic rules.".to_string()
                }
            } else if es {
                format!("Recomendada por {}.", phrases.join(", "))
            } else {
                format!("Recommended for {}.", phrases.join(", "))
            };
            RankedCandidate {
                candidate_id: c.candidate_id.clone(),
                rank: i as u32 + 1,
                explanation,
                cited_sources: vec![c.candidate_id.clone()],
            }
        })
        .collect();
    let output = AppointmentRankingV1 {
        schema_version: APPOINTMENT_RANKING_SCHEMA.into(),
        ranked,
        overall_note: Some(if es {
            "Orden determinista: las opciones se ordenan por la puntuación transparente del buscador."
                .to_string()
        } else {
            "Deterministic order: options are sorted by the matcher's transparent score."
                .to_string()
        }),
        cited_sources: req.fact_refs(),
        confidence: Confidence::High,
        limitations: vec![if es {
            "Explicación determinista; no sustituye la elección del paciente ni la valoración clínica."
                .to_string()
        } else {
            "Deterministic explanation; does not replace patient choice or clinical judgement."
                .to_string()
        }],
    };
    output
        .validate(&req.candidate_ids(), &req.fact_refs())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(AccessResponse {
        output,
        provider,
        prompt_version: APPOINTMENT_RANKING_DETERMINISTIC_PROMPT_VERSION.into(),
        input_hash: hash_json(req),
        usage: None,
    })
}

// ---------------------------------------------------------------------------
// cancellation-recovery.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RecoveryRankingRequest {
    pub template: String,
    pub language: String,
    /// Facts about the freed slot (reference, statement).
    pub facts: Vec<(String, String)>,
    /// Deterministically eligible entries, already in fairness order.
    pub entries: Vec<EligibleEntry>,
    /// How many hours of extra waiting one entry may be demoted below a
    /// shorter-waiting one (tenant policy).
    pub max_wait_demotion_hours: i64,
}

impl RecoveryRankingRequest {
    pub fn entry_ids(&self) -> Vec<Uuid> {
        self.entries.iter().map(|e| e.entry_id).collect()
    }

    pub fn fact_refs(&self) -> Vec<String> {
        self.facts.iter().map(|(r, _)| r.clone()).collect()
    }

    pub fn check(&self) -> Result<(), GatewayError> {
        if self.template != CANCELLATION_RECOVERY_TEMPLATE {
            return Err(GatewayError::PolicyDenied(format!(
                "unsupported template {}",
                self.template
            )));
        }
        if self.entries.is_empty() {
            return Err(GatewayError::InvalidOutput("no eligible entries".into()));
        }
        if self.entries.len() > MAX_RECOVERY_ENTRIES {
            return Err(GatewayError::PolicyDenied(format!(
                "at most {MAX_RECOVERY_ENTRIES} entries per recovery call"
            )));
        }
        Ok(())
    }
}

/// Parse and validate, including the urgency/fairness floors: an order the
/// floors reject is an invalid output, never silently corrected.
pub fn parse_recovery(
    raw: &Value,
    req: &RecoveryRankingRequest,
) -> Result<CancellationRecoveryV1, GatewayError> {
    let mut raw = raw.clone();
    if let Some(obj) = raw.as_object_mut() {
        obj.insert(
            "schema_version".into(),
            Value::from(CANCELLATION_RECOVERY_SCHEMA),
        );
    }
    let parsed: CancellationRecoveryV1 = serde_json::from_value(raw).map_err(|e| {
        GatewayError::InvalidOutput(format!("recovery ranking does not match schema: {e}"))
    })?;
    parsed
        .validate(&req.entry_ids(), &req.fact_refs())
        .map_err(GatewayError::InvalidOutput)?;
    wellos_domain::recovery::enforce_floors(
        &req.entries,
        &parsed.ordered_entry_ids,
        req.max_wait_demotion_hours,
    )
    .map_err(GatewayError::InvalidOutput)?;
    Ok(parsed)
}

pub fn deterministic_recovery(
    req: &RecoveryRankingRequest,
    provider: ProviderInfo,
) -> Result<AccessResponse<CancellationRecoveryV1>, GatewayError> {
    req.check()?;
    let es = req.language.starts_with("es");
    let mut entries = req.entries.clone();
    entries.sort_by_key(|e| e.rank);
    let explanations = entries
        .iter()
        .map(|e| RecoveryExplanation {
            entry_id: e.entry_id,
            explanation: if es {
                format!(
                    "Urgencia {} y {} horas de espera.",
                    e.urgency.as_str(),
                    e.waited_hours
                )
            } else {
                format!(
                    "Urgency {} and {} hours waited.",
                    e.urgency.as_str(),
                    e.waited_hours
                )
            },
            cited_sources: vec![format!("entry:{}", e.entry_id)],
        })
        .collect();
    let output = CancellationRecoveryV1 {
        schema_version: CANCELLATION_RECOVERY_SCHEMA.into(),
        ordered_entry_ids: entries.iter().map(|e| e.entry_id).collect(),
        explanations,
        cited_sources: req.fact_refs(),
        confidence: Confidence::High,
        limitations: vec![if es {
            "Orden determinista por urgencia y tiempo de espera; la oferta sigue requiriendo la aceptación del paciente.".to_string()
        } else {
            "Deterministic order by urgency and waiting time; the offer still requires the patient's acceptance.".to_string()
        }],
    };
    let raw =
        serde_json::to_value(&output).map_err(|e| GatewayError::InvalidOutput(e.to_string()))?;
    let output = parse_recovery(&raw, req)?;
    Ok(AccessResponse {
        output,
        provider,
        prompt_version: CANCELLATION_RECOVERY_DETERMINISTIC_PROMPT_VERSION.into(),
        input_hash: hash_json(req),
        usage: None,
    })
}

// ---------------------------------------------------------------------------
// capacity-explanation.v1
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CapacityExplanationRequest {
    pub template: String,
    pub language: String,
    pub scope_label: String,
    pub forecast: Forecast,
}

impl CapacityExplanationRequest {
    pub fn dates(&self) -> Vec<NaiveDate> {
        match &self.forecast {
            Forecast::Ready { days, .. } => days.iter().map(|d| d.date).collect(),
            Forecast::InsufficientHistory { .. } => Vec::new(),
        }
    }

    /// Everything an explanation may cite: the forecast itself, each day
    /// and each contributing factor.
    pub fn fact_refs(&self) -> Vec<String> {
        let mut out = vec!["forecast:summary".to_string()];
        if let Forecast::Ready { days, .. } = &self.forecast {
            for d in days {
                out.push(format!("forecast:day:{}", d.date));
                for f in &d.factors {
                    let r = format!("forecast:factor:{}", f.code);
                    if !out.contains(&r) {
                        out.push(r);
                    }
                }
            }
        }
        out
    }

    pub fn check(&self) -> Result<(), GatewayError> {
        if self.template != CAPACITY_EXPLANATION_TEMPLATE {
            return Err(GatewayError::PolicyDenied(format!(
                "unsupported template {}",
                self.template
            )));
        }
        Ok(())
    }
}

pub fn parse_capacity(
    raw: &Value,
    req: &CapacityExplanationRequest,
) -> Result<CapacityExplanationV1, GatewayError> {
    let mut raw = raw.clone();
    if let Some(obj) = raw.as_object_mut() {
        obj.insert(
            "schema_version".into(),
            Value::from(CAPACITY_EXPLANATION_SCHEMA),
        );
        if let Some(list) = obj.get_mut("recommendations").and_then(Value::as_array_mut) {
            for r in list.iter_mut() {
                if let Some(r) = r.as_object_mut() {
                    r.insert("requires_confirmation".into(), Value::from(true));
                }
            }
        }
    }
    let parsed: CapacityExplanationV1 = serde_json::from_value(raw).map_err(|e| {
        GatewayError::InvalidOutput(format!("capacity explanation does not match schema: {e}"))
    })?;
    parsed
        .validate(&req.dates(), &req.fact_refs())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(parsed)
}

fn weekday_name(d: NaiveDate, es: bool) -> &'static str {
    match (d.weekday().number_from_monday(), es) {
        (1, false) => "Monday",
        (1, true) => "lunes",
        (2, false) => "Tuesday",
        (2, true) => "martes",
        (3, false) => "Wednesday",
        (3, true) => "miércoles",
        (4, false) => "Thursday",
        (4, true) => "jueves",
        (5, false) => "Friday",
        (5, true) => "viernes",
        (6, false) => "Saturday",
        (6, true) => "sábado",
        (_, false) => "Sunday",
        (_, true) => "domingo",
    }
}

pub fn deterministic_capacity(
    req: &CapacityExplanationRequest,
    provider: ProviderInfo,
) -> Result<AccessResponse<CapacityExplanationV1>, GatewayError> {
    req.check()?;
    let es = req.language.starts_with("es");
    let refs = req.fact_refs();
    let output = match &req.forecast {
        Forecast::InsufficientHistory {
            history_weeks,
            required_weeks,
            ..
        } => CapacityExplanationV1 {
            schema_version: CAPACITY_EXPLANATION_SCHEMA.into(),
            summary: if es {
                format!(
                    "No hay historial suficiente para {}: {} semanas registradas, se requieren {}.",
                    req.scope_label, history_weeks, required_weeks
                )
            } else {
                format!(
                    "Not enough history for {}: {} weeks recorded, {} required.",
                    req.scope_label, history_weeks, required_weeks
                )
            },
            pressure_points: vec![],
            recommendations: vec![CapacityRecommendation {
                category: "collect_more_history".into(),
                text: if es {
                    "Seguir registrando demanda antes de planificar cambios de capacidad.".into()
                } else {
                    "Keep recording demand before planning capacity changes.".into()
                },
                requires_confirmation: true,
            }],
            cited_sources: vec!["forecast:summary".into()],
            confidence: Confidence::Low,
            limitations: vec![if es {
                "Sin previsión: la evidencia es insuficiente.".into()
            } else {
                "No forecast: the evidence is insufficient.".into()
            }],
        },
        Forecast::Ready {
            days,
            confidence,
            pressure_days,
            ..
        } => {
            let total_demand: f64 = days.iter().map(|d| d.expected_demand).sum();
            let total_capacity: f64 = days.iter().map(|d| d.available_capacity).sum();
            let gap = total_demand - total_capacity;
            let summary = if es {
                format!(
                    "Para {} se esperan {:.0} solicitudes frente a {:.0} huecos disponibles ({}{:.0}); {} día(s) con presión.",
                    req.scope_label,
                    total_demand,
                    total_capacity,
                    if gap >= 0.0 { "déficit de " } else { "excedente de " },
                    gap.abs(),
                    pressure_days.len()
                )
            } else {
                format!(
                    "For {} the forecast expects {:.0} requests against {:.0} available slots ({} of {:.0}); {} pressure day(s).",
                    req.scope_label,
                    total_demand,
                    total_capacity,
                    if gap >= 0.0 { "shortfall" } else { "surplus" },
                    gap.abs(),
                    pressure_days.len()
                )
            };
            let pressure_points = days
                .iter()
                .filter(|d| pressure_days.contains(&d.date))
                .take(10)
                .map(|d| {
                    let mut cited = vec![format!("forecast:day:{}", d.date)];
                    let mut top: Vec<_> = d.factors.iter().collect();
                    top.sort_by(|a, b| {
                        b.effect
                            .abs()
                            .partial_cmp(&a.effect.abs())
                            .unwrap_or(std::cmp::Ordering::Equal)
                    });
                    let details: Vec<String> = top
                        .iter()
                        .take(2)
                        .map(|f| {
                            cited.push(format!("forecast:factor:{}", f.code));
                            f.detail.clone()
                        })
                        .collect();
                    PressurePoint {
                        date: d.date,
                        explanation: if es {
                            format!(
                                "{} {}: demanda esperada {:.0} frente a {:.0} huecos ({}).",
                                weekday_name(d.date, true),
                                d.date,
                                d.expected_demand,
                                d.available_capacity,
                                details.join("; ")
                            )
                        } else {
                            format!(
                                "{} {}: expected demand {:.0} against {:.0} slots ({}).",
                                weekday_name(d.date, false),
                                d.date,
                                d.expected_demand,
                                d.available_capacity,
                                details.join("; ")
                            )
                        },
                        cited_sources: cited,
                    }
                })
                .collect();
            let mut recommendations = Vec::new();
            if !pressure_days.is_empty() {
                recommendations.push(CapacityRecommendation {
                    category: "open_capacity".into(),
                    text: if es {
                        "Valorar abrir capacidad adicional en los días con presión.".into()
                    } else {
                        "Consider opening additional capacity on the pressure days.".into()
                    },
                    requires_confirmation: true,
                });
                recommendations.push(CapacityRecommendation {
                    category: "adjust_reminders".into(),
                    text: if es {
                        "Reforzar recordatorios y solicitudes de confirmación en los días con presión.".into()
                    } else {
                        "Strengthen reminders and confirmation requests on the pressure days.".into()
                    },
                    requires_confirmation: true,
                });
            } else {
                recommendations.push(CapacityRecommendation {
                    category: "monitor".into(),
                    text: if es {
                        "Sin presión prevista; seguir monitorizando.".into()
                    } else {
                        "No pressure forecast; keep monitoring.".into()
                    },
                    requires_confirmation: true,
                });
            }
            CapacityExplanationV1 {
                schema_version: CAPACITY_EXPLANATION_SCHEMA.into(),
                summary,
                pressure_points,
                recommendations,
                cited_sources: refs.clone(),
                confidence: if *confidence >= 0.75 {
                    Confidence::High
                } else if *confidence >= 0.5 {
                    Confidence::Medium
                } else {
                    Confidence::Low
                },
                limitations: vec![if es {
                    "Explicación determinista de una previsión estadística; no modifica agendas ni prioridades.".into()
                } else {
                    "Deterministic explanation of a statistical forecast; it changes no schedule or priority.".into()
                }],
            }
        }
    };
    output
        .validate(&req.dates(), &refs)
        .map_err(GatewayError::InvalidOutput)?;
    Ok(AccessResponse {
        output,
        provider,
        prompt_version: CAPACITY_EXPLANATION_DETERMINISTIC_PROMPT_VERSION.into(),
        input_hash: hash_json(req),
        usage: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;
    use wellos_domain::capacity::{DayForecast, Factor};

    fn provider() -> ProviderInfo {
        ProviderInfo {
            provider: "test".into(),
            model: "m".into(),
            model_version: "1".into(),
        }
    }

    fn term(code: &str, en: &str, es: &str, syn: &[&str]) -> VocabularyTerm {
        VocabularyTerm {
            code: code.into(),
            name_en: en.into(),
            name_es: es.into(),
            synonyms: syn.iter().map(|s| s.to_string()).collect(),
        }
    }

    fn intent_req(text: &str) -> AccessIntentRequest {
        AccessIntentRequest {
            template: ACCESS_INTENT_TEMPLATE.into(),
            language: "en".into(),
            free_text: text.into(),
            urgency: Urgency::Routine,
            already_known: vec![],
            services: vec![
                term(
                    "derm_consult",
                    "Dermatology consultation",
                    "Consulta de dermatología",
                    &["skin", "piel", "mole"],
                ),
                term(
                    "physio_session",
                    "Physiotherapy session",
                    "Sesión de fisioterapia",
                    &["physio", "fisio"],
                ),
            ],
            specialties: vec![],
            modalities: vec![
                term("in_person", "In person", "Presencial", &[]),
                term("video", "Video consultation", "Videoconsulta", &["video"]),
            ],
            accessibility: vec![term(
                "wheelchair",
                "Wheelchair access",
                "Acceso en silla de ruedas",
                &["wheelchair", "silla de ruedas"],
            )],
            facilities: vec![term("north", "North clinic", "Clínica Norte", &[])],
        }
    }

    #[test]
    fn intent_extracts_catalog_codes_and_windows_without_urgency() {
        let r = deterministic_intent(
            &intent_req("I need someone to look at a mole on my skin, Tuesday mornings by video, wheelchair access please"),
            provider(),
        )
        .unwrap();
        let o = r.output;
        assert_eq!(o.service_code.as_deref(), Some("derm_consult"));
        assert_eq!(o.modality_codes, vec!["video".to_string()]);
        assert_eq!(o.accessibility_codes, vec!["wheelchair".to_string()]);
        assert_eq!(o.preferred_windows.len(), 1);
        assert_eq!(o.preferred_windows[0].weekday, 2);
        assert!(!o.clinical_triage_suggested);
        assert!(o
            .cited_sources
            .contains(&"catalog:service:derm_consult".to_string()));
    }

    #[test]
    fn intent_flags_red_flags_via_deterministic_floor_and_asks_for_missing() {
        let r = deterministic_intent(
            &intent_req("Tengo dolor en el pecho desde ayer"),
            provider(),
        )
        .unwrap();
        assert!(r.output.clinical_triage_suggested);
        assert_eq!(
            r.output.triage_reasons,
            vec!["possible_acute_symptom".to_string()]
        );
        assert!(r.output.service_code.is_none());
        assert!(!r.output.missing_information.is_empty());
        assert_eq!(r.output.confidence, Confidence::Low);
    }

    #[test]
    fn parsed_intent_cannot_clear_floor_or_invent_codes() {
        let req = intent_req("chest pain and a mole");
        let raw = serde_json::json!({
            "service_code": "derm_consult",
            "specialty_code": null,
            "modality_codes": [],
            "facility_codes": [],
            "accessibility_codes": [],
            "preferred_windows": [],
            "earliest_date": null,
            "latest_date": null,
            "language": null,
            "continuity_requested": false,
            "transport_requested": false,
            "missing_information": [],
            "clinical_triage_suggested": false,
            "triage_reasons": [],
            "cited_sources": ["request:text"],
            "confidence": "high",
            "limitations": []
        });
        let out = parse_intent(&raw, &req).unwrap();
        assert!(out.clinical_triage_suggested);
        let mut invented = raw.clone();
        invented["service_code"] = Value::from("oncology");
        assert!(matches!(
            parse_intent(&invented, &req),
            Err(GatewayError::InvalidOutput(_))
        ));
    }

    fn cand(id: &str, score: f64, hour: u32) -> RankingCandidate {
        RankingCandidate {
            candidate_id: id.into(),
            starts_at: Utc.with_ymd_and_hms(2026, 6, 1, hour, 0, 0).unwrap(),
            ends_at: Utc.with_ymd_and_hms(2026, 6, 1, hour, 20, 0).unwrap(),
            facility_label: "North".into(),
            modality_code: "in_person".into(),
            resource_labels: vec!["Dr A".into()],
            score,
            factors: vec![
                ("soonness".into(), 20.0, "2 days".into()),
                ("travel".into(), 5.0, "12 min".into()),
            ],
            travel_minutes: Some(12.0),
            reasons: vec![],
        }
    }

    fn ranking_req() -> RankingRequest {
        RankingRequest {
            template: APPOINTMENT_RANKING_TEMPLATE.into(),
            language: "en".into(),
            facts: vec![("request:preferences".into(), "prefers mornings".into())],
            candidates: vec![cand("c1", 80.0, 9), cand("c2", 70.0, 10)],
        }
    }

    #[test]
    fn deterministic_ranking_preserves_order_and_cites_candidates() {
        let r = deterministic_ranking(&ranking_req(), provider()).unwrap();
        assert_eq!(r.output.order(), vec!["c1".to_string(), "c2".to_string()]);
        assert!(r.output.ranked[0].explanation.contains("early date"));
        assert_eq!(r.output.ranked[0].cited_sources, vec!["c1".to_string()]);
    }

    #[test]
    fn parsed_ranking_rejects_invented_or_dropped_candidates() {
        let req = ranking_req();
        let good = serde_json::json!({
            "ranked": [
                {"candidate_id": "c2", "rank": 1, "explanation": "later but preferred", "cited_sources": ["c2", "request:preferences"]},
                {"candidate_id": "c1", "rank": 2, "explanation": "earlier", "cited_sources": ["c1"]}
            ],
            "overall_note": null,
            "cited_sources": [],
            "confidence": "medium",
            "limitations": []
        });
        assert_eq!(
            parse_ranking(&good, &req).unwrap().order(),
            vec!["c2".to_string(), "c1".to_string()]
        );
        let mut invented = good.clone();
        invented["ranked"][0]["candidate_id"] = Value::from("c9");
        assert!(parse_ranking(&invented, &req).is_err());
        let mut dropped = good.clone();
        dropped["ranked"].as_array_mut().unwrap().pop();
        assert!(parse_ranking(&dropped, &req).is_err());
        let mut uncited = good;
        uncited["ranked"][0]["cited_sources"] = serde_json::json!(["fact:unknown"]);
        assert!(parse_ranking(&uncited, &req).is_err());
        let mut too_many = ranking_req();
        too_many.candidates = (0..MAX_RANKING_CANDIDATES + 1)
            .map(|i| cand(&format!("c{i}"), 1.0, 9))
            .collect();
        assert!(matches!(
            deterministic_ranking(&too_many, provider()),
            Err(GatewayError::PolicyDenied(_))
        ));
    }

    fn entry(n: u128, urgency: Urgency, waited: i64, rank: usize) -> EligibleEntry {
        EligibleEntry {
            entry_id: Uuid::from_u128(n),
            patient_id: Uuid::from_u128(100 + n),
            urgency,
            waited_hours: waited,
            rank,
            reasons: vec![],
        }
    }

    #[test]
    fn recovery_parse_enforces_urgency_floor() {
        let req = RecoveryRankingRequest {
            template: CANCELLATION_RECOVERY_TEMPLATE.into(),
            language: "es".into(),
            facts: vec![],
            entries: vec![
                entry(1, Urgency::Urgent, 10, 1),
                entry(2, Urgency::Routine, 500, 2),
                entry(3, Urgency::Routine, 490, 3),
            ],
            max_wait_demotion_hours: 72,
        };
        let det = deterministic_recovery(&req, provider()).unwrap();
        assert_eq!(det.output.ordered_entry_ids[0], Uuid::from_u128(1));
        assert!(det.output.explanations[0].explanation.contains("Urgencia"));
        let swap_routine = serde_json::json!({
            "ordered_entry_ids": [Uuid::from_u128(1), Uuid::from_u128(3), Uuid::from_u128(2)],
            "explanations": [],
            "cited_sources": [],
            "confidence": "medium",
            "limitations": []
        });
        assert!(parse_recovery(&swap_routine, &req).is_ok());
        let demote_urgent = serde_json::json!({
            "ordered_entry_ids": [Uuid::from_u128(2), Uuid::from_u128(1), Uuid::from_u128(3)],
            "explanations": [],
            "cited_sources": [],
            "confidence": "medium",
            "limitations": []
        });
        assert!(matches!(
            parse_recovery(&demote_urgent, &req),
            Err(GatewayError::InvalidOutput(_))
        ));
    }

    #[test]
    fn capacity_explanation_handles_ready_and_insufficient() {
        let d = NaiveDate::from_ymd_opt(2026, 8, 3).unwrap();
        let ready = CapacityExplanationRequest {
            template: CAPACITY_EXPLANATION_TEMPLATE.into(),
            language: "en".into(),
            scope_label: "Dermatology / North".into(),
            forecast: Forecast::Ready {
                version: "capacity-forecast.v1".into(),
                days: vec![DayForecast {
                    date: d,
                    expected_demand: 30.0,
                    available_capacity: 20.0,
                    gap: 10.0,
                    demand_low: 25.0,
                    demand_high: 35.0,
                    factors: vec![Factor {
                        code: "seasonal".into(),
                        effect: 1.4,
                        detail: "August peak".into(),
                    }],
                }],
                confidence: 0.8,
                history_weeks: 12,
                recommendations: vec![],
                pressure_days: vec![d],
            },
        };
        let r = deterministic_capacity(&ready, provider()).unwrap();
        assert_eq!(r.output.pressure_points.len(), 1);
        assert!(r
            .output
            .recommendations
            .iter()
            .all(|x| x.requires_confirmation));
        assert!(r
            .output
            .recommendations
            .iter()
            .any(|x| x.category == "open_capacity"));
        let insufficient = CapacityExplanationRequest {
            forecast: Forecast::InsufficientHistory {
                version: "capacity-forecast.v1".into(),
                history_weeks: 1,
                required_weeks: 4,
                detail: "x".into(),
            },
            ..ready.clone()
        };
        let r = deterministic_capacity(&insufficient, provider()).unwrap();
        assert!(r.output.pressure_points.is_empty());
        assert_eq!(r.output.recommendations[0].category, "collect_more_history");
        let mut raw = serde_json::to_value(&r.output).unwrap();
        raw["pressure_points"] = serde_json::json!([{ "date": "2026-08-03", "explanation": "x", "cited_sources": ["forecast:summary"] }]);
        assert!(parse_capacity(&raw, &insufficient).is_err());
    }
}
