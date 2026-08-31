//! WellOS clinical domain: pure, deterministic types and rules.
//!
//! This crate contains no I/O. Everything here must be testable without a
//! database, network, or AI provider. Deterministic clinical logic (critical
//! result evaluation, unit normalization, state machines) lives here so it can
//! never depend on model output.

pub mod ai;
pub mod events;
pub mod ids;
pub mod result_loop;
pub mod rules;
pub mod units;

pub use ids::*;
