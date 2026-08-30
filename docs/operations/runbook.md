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
- **Staging/production**: set `WELLOS_OIDC_ISSUER`, `WELLOS_OIDC_AUDIENCE`,
  and `WELLOS_OIDC_JWKS_JSON` or `WELLOS_OIDC_JWKS_PATH` (plus optional
  `WELLOS_OIDC_LEEWAY_SECS`). The server **fails to start** if dev auth is
  enabled outside development, or if neither dev auth nor OIDC is
  configured. Provision users by inserting rows with the IdP's stable `sub`
  in `users.oidc_subject`; roles are assigned in `role_assignments`.
- **Service credentials** live in `service_credentials`: rotate by inserting
  a new hashed credential and revoking the old one
  (`UPDATE service_credentials SET revoked_at = now() WHERE id = ...`).
  Expiry (`expires_at`) and last use (`last_used_at`) support rotation
  hygiene. Scopes (e.g. `result.ingest`) bound what the credential can do.
- **JWKS rotation**: update the configured JWKS (file or env) and restart.
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
| 401 responses | Dev token (`dev-<seeded username>`) with dev auth enabled, a live `wsk_` service credential, or a valid OIDC JWT with a mapped `oidc_subject` |
| 403 responses | Role, scope, or purpose-of-use does not permit the action — see `policy.rs`; denials are audited |
| 404 for a resource you expect | Nonexistent — or belongs to another tenant (cross-tenant probes are indistinguishable by design) |
| 409 on transitions | Stale `version` — refetch the service request |
| AI artifact `unavailable` | Expected degradation path; clinical flow continues |

## Logging

Structured logs to stdout; identifiers only, never clinical payloads. Do not
raise log verbosity in shared environments without checking PHI rules.
