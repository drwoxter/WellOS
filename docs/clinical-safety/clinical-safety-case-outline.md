# Clinical Safety Case Outline

Status: outline for a development prototype. No clinical validation has been
performed; this document structures the argument that will be developed before
any real-world use, in the spirit of DCB0129/DCB0160-style safety cases.

## Top-level claim

WellOS's closed-loop diagnostic result workflow is acceptably safe for its
intended (currently synthetic/demonstration) use, because hazardous outcomes
are controlled by deterministic logic, enforced human accountability, and
auditable state.

## Argument structure

1. **Correct criticality determination**
   - Versioned deterministic rules, unit-safe evaluation, refusal on unit
     mismatch with a recorded DataQualityIssue.
   - Evidence: `wellos-domain::rules` unit tests; integration tests.
2. **No lost critical results**
   - Loop state machine forbids skipping review/notification; overdue
     unreviewed results escalate deterministically; amended results reopen
     review.
   - Evidence: state-machine unit tests; escalation and amendment integration
     tests.
3. **AI cannot cause autonomous harm**
   - A2 ceiling for summaries; artifacts require human review; AI failure
     degrades gracefully; no AI writes to clinical fields.
   - Evidence: AIArtifact lifecycle tests; AI-unavailable integration test.
4. **Right people, right data**
   - Central policy (RBAC + context), tenant isolation, consent gates,
     break-glass with mandatory reason and enhanced audit.
   - Evidence: authorization matrix and cross-tenant integration tests.
5. **Reconstructable history**
   - Append-only observations and audit; provenance links; idempotent
     ingestion prevents duplicate clinical objects.
6. **Safe operational triage (access → arrival → triage → consultation)**
   - Deterministic, versioned safety floor from explicit red flags, urgent
     arrival and abnormal vitals; priorities below the floor are refused
     server-side; dMind triage proposals are clamped to the floor, bound to the
     exact triage version and applied only after explicit human review.
   - Internal alerts are directed to the assigned professional or the facility
     service queue, re-raised on re-routing and resolved when the consultation
     starts; visit transitions are locked and version-checked.
   - Evidence: `wellos-domain::triage` unit tests; `access_triage_integration`
     tests (floor enforcement, stale proposal refusal, routing, authorization);
     Playwright golden path. Not a validated triage scale — see
     `docs/architecture/patient-access-and-triage.md` for the governance work
     required before real use.

## Out of scope of this outline

Human-factors validation, clinical pilot evidence, quantified risk acceptance
criteria, and regulatory classification — all prerequisites to any non-demo
deployment.
