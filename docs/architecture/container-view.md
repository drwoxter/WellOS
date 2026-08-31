# Container View

## Containers

| Container | Technology | Responsibility |
| --- | --- | --- |
| `wellos-server` | Rust, Axum, Tokio, SQLx | HTTP API, policy, audit, outbox, FHIR facade, background escalation job endpoint |
| `wellos-domain` | Rust (pure, no I/O) | Typed IDs, UCUM-aware quantities, deterministic versioned rules, result-loop state machine, AIArtifact lifecycle, event envelopes |
| `dmind-gateway` | Rust | Provider-neutral `ModelGateway` trait; deterministic offline `FakeProvider` |
| `apps/web` | Next.js 14, strict TypeScript | Clinician UI: sign-in, worklist, result detail, review/notify/close |
| PostgreSQL 16 | Docker | Authoritative transactional store, migrations, seed |

## Modular monolith layout

`wellos-server` is a single deployable with internal bounded contexts (routes
modules): patients, encounters, service requests, lab ingestion, worklist, AI
artifacts, consent, audit, jobs, FHIR. Contexts communicate through the
database transaction and the outbox — not through direct cross-module calls
into each other's tables — keeping later extraction into services possible
(ADR-0001).

## Key runtime flows

- **Result ingestion** (`POST /api/v1/lab/results`): one DB transaction writes
  the observation, rule evaluation, alert/task (if critical), loop-state
  transition, audit event, and outbox event. AI summarization runs **after
  commit**; its failure never blocks the clinical write (ADR-0007/0008).
- **Idempotency**: unique `(tenant_id, idempotency_key)` on observations; a
  replay returns the original identifiers without new writes.
- **Optimistic concurrency**: service requests carry a `version`; stale
  transitions are rejected with 409.

## Not yet present (deliberate)

NATS JetStream (outbox table is transport-ready), Valkey/Redis cache,
S3-compatible artifact store, OpenTelemetry exporter. Each has a seam in the
code and an ADR describing the direction.
