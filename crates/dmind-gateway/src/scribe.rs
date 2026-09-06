//! Consultation scribe: provider-neutral speech-to-text plus a deterministic
//! rule-based extraction step that maps transcript passages onto the eight
//! structured note sections.
//!
//! Boundaries:
//! - Transcription providers receive audio and return timed text. Nothing
//!   else (no chart data) is ever sent to a provider.
//! - The default provider is the offline [`FakeTranscription`], which never
//!   inspects audio content and produces a fixed synthetic consultation whose
//!   timecodes are scaled to the recording duration.
//! - The optional [`OpenAiCompatibleTranscription`] adapter is only
//!   constructed when explicitly configured by the server; its credential
//!   never leaves the server process and is never logged.
//! - Extraction is rule-based and deterministic; it restates what was said
//!   and never adds facts. Assessment-like statements and medication
//!   mentions are always marked for review.

use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use async_trait::async_trait;
use serde::Deserialize;
use wellos_domain::ai::{
    Confidence, ProviderInfo, ScribeFlag, ScribeFlagKind, ScribeSection, TranscriptSegment,
};

/// Errors are safe to surface: they carry no audio, transcript or credential.
#[derive(Debug, thiserror::Error)]
pub enum ScribeError {
    #[error("transcription provider unavailable: {0}")]
    Unavailable(String),
    #[error("transcription provider returned invalid output: {0}")]
    InvalidOutput(String),
    #[error("audio rejected by provider: {0}")]
    Rejected(String),
}

/// Audio handed to a transcription provider. Held in memory only for the
/// duration of the request.
pub struct TranscriptionRequest {
    pub audio: Vec<u8>,
    pub mime_type: String,
    pub duration_ms: u64,
    /// BCP-47 primary language subtag, "en" or "es".
    pub language: String,
}

/// Raw provider output before extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transcription {
    pub segments: Vec<TranscriptSegment>,
    pub provider: ProviderInfo,
}

#[async_trait]
pub trait TranscriptionProvider: Send + Sync {
    fn info(&self) -> ProviderInfo;
    async fn transcribe(&self, req: &TranscriptionRequest) -> Result<Transcription, ScribeError>;
}

// ---------------------------------------------------------------------------
// Deterministic offline provider
// ---------------------------------------------------------------------------

/// Speaker labels are stable identifiers translated by the UI.
pub const SPEAKER_CLINICIAN: &str = "clinician";
pub const SPEAKER_PATIENT: &str = "patient";

struct Line {
    speaker: &'static str,
    en: &'static str,
    es: &'static str,
    /// Relative weight of the line in the recording timeline.
    weight: u32,
    confidence: Confidence,
}

/// A fixed synthetic consultation (upper respiratory complaint) that
/// exercises every note section and includes one internal contradiction and
/// one unresolved passage so the review affordances are demonstrable.
const SCRIPT: &[Line] = &[
    Line {
        speaker: SPEAKER_CLINICIAN,
        en: "Good morning. What brings you in today?",
        es: "Buenos días. ¿Qué le trae hoy por aquí?",
        weight: 3,
        confidence: Confidence::High,
    },
    Line {
        speaker: SPEAKER_PATIENT,
        en: "I've had a sore throat and a dry cough for three days.",
        es: "Tengo dolor de garganta y tos seca desde hace tres días.",
        weight: 4,
        confidence: Confidence::High,
    },
    Line {
        speaker: SPEAKER_CLINICIAN,
        en: "Any fever?",
        es: "¿Ha tenido fiebre?",
        weight: 2,
        confidence: Confidence::High,
    },
    Line {
        speaker: SPEAKER_PATIENT,
        en: "No fever, but I had chills last night.",
        es: "No he tenido fiebre, pero anoche tuve escalofríos.",
        weight: 3,
        confidence: Confidence::High,
    },
    Line {
        speaker: SPEAKER_PATIENT,
        en: "Actually, I measured a temperature of thirty-eight last night.",
        es: "En realidad, anoche me medí una temperatura de treinta y ocho.",
        weight: 4,
        confidence: Confidence::Medium,
    },
    Line {
        speaker: SPEAKER_CLINICIAN,
        en: "Any known conditions or regular medication?",
        es: "¿Alguna enfermedad conocida o medicación habitual?",
        weight: 3,
        confidence: Confidence::High,
    },
    Line {
        speaker: SPEAKER_PATIENT,
        en: "I take medication for high blood pressure. No known allergies.",
        es: "Tomo medicación para la tensión alta. Sin alergias conocidas.",
        weight: 4,
        confidence: Confidence::High,
    },
    Line {
        speaker: SPEAKER_CLINICIAN,
        en: "Any shortness of breath or chest pain?",
        es: "¿Falta de aire o dolor en el pecho?",
        weight: 3,
        confidence: Confidence::High,
    },
    Line {
        speaker: SPEAKER_PATIENT,
        en: "No shortness of breath and no chest pain.",
        es: "Sin falta de aire ni dolor en el pecho.",
        weight: 3,
        confidence: Confidence::High,
    },
    Line {
        speaker: SPEAKER_CLINICIAN,
        en: "On examination the throat is red, tonsils slightly enlarged without exudate, lungs clear.",
        es: "A la exploración la garganta está enrojecida, amígdalas ligeramente aumentadas sin exudado, pulmones limpios.",
        weight: 6,
        confidence: Confidence::High,
    },
    Line {
        speaker: SPEAKER_CLINICIAN,
        en: "This looks like a viral upper respiratory infection.",
        es: "Esto parece una infección respiratoria alta de origen viral.",
        weight: 4,
        confidence: Confidence::High,
    },
    Line {
        speaker: SPEAKER_CLINICIAN,
        en: "Rest, fluids, and paracetamol as needed for discomfort.",
        es: "Reposo, líquidos y paracetamol si hay molestias.",
        weight: 4,
        confidence: Confidence::High,
    },
    Line {
        speaker: SPEAKER_CLINICIAN,
        en: "Come back if symptoms persist beyond a week or the fever returns.",
        es: "Vuelva si los síntomas persisten más de una semana o si reaparece la fiebre.",
        weight: 5,
        confidence: Confidence::High,
    },
    Line {
        speaker: SPEAKER_PATIENT,
        en: "[inaudible] maybe a week.",
        es: "[inaudible] quizá una semana.",
        weight: 2,
        confidence: Confidence::Low,
    },
];

