# Security Policy

WellOS is a development-stage project. It must not be deployed with real
patient data in its current form.

## Reporting a vulnerability

Email the maintainer (repository owner) with a description, reproduction steps,
and impact. Do not open public issues for exploitable vulnerabilities. You will
receive an acknowledgement within 5 business days.

## Authentication

- **Human identities (production path):** OIDC / OAuth 2.1 bearer JWTs
  validated against a configured JWKS (`WELLOS_OIDC_ISSUER`,
  `WELLOS_OIDC_AUDIENCE`, `WELLOS_OIDC_JWKS_JSON`/`_PATH`). Signature,
  issuer, audience, `exp`, `nbf` and `iat` are validated with a configurable
  clock skew. Only asymmetric algorithms (RS*/ES*/EdDSA) are accepted. The
  stable `sub` claim is mapped to a local identity record; tenant, roles,
  permissions and email are **never** taken from token claims — they are
  resolved exclusively from the local database.
- **Development tokens** (`dev-<username>`) authenticate seeded synthetic
  users only when `WELLOS_ENV=development` **and** `WELLOS_DEV_AUTH=true`.
  Startup fails closed if dev auth is enabled in any other environment, and
  fails closed if neither dev auth nor an identity provider is configured.
- **Service credentials:** machines authenticate with random 256-bit
  `wsk_...` secrets. Only a SHA-256 hash is stored, with principal, scopes
  (e.g. `result.ingest`), creation, expiration, revocation and last-used
  metadata. Service credentials cannot authenticate human users, and human
  tokens cannot authenticate machine principals. Credentials are seeded only
  in local development and shown once.

## Authorization

- All access decisions flow through a central RBAC + contextual ABAC policy
  module; allows and denials are audited with the effective purpose of use.
- **Purpose of use** is a typed enum (`treatment`, `operations`,
  `emergency`, `quality`) enforced against a per-action matrix. Asserting a
  different purpose can only narrow access, never widen it.
- **Break-glass** is least-privilege: it requires the dedicated
  `break_glass_authorized` role, `purpose_of_use=emergency`, a bounded
  8–500 character reason, a patient-specific same-tenant resource, and is
  limited to emergency *reads*. It never authorizes result review, patient
  notification, loop closure, AI review, consent changes or any other
  clinical write. Every activation is stored immutably (actor, patient,
  reason, timestamp, correlation ID, review status), rate-limited per user
  per hour (`WELLOS_BREAK_GLASS_HOURLY_LIMIT`, default 5), and must be
  reviewed post hoc by privacy/security roles via
  `/api/v1/break-glass/:id/review`.
- Tenant isolation is enforced in every query path; cross-tenant probes
  return the same `404` as nonexistent resources so resource IDs cannot be
  enumerated, while the denial is still audited.

## Browser sessions

- The web app never stores access tokens in `localStorage`,
  `sessionStorage` or any JavaScript-readable storage. The Next.js server
  acts as a BFF: credentials live in an `HttpOnly`, `SameSite=Strict`
  cookie (`Secure` in production) and API calls are proxied same-origin.
- Logout deletes the server-side session cookie.
- The token-entry form renders only in explicit local development.
- CORS uses an explicit allowlist (`WELLOS_ALLOWED_ORIGINS`); production
  deployments must configure their origins.

## Logging

- Logs and outbox events carry identifiers, not clinical payloads.
- Tokens, authorization headers and secret material are never logged; the
  seed tool prints a development credential once, locally only.

## Known gaps (tracked in docs/roadmap.md)

- No row-level security in PostgreSQL yet (application-level isolation only).
- No tamper-evident hash chain on audit events yet.
- No general request rate limiting or WAF in the development server (only the
  per-user break-glass rate limit is enforced).
- JWKS is static configuration; there is no automatic JWKS refresh/rotation
  endpoint polling yet, and no OIDC `iss` discovery. Rotate by updating the
  configured JWKS and restarting.
- No token revocation list for human OIDC sessions (revocation is delegated
  to the IdP via short token lifetimes).
- TLS is expected from the deployment environment, not the app process.
