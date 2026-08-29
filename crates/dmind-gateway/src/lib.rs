//! dMind Model Gateway: provider-neutral inference boundary.
//!
//! All inference in WellOS passes through [`ModelGateway`]. Domain code never
//! calls a provider SDK directly. The gateway enforces structured outputs,
//! records operational metadata without PHI, and degrades gracefully: a
//! provider failure yields [`GatewayError::Unavailable`], never a blocked
//! clinical workflow.

pub mod fake;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use wellos_domain::ai::ResultSummaryV1;

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
    /// Hash of the rendered input, for provenance without storing PHI.
    pub input_hash: String,
}

#[derive(Debug, thiserror::Error)]
pub enum GatewayError {
    #[error("model provider unavailable: {0}")]
    Unavailable(String),
    #[error("provider output failed schema validation: {0}")]
    InvalidOutput(String),
    #[error("policy denied this route: {0}")]
    PolicyDenied(String),
}

#[async_trait]
pub trait ModelGateway: Send + Sync {
    async fn summarize_result(&self, req: &SummaryRequest)
        -> Result<GatewayResponse, GatewayError>;
}

pub fn input_hash(req: &SummaryRequest) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(serde_json::to_vec(req).expect("serializable"));
    hex::encode(hasher.finalize())
}
