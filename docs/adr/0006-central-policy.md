# ADR-0006: Centralized Policy — RBAC plus Contextual ABAC

Status: Accepted · Date: 2026-08-29

## Context

Scattered per-route permission checks drift and rot; healthcare access control
needs role, tenant, purpose-of-use, and emergency (break-glass) context in one
auditable place.

## Decision

- One policy module (`wellos-server/src/policy.rs`) with named action
  constants (`patient.read`, `loop.close`, `audit.read`, ...); deny by
  default.
- Decisions combine roles (RBAC) with context (ABAC): tenant match, typed
  purpose of use checked against a per-action matrix, service credential
  scopes, break-glass context. Every allow and denial is audited with the
  effective purpose.
- Purpose of use is an enum (`treatment`, `operations`, `emergency`,
  `quality`); an asserted purpose can only narrow access, never widen it.
  Asserting `emergency` for patient search/read additionally requires the
  dedicated `break_glass_authorized` role, so emergency purpose never
  grants tenant-wide lookup to ordinary users.
- Break-glass: requires the dedicated `break_glass_authorized` role,
  `purpose_of_use=emergency`, a bounded reason, a patient-specific
  same-tenant resource; limited to emergency reads; per-user hourly rate
  limit; immutable event with mandatory post-hoc review by privacy/security
  roles — it never widens tenancy and never authorizes clinical writes.
- Cross-tenant denials surface as `404` (identical to nonexistent
  resources) to prevent resource-ID probing; the denial is still audited.
- Identity is resolved server-side: OIDC/OAuth 2.1 JWTs validated against a
  static or discovery-resolved JWKS (issuer-pinned, HTTPS-only, cached with
  bounded auto-refresh) with the validated `(issuer, sub)` pair mapped to a
  local user via `user_identities` (tenant/roles come only from the
  database); optional MFA enforcement from validated `amr`/`acr` claims;
  hashed scoped `wsk_` service credentials for machines (administered via
  an audited privacy-officer-only API); opaque hashed `wss_` browser
  sessions with absolute + inactivity timeouts, rotation, revocation and
  CSRF double-submit protection; and development tokens (`dev-<username>`)
  only when `WELLOS_ENV=development` and `WELLOS_DEV_AUTH=true`.
- Service-credential administration (`service_credential.manage`/`.read`)
  is restricted to `privacy_officer` (read also `security_auditor`) under
  the `operations` purpose.

- Role assignments carry an optional `facility_id`, but enforcement today is
  **tenant-wide**: a role grants its actions across the whole tenant.
  Facility-scoped enforcement is a documented follow-up, not an implemented
  guarantee.

## Consequences

- The authorization matrix is testable as data (integration tests cover
  nurse/research/audit/cross-tenant cases).
- Adding an endpoint forces naming its action — no accidental open routes.
