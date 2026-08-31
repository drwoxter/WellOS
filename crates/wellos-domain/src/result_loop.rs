//! Closed-loop diagnostic result state machine.
//!
//! A result loop is only closed when receipt, review, patient notification,
//! and follow-up disposition are all documented. Transitions are explicit and
//! invalid transitions are rejected.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopState {
    Ordered,
    Received,
    Reviewed,
    Notified,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LoopTransition {
    ResultReceived,
    ResultReviewed,
    PatientNotified,
    LoopClosed,
    /// An amendment reopens review when the loop had progressed past receipt.
    ResultAmended,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
#[error("invalid transition {transition:?} from state {from:?}")]
pub struct InvalidTransition {
    pub from: LoopState,
    pub transition: LoopTransition,
}

impl LoopState {
    pub fn apply(self, t: LoopTransition) -> Result<LoopState, InvalidTransition> {
        use LoopState::*;
        use LoopTransition::*;
        match (self, t) {
            (Ordered, ResultReceived) => Ok(Received),
            (Received, ResultReviewed) => Ok(Reviewed),
            (Reviewed, PatientNotified) => Ok(Notified),
            (Notified, LoopClosed) => Ok(Closed),
            // Amendment after review/notification reopens the review step.
            (Received, ResultAmended) => Ok(Received),
            (Reviewed, ResultAmended) | (Notified, ResultAmended) => Ok(Received),
            (from, transition) => Err(InvalidTransition { from, transition }),
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            LoopState::Ordered => "ordered",
            LoopState::Received => "received",
            LoopState::Reviewed => "reviewed",
            LoopState::Notified => "notified",
            LoopState::Closed => "closed",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "ordered" => LoopState::Ordered,
            "received" => LoopState::Received,
            "reviewed" => LoopState::Reviewed,
            "notified" => LoopState::Notified,
            "closed" => LoopState::Closed,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use LoopState::*;
    use LoopTransition::*;

    #[test]
    fn happy_path_reaches_closed() {
        let s = Ordered
            .apply(ResultReceived)
            .and_then(|s| s.apply(ResultReviewed))
            .and_then(|s| s.apply(PatientNotified))
            .and_then(|s| s.apply(LoopClosed))
            .unwrap();
        assert_eq!(s, Closed);
    }

    #[test]
    fn cannot_close_before_notification() {
        assert!(Reviewed.apply(LoopClosed).is_err());
        assert!(Received.apply(LoopClosed).is_err());
        assert!(Ordered.apply(LoopClosed).is_err());
    }

    #[test]
    fn cannot_review_before_receipt() {
        assert!(Ordered.apply(ResultReviewed).is_err());
    }

    #[test]
    fn amendment_reopens_review() {
        assert_eq!(Reviewed.apply(ResultAmended).unwrap(), Received);
        assert_eq!(Notified.apply(ResultAmended).unwrap(), Received);
    }

    #[test]
    fn amendment_after_close_is_rejected() {
        // A closed loop must be reopened by an explicit clinical process,
        // not silently by an inbound message.
        assert!(Closed.apply(ResultAmended).is_err());
    }
}
