# Threat Model

Scope: the implemented slice (web UI, API server, PostgreSQL, fake AI
provider). Method: STRIDE per trust boundary. Synthetic data only today; the
model is written for the intended clinical use.

## Assets

Patient records (future PHI), audit trail integrity, AI artifacts and their
provenance, credentials/tokens, tenant isolation guarantees.

## STRIDE summary

| Threat | Vector | Mitigations (implemented) | Planned |
| --- | --- | --- | --- |
| Spoofing | Forged identity | Server-side token→identity resolution; dev tokens only match seeded users; tenant derived server-side | OIDC/OAuth 2.1, MFA, short-lived tokens |
| Tampering | Modify clinical history or audit | Append-only observations & audit; amendments linked, never overwrite; parameterized SQL throughout | Audit hash chain; Postgres RLS; WORM storage for audit |
| Repudiation | Deny having acted | Every access/transition/AI event audited with actor, purpose, correlation id; break-glass requires reason | Time-stamping service |
| Information disclosure | Cross-tenant reads, PHI in logs/events | Tenant scoping in all queries; cross-tenant denial audited; outbox/logs carry ids not clinical payloads; external AI off by default + consent gate | Field-level encryption; redaction layer at model gateway |
| Denial of service | Flooding ingestion or AI calls | Idempotent ingestion; AI async and non-blocking; bounded DB pool | Rate limiting, quotas per tenant |
| Elevation of privilege | Role abuse, break-glass misuse | Central least-privilege policy; contextual checks; break-glass same-tenant only, reason mandatory, enhanced audit for retrospective review | Privileged-access review workflow, anomaly detection |

## Abuse cases exercised by tests

Cross-tenant access (denied, incl. break-glass), nurse closing a loop
(denied), research role touching clinical data (denied), audit read by
non-audit roles (denied), duplicate inbound results (no duplicates), stale
version writes (409).

## Assumptions

TLS terminated by the deployment environment; database credentials via
environment; no untrusted code in the deployment; single-cell deployment (no
cross-cell traffic to protect yet).
