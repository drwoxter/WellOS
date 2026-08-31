//! Versioned, transport-neutral event envelopes.
//!
//! Events carry minimal identifiers and metadata only — never full clinical
//! payloads or PHI in routing keys. Consumers fetch authorized data through
//! governed APIs.

use crate::ids::{CorrelationId, EventId, TenantId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EventEnvelope {
    pub event_id: EventId,
    /// e.g. "result.received"
    pub event_type: String,
    pub schema_version: String,
    pub tenant_id: TenantId,
    /// Regional cell identifier (development baseline: "cell-dev-1").
    pub cell: String,
    /// Human user or service principal that caused the event.
    pub actor: String,
    pub correlation_id: CorrelationId,
    pub causation_id: Option<EventId>,
    pub occurred_at: DateTime<Utc>,
    pub recorded_at: DateTime<Utc>,
    pub source: String,
    /// Minimal resource references (ids only, no clinical payloads).
    pub resource_refs: Value,
}

/// Event vocabulary implemented by the vertical slice.
pub const EVENT_TYPES: &[&str] = &[
    "patient.registered",
    "encounter.started",
    "encounter.cancelled",
    "encounter.note.saved",
    "encounter.note.signed",
    "encounter.note.addendum",
    "encounter.vitals.recorded",
    "encounter.diagnosis.added",
    "service_request.created",
    "result.received",
    "result.amended",
    "result.critical_flagged",
    "result.reviewed",
    "patient.notified",
    "follow_up.created",
    "follow_up.overdue",
    "result_loop.closed",
    "consent.changed",
    "policy.access_denied",
    "break_glass.activated",
    "break_glass.reviewed",
    "ai.artifact.requested",
    "ai.artifact.generated",
    "ai.artifact.reviewed",
    "ai.provider.unavailable",
    "ai.generation.failed",
];

pub fn is_known_event_type(t: &str) -> bool {
    EVENT_TYPES.contains(&t)
}
