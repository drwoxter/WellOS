# ADR-0004: Transactional Outbox and Event Transport

Status: Accepted · Date: 2026-08-29

## Context

Domain events must never diverge from clinical state (a critical-result event
without the observation, or vice versa, is a safety defect). A broker adds
operational weight the slice doesn't need yet.

## Decision

Write events to an `outbox_events` table **in the same transaction** as the
state change, using a versioned envelope (see event-model doc). Payloads carry
identifiers/metadata, never full clinical content. Transport is deferred: the
intended dispatcher publishes to NATS JetStream; consumers deduplicate on
`event_id` (at-least-once).

## Consequences

- Exactly-once *recording* of events with zero broker dependency today.
- Consumers are not yet real; the table doubles as an integration test point.
- Valkey/Redis remains cache/ephemeral only — never an event transport or
  source of truth.
