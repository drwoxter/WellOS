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

Seed is idempotent-ish for demos but intended for empty databases; to reset:
`docker compose -f infra/docker-compose.yml down -v && make up && make migrate && make seed`.

## Common tasks

- **Apply new migrations**: `make migrate` (runs the `migrate` binary; SQLx
  tracks applied migrations in `_sqlx_migrations`).
- **Run overdue-result escalation**: `POST /api/v1/jobs/escalate-overdue`
  (clinical administrator role) — deterministic; safe to re-run.
- **Inspect audit trail**: `GET /api/v1/audit` as `dev-privacy.wolf` or
  `dev-audit.stone`.
- **Force AI degradation (tests)**: the fake provider exposes
  `set_unavailable(true)`; in integration tests only.

## Troubleshooting

| Symptom | Check |
| --- | --- |
| `/ready` fails | DB container up? `DATABASE_URL` correct? migrations applied? |
| 401 responses | Token must be `dev-<seeded username>`; user must exist in seed |
| 403 responses | Role lacks the action — see `policy.rs`; denials are audited |
| 409 on transitions | Stale `version` — refetch the service request |
| AI artifact `unavailable` | Expected degradation path; clinical flow continues |

## Logging

Structured logs to stdout; identifiers only, never clinical payloads. Do not
raise log verbosity in shared environments without checking PHI rules.
