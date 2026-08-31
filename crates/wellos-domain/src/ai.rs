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