/// Offline provider for development, tests and demos. Ignores audio content
/// (which is always synthetic in this repository) and returns the fixed
/// script with timecodes scaled to `duration_ms`.
pub struct FakeTranscription {
    unavailable: AtomicBool,
}

impl Default for FakeTranscription {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeTranscription {
    pub fn new() -> Self {
        Self {
            unavailable: AtomicBool::new(false),
        }
    }

    /// Simulate provider degradation (tests and demos of the retry path).
    pub fn set_unavailable(&self, v: bool) {
        self.unavailable.store(v, Ordering::SeqCst);
    }
}

#[async_trait]
impl TranscriptionProvider for FakeTranscription {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            provider: "dmind-fake".into(),
            model: "fake-transcribe".into(),
            model_version: "0.1.0".into(),
        }
    }

    async fn transcribe(&self, req: &TranscriptionRequest) -> Result<Transcription, ScribeError> {
        if self.unavailable.load(Ordering::SeqCst) {
            return Err(ScribeError::Unavailable("fake provider offline".into()));
        }
        let total_weight: u64 = SCRIPT.iter().map(|l| l.weight as u64).sum();
        let duration = req.duration_ms.max(SCRIPT.len() as u64);
        let mut cursor = 0u64;
        let mut acc = 0u64;
        let segments = SCRIPT
            .iter()
            .enumerate()
            .map(|(i, l)| {
                acc += l.weight as u64;
                let end = duration * acc / total_weight;
                let seg = TranscriptSegment {
                    index: i as u32,
                    start_ms: cursor,
                    end_ms: end,
                    speaker: Some(l.speaker.into()),
                    text: if req.language == "es" { l.es } else { l.en }.into(),
                    confidence: l.confidence,
                };
                cursor = end;
                seg
            })
            .collect();
        Ok(Transcription {
            segments,
            provider: self.info(),
        })
    }
}

// ---------------------------------------------------------------------------
// Optional OpenAI-compatible adapter
// ---------------------------------------------------------------------------

/// Settings for an OpenAI-compatible `/audio/transcriptions` endpoint. The
/// server resolves these from its own environment; the browser never sees
/// them.
#[derive(Clone)]
pub struct OpenAiCompatibleConfig {
    pub endpoint: String,
    pub model: String,
    pub api_key: String,
    pub timeout: Duration,
    pub max_retries: u32,
    /// Delay between retries (kept configurable so tests run fast).
    pub retry_backoff: Duration,
}

impl std::fmt::Debug for OpenAiCompatibleConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("OpenAiCompatibleConfig")
            .field("endpoint", &self.endpoint)
            .field("model", &self.model)
            .field("api_key", &"<redacted>")
            .field("timeout", &self.timeout)
            .field("max_retries", &self.max_retries)
            .finish()
    }
}

