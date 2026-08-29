# ADR-0007: AI Model Gateway with Deterministic Fake Provider

Status: Accepted · Date: 2026-08-29

## Context

Clinical AI must be provider-neutral, testable without network calls, and
governable at a single seam (routing, redaction, consent, quotas, evaluation).
Development must not depend on paid external providers or send synthetic data
outward by default.

## Decision

- `dmind-gateway` crate defines a `ModelGateway` trait; all AI calls go
  through it — routes never talk to a provider directly.
- The only current implementation is `FakeProvider`: deterministic, offline,
  EN/ES, produces cited structured summaries, and can be forced unavailable
  for degradation testing. It is explicitly labeled non-production.
- External providers are additionally gated on `allow_external_ai` (default
  `false`) and purpose-specific patient consent.
- AI generation runs after the clinical transaction commits; provider failure
  yields an `unavailable` artifact and never blocks clinical flow.

## Consequences

- CI and tests are hermetic; degradation paths are testable.
- A real provider adapter later needs redaction + evaluation harness at this
  same seam (roadmap #9), with no route changes.
