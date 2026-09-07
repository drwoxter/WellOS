//! AIArtifact lifecycle and agent autonomy levels.
//!
//! AI output is never chart truth by default: it is an artifact with an
//! explicit review lifecycle, provenance, and citations.

use serde::{Deserialize, Serialize};

/// Agent autonomy levels used across the system.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum AutonomyLevel {
    /// Deterministic automation; no generative AI.
    A0,
    /// Summarize/transcribe/translate/draft; no action.
    A1,
    /// Recommend alternatives with sources, uncertainty, limitations.
    A2,
    /// Prepare a consequential action requiring explicit human approval.
    A3,
    /// Bounded automatic execution for preapproved low-risk cases.
    A4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArtifactStatus {
    Draft,
    AwaitingReview,
    Approved,
    Rejected,
    Superseded,
    Withdrawn,
    Invalidated,
    /// The provider was unavailable; care continues without the artifact.
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewDecision {
    Approved,
    Rejected,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("artifact in status {from:?} cannot be reviewed")]
pub struct InvalidReview {
    pub from: ArtifactStatus,
}

impl ArtifactStatus {
    /// Only artifacts awaiting review can be approved or rejected.
    pub fn review(self, decision: ReviewDecision) -> Result<ArtifactStatus, InvalidReview> {
        match self {
            ArtifactStatus::AwaitingReview => Ok(match decision {
                ReviewDecision::Approved => ArtifactStatus::Approved,
                ReviewDecision::Rejected => ArtifactStatus::Rejected,
            }),
            from => Err(InvalidReview { from }),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            ArtifactStatus::Draft => "draft",
            ArtifactStatus::AwaitingReview => "awaiting_review",
            ArtifactStatus::Approved => "approved",
            ArtifactStatus::Rejected => "rejected",
            ArtifactStatus::Superseded => "superseded",
            ArtifactStatus::Withdrawn => "withdrawn",
            ArtifactStatus::Invalidated => "invalidated",
            ArtifactStatus::Unavailable => "unavailable",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "draft" => ArtifactStatus::Draft,
            "awaiting_review" => ArtifactStatus::AwaitingReview,
            "approved" => ArtifactStatus::Approved,
            "rejected" => ArtifactStatus::Rejected,
            "superseded" => ArtifactStatus::Superseded,
            "withdrawn" => ArtifactStatus::Withdrawn,
            "invalidated" => ArtifactStatus::Invalidated,
            "unavailable" => ArtifactStatus::Unavailable,
            _ => return None,
        })
    }
}

/// Structured output produced by an A1/A2 result-summary agent.
///
/// The schema is versioned; provider output that does not deserialize into
/// this schema is rejected.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResultSummaryV1 {
    pub schema_version: String,
    pub summary: String,
    pub relevant_trend: Option<String>,
    /// References to source facts (observation ids, etc.) the summary cites.
    pub cited_sources: Vec<String>,
    pub limitations: Vec<String>,
    /// Suggested next-step categories only — never orders or prescriptions.
    pub suggested_next_step_categories: Vec<String>,
}

/// The eight structured consultation-note sections, in documentation order.
/// Scribe output may only target these.
pub const NOTE_SECTIONS: &[&str] = &[
    "reason_for_encounter",
    "history_present_illness",
    "medical_history",
    "review_of_systems",
    "physical_exam",
    "assessment",
    "plan",
    "follow_up",
];

/// Coarse confidence attached to transcript segments and extracted sections.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Confidence {
    Low,
    Medium,
    High,
}

/// One transcript segment with timecodes relative to the recording start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranscriptSegment {
    pub index: u32,
    pub start_ms: u64,
    pub end_ms: u64,
    /// Optional diarization label, e.g. "clinician" or "patient".
    pub speaker: Option<String>,
    pub text: String,
    pub confidence: Confidence,
}

/// A proposed note section extracted from the transcript. The text is a
/// draft for clinician review; `segments` link it back to its timecodes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScribeSection {
    pub section: String,
    pub text: String,
    pub confidence: Confidence,
    pub review_needed: bool,
    /// Human-readable reasons review is needed (uncertain source,
    /// contradiction, medication mention, clinical impression restated).
    pub reasons: Vec<String>,
    pub segments: Vec<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScribeFlagKind {
    Contradiction,
    Uncertain,
}

/// Content the clinician must look at: contradictory statements within the
/// conversation or passages the transcription could not resolve.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScribeFlag {
    pub kind: ScribeFlagKind,
    pub message: String,
    pub segments: Vec<u32>,
    pub sections: Vec<String>,
}

/// Provider identification recorded for provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderInfo {
    pub provider: String,
    pub model: String,
    pub model_version: String,
}

/// Structured output of the A1 consultation scribe: transcript plus proposed
/// note sections, bound to the encounter and the exact note version it was
/// produced against. It is a draft; nothing in it enters the record without
/// explicit clinician action.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScribeDraftV1 {
    pub schema_version: String,
    pub encounter_id: uuid::Uuid,
    pub source_note_version: Option<i64>,
    pub language: String,
    pub transcript: Vec<TranscriptSegment>,
    pub sections: Vec<ScribeSection>,
    pub flags: Vec<ScribeFlag>,
    pub transcription: ProviderInfo,
    pub extraction: ProviderInfo,
    pub generated_at: chrono::DateTime<chrono::Utc>,
    pub limitations: Vec<String>,
}

pub const SCRIBE_DRAFT_SCHEMA: &str = "scribe-draft.v1";

impl ScribeDraftV1 {
    /// Structural validation before persistence: known, unique, non-empty
    /// sections; every segment reference resolves; timecodes are ordered.
    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != SCRIBE_DRAFT_SCHEMA {
            return Err(format!("unexpected schema {}", self.schema_version));
        }
        let mut last_end = 0u64;
        for (i, seg) in self.transcript.iter().enumerate() {
            if seg.index as usize != i {
                return Err(format!("segment index {} out of order", seg.index));
            }
            if seg.end_ms < seg.start_ms || seg.start_ms < last_end {
                return Err(format!("segment {} has non-monotonic timecodes", seg.index));
            }
            if seg.text.trim().is_empty() {
                return Err(format!("segment {} is empty", seg.index));
            }
            last_end = seg.end_ms;
        }
        let n = self.transcript.len() as u32;
        let mut seen: Vec<&str> = Vec::new();
        for s in &self.sections {
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
            if s.segments.iter().any(|r| *r >= n) {
                return Err(format!(
                    "section {} references a missing segment",
                    s.section
                ));
            }
        }
        for f in &self.flags {
            if f.segments.iter().any(|r| *r >= n) {
                return Err("flag references a missing segment".into());
            }
            if f.sections.iter().any(|s| !seen.contains(&s.as_str())) {
                return Err("flag references a section not in the draft".into());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn awaiting_review_can_be_approved_or_rejected() {
        assert_eq!(
            ArtifactStatus::AwaitingReview
                .review(ReviewDecision::Approved)
                .unwrap(),
            ArtifactStatus::Approved
        );
        assert_eq!(
            ArtifactStatus::AwaitingReview
                .review(ReviewDecision::Rejected)
                .unwrap(),
            ArtifactStatus::Rejected
        );
    }

    #[test]
    fn approved_artifact_cannot_be_re_reviewed() {
        assert!(ArtifactStatus::Approved
            .review(ReviewDecision::Rejected)
            .is_err());
        assert!(ArtifactStatus::Unavailable
            .review(ReviewDecision::Approved)
            .is_err());
    }
}