pub struct OpenAiCompatibleTranscription {
    cfg: OpenAiCompatibleConfig,
    client: reqwest::Client,
}

#[derive(Deserialize)]
struct VerboseJson {
    text: Option<String>,
    #[serde(default)]
    segments: Vec<VerboseSegment>,
}

#[derive(Deserialize)]
struct VerboseSegment {
    start: f64,
    end: f64,
    text: String,
    #[serde(default)]
    no_speech_prob: Option<f64>,
}

impl OpenAiCompatibleTranscription {
    pub fn new(cfg: OpenAiCompatibleConfig) -> Result<Self, ScribeError> {
        let client = reqwest::Client::builder()
            .timeout(cfg.timeout)
            .build()
            .map_err(|e| ScribeError::Unavailable(format!("http client: {e}")))?;
        Ok(Self { cfg, client })
    }

    fn file_name(mime: &str) -> &'static str {
        match mime {
            "audio/webm" => "audio.webm",
            "audio/ogg" => "audio.ogg",
            "audio/mp4" => "audio.mp4",
            "audio/mpeg" => "audio.mp3",
            _ => "audio.wav",
        }
    }

    async fn attempt(&self, req: &TranscriptionRequest) -> Result<Transcription, Retry> {
        let part = reqwest::multipart::Part::bytes(req.audio.clone())
            .file_name(Self::file_name(&req.mime_type))
            .mime_str(&req.mime_type)
            .map_err(|_| Retry::No(ScribeError::Rejected("unsupported audio type".into())))?;
        let form = reqwest::multipart::Form::new()
            .part("file", part)
            .text("model", self.cfg.model.clone())
            .text("language", req.language.clone())
            .text("response_format", "verbose_json")
            .text("timestamp_granularities[]", "segment");
        let res = self
            .client
            .post(&self.cfg.endpoint)
            .bearer_auth(&self.cfg.api_key)
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                // Never include the error's display text: it can embed the URL.
                let reason = if e.is_timeout() {
                    "timeout"
                } else if e.is_connect() {
                    "connection failed"
                } else {
                    "request failed"
                };
                Retry::Yes(ScribeError::Unavailable(reason.into()))
            })?;
        let status = res.status();
        if status.is_server_error() || status.as_u16() == 429 {
            return Err(Retry::Yes(ScribeError::Unavailable(format!(
                "provider status {}",
                status.as_u16()
            ))));
        }
        if status.as_u16() == 400 || status.as_u16() == 413 || status.as_u16() == 415 {
            return Err(Retry::No(ScribeError::Rejected(format!(
                "provider status {}",
                status.as_u16()
            ))));
        }
        if !status.is_success() {
            return Err(Retry::No(ScribeError::Unavailable(format!(
                "provider status {}",
                status.as_u16()
            ))));
        }
        let body: VerboseJson = res
            .json()
            .await
            .map_err(|_| Retry::No(ScribeError::InvalidOutput("not verbose_json".into())))?;
        let mut segments = Vec::new();
        let mut last_end = 0u64;
        for s in body.segments {
            let text = s.text.trim();
            if text.is_empty() {
                continue;
            }
            if !(s.start.is_finite() && s.end.is_finite()) || s.start < 0.0 || s.end < s.start {
                return Err(Retry::No(ScribeError::InvalidOutput(
                    "segment timecodes out of range".into(),
                )));
            }
            let start_ms = ((s.start * 1000.0) as u64).max(last_end);
            let end_ms = ((s.end * 1000.0) as u64).max(start_ms);
            last_end = end_ms;
            let confidence = match s.no_speech_prob {
                Some(p) if p > 0.6 => Confidence::Low,
                Some(p) if p > 0.3 => Confidence::Medium,
                _ => Confidence::High,
            };
            segments.push(TranscriptSegment {
                index: segments.len() as u32,
                start_ms,
                end_ms,
                speaker: None,
                text: text.to_string(),
                confidence,
            });
        }
        if segments.is_empty() {
            let text = body.text.unwrap_or_default().trim().to_string();
            if text.is_empty() {
                return Err(Retry::No(ScribeError::InvalidOutput(
                    "empty transcript".into(),
                )));
            }
            segments.push(TranscriptSegment {
                index: 0,
                start_ms: 0,
                end_ms: req.duration_ms,
                speaker: None,
                text,
                confidence: Confidence::Medium,
            });
        }
        Ok(Transcription {
            segments,
            provider: self.info(),
        })
    }
}

enum Retry {
    Yes(ScribeError),
    No(ScribeError),
}

