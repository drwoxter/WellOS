//! dMind Live Intelligence & Evaluation v1.
//!
//! Measures how a configured model provider behaves on a versioned,
//! bilingual, entirely fictional corpus covering every registered governed
//! dMind operation. Evaluation is assistive infrastructure: it writes only to
//! the `dmind_eval_*` tables, never creates clinical artifacts, and is gated
//! to synthetic tenants. It is not clinical validation.

pub mod corpus;
pub mod ops;
pub mod registry;
pub mod rubric;
pub mod store;

pub use corpus::{Corpus, EvalCase, Expectations, CORPUS_VERSION, SUITE_KEY};
pub use ops::{CaseInput, CaseOutput};
pub use registry::{registry, RegisteredOperation};
pub use rubric::{MetricResult, Outcome, Severity, Verification, RUBRIC_VERSION};
