# ADR-0008: AIArtifact Lifecycle and Human Approval

Status: Accepted · Date: 2026-08-29

## Context

AI output that flows directly into clinical fields is a safety hazard and an
accountability gap. Every AI output needs identity, provenance, review state,
and a bounded autonomy level.

## Decision

- Every AI output is persisted as an **AIArtifact**: schema-versioned
  structured output (e.g. `ResultSummaryV1` with citations, limitations,
  suggested next-step categories), model metadata, input hash, autonomy level.
- Lifecycle: `draft → awaiting_review → approved | rejected`, plus
  `superseded`, `withdrawn`, `invalidated`, and `unavailable` (gateway
  failure).
- Autonomy levels A0–A4; result summaries run at **A2** (assistive draft,
  human review mandatory). No code path exists from an artifact to a clinical
  action; A4 autonomous clinical action is prohibited platform-wide.
- Generation and review are audit events; the UI must render the disclaimer
  and citations alongside the summary.

## Consequences

- Human accountability is structural, not procedural.
- Artifacts are reproducible (input hash) and supersedable when results are
  amended.