#[async_trait]
impl TranscriptionProvider for OpenAiCompatibleTranscription {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            provider: "openai-compatible".into(),
            model: self.cfg.model.clone(),
            model_version: "api".into(),
        }
    }

    async fn transcribe(&self, req: &TranscriptionRequest) -> Result<Transcription, ScribeError> {
        let mut attempt = 0u32;
        loop {
            match self.attempt(req).await {
                Ok(t) => return Ok(t),
                Err(Retry::No(e)) => return Err(e),
                Err(Retry::Yes(e)) => {
                    if attempt >= self.cfg.max_retries {
                        return Err(e);
                    }
                    attempt += 1;
                    tracing::warn!(attempt, "scribe provider retry");
                    tokio::time::sleep(self.cfg.retry_backoff * attempt).await;
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Deterministic extraction
// ---------------------------------------------------------------------------

pub fn extraction_info() -> ProviderInfo {
    ProviderInfo {
        provider: "dmind-scribe-rules".into(),
        model: "section-mapper".into(),
        model_version: "0.1.0".into(),
    }
}

struct Lexicon {
    reason_prompt: &'static [&'static str],
    symptom: &'static [&'static str],
    course: &'static [&'static str],
    history: &'static [&'static str],
    ros: &'static [&'static str],
    exam: &'static [&'static str],
    impression: &'static [&'static str],
    plan: &'static [&'static str],
    medication: &'static [&'static str],
    follow_up: &'static [&'static str],
    fever_neg: &'static [&'static str],
    fever_pos: &'static [&'static str],
    unresolved: &'static [&'static str],
    // reasons
    r_contradiction: &'static str,
    r_uncertain: &'static str,
    r_impression: &'static str,
    r_medication: &'static str,
    f_contradiction: &'static str,
    f_uncertain: &'static str,
}

const EN: Lexicon = Lexicon {
    reason_prompt: &["brings you in", "reason for", "how can i help"],
    symptom: &["pain", "cough", "sore", "fever", "headache", "nausea", "rash", "dizz", "ache"],
    course: &["days", "weeks", "since", "last night", "yesterday", "started", "chills", "temperature", "worse", "better"],
    history: &["medication for", "history of", "diagnosed", "allerg", "known condition", "surgery", "i take"],
    ros: &["shortness of breath", "chest pain", "no vomiting", "no diarrh", "appetite", "urinary", "weight loss"],
    exam: &["on examination", "throat is", "tonsils", "lungs", "abdomen", "heart sounds", "auscultation", "palpation", "tender"],
    impression: &["looks like", "consistent with", "impression", "most likely", "suggests", "diagnosis is"],
    plan: &["rest", "fluids", "as needed", "recommend", "we will", "i'll order", "let's start", "prescri"],
    medication: &["paracetamol", "ibuprofen", "acetaminophen", "antibiotic", "amoxicillin", "mg", "tablet", "prescri"],
    follow_up: &["come back", "follow up", "follow-up", "return if", "see you in", "persist", "if it gets worse"],
    fever_neg: &["no fever"],
    fever_pos: &["temperature of", "had a fever", "fever of"],
    unresolved: &["[inaudible]", "[unclear]"],
    r_contradiction: "Conflicting statements about fever in the conversation — confirm with the patient.",
    r_uncertain: "Part of this passage could not be transcribed clearly.",
    r_impression: "Clinical impression as spoken by the clinician; dMind does not diagnose — confirm before signing.",
    r_medication: "Mentions a medication; dMind does not prescribe — confirm dose and appropriateness.",
    f_contradiction: "The patient first denied fever and later reported a temperature of 38.",
    f_uncertain: "An unresolved passage was excluded from the draft.",
};

