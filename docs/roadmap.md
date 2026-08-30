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

## Next 10 backlog items (priority order)

1. **Identity phase 3**: browser OIDC authorization-code + PKCE flow,
   IdP-driven user provisioning (SCIM), facility-scoped role enforcement,
   general per-tenant rate limiting.
2. **PostgreSQL row-level security** as a second tenant-isolation layer, plus
   audit hash-chaining for tamper evidence.
3. **Outbox dispatcher + NATS JetStream**: publish outbox rows, consumer
   deduplication, dead-letter handling.
4. **Escalation delivery**: on-call schedules and notification channels for
   overdue critical results (currently a deterministic job + audit only).
5. **FHIR hardening**: search, Bundles, CapabilityStatement, validator-backed
   contract tests in CI.
6. **Frontend test depth**: component tests, automated accessibility (axe)
   checks, and browser E2E of the primary and failure scenarios.
7. **Observability**: OpenTelemetry traces/metrics with PHI-free attribute
   linting, dashboards for loop latency and overdue counts.
8. **Object storage abstraction** (S3-compatible) for large artifacts, with
   per-tenant encryption context.
9. **Real model provider adapter** behind the gateway with redaction,
   evaluation harness, and shadow-mode comparison against the fake provider.
10. **Backup/restore automation** and load smoke tests in CI against a
    disposable environment.

## Later

Specialty engines, Workflow Studio, Command Center, research/de-identification
pipelines, Brand Studio UI, multi-cell operations tooling, Exchange.
