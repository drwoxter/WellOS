# Event Model

## Envelope

Every domain event written to the transactional outbox uses a common envelope
(`wellos-domain::events`):

| Field | Meaning |
| --- | --- |
| `event_id` | UUIDv7 |
| `event_type` | dot-namespaced, versioned, e.g. `result.received.v1` |
| `occurred_at` | UTC timestamp |
| `tenant_id` | isolation scope |
| `correlation_id` | ties a request chain together |
| `actor` | user or service identity |
| `payload` | identifiers and minimal metadata — never full clinical content |

## Vocabulary (current slice)

- `patient.registered.v1`
- `encounter.started.v1`
- `service_request.created.v1`
- `result.received.v1` / `result.amended.v1`
- `result.critical_flagged.v1`
- `result.reviewed.v1` / `patient.notified.v1` / `loop.closed.v1`
- `ai.artifact_created.v1` / `ai.artifact_reviewed.v1`
- `consent.recorded.v1`
- `escalation.triggered.v1`

## Transport

Events are written to the `outbox_events` table in the **same transaction** as
the state change (transactional outbox, ADR-0004). No broker is wired yet; the
intended transport is NATS JetStream with per-tenant subjects. Consumers must
treat delivery as at-least-once and deduplicate on `event_id`.

## PHI rule

Payloads carry identifiers (patient id, service request id) and coded metadata
only. A consumer needing clinical content must fetch it through the API under
its own authorization context — events are pointers, not copies.
