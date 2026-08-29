# AI-Native Platform (dMind)

## Governance model

AI in WellOS is **governed by construction**:

1. **Every AI output is an AIArtifact** — a first-class, persisted, versioned
   record with status lifecycle, autonomy level, model metadata, input hash,
   and structured output. There is no path from model output directly into
   clinical fields.
2. **Autonomy levels A0–A4** bound what a route may do:
   - A0 — none; A1 — retrieval/summarization of existing record data;
   - A2 — assistive draft requiring human review (current result summary);
   - A3 — proposed action requiring explicit human approval;
   - A4 — reserved; no autonomous clinical action is permitted anywhere.
3. **Hard prohibitions**: AI never orders, prescribes, diagnoses, or changes
   treatment. Deterministic versioned rules — not models — decide criticality
   and thresholds.
4. **Asynchrony**: AI generation runs after the clinical transaction commits.
   Provider failure yields an `unavailable` artifact; the clinical workflow is
   never blocked (graceful degradation, tested).
5. **Consent and policy gates**: external AI processing requires
   purpose-specific consent and the tenant/policy flag `allow_external_ai`
   (default `false`). The development provider is local and deterministic.
6. **Provenance**: artifacts cite the exact source facts used, list
   limitations, and record the input hash for reproducibility; generation and
   review are audit events.

## Model Gateway

`dmind-gateway` exposes a provider-neutral `ModelGateway` trait. Providers are
adapters behind it (routing, redaction, quota, and evaluation hooks live at
this seam). The only implementation today is `FakeProvider`: deterministic,
offline, EN/ES, failure-injectable — explicitly non-production.

## Structured outputs

Outputs are schema-versioned (e.g. `ResultSummaryV1`: summary, trend,
citations, limitations, suggested next-step categories). The UI renders the
structure with a mandatory disclaimer; free-form generation is not exposed to
clinicians as a primary interface.
