//! dMind Model Gateway: provider-neutral inference boundary.
//!
//! All inference in WellOS passes through [`ModelGateway`]. Domain code never
//! calls a provider SDK directly. The gateway enforces structured outputs,
//! records operational metadata without PHI, and degrades gracefully: a
//! provider failure yields [`GatewayError::Unavailable`], never a blocked
//! clinical workflow.
//!
//! Providers:
//! - [`openai::OpenAiCompatibleModel`] — the real external HTTP provider;
//! - [`DisabledGateway`] — no provider configured (or configuration
//!   invalid); every operation reports [`GatewayError::Disabled`];
//! - [`fake::FakeProvider`] — deterministic offline fixtures, compiled only
//!   with the `dev-fixtures` feature and never a fallback for a failed
//!   real call.

#[cfg(any(feature = "dev-fixtures", test))]
pub mod fake;
pub mod notes;
pub mod openai;
pub mod risk;
pub mod scribe;
pub mod trends;
pub mod triage;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use wellos_domain::ai::{ProviderInfo, ResultSummaryV1};

pub use notes::{NoteDraftRequest, NoteDraftResponse};
pub use risk::{RiskSummaryRequest, RiskSummaryResponse};
pub use triage::{TriageRequest, TriageResponse};

/// Token accounting reported by a provider, when it reports any. Stored on
/// the artifact for cost governance; never contains content.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Usage {
    pub prompt_tokens: Option<u64>,
    pub completion_tokens: Option<u64>,
    pub total_tokens: Option<u64>,
}

/// Honest availability of one AI capability, as reported by readiness and
/// consumed by the UI to enable or disable the matching action.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilityState {
    /// A provider is configured and its last calls succeeded.
    Ready,
    /// A provider is configured but recent calls failed.
    Degraded,
    /// No provider is configured (`disabled`).
    Disabled,
    /// A provider was requested but its configuration is invalid; nothing
    /// was selected in its place.
    InvalidConfiguration,
}

impl CapabilityState {
    /// Whether a request may be attempted at all. Degraded providers are
    /// still called (they may have recovered); disabled or misconfigured
    /// capabilities are never called.
    pub fn is_callable(self) -> bool {
        matches!(self, Self::Ready | Self::Degraded)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapabilityStatus {
    pub state: CapabilityState,
    pub provider: String,
    pub model: Option<String>,
    /// Operator-facing reason, free of secrets and endpoint details.
    pub reason: Option<String>,
    /// True when the provider runs outside the WellOS cell (external HTTP).
    pub external: bool,
    /// True only for deterministic development fixtures.
    pub synthetic: bool,
}

impl CapabilityStatus {
    pub fn disabled(reason: impl Into<String>) -> Self {
        Self {
            state: CapabilityState::Disabled,
            provider: "disabled".into(),
            model: None,
            reason: Some(reason.into()),
            external: false,
            synthetic: false,
        }
    }

    pub fn invalid(provider: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            state: CapabilityState::InvalidConfiguration,
            provider: provider.into(),
            model: None,
            reason: Some(reason.into()),
            external: false,
            synthetic: false,
        }
    }

    pub fn available(&self) -> bool {
        matches!(
            self.state,
            CapabilityState::Ready | CapabilityState::Degraded
        )
    }
}

/// Window after the last failure during which a provider reports `Degraded`.
pub const DEGRADED_WINDOW_SECS: i64 = 300;

/// Lock-free health record shared by the external adapters: consecutive
/// failures and the time of the last one. Never stores request content.
#[derive(Debug, Default)]
pub struct ProviderHealth {
    consecutive_failures: std::sync::atomic::AtomicU32,
    last_failure_unix: std::sync::atomic::AtomicI64,
}

impl ProviderHealth {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_success(&self) {
        self.consecutive_failures
            .store(0, std::sync::atomic::Ordering::SeqCst);
    }

    pub fn record_failure(&self) {
        self.consecutive_failures
            .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.last_failure_unix.store(
            chrono::Utc::now().timestamp(),
            std::sync::atomic::Ordering::SeqCst,
        );
    }

