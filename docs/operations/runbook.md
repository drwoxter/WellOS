# Runbook (Local / Development)

## Services

| Service | Start | Port | Health |
| --- | --- | --- | --- |
| PostgreSQL 16 | `make up` (docker compose, `infra/docker-compose.yml`) | 5432 | `pg_isready -h localhost -U wellos` |
| API server | `make server-fixtures` (synthetic-fixture mode) or `make server` (`.env`, production-intent) | 8080 | `GET /health` (liveness), `GET /ready` (DB + per-capability AI status) |
| Web UI | `make web` | 3000 | page load |

## First-time setup

```bash
cp .env.development.example .env.development   # explicit synthetic-fixture mode
make up && make migrate && make seed
make server-fixtures
```

`.env.example` is the production-safe template (`WELLOS_ENV=production`,
dev auth off, AI providers `disabled`, seed refused); `make server` reads
`.env` and starts a build without `dev-fixtures`. `make seed` and
`make server-fixtures` pass `--features dev-fixtures` explicitly and set
`WELLOS_ENV`, `WELLOS_ALLOW_SYNTHETIC_SEED` and the `fake` providers on the
command line so fixture mode is never implicit. See `README.md` for the
equivalent PowerShell commands and the real-provider local mode.

Seeding prints the lab-adapter service credential (`wsk_...`) once — store it
in your local `.env` workflow if you need it; it is never persisted in
plaintext (only a SHA-256 hash is stored) and expires after 90 days.

## Authentication configuration

- **Local development**: `WELLOS_ENV=development` + `WELLOS_DEV_AUTH=true`
  on a `dev-fixtures` build enable `dev-<username>` tokens for seeded
  synthetic users. Both are set in `.env.development.example`;
  `.env.example` leaves dev auth off. `WELLOS_ENV` must always be set —
  a missing or unknown value refuses to start.
- **Staging/production**: set `WELLOS_OIDC_ISSUER` and
  `WELLOS_OIDC_AUDIENCE`, then either a static JWKS
  (`WELLOS_OIDC_JWKS_JSON`/`WELLOS_OIDC_JWKS_PATH`) or
  `WELLOS_OIDC_DISCOVERY=true` (issuer metadata fetched at startup,
  issuer-pinned, HTTPS-only `jwks_uri`; cache tuned with
  `WELLOS_OIDC_JWKS_REFRESH_SECS` / `WELLOS_OIDC_JWKS_MIN_REFRESH_SECS`).
  Optional: `WELLOS_OIDC_LEEWAY_SECS`, and MFA policy via
  `WELLOS_OIDC_REQUIRE_MFA` + `WELLOS_OIDC_ACCEPTED_AMR` /
  `WELLOS_OIDC_ACCEPTED_ACR`. The server **fails to start** if dev auth,
  fake providers or synthetic seeding are enabled in staging/production or
  on a build without `dev-fixtures`, if neither dev auth nor OIDC is configured,
  or (outside development) if `DATABASE_URL` or `WELLOS_ALLOWED_ORIGINS`
  is missing. Provision users by inserting `(issuer, subject)` rows in
  `user_identities` (legacy `users.oidc_subject` still matches and is
  migrated lazily); roles are assigned in `role_assignments`. A NULL
  `facility_id` on an assignment grants tenant-wide access only for
  allowlisted administrative/oversight/machine roles; ordinary clinical
  roles need one row per facility.
