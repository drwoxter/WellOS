//! A1 structured consultation-note draft: the typed dMind operation that
//! turns a genuine transcript plus authorized context into proposed note
//! sections, each citing the transcript segments it restates.
//!
//! The same validation applies to every provider: unknown or duplicate
//! sections, empty text, dangling segment references and uncited sections
//! are rejected as invalid output. Sections that document clinical judgement
//! (examination, assessment, plan, follow-up) are always marked for review
//! when a generative provider produced them.

use crate::scribe::{extract_sections, extraction_info};
use crate::{hash_json, GatewayError, Usage};
use serde::{Deserialize, Serialize};
use wellos_domain::ai::{
    ProviderInfo, ScribeFlag, ScribeSection, TranscriptSegment, NOTE_SECTIONS,
};

pub const NOTE_DRAFT_TEMPLATE: &str = "consultation-note-draft@1.0.0";
/// Prompt version of the deterministic rule-based baseline.
pub const NOTE_DRAFT_DETERMINISTIC_PROMPT_VERSION: &str = "note-draft-deterministic.v1";

/// Longest proposed section text accepted from a provider (characters).
pub const MAX_SECTION_CHARS: usize = 8_000;

/// Sections restating clinical judgement. A generative provider's proposal
/// for them is always `review_needed`.
pub const CLINICIAN_JUDGEMENT_SECTIONS: &[&str] =
    &["physical_exam", "assessment", "plan", "follow_up"];

/// Input to the note-draft operation. `context_facts` carries only the
/// (reference, statement) pairs the caller was authorized to share; the
/// provider may cite them but the draft is built from the transcript.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteDraftRequest {
    pub template: String,
    /// BCP-47 language tag of the consultation.
    pub language: String,
    pub transcript: Vec<TranscriptSegment>,
    pub context_facts: Vec<(String, String)>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NoteDraftResponse {
    pub sections: Vec<ScribeSection>,
    pub flags: Vec<ScribeFlag>,
    /// Provider-specific limitations, appended to the standard ones.
    pub limitations: Vec<String>,
    pub provider: ProviderInfo,
    pub prompt_version: String,
    pub input_hash: String,
    pub usage: Option<Usage>,
}

pub fn note_input_hash(req: &NoteDraftRequest) -> String {
    hash_json(req)
}

/// Provider-independent structural validation of a proposed draft.
pub fn validate_note_draft(
    sections: &[ScribeSection],
    flags: &[ScribeFlag],
    segment_count: usize,
) -> Result<(), String> {
    let n = segment_count as u32;
    let mut seen: Vec<&str> = Vec::new();
    for s in sections {
        if !NOTE_SECTIONS.contains(&s.section.as_str()) {
            return Err(format!("unknown note section {}", s.section));
        }
        if seen.contains(&s.section.as_str()) {
            return Err(format!("duplicate note section {}", s.section));
        }
        seen.push(&s.section);
        if s.text.trim().is_empty() {
            return Err(format!("section {} is empty", s.section));
        }
        if s.text.chars().count() > MAX_SECTION_CHARS {
            return Err(format!("section {} exceeds the length limit", s.section));
        }
        if s.segments.is_empty() {
            return Err(format!("section {} cites no transcript segment", s.section));
        }
        if s.segments.iter().any(|r| *r >= n) {
            return Err(format!(
                "section {} references a missing segment",
                s.section
            ));
        }
    }
    for f in flags {
        if f.message.trim().is_empty() {
            return Err("flag has no message".into());
        }
        if f.segments.iter().any(|r| *r >= n) {
            return Err("flag references a missing segment".into());
        }
        if f.sections.iter().any(|s| !seen.contains(&s.as_str())) {
            return Err("flag references a section not in the draft".into());
        }
    }
    Ok(())
}

/// Deterministic rule-based baseline used by the development fixtures and
/// by tests; never selected for a real deployment.
pub fn deterministic_draft(req: &NoteDraftRequest) -> Result<NoteDraftResponse, GatewayError> {
    if req.template != NOTE_DRAFT_TEMPLATE {
        return Err(GatewayError::PolicyDenied(format!(
            "unsupported template {}",
            req.template
        )));
    }
    let (sections, flags) = extract_sections(&req.transcript, &req.language);
    validate_note_draft(&sections, &flags, req.transcript.len())
        .map_err(GatewayError::InvalidOutput)?;
    Ok(NoteDraftResponse {
        sections,
        flags,
        limitations: Vec::new(),
        provider: extraction_info(),
        prompt_version: NOTE_DRAFT_DETERMINISTIC_PROMPT_VERSION.into(),
        input_hash: note_input_hash(req),
        usage: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use wellos_domain::ai::Confidence;

    fn seg(i: u32, text: &str) -> TranscriptSegment {
        TranscriptSegment {
            index: i,
            start_ms: u64::from(i) * 1000,
            end_ms: u64::from(i) * 1000 + 900,
            speaker: None,
            text: text.into(),
            confidence: Confidence::High,
        }
    }

    fn section(name: &str, segments: Vec<u32>) -> ScribeSection {
        ScribeSection {
            section: name.into(),
            text: "text".into(),
            confidence: Confidence::Medium,
            review_needed: true,
            reasons: vec![],
            segments,
        }
    }

    #[test]
    fn rejects_uncited_unknown_duplicate_and_dangling_sections() {
        assert!(validate_note_draft(&[section("plan", vec![])], &[], 2).is_err());
        assert!(validate_note_draft(&[section("diagnosis", vec![0])], &[], 2).is_err());
        assert!(validate_note_draft(
            &[section("plan", vec![0]), section("plan", vec![1])],
            &[],
            2
        )
        .is_err());
        assert!(validate_note_draft(&[section("plan", vec![2])], &[], 2).is_err());
        assert!(validate_note_draft(&[section("plan", vec![1])], &[], 2).is_ok());
    }

    #[test]
    fn deterministic_baseline_is_stable() {
        let req = NoteDraftRequest {
            template: NOTE_DRAFT_TEMPLATE.into(),
            language: "en".into(),
            transcript: vec![
                seg(0, "clinician: What brings you in today?"),
                seg(1, "patient: I have had a cough for three days."),
            ],
            context_facts: vec![],
        };
        let a = deterministic_draft(&req).unwrap();
        let b = deterministic_draft(&req).unwrap();
        assert_eq!(a.sections, b.sections);
        assert_eq!(a.input_hash, b.input_hash);
        assert_eq!(a.prompt_version, NOTE_DRAFT_DETERMINISTIC_PROMPT_VERSION);
    }

    #[test]
    fn deterministic_baseline_refuses_unknown_template() {
        let req = NoteDraftRequest {
            template: "other@1".into(),
            language: "en".into(),
            transcript: vec![seg(0, "hello")],
            context_facts: vec![],
        };
        assert!(matches!(
            deterministic_draft(&req),
            Err(GatewayError::PolicyDenied(_))
        ));
    }
}