    /// `Degraded` with a reason while failures are recent, otherwise `Ready`.
    pub fn state(&self) -> (CapabilityState, Option<String>) {
        let failures = self
            .consecutive_failures
            .load(std::sync::atomic::Ordering::SeqCst);
        let last = self
            .last_failure_unix
            .load(std::sync::atomic::Ordering::SeqCst);
        let recent = chrono::Utc::now().timestamp() - last < DEGRADED_WINDOW_SECS;
        if failures > 0 && recent {
            (
                CapabilityState::Degraded,
                Some(format!("{failures} consecutive provider failure(s)")),
            )
        } else {
            (CapabilityState::Ready, None)
        }
    }
}

/// A request for an A1/A2 structured result summary.
///
/// Inputs are already policy-filtered facts — the gateway receives only what
/// the caller was authorized to share with the selected route.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SummaryRequest {
    /// Prompt/template identifier and version, e.g. "result-summary@1.0.0".
    pub template: String,
    /// Source facts as (reference, statement) pairs. References are cited in
    /// the output; statements are synthetic/authorized snippets only.
    pub facts: Vec<(String, String)>,
    /// BCP-47 language tag for the summary ("en", "es").
    pub language: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayResponse {
    pub output: ResultSummaryV1,
    pub model: String,
    pub model_version: String,
    pub route: String,
    /// Identifier of the prompt family that produced the output.
    pub prompt_version: String,
    /// Hash of the rendered input, for provenance without storing PHI.
    pub input_hash: String,
    pub usage: Option<Usage>,
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("model provider unavailable: {0}")]
    Unavailable(String),
    #[error("provider output failed schema validation: {0}")]
    InvalidOutput(String),
    #[error("policy denied this route: {0}")]
    PolicyDenied(String),
    /// No model provider is configured for this deployment. Callers report
    /// this honestly instead of substituting any content.
    #[error("model provider disabled: {0}")]
    Disabled(String),
}

/// The typed operations a gateway implements. Used to look up the prompt
/// version a provider would apply *before* executing, so an identical prior
/// execution can be reused instead of spending provider credit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    ResultSummary,
    TriageProposal,
    RiskSummary,
    NoteDraft,
}

#[async_trait]
pub trait ModelGateway: Send + Sync {
    /// Provenance identity of the provider (no secrets).
    fn info(&self) -> ProviderInfo;

    /// Current honest availability, without performing a network call.
    fn status(&self) -> CapabilityStatus;

    /// Prompt/template family this provider applies to `op`; recorded on
    /// every artifact and part of the deduplication key.
    fn prompt_version(&self, op: Operation) -> String;

    async fn summarize_result(&self, req: &SummaryRequest)
        -> Result<GatewayResponse, GatewayError>;

    /// A2 triage assistance: proposes operational priority, destination and
    /// a handoff summary from policy-filtered triage facts. The caller clamps
    /// the result to the deterministic safety floor and gates it behind an
    /// explicit human decision.
    async fn propose_triage(&self, req: &TriageRequest) -> Result<TriageResponse, GatewayError>;

    /// A2 risk explanation: turns a deterministic risk assessment into a
    /// structured `risk-summary.v1` proposal. The caller aligns the output
    /// to the deterministic floor and gates every suggestion behind an
    /// explicit human confirmation.
    async fn summarize_risk(
        &self,
        req: &RiskSummaryRequest,
    ) -> Result<RiskSummaryResponse, GatewayError>;

    /// A1 structured consultation-note draft from a genuine transcript plus
    /// authorized context. Every proposed section cites the transcript
    /// segments it restates; the caller binds the result to the encounter
    /// and note version and gates it behind per-section clinician review.
    async fn draft_note(&self, req: &NoteDraftRequest) -> Result<NoteDraftResponse, GatewayError>;
}

/// Gateway installed when `DMIND_MODEL_PROVIDER=disabled` or when the
/// requested provider's configuration is invalid. It never produces output.
pub struct DisabledGateway {
    status: CapabilityStatus,
}

impl DisabledGateway {
    pub fn disabled(reason: impl Into<String>) -> Self {
        Self {
            status: CapabilityStatus::disabled(reason),
        }
    }

    pub fn invalid(provider: impl Into<String>, reason: impl Into<String>) -> Self {
        Self {
            status: CapabilityStatus::invalid(provider, reason),
        }
    }

    fn err(&self) -> GatewayError {
        GatewayError::Disabled(
            self.status
                .reason
                .clone()
                .unwrap_or_else(|| "no model provider configured".into()),
        )
    }
}

#[async_trait]
impl ModelGateway for DisabledGateway {
    fn info(&self) -> ProviderInfo {
        ProviderInfo {
            provider: self.status.provider.clone(),
            model: "none".into(),
            model_version: "none".into(),
        }
    }

    fn status(&self) -> CapabilityStatus {
        self.status.clone()
    }

    fn prompt_version(&self, _op: Operation) -> String {
        "none".into()
    }

    async fn summarize_result(
        &self,
        _req: &SummaryRequest,
    ) -> Result<GatewayResponse, GatewayError> {
        Err(self.err())
    }

    async fn propose_triage(&self, _req: &TriageRequest) -> Result<TriageResponse, GatewayError> {
        Err(self.err())
    }

    async fn summarize_risk(
        &self,
        _req: &RiskSummaryRequest,
    ) -> Result<RiskSummaryResponse, GatewayError> {
        Err(self.err())
    }

    async fn draft_note(&self, _req: &NoteDraftRequest) -> Result<NoteDraftResponse, GatewayError> {
        Err(self.err())
    }
}

pub fn input_hash(req: &SummaryRequest) -> String {
    hash_json(req)
}

/// Stable SHA-256 of a value's canonical JSON, used as provenance input hash.
pub fn hash_json<T: Serialize>(value: &T) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(value).expect("serializable"));
    hex::encode(hasher.finalize())
}
