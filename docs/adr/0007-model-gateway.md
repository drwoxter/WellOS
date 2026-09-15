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
- Implementations are selected by `DMIND_MODEL_PROVIDER`: `disabled`
  (default; every operation reports the capability as disabled),
  `openai_compatible` (real external HTTP adapter with typed,
  prompt-versioned operations, schema and evidence validation, HTTPS + exact
  host allowlist, no redirects, timeouts, response-size caps, bounded
  retries, concurrency limits and secret-safe errors) and `fake`
  (deterministic, offline, EN/ES, forceable unavailable) which is compiled
  only with the `dev-fixtures` feature and refused at runtime outside
  `development`/`test`. There is no fallback between providers.
- External providers are additionally gated on `WELLOS_ALLOW_EXTERNAL_AI`,
  the tenant flag `allow_external_ai` (default `false`) and purpose-specific
  patient consent.
- Every execution passes through the shared governance module (`aigov`):
  identical requests reuse the existing artifact, hourly per-tenant and
  per-task quotas are reserved atomically, and provenance (provider, model,
  prompt version, schema version, input references, usage, synthetic flag)
  is persisted on the AIArtifact.
- AI generation runs after the clinical transaction commits; provider failure
  yields an `unavailable` artifact and never blocks clinical flow.

## Consequences

- CI and tests are hermetic; degradation paths are testable.
- CI runs with the fixture provider explicitly configured and never calls
  an external model; the real adapter is covered by a controlled local HTTP
  server.
- Redaction and an evaluation harness for the real adapter belong at this
  same seam (roadmap #10), with no route changes.
