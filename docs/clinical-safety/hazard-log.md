# Hazard Log

Living document. Severity/likelihood are qualitative pending a formal risk
matrix. All hazards refer to the intended future clinical use; the current
system runs on synthetic data only.

| ID | Hazard | Cause | Potential harm | Controls (implemented) | Residual risk / planned |
| --- | --- | --- | --- | --- | --- |
| H-01 | Critical result not acted on | Alert missed, workflow abandoned | Delayed treatment | Loop state machine requires review→notify→close; worklist surfaces open loops; deterministic overdue escalation | Escalation delivery (paging/on-call) not built |
| H-02 | Wrong criticality classification | Threshold error, unit confusion | Missed critical / false alarm | Versioned deterministic rules; UCUM-aware conversion; refusal + DataQualityIssue on unknown units | Rule library review process needed |
| H-03 | AI summary misleads clinician | Model error/hallucination | Wrong clinical impression | A2 autonomy; mandatory human review; citations to source facts; limitations displayed; disclaimer; deterministic rule remains authoritative | Evaluation harness & drift monitoring planned |
| H-04 | AI acts autonomously | Design/regression error | Unauthorized order/prescription | No code path from artifact to clinical action; policy actions for orders require human identity; tests | Keep negative tests in CI |
| H-05 | Duplicate result creates duplicate alerts | Interface replay | Alert fatigue, confusion | Idempotency key unique per tenant; replay returns original ids | — |
| H-06 | Amended result unseen | Correction after review | Decisions on stale data | Amendment preserves history and reopens review | UI diff of amended values planned |
| H-07 | Wrong patient/tenant data exposure | AuthZ defect | Privacy breach, wrong-patient action | Central policy; tenant scoping in all queries; cross-tenant denial incl. break-glass; audit of denials | Postgres RLS planned |
| H-08 | Unauthorized loop closure | Role misconfiguration | Accountability gap | Closure restricted to authorized clinician roles; nurse-closure denial tested | Role review workflow planned |
| H-09 | PHI leakage via AI provider | Misconfigured external AI | Privacy breach | `allow_external_ai=false` default; purpose-specific consent gate; offline fake provider in dev | Redaction layer before any real provider |
| H-10 | Audit trail gaps | Missed instrumentation | Unaccountable actions | Audit emitted inside the same transaction as state changes | Tamper-evident hash chain planned |
| H-11 | Concurrent edits corrupt loop state | Race between users | Inconsistent state | Optimistic concurrency (`version`), 409 on stale writes | — |
| H-12 | Inaccessible UI blocks urgent action | A11y defects | Delayed response | Semantic tables, labels, `role="alert"`, keyboard operability; WCAG 2.2 AA target | Automated a11y tests planned |
| H-13 | Under-triage of a deteriorating patient | Priority set too low; AI proposal trusted over findings | Delayed emergency care | Deterministic safety floor (`triage-safety@1.0.0`) from explicit red flags, urgent arrival and abnormal vitals; UI disables and server rejects priorities below the floor; dMind proposals clamped to the floor and never applied without explicit human review | Rules are not a validated triage scale; clinical sign-off, prospective evaluation and reassessment timers required before real use |
| H-14 | dMind triage proposal acts on stale facts | Triage edited after generation | Decision on outdated vitals/flags | Proposal bound to the exact triage version; acceptance of a stale version refused (409); new proposal required | — |
| H-15 | Ready patient not seen | Alert unnoticed, wrong recipient | Prolonged waiting, deterioration | Directed alert to the assigned professional or the facility service queue; re-routing resolves and re-raises; Ready list on dashboard and access board sorted by priority then wait; acknowledgement audited | No escalation timers or paging; alerts are internal only |
| H-16 | Wrong-patient consultation from the worklist | Similar names, stale board | Wrong-patient documentation | Cards show name, identifier, age, reason and facility; start-consultation locks patient and visit and re-checks status/version; encounter header shows safety info | Photo/second identifier confirmation not implemented |
| H-17 | Unauthorized access via care-team routing | Role misconfiguration; assignment to a professional outside the facility | Privacy breach | Care relationship is patient-specific (assignment or encounter), never implied by role; routing restricted to the visit's facility; central policy re-checks every action; capability hints are presentation only | Care-team management UI and periodic assignment review planned |