- **Browser OIDC login (Authorization Code + PKCE)**: register the exact
  BFF callback (`<web origin>/api/auth/oidc/callback`) at the IdP and set
  `WELLOS_OIDC_CLIENT_ID`, `WELLOS_OIDC_REDIRECT_URI`, optionally
  `WELLOS_OIDC_CLIENT_SECRET`, with `WELLOS_OIDC_DISCOVERY=true` (required;
  the authorization/token endpoints come from issuer-pinned metadata; HTTPS
  outside development). Login transactions are single-use rows in
  `login_transactions` (lifetime `WELLOS_OIDC_LOGIN_TXN_SECS`, max 600s);
  expired rows are cleaned opportunistically. The sign-in page asks the API
  (`GET /api/v1/auth/providers`) which methods exist and renders only those:
  the OIDC button when an issuer is configured, and synthetic development
  identities (`GET /api/v1/auth/dev/users`) only from a `dev-fixtures` build
  running with `WELLOS_ENV=development|test` and `WELLOS_DEV_AUTH=true`.
  No client-side variable can enable development sign-in.
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
- **Rate limits**: shared fixed-window counters in `rate_limit_windows`
  (per-minute, atomic across replicas): `WELLOS_RATE_LOGIN_PER_MIN`
  (default 10, per hashed client address; list the BFF/reverse-proxy peer
  IPs in `WELLOS_TRUSTED_PROXIES` so their asserted client address —
  `x-wellos-client-address` or the rightmost `x-forwarded-for` entry — is
  honored; set `WELLOS_WEB_BEHIND_TRUSTED_PROXY=true` on the web app only
  when a trusted platform proxy fronts it),
  `WELLOS_RATE_SEARCH_PER_MIN` (30), `WELLOS_RATE_CRED_ADMIN_PER_MIN` (30),
  `WELLOS_RATE_API_PER_MIN` (600, per tenant+principal),
  `WELLOS_RATE_VISIT_CREATE_PER_MIN` (30, per tenant+principal; visit and
  appointment registration). Exhaustion returns
  429 with `Retry-After`; if PostgreSQL is unreachable the limiter fails
  closed. Old windows can be pruned with
  `DELETE FROM rate_limit_windows WHERE window_start < now() - interval '1 hour'`.

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
- **Check AI capability**: `GET /ready` returns `ai_capabilities.model`,
  `.transcription` and `.structured_note`, each with `state`
  (`ready | degraded | disabled | invalid_configuration`), `provider`,
  `model`, `reason`, `external` and `synthetic`, plus
  `transcription_languages`. `degraded` means the real provider is
  configured but recent calls failed; the UI disables only the affected
  action and shows the reason. Nothing is fabricated in any state.
- **Enable a real provider**: set `WELLOS_ALLOW_EXTERNAL_AI=true`,
  `DMIND_MODEL_PROVIDER=openai_compatible` with `DMIND_MODEL_ENDPOINT`,
  `DMIND_MODEL_ALLOWED_HOSTS`, `DMIND_MODEL_NAME`, `DMIND_MODEL_API_KEY`
  (and the `WELLOS_SCRIBE_*` equivalents for transcription), then restart;
  the server validates the endpoint/allowlist at startup and refuses
  `http://`, IP literals, embedded credentials or unlisted hosts outside
  development. Tune `DMIND_MODEL_TIMEOUT_SECS`, `DMIND_MODEL_MAX_RETRIES`,
  `DMIND_MODEL_MAX_RESPONSE_KIB`, `DMIND_MODEL_MAX_CONCURRENCY`,
  `DMIND_QUOTA_TENANT_PER_HOUR` and `DMIND_QUOTA_TASK_PER_HOUR` as needed;
  quota consumption is visible in `ai_executions` (one row per provider
  call, never per reuse). Rotating the key is a restart with the new value;
  keys never appear in logs or error responses.
- **Force AI degradation (tests)**: the fake provider exposes
  `set_unavailable(true)`; in integration tests only.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| `/ready` fails | DB container up? `DATABASE_URL` correct? migrations applied? |
| Startup fails with auth error | `WELLOS_DEV_AUTH=true` outside development/test or on a non-`dev-fixtures` build, or no IdP configured — intentional fail-closed behavior |
| Startup fails with `WELLOS_ENV` / provider / seed error | Missing or unknown `WELLOS_ENV`; `fake` provider or `WELLOS_ALLOW_SYNTHETIC_SEED=true` outside development/test or without `dev-fixtures`; `openai_compatible` without `WELLOS_ALLOW_EXTERNAL_AI=true`, endpoint, allowlist, model or key — intentional |
| `503 ai_disabled` / `scribe_disabled` | Provider is `disabled` or misconfigured; `/ready` shows the reason. The clinical workflow continues without the AI action |
| `503 ai_unavailable` / `scribe_unavailable` | Real provider timed out or failed; retry. Repeated failures mark the capability `degraded` |
| `502 ai_invalid_output` | Model returned schema-invalid or uncited output; rejected, nothing stored |
| `429 ai_quota_exceeded` | Hourly tenant/task quota reached (`DMIND_QUOTA_*`) |
| `403 ai_policy_denied` | External provider configured but tenant policy or patient consent (`ai_external_processing`) does not allow it |
| 401 responses | Dev token (`dev-<seeded username>`) with dev auth enabled, a live `wsk_` service credential, a live `wss_` session, or a valid OIDC JWT with a mapped identity (check MFA policy and JWKS freshness too) |
| 403 responses | Role, scope, or purpose-of-use does not permit the action — see `policy.rs`; denials are audited |
| 404 for a resource you expect | Nonexistent — or belongs to another tenant (cross-tenant probes are indistinguishable by design) |
| 409 on transitions | Stale `version` — refetch the service request |
| AI artifact `unavailable` | Expected degradation path; clinical flow continues |

## Logging

Structured logs to stdout; identifiers only, never clinical payloads. Do not
raise log verbosity in shared environments without checking PHI rules.
