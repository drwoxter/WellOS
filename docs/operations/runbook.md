# Runbook (Local / Development)

## Services

| Service | Start | Port | Health |
| --- | --- | --- | --- |
| PostgreSQL 16 | `make up` (docker compose, `infra/docker-compose.yml`) | 5432 | `pg_isready -h localhost -U wellos` |
| API server | `make server` | 8080 | `GET /health` (liveness), `GET /ready` (checks DB) |
| Web UI | `make web` | 3000 | page load |

## First-time setup

```bash
cp .env.example .env
make up && make migrate && make seed
```

Seeding prints the lab-adapter service credential (`wsk_...`) once — store it
in your local `.env` workflow if you need it; it is never persisted in
plaintext (only a SHA-256 hash is stored) and expires after 90 days.

## Authentication configuration

- **Local development**: `WELLOS_ENV=development` + `WELLOS_DEV_AUTH=true`
  enable `dev-<username>` tokens for seeded synthetic users. Both are set in
  `.env.example`.
- **Staging/production**: set `WELLOS_OIDC_ISSUER` and
  `WELLOS_OIDC_AUDIENCE`, then either a static JWKS
  (`WELLOS_OIDC_JWKS_JSON`/`WELLOS_OIDC_JWKS_PATH`) or
  `WELLOS_OIDC_DISCOVERY=true` (issuer metadata fetched at startup,
  issuer-pinned, HTTPS-only `jwks_uri`; cache tuned with
  `WELLOS_OIDC_JWKS_REFRESH_SECS` / `WELLOS_OIDC_JWKS_MIN_REFRESH_SECS`).
  Optional: `WELLOS_OIDC_LEEWAY_SECS`, and MFA policy via
  `WELLOS_OIDC_REQUIRE_MFA` + `WELLOS_OIDC_ACCEPTED_AMR` /
  `WELLOS_OIDC_ACCEPTED_ACR`. The server **fails to start** if dev auth is
  enabled outside development, if neither dev auth nor OIDC is configured,
  or (outside development) if `DATABASE_URL` or `WELLOS_ALLOWED_ORIGINS`
  is missing. Provision users by inserting `(issuer, subject)` rows in
  `user_identities` (legacy `users.oidc_subject` still matches and is
  migrated lazily); roles are assigned in `role_assignments`.
- **Service credentials** are administered via the audited
  privacy-officer API (`purpose=operations`):
  `POST /api/v1/admin/service-credentials` (issue; plaintext shown once),
  `GET` (metadata incl. expiry/last use), `POST .../:id/rotate` (revokes
  old, returns new secret once), `POST .../:id/revoke`. Scopes (e.g.
  `result.ingest`) bound what the credential can do.
- **Browser sessions**: opaque `wss_` identifiers stored hashed in
  `web_sessions`; lifetimes via `WELLOS_SESSION_ABSOLUTE_SECS` (default 8h)
  and `WELLOS_SESSION_IDLE_SECS` (default 30m). Revoke a session
  immediately with `UPDATE web_sessions SET revoked_at = now() WHERE ...`.
- **JWKS rotation**: with discovery enabled, new key IDs are picked up
  automatically (bounded by the min-refresh interval). With a static JWKS,
  update the configured JWKS (file or env) and restart.
- **Break-glass review**: privacy/security roles list pending events at
  `GET /api/v1/break-glass` and record the mandatory review with
  `POST /api/v1/break-glass/:id/review` (purpose `operations` or `quality`).
  The per-user activation limit is `WELLOS_BREAK_GLASS_HOURLY_LIMIT`.

Seed is idempotent-ish for demos but intended for empty databases; to reset:
`docker compose -f infra/docker-compose.yml down -v && make up && make migrate && make seed`.

## Common tasks

- **Apply new migrations**: `make migrate` (runs the `migrate` binary; SQLx
  tracks applied migrations in `_sqlx_migrations`).
- **Run overdue-result escalation**: `POST /api/v1/jobs/escalate-overdue`
  (clinical administrator role) — deterministic; safe to re-run.
- **Inspect audit trail**: `GET /api/v1/audit` as `dev-privacy.wolf` or
  `dev-audit.stone` with header `X-Purpose-Of-Use: operations` (or
  `quality`).
- **Force AI degradation (tests)**: the fake provider exposes
  `set_unavailable(true)`; in integration tests only.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| `/ready` fails | DB container up? `DATABASE_URL` correct? migrations applied? |
| Startup fails with auth error | `WELLOS_DEV_AUTH=true` outside development, or no IdP configured — intentional fail-closed behavior |
| 401 responses | Dev token (`dev-<seeded username>`) with dev auth enabled, a live `wsk_` service credential, a live `wss_` session, or a valid OIDC JWT with a mapped identity (check MFA policy and JWKS freshness too) |
| 403 responses | Role, scope, or purpose-of-use does not permit the action — see `policy.rs`; denials are audited |
| 404 for a resource you expect | Nonexistent — or belongs to another tenant (cross-tenant probes are indistinguishable by design) |
| 409 on transitions | Stale `version` — refetch the service request |
| AI artifact `unavailable` | Expected degradation path; clinical flow continues |

## Logging

Structured logs to stdout; identifiers only, never clinical payloads. Do not
raise log verbosity in shared environments without checking PHI rules.
