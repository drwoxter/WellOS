# Roadmap

## Done (this foundation)

Closed-loop diagnostic result slice end to end: schema/migrations/seed, policy
(RBAC+ABAC), audit/provenance, transactional outbox, idempotent + amendable
result ingestion, deterministic versioned criticality rules with unit-safety,
alerts/follow-up tasks, dMind gateway + AIArtifact lifecycle (A2), consent
gates, break-glass, minimal FHIR facade, clinician UI (EN/ES, two themes),
unit + API integration tests, CI.

Identity/authorization hardening (phase 1): OIDC/OAuth 2.1 JWT boundary with
configured JWKS and local `sub` mapping; environment-gated dev auth with
fail-closed startup; hashed scoped expiring/revocable service credentials;
typed purpose-of-use enforcement; least-privilege break-glass (dedicated
role, emergency purpose, read-only, rate limited, reviewed); anti-probing
404s for cross-tenant resources; HttpOnly-cookie BFF browser sessions.

Identity/authorization hardening (phase 2): OIDC discovery with issuer
pinning, HTTPS-only JWKS resolution, cached auto-refreshing key sets with
bounded refresh intervals and fail-safe behavior; provider-aware
`(issuer, sub)` identity mapping with lazy migration from the legacy global
subject column; configurable MFA enforcement from validated `amr`/`acr`
claims; opaque hashed server-side browser sessions with absolute +
inactivity timeouts, rotation, logout revocation and CSRF protection;
service-credential admin API (issue/list/rotate/revoke, audited, one-time
secrets); production startup hardening (required `DATABASE_URL` and CORS
origins outside development) and security response headers; emergency
purpose gated behind the dedicated break-glass role for patient
search/read.

Identity/authorization hardening (phase 3A): browser OIDC login via
Authorization Code + PKCE (S256) with discovery-validated, issuer-pinned
authorization/token endpoints, server-side single-use login transactions
(hashed state/nonce, ≤ 10 minutes, atomic consumption), server-side code
exchange, ID-token validation through the existing OIDC boundary, opaque
session issuance, and optional provider logout; central facility-scoped
authorization (trusted-relationship facility derivation, explicit
NULL-facility allowlist, facility-scoped patient search, break-glass
facility coverage); shared PostgreSQL-backed rate limiting (anonymous
login/callback per hashed client address, per-principal patient search /
credential admin / general API, HTTP 429 + Retry-After, fail-closed store).

Clinical Workspace MVP: demo-ready clinical workspace on the existing APIs —
authenticated app shell (sidebar/mobile nav, facility + user context,
language/theme, sign-out), development role-card sign-in, `/dashboard`
(workload counts via a minimal worklist summary endpoint, prioritized pending
results, quick actions), `/patients` directory (search + registration),
`/patients/[id]` workspace (safety information, tabs, clinical timeline,
start encounter, order laboratory test), `/results` worklist (priority-first,
filters, responsive table/cards; `/worklist` redirects), `/requests/[id]`
result detail (workflow stepper, critical banner, advisory dMind section,
confirmed transitions), richer synthetic demo states, component + Playwright
browser/accessibility/responsive tests.

Clinical Encounter and Documentation MVP: `/encounters/[id]` consultation
workspace (persistent patient safety header, encounter status, reason for
consultation, structured clinical note with collapsible optional sections,
draft save with optimistic concurrency, sign-and-complete with confirmation,
cancellation); structured vital signs with hard/usual range validation,
explicit confirmation of unusual values, server-side BMI and previous-vitals
display; diagnoses by human-readable label with optional code and bounded
status, linked to the encounter and patient chart; immutable signed notes
with dated append-only addenda and full audit; a review-gated deterministic
dMind documentation aid (facts used, limitations, explicit accept/reject with
provenance, assistive-only); patient workspace integration (start/resume
consultation, timeline links to encounter summaries, addenda counts, recent
vital trends); seeded draft/signed/addendum demo encounters; backend
integration, component, Playwright journey, accessibility and 390px coverage.

## Next 10 backlog items (priority order)

1. **Identity phase 3B**: IdP-driven user provisioning (SCIM), token-bucket
   rate limiting with tenant-level aggregate caps, encrypted-at-rest login
   transactions.
2. **Care-team assignment model**: represent nurses and other staff assigned
   to a patient's care so consequential actions (e.g. patient notification)
   can be authorized beyond the single encounter practitioner. Until then,
   notification is physician-only.
3. **PostgreSQL row-level security** as a second tenant-isolation layer, plus
   audit hash-chaining for tamper evidence.
4. **Outbox dispatcher + NATS JetStream**: publish outbox rows, consumer
   deduplication, dead-letter handling.
5. **Escalation delivery**: on-call schedules and notification channels for
   overdue critical results (currently a deterministic job + audit only).
6. **FHIR hardening**: search, Bundles, CapabilityStatement, validator-backed
   contract tests in CI.
7. **Frontend test depth**: browser E2E of failure and permission-denied
   scenarios beyond the golden clinician path now covered.
8. **Observability**: OpenTelemetry traces/metrics with PHI-free attribute
   linting, dashboards for loop latency and overdue counts.
9. **Object storage abstraction** (S3-compatible) for large artifacts, with
   per-tenant encryption context.
10. **Real model provider adapter** behind the gateway with redaction,
   evaluation harness, and shadow-mode comparison against the fake provider.
11. **Backup/restore automation** and load smoke tests in CI against a
    disposable environment.

## Later

Specialty engines, Workflow Studio, Command Center, research/de-identification
pipelines, Brand Studio UI, multi-cell operations tooling, Exchange.
