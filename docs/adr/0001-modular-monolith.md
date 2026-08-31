# ADR-0001: Modular Monolith with Bounded Contexts

Status: Accepted · Date: 2026-08-29

## Context

WellOS spans many future modules, but the team and product are early. Premature
microservices multiply operational burden and blur clinical transaction
boundaries.

## Decision

Build a single deployable (`wellos-server`) organized into bounded contexts
(patients, encounters, orders/results, AI artifacts, consent, audit, FHIR).
Contexts share one PostgreSQL database and communicate via transactions and the
outbox, not via cross-context table access, keeping later extraction feasible.

## Consequences

- One transaction can atomically write clinical state + audit + outbox —
  essential for safety invariants.
- Deployment/ops stay simple; a future service split follows the outbox seams.
- Discipline required: context boundaries are convention-enforced (reviews,
  tests) until extraction pressure justifies harder boundaries.
