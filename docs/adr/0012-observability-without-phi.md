# ADR-0012: Observability Without PHI

Status: Accepted · Date: 2026-08-29

## Context

Operating a hospital system requires deep telemetry, but telemetry pipelines
(logs, traces, metrics, error trackers) are a classic PHI leak path.

## Decision

- Logs, events, and future traces carry **identifiers and coded metadata
  only** — patient ids, request ids, correlation ids, action names, outcome
  codes — never names, free-text notes, or clinical values.
- Health endpoints (`/health`, `/ready`) and metrics are PHI-free by
  construction.
- OpenTelemetry-compatible export is the target (roadmap #7), with an
  attribute allowlist enforced at the instrumentation layer and a PHI-safe
  telemetry test suite.
- Error responses to clients use structured codes/messages without internal
  details or clinical content.

## Consequences

- Debugging sometimes requires joining telemetry ids back to the database
  under proper authorization — accepted friction.
- Correlation ids (per request) make cross-layer tracing possible without
  payload capture.
