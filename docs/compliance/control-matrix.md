# Control Matrix

Maps implemented technical controls to common regulatory themes (GDPR, HIPAA
Security Rule, and general health-data regulation). **This is an engineering
traceability aid, not a compliance claim** — no audit or legal assessment has
been performed.

| Control theme | Implementation | Evidence |
| --- | --- | --- |
| Access control / least privilege | Central RBAC + contextual ABAC (`policy.rs`); per-action constants; deny-by-default | Authorization integration tests |
| Unique user identification | Per-user identities and role assignments; no shared accounts in seed | `users`, `role_assignments` |
| Audit controls | Audit event per access/transition/AI generation/denial, written in-transaction | `audit_events`; audit tests |
| Emergency access ("break-glass") | Reason mandatory; same-tenant only; enhanced audit records for review | `break_glass_events`; tests |
| Data minimization | Events/logs carry identifiers, not clinical payloads; AI inputs hashed and cited | outbox schema; gateway |
| Purpose limitation & consent | Purpose-specific versioned consents; external AI requires consent + tenant flag (default off) | `consents`; consent gate |
| Integrity | Append-only observations/audit; amendments linked; optimistic concurrency | schema; amendment tests |
| Tenant/data segregation | `tenant_id` on all clinical tables; scoped queries; cross-tenant denial audited | isolation tests |
| Data residency | Regional-cell deployment model; no cross-cell replication | regional-cell doc |
| Transmission security | TLS expected at deployment boundary; no PHI in URLs | deployment assumption |
| Person accountable for closure | Loop closure restricted to authorized clinicians; recorded with actor | closure tests |
| Right of access / rectification (direction) | FHIR R4 read facade; amendment model preserves history | FHIR endpoints |
| Automated-decision safeguards | AI capped at A2 (assistive); human review mandatory; deterministic rules decide criticality | AIArtifact lifecycle |

## Known gaps

Encryption at rest (deployment concern), retention/erasure workflows, DPIA,
records of processing, breach-notification tooling, RLS enforcement layer.
Tracked in `docs/roadmap.md`.
