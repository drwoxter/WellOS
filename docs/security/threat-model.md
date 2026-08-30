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
| Spoofing | Forged identity | OIDC JWT validation against static or discovery-resolved JWKS (issuer-pinned, HTTPS-only, cached with bounded auto-refresh; signature/iss/aud/exp/nbf/iat, asymmetric algorithms only), `(issuer, sub)`→local identity mapping; optional MFA enforcement from validated `amr`/`acr` claims (fails closed); hashed scoped service credentials with expiry/revocation and an audited admin API; opaque hashed browser sessions with rotation/revocation; dev tokens only in explicit local development (startup fails closed otherwise); tenant/roles derived server-side only | Token binding, SCIM provisioning |
| Tampering | Modify clinical history or audit | Append-only observations & audit; amendments linked, never overwrite; parameterized SQL throughout | Audit hash chain; Postgres RLS; WORM storage for audit |
| Repudiation | Deny having acted | Every access/transition/AI event audited with actor, purpose, correlation id; break-glass requires reason | Time-stamping service |
| Information disclosure | Cross-tenant reads, resource-ID probing, PHI in logs/events, token theft via XSS | Tenant scoping in all queries; cross-tenant probes return 404 identical to missing resources (denial still audited); outbox/logs carry ids not clinical payloads; no access tokens in cookies — only opaque hashed `wss_` sessions in HttpOnly cookies via the BFF, CSRF double-submit on state-changing requests, security headers (nosniff/no-referrer/frame-deny/CSP/HSTS); external AI off by default + consent gate | Field-level encryption; redaction layer at model gateway |
| Denial of service | Flooding ingestion or AI calls | Idempotent ingestion; AI async and non-blocking; bounded DB pool; per-user break-glass rate limit | General rate limiting, quotas per tenant |
| Elevation of privilege | Role abuse, break-glass misuse, purpose-header widening | Central least-privilege policy; typed purpose-of-use matrix (headers can only narrow access); service credentials scope-limited and unable to act as humans; break-glass requires dedicated role + emergency purpose, read-only, same-tenant, bounded reason, per-user hourly limit, immutable event with mandatory privacy/security review | Anomaly detection on break-glass patterns |

## Abuse cases exercised by tests

Cross-tenant access (404, incl. break-glass), nurse closing a loop
(denied), research role touching clinical data (denied), audit read by
non-audit roles or wrong purpose (denied), unauthorized physicians using
break-glass (denied), break-glass mutations (denied), break-glass rate
limiting and once-only review, expired/revoked/wrong-scope/malformed
service credentials (rejected), invalid OIDC signature/issuer/audience/
expiry/subject (rejected), unknown `kid` after rotation (refreshed) and
unknown keys with unavailable JWKS (rejected), missing/malformed MFA claims
under MFA policy (rejected), expired/idle/revoked/rotated-away sessions
(rejected), missing/wrong CSRF token on writes (rejected), cross-tenant
service-credential admin (404), emergency search without the break-glass
role (denied), dev tokens outside development (rejected), duplicate inbound
results (no duplicates), stale version writes (409).

## Residual risk: break-glass abuse controls

The per-user hourly rate limit is persistent (database-backed) and
configurable, but there is no anomaly detection or automatic alerting on
unusual break-glass volume; detection relies on the mandatory post-hoc
review queue. An authorized emergency user within the rate limit can read
any same-tenant patient record; the compensating controls are the immutable
event trail and privacy-role review.

## Assumptions

TLS terminated by the deployment environment; database credentials via
environment; no untrusted code in the deployment; single-cell deployment (no
cross-cell traffic to protect yet).