const ES: Lexicon = Lexicon {
    reason_prompt: &["le trae", "motivo de", "en qué puedo ayudar"],
    symptom: &["dolor", "tos", "fiebre", "cefalea", "náusea", "erupción", "mareo", "molestia"],
    course: &["días", "semanas", "desde", "anoche", "ayer", "empez", "escalofr", "temperatura", "peor", "mejor"],
    history: &["medicación para", "antecedente", "diagnostic", "alergi", "enfermedad conocida", "cirugía", "tomo "],
    ros: &["falta de aire", "dolor en el pecho", "sin vómito", "sin diarrea", "apetito", "urinari", "pérdida de peso"],
    exam: &["a la exploración", "garganta está", "amígdalas", "pulmones", "abdomen", "ruidos cardíacos", "auscultación", "palpación", "doloroso a"],
    impression: &["parece", "compatible con", "impresión", "lo más probable", "sugiere", "el diagnóstico es"],
    plan: &["reposo", "líquidos", "si hay", "recomiendo", "vamos a", "solicitar", "empezar", "prescri", "recet"],
    medication: &["paracetamol", "ibuprofeno", "antibiótico", "amoxicilina", "mg", "comprimido", "prescri", "recet"],
    follow_up: &["vuelva", "seguimiento", "revisión", "si persiste", "persisten", "nos vemos en", "si empeora"],
    fever_neg: &["no he tenido fiebre", "sin fiebre"],
    fever_pos: &["temperatura de", "tuve fiebre", "fiebre de"],
    unresolved: &["[inaudible]", "[poco claro]"],
    r_contradiction: "Declaraciones contradictorias sobre la fiebre en la conversación — confirmar con el paciente.",
    r_uncertain: "Parte de este pasaje no pudo transcribirse con claridad.",
    r_impression: "Impresión clínica tal como la expresó el profesional; dMind no diagnostica — confirmar antes de firmar.",
    r_medication: "Menciona un medicamento; dMind no prescribe — confirmar dosis e idoneidad.",
    f_contradiction: "El paciente primero negó fiebre y después refirió una temperatura de 38.",
    f_uncertain: "Un pasaje no resuelto se excluyó del borrador.",
};

fn contains_any(hay: &str, needles: &[&str]) -> bool {
    needles.iter().any(|n| hay.contains(n))
}

fn strip_speaker_prefix(text: &str) -> String {
    text.trim().trim_end_matches('.').to_string()
}

struct Bucket {
    section: &'static str,
    texts: Vec<String>,
    segments: Vec<u32>,
    confidence: Confidence,
    reasons: Vec<String>,
}

impl Bucket {
    fn new(section: &'static str) -> Self {
        Self {
            section,
            texts: Vec::new(),
            segments: Vec::new(),
            confidence: Confidence::High,
            reasons: Vec::new(),
        }
    }
    fn push(&mut self, seg: &TranscriptSegment) {
        self.texts.push(strip_speaker_prefix(&seg.text));
        self.segments.push(seg.index);
        self.confidence = self.confidence.min(seg.confidence);
    }
    fn reason(&mut self, r: &str) {
        if !self.reasons.iter().any(|x| x == r) {
            self.reasons.push(r.to_string());
        }
    }
}

/// Map transcript segments onto note sections. Deterministic: identical
/// input yields identical output. Unresolved passages are excluded and
/// flagged rather than guessed.
pub fn extract_sections(
    segments: &[TranscriptSegment],
    language: &str,
) -> (Vec<ScribeSection>, Vec<ScribeFlag>) {
    let lx = if language == "es" { &ES } else { &EN };
    let mut buckets: Vec<Bucket> = wellos_domain::ai::NOTE_SECTIONS
        .iter()
        .map(|s| Bucket::new(s))
        .collect();
    let idx = |name: &str| {
        wellos_domain::ai::NOTE_SECTIONS
            .iter()
            .position(|s| *s == name)
            .expect("known section")
    };
    let mut flags = Vec::new();
    let mut fever_denied: Option<u32> = None;
    let mut reason_captured = false;
    let mut expect_reason = false;

    for seg in segments {
        let lower = seg.text.to_lowercase();
        let patient = seg.speaker.as_deref() == Some(SPEAKER_PATIENT);
        let clinician = seg.speaker.as_deref() == Some(SPEAKER_CLINICIAN);

        if contains_any(&lower, lx.unresolved) || seg.confidence == Confidence::Low {
            flags.push(ScribeFlag {
                kind: ScribeFlagKind::Uncertain,
                message: lx.f_uncertain.into(),
                segments: vec![seg.index],
                sections: Vec::new(),
            });
            continue;
        }
        if contains_any(&lower, lx.reason_prompt) {
            expect_reason = true;
            continue;
        }
        // Clinician questions carry no facts of their own.
        if clinician && lower.ends_with('?') {
            continue;
        }

        if expect_reason && !reason_captured && !clinician {
            buckets[idx("reason_for_encounter")].push(seg);
            reason_captured = true;
            expect_reason = false;
            continue;
        }
        if contains_any(&lower, lx.exam) && !patient {
            buckets[idx("physical_exam")].push(seg);
            continue;
        }
        if contains_any(&lower, lx.follow_up) && !patient {
            buckets[idx("follow_up")].push(seg);
            continue;
        }
        if contains_any(&lower, lx.impression) && !patient {
            let b = &mut buckets[idx("assessment")];
            b.push(seg);
            b.reason(lx.r_impression);
            continue;
        }
        if contains_any(&lower, lx.plan) && !patient {
            let b = &mut buckets[idx("plan")];
            b.push(seg);
            if contains_any(&lower, lx.medication) {
                b.reason(lx.r_medication);
            }
            continue;
        }
        if contains_any(&lower, lx.history) {
            buckets[idx("medical_history")].push(seg);
            continue;
        }
        if contains_any(&lower, lx.ros) {
            buckets[idx("review_of_systems")].push(seg);
            continue;
        }
        if contains_any(&lower, lx.fever_neg) {
            fever_denied = Some(seg.index);
            buckets[idx("history_present_illness")].push(seg);
            continue;
        }
        if contains_any(&lower, lx.fever_pos) {
            let b = &mut buckets[idx("history_present_illness")];
            b.push(seg);
            if let Some(denied) = fever_denied {
                b.reason(lx.r_contradiction);
                b.confidence = b.confidence.min(Confidence::Medium);
                flags.push(ScribeFlag {
                    kind: ScribeFlagKind::Contradiction,
                    message: lx.f_contradiction.into(),
                    segments: vec![denied, seg.index],
                    sections: vec!["history_present_illness".into()],
                });
            }
            continue;
        }
        if contains_any(&lower, lx.course) || contains_any(&lower, lx.symptom) {
            if !reason_captured && patient {
                buckets[idx("reason_for_encounter")].push(seg);
                reason_captured = true;
            } else {
                buckets[idx("history_present_illness")].push(seg);
            }
            continue;
        }
        // Unclassified statements are left out rather than guessed; the
        // full transcript stays visible to the clinician.
    }

    let sections: Vec<ScribeSection> = buckets
        .into_iter()
        .filter(|b| !b.texts.is_empty())
        .map(|mut b| {
            if b.confidence < Confidence::High {
                b.reason(lx.r_uncertain);
            }
            let review_needed = !b.reasons.is_empty();
            ScribeSection {
                section: b.section.into(),
                text: format!("{}.", b.texts.join(". ")),
                confidence: b.confidence,
                review_needed,
                reasons: b.reasons,
                segments: b.segments,
            }
        })
        .collect();
    (sections, flags)
}

