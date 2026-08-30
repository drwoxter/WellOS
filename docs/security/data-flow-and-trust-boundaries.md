# Data Flow and Trust Boundaries

```
[Browser]
   │ B0: HTTPS, same-origin; opaque wss_ session in HttpOnly SameSite=Strict
   │     cookie + JS-readable CSRF cookie (double-submit header)
   ▼
[Next.js BFF (/api/session, /api/v1 proxy)]
   │ B1: Authorization: Bearer <opaque session> attached server-side only
   ▼
[wellos-server]
   │  ── B2: token → identity, tenant, roles (auth.rs)
   │  ── B3: policy decision per action (policy.rs) + audit emit
   │
   ├── B4: SQLx (parameterized, tenant-scoped) ──▶ [PostgreSQL]
   │        same txn: clinical write + rule eval + audit + outbox
   │
   └── B5: after commit ──▶ [dMind gateway → FakeProvider]
            consent + allow_external_ai gate; input hash recorded;
            offline in development, no network egress
```

## Boundaries

- **B0 Browser→BFF**: access tokens are never exposed to browser JavaScript
  or stored in cookies. On sign-in the BFF exchanges the submitted
  credential for an opaque `wss_` session (hashed server-side in
  `web_sessions`, absolute + inactivity timeouts, rotation, revocation) and
  stores only that identifier in an HttpOnly cookie, plus a JS-readable
  CSRF cookie whose value must be echoed in `x-csrf-token` on
  state-changing requests. `GET /api/session` validates the server-side
  record; logout revokes it and clears both cookies. The token-entry form
  is development-only.
- **B1 BFF→server**: untrusted input. All payloads validated by typed
  deserialization; errors return structured codes without internals.
- **B2 Identity**: the client can never assert tenant, roles, permissions,
  or user id — only the token. Human production identities are OIDC JWTs
  validated against a static or discovery-resolved JWKS (issuer-pinned,
  HTTPS-only, cached with bounded auto-refresh on unknown `kid`; refresh
  failure keeps the last known-good keys and unknown keys fail closed) —
  signature, issuer, audience, exp, nbf, iat and clock skew all checked —
  with the validated `(issuer, sub)` pair mapped to a local user via
  `user_identities`; optional MFA enforcement reads validated `amr`/`acr`
  claims and fails closed. Machines use hashed, scoped, expiring, revocable
  `wsk_` credentials; browsers use opaque hashed `wss_` sessions;
  development tokens work only with `WELLOS_ENV=development` +
  `WELLOS_DEV_AUTH=true` and startup fails closed otherwise. Outside
  development, startup also requires explicit `DATABASE_URL` and
  `WELLOS_ALLOWED_ORIGINS`, and responses carry nosniff/no-referrer/
  frame-deny/CSP headers plus HSTS.
- **B3 Policy**: every route names an action constant checked against
  roles, tenant, service scopes and a typed purpose-of-use matrix; allows
  and denials are audited. Break-glass requires the dedicated role, the
  emergency purpose and a bounded reason; it is read-only, rate limited and
  reviewed post hoc; it never crosses tenants. Cross-tenant probes return
  404, indistinguishable from missing resources.
- **B4 Database**: single authoritative store. Observations and audit are
  append-only. Outbox rows carry identifiers, not clinical payloads.
- **B5 AI gateway**: fires only after the clinical transaction commits;
  failures produce an `unavailable` artifact. External providers are disabled
  by default and additionally gated on purpose-specific consent.

## Data classes

| Class | Examples | Handling |
| --- | --- | --- |
| Clinical (synthetic today) | observations, conditions | tenant-scoped tables only; never in logs/events/telemetry |
| Governance | audit, break-glass, consent, rule evaluations | append-only; restricted read (audit roles) |
| Operational | health/ready, counts | PHI-free by construction |
