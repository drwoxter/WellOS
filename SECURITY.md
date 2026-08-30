# Security Policy

WellOS is a development-stage project. It must not be deployed with real
patient data in its current form.

## Reporting a vulnerability

Email the maintainer (repository owner) with a description, reproduction steps,
and impact. Do not open public issues for exploitable vulnerabilities. You will
receive an acknowledgement within 5 business days.

## Authentication

- **Human identities (production path):** OIDC / OAuth 2.1 bearer JWTs
  validated against a JWKS that is either configured statically
  (`WELLOS_OIDC_JWKS_JSON`/`_PATH`) or resolved through OIDC discovery
  (`WELLOS_OIDC_DISCOVERY=true`): the issuer's
  `/.well-known/openid-configuration` is fetched at startup, the metadata
  `issuer` must match the configured issuer exactly (issuer pinning), and
  the advertised `jwks_uri` must be HTTPS (development may use plain HTTP
  toward loopback hosts only). The JWKS is
  cached (refresh interval `WELLOS_OIDC_JWKS_REFRESH_SECS`, default 1h) and
  refreshed automatically when an unknown `kid` appears, bounded by
  `WELLOS_OIDC_JWKS_MIN_REFRESH_SECS` (default 30s) so unknown-kid probing
  cannot hammer the IdP. If a refresh fails, the last known-good key set is
  kept and tokens signed with unknown keys fail closed. Signature, issuer,
  audience, `exp`, `nbf` and `iat` are validated with a configurable clock
  skew. Only asymmetric algorithms (RS*/ES*/EdDSA) are accepted.
- **Identity mapping is provider-aware:** the validated `(issuer, sub)`
  pair maps to a local user via the `user_identities` table. A legacy
  `users.oidc_subject` match is honored once and migrated lazily into
  `user_identities` scoped to the configured issuer. Tenant, roles,
  permissions and email are **never** taken from token claims — they are
  resolved exclusively from the local database.
- **MFA policy:** when `WELLOS_OIDC_REQUIRE_MFA=true`, the validated token
  must carry an MFA signal: an `amr` claim that is an array of strings
  containing one of `WELLOS_OIDC_ACCEPTED_AMR` (default `mfa,otp,hwk`), or
  an `acr` claim that is a string equal to one of
  `WELLOS_OIDC_ACCEPTED_ACR`. Missing, malformed or insufficient claims
  fail closed with 401. MFA is never inferred from email, role or any
  client-provided header.
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
- **Service-credential administration:** `/api/v1/admin/service-credentials`
  (issue, list metadata, rotate, revoke) is restricted to the
  `privacy_officer` role (`security_auditor` may list metadata) under
  `purpose_of_use=operations`. Plaintext secrets are returned exactly once
  at issuance or rotation; no endpoint can return an existing secret and
  only hashes are stored. Tenant, scopes and the machine principal are
  resolved server-side, cross-tenant access returns 404, and every
  operation is audited.
- **Browser sessions (`wss_...`):** authenticated principals exchange their
  credential for an opaque random 256-bit session identifier via
  `POST /api/v1/auth/session`. Only a SHA-256 hash is stored server-side in
  PostgreSQL (`web_sessions`), with absolute expiration
  (`WELLOS_SESSION_ABSOLUTE_SECS`, default 8h), inactivity timeout
  (`WELLOS_SESSION_IDLE_SECS`, default 30m), rotation
  (`POST /api/v1/auth/session/rotate`, which revokes the old session) and
  logout revocation (`DELETE /api/v1/auth/session`). Sessions are never
  issued to service principals or from an existing session.

## Authorization

- All access decisions flow through a central RBAC + contextual ABAC policy
  module; allows and denials are audited with the effective purpose of use.
- **Purpose of use** is a typed enum (`treatment`, `operations`,
  `emergency`, `quality`) enforced against a per-action matrix. Asserting a
  different purpose can only narrow access, never widen it. Emergency
  purpose does **not** grant tenant-wide patient search or reads to
  ordinary users: patient search/read under `purpose_of_use=emergency`
  additionally requires the dedicated `break_glass_authorized` role.
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
- **Facility scope (documented limitation):** role assignments record a
  facility, but authorization is currently enforced at tenant scope: a role
  grants its actions across all facilities of the tenant. This is a
  deliberate, bounded limitation for the vertical slice; facility-scoped
  enforcement is on the roadmap.

## Browser sessions

- The web app never stores access tokens in `localStorage`,
  `sessionStorage` or any JavaScript-readable storage, and raw bearer
  tokens are never placed in cookies. On sign-in the Next.js BFF exchanges
  the submitted credential for an opaque `wss_` session via
  `POST /api/v1/auth/session` and stores only that opaque identifier in an
  `HttpOnly`, `SameSite=Strict`, `Path=/` cookie (`Secure` in production).
  API calls are proxied same-origin; frontend JavaScript never receives an
  access token.
- **CSRF:** state-changing requests must carry an `x-csrf-token` header
  matching the per-session CSRF secret (`wsc_...`, stored hashed alongside
  the session). The CSRF secret lives in a JavaScript-readable
  `SameSite=Strict` cookie (`wellos_csrf`) — deliberately non-HttpOnly so
  the double-submit header can be attached — while the session cookie
  itself remains HttpOnly. Safe methods (GET/HEAD/OPTIONS) are exempt.
- `GET /api/session` validates the server-side session record (not mere
  cookie presence); logout revokes the server-side session and clears both
  cookies.
- The token-entry form renders only in explicit local development.
- CORS uses an explicit allowlist (`WELLOS_ALLOWED_ORIGINS`); startup fails
  outside development if it is not configured. `DATABASE_URL` is likewise
  required outside development (the localhost fallback is dev-only).
- API responses carry hardening headers: `X-Content-Type-Options: nosniff`,
  `Referrer-Policy: no-referrer`, `X-Frame-Options: DENY`, a deny-all CSP
  (`default-src 'none'; frame-ancestors 'none'`, appropriate for a JSON
  API), and HSTS outside development.

## Logging

- Logs and outbox events carry identifiers, not clinical payloads.
- Tokens, authorization headers and secret material are never logged; the
  seed tool prints a development credential once, locally only.

## Known gaps (tracked in docs/roadmap.md)

- No row-level security in PostgreSQL yet (application-level isolation only).
- No tamper-evident hash chain on audit events yet.
- No general request rate limiting or WAF in the development server (only the
  per-user break-glass rate limit is enforced).
- No token revocation list for human OIDC access tokens (revocation is
  delegated to the IdP via short token lifetimes); opaque browser sessions
  are revocable server-side.
- Role assignments are enforced tenant-wide, not facility-scoped (see
  Authorization above).
- The BFF sign-in endpoint accepts a pasted credential in development; a
  full OIDC authorization-code + PKCE browser flow is future work.
- TLS is expected from the deployment environment, not the app process.