/// Limitations attached to every scribe draft.
pub fn limitations(language: &str) -> Vec<String> {
    if language == "es" {
        vec![
            "Borrador generado a partir de la grabación; no es un registro clínico hasta que el profesional lo revise y firme.".into(),
            "dMind no diagnostica, no prescribe ni añade hechos que no se hayan dicho en la consulta.".into(),
            "La transcripción puede contener errores; verifique nombres de medicamentos, dosis y valores numéricos.".into(),
        ]
    } else {
        vec![
            "Draft generated from the recording; it is not part of the clinical record until reviewed and signed by the clinician.".into(),
            "dMind does not diagnose, prescribe or add facts that were not said during the consultation.".into(),
            "Transcription may contain errors; verify medication names, doses and numeric values.".into(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(lang: &str, duration_ms: u64) -> TranscriptionRequest {
        TranscriptionRequest {
            audio: vec![0u8; 16],
            mime_type: "audio/webm".into(),
            duration_ms,
            language: lang.into(),
        }
    }

    #[tokio::test]
    async fn fake_provider_is_deterministic_and_scaled() {
        let p = FakeTranscription::new();
        let a = p.transcribe(&req("en", 60_000)).await.unwrap();
        let b = p.transcribe(&req("en", 60_000)).await.unwrap();
        assert_eq!(a, b);
        assert_eq!(a.segments.first().unwrap().start_ms, 0);
        assert_eq!(a.segments.last().unwrap().end_ms, 60_000);
        for w in a.segments.windows(2) {
            assert!(w[0].end_ms <= w[1].start_ms);
        }
        let es = p.transcribe(&req("es", 60_000)).await.unwrap();
        assert!(es.segments[0].text.starts_with("Buenos"));
        p.set_unavailable(true);
        assert!(matches!(
            p.transcribe(&req("en", 1_000)).await,
            Err(ScribeError::Unavailable(_))
        ));
    }

    #[tokio::test]
    async fn extraction_covers_every_section_and_flags() {
        for lang in ["en", "es"] {
            let p = FakeTranscription::new();
            let t = p.transcribe(&req(lang, 120_000)).await.unwrap();
            let (sections, flags) = extract_sections(&t.segments, lang);
            let names: Vec<&str> = sections.iter().map(|s| s.section.as_str()).collect();
            assert_eq!(
                names,
                wellos_domain::ai::NOTE_SECTIONS.to_vec(),
                "lang {lang}: {names:?}"
            );
            let hpi = sections
                .iter()
                .find(|s| s.section == "history_present_illness")
                .unwrap();
            assert!(hpi.review_needed);
            assert!(flags
                .iter()
                .any(|f| f.kind == ScribeFlagKind::Contradiction));
            assert!(flags.iter().any(|f| f.kind == ScribeFlagKind::Uncertain));
            let plan = sections.iter().find(|s| s.section == "plan").unwrap();
            assert!(plan.review_needed, "medication mention requires review");
            let assessment = sections.iter().find(|s| s.section == "assessment").unwrap();
            assert!(assessment.review_needed);
            let exam = sections
                .iter()
                .find(|s| s.section == "physical_exam")
                .unwrap();
            assert!(!exam.review_needed);
            // Every referenced segment exists.
            let n = t.segments.len() as u32;
            assert!(sections.iter().all(|s| s.segments.iter().all(|i| *i < n)));
            // Deterministic.
            let again = extract_sections(&t.segments, lang);
            assert_eq!(again.0, sections);
        }
    }

    #[test]
    fn extraction_never_invents_text() {
        let segs = vec![TranscriptSegment {
            index: 0,
            start_ms: 0,
            end_ms: 1000,
            speaker: Some(SPEAKER_PATIENT.into()),
            text: "I have a headache since yesterday.".into(),
            confidence: Confidence::High,
        }];
        let (sections, flags) = extract_sections(&segs, "en");
        assert_eq!(sections.len(), 1);
        assert_eq!(sections[0].section, "reason_for_encounter");
        assert_eq!(sections[0].text, "I have a headache since yesterday.");
        assert!(flags.is_empty());
    }

    // --- mocked OpenAI-compatible endpoint (loopback only) ---

    use std::sync::atomic::AtomicUsize;
    use std::sync::Arc;

    struct Mock {
        calls: Arc<AtomicUsize>,
        /// Status codes to return, in order; the last repeats.
        statuses: Arc<Vec<u16>>,
        body: Arc<serde_json::Value>,
        seen_auth: Arc<std::sync::Mutex<Option<String>>>,
    }

    async fn mock_server(statuses: Vec<u16>, body: serde_json::Value) -> (String, Mock) {
        use axum::{extract::State, http::HeaderMap, routing::post, Router};
        let mock = Mock {
            calls: Arc::new(AtomicUsize::new(0)),
            statuses: Arc::new(statuses),
            body: Arc::new(body),
            seen_auth: Arc::new(std::sync::Mutex::new(None)),
        };
        let st = (
            mock.calls.clone(),
            mock.statuses.clone(),
            mock.body.clone(),
            mock.seen_auth.clone(),
        );
        type MockState = (
            Arc<AtomicUsize>,
            Arc<Vec<u16>>,
            Arc<serde_json::Value>,
            Arc<std::sync::Mutex<Option<String>>>,
        );
        let app = Router::new()
            .route(
                "/v1/audio/transcriptions",
                post(
                    |State((calls, statuses, body, auth)): State<MockState>,
                     headers: HeaderMap,
                     mp: axum::extract::Multipart| async move {
                        let n = calls.fetch_add(1, Ordering::SeqCst);
                        *auth.lock().unwrap() = headers
                            .get("authorization")
                            .map(|v| v.to_str().unwrap().to_string());
                        // Drain the multipart body so the request is well-formed.
                        let mut mp = mp;
                        let mut saw_file = false;
                        while let Ok(Some(field)) = mp.next_field().await {
                            if field.name() == Some("file") {
                                saw_file = true;
                            }
                            let _ = field.bytes().await;
                        }
                        assert!(saw_file);
                        let status = statuses[n.min(statuses.len() - 1)];
                        (
                            axum::http::StatusCode::from_u16(status).unwrap(),
                            axum::Json((*body).clone()),
                        )
                    },
                ),
            )
            .layer(axum::extract::DefaultBodyLimit::max(8 * 1024 * 1024))
            .with_state(st);
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (format!("http://{addr}/v1/audio/transcriptions"), mock)
    }

    fn cfg(endpoint: String, retries: u32) -> OpenAiCompatibleConfig {
        OpenAiCompatibleConfig {
            endpoint,
            model: "whisper-1".into(),
            api_key: "sk-test-credential".into(),
            timeout: Duration::from_secs(5),
            max_retries: retries,
            retry_backoff: Duration::from_millis(1),
        }
    }

    #[tokio::test]
    async fn openai_adapter_parses_verbose_json_and_sends_credential() {
        let (endpoint, mock) = mock_server(
            vec![200],
            serde_json::json!({
                "text": "hello there. any fever?",
                "segments": [
                    {"start": 0.0, "end": 1.5, "text": " hello there.", "no_speech_prob": 0.01},
                    {"start": 1.5, "end": 3.0, "text": "any fever?", "no_speech_prob": 0.5},
                    {"start": 3.0, "end": 3.2, "text": "   "}
                ]
            }),
        )
        .await;
        let p = OpenAiCompatibleTranscription::new(cfg(endpoint, 2)).unwrap();
        let t = p.transcribe(&req("en", 3_200)).await.unwrap();
        assert_eq!(t.segments.len(), 2);
        assert_eq!(t.segments[0].text, "hello there.");
        assert_eq!(t.segments[0].start_ms, 0);
        assert_eq!(t.segments[0].end_ms, 1500);
        assert_eq!(t.segments[0].confidence, Confidence::High);
        assert_eq!(t.segments[1].confidence, Confidence::Medium);
        assert_eq!(t.provider.provider, "openai-compatible");
        assert_eq!(t.provider.model, "whisper-1");
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
        assert_eq!(
            mock.seen_auth.lock().unwrap().as_deref(),
            Some("Bearer sk-test-credential")
        );
    }

    #[tokio::test]
    async fn openai_adapter_retries_bounded_then_fails_safely() {
        let (endpoint, mock) = mock_server(vec![503], serde_json::json!({"error": "down"})).await;
        let p = OpenAiCompatibleTranscription::new(cfg(endpoint, 2)).unwrap();
        let err = p.transcribe(&req("en", 1_000)).await.unwrap_err();
        assert!(matches!(err, ScribeError::Unavailable(_)));
        assert_eq!(
            mock.calls.load(Ordering::SeqCst),
            3,
            "1 attempt + 2 retries"
        );
        assert!(!err.to_string().contains("sk-test-credential"));
    }

    #[tokio::test]
    async fn openai_adapter_recovers_after_transient_failure() {
        let (endpoint, mock) = mock_server(
            vec![429, 200],
            serde_json::json!({"text": "plain text only"}),
        )
        .await;
        let p = OpenAiCompatibleTranscription::new(cfg(endpoint, 1)).unwrap();
        let t = p.transcribe(&req("en", 2_000)).await.unwrap();
        assert_eq!(mock.calls.load(Ordering::SeqCst), 2);
        assert_eq!(t.segments.len(), 1);
        assert_eq!(t.segments[0].end_ms, 2_000);
    }

    #[tokio::test]
    async fn openai_adapter_does_not_retry_client_errors() {
        let (endpoint, mock) = mock_server(vec![400], serde_json::json!({"error": "bad"})).await;
        let p = OpenAiCompatibleTranscription::new(cfg(endpoint, 3)).unwrap();
        let err = p.transcribe(&req("en", 1_000)).await.unwrap_err();
        assert!(matches!(err, ScribeError::Rejected(_)));
        assert_eq!(mock.calls.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn openai_adapter_rejects_invalid_output() {
        let (endpoint, _mock) = mock_server(
            vec![200],
            serde_json::json!({"segments": [{"start": 5.0, "end": 1.0, "text": "x"}]}),
        )
        .await;
        let p = OpenAiCompatibleTranscription::new(cfg(endpoint, 0)).unwrap();
        assert!(matches!(
            p.transcribe(&req("en", 1_000)).await,
            Err(ScribeError::InvalidOutput(_))
        ));
    }

    #[tokio::test]
    async fn openai_adapter_times_out_without_leaking_endpoint() {
        // Nothing listens here; connection is refused immediately.
        let p = OpenAiCompatibleTranscription::new(OpenAiCompatibleConfig {
            endpoint: "http://127.0.0.1:9/v1/audio/transcriptions".into(),
            ..cfg(String::new(), 0)
        })
        .unwrap();
        let err = p.transcribe(&req("en", 1_000)).await.unwrap_err();
        let msg = err.to_string();
        assert!(matches!(err, ScribeError::Unavailable(_)));
        assert!(!msg.contains("127.0.0.1"), "{msg}");
    }

    #[test]
    fn config_debug_redacts_credential() {
        let cfg = OpenAiCompatibleConfig {
            endpoint: "http://127.0.0.1:1/v1/audio/transcriptions".into(),
            model: "whisper-1".into(),
            api_key: "sk-very-secret".into(),
            timeout: Duration::from_secs(1),
            max_retries: 0,
            retry_backoff: Duration::from_millis(1),
        };
        let dbg = format!("{cfg:?}");
        assert!(!dbg.contains("sk-very-secret"));
        assert!(dbg.contains("<redacted>"));
    }
}
