# WellOS

WellOS is an AI-native hospital operating system in early development. dMind is
its governed clinical, operational, research, and machine-learning intelligence
layer.

**Status: development prototype.** Everything in this repository uses synthetic
data only. Nothing here is clinically validated, certified as a medical device,
or approved for use with real patients. See [Limitations](#limitations).

## What is implemented

A single end-to-end clinical vertical slice, **Closed-Loop Diagnostic Result**,
proving that the core architecture, AI governance, access control, consent, and
audit work together:

1. Registration staff registers a synthetic patient; a clinician opens an
   encounter and orders a laboratory test (ServiceRequest).
2. A synthetic laboratory adapter delivers an Observation (idempotent; amended
   results preserve history).
3. A deterministic, versioned rule evaluates criticality (never AI).
4. Critical results create a high-priority alert and a follow-up task.
5. dMind generates a structured, assistive summary (AIArtifact, autonomy A2)
   with cited source facts, limitations, and suggested next-step categories.
   AI never orders, prescribes, diagnoses, or changes treatment.
6. The clinician reviews, records patient notification, and closes the loop.
   Every access, rule execution, AI generation, and transition is audited.

## Repository layout

| Path | Purpose |
| --- | --- |
| `crates/wellos-domain` | Pure domain logic: typed IDs, units, deterministic rules, loop state machine, AIArtifact lifecycle, event envelopes |
| `crates/dmind-gateway` | Provider-neutral model gateway + deterministic offline fake provider |
| `crates/wellos-server` | Axum HTTP API, PostgreSQL persistence, policy engine, audit, outbox, FHIR facade |
| `apps/web` | Next.js clinician UI (EN/ES, two themes) |
| `docs/` | Architecture, ADRs, clinical safety, security, compliance, operations |
| `infra/` | docker-compose for local PostgreSQL |

## Quick start

Prerequisites: Rust (stable ≥ 1.85), Node 20+, Docker.

```bash
cp .env.example .env
make up        # start PostgreSQL 16 in Docker
make migrate   # apply SQL migrations
make seed      # load synthetic demo data (two tenants)
make server    # run the API on :8080
make web       # run the clinician UI on :3000 (separate shell)
```

Sign in at http://localhost:3000 with a development token such as
`dev-dr.garcia` (physician), `dev-reg.rivera` (registration),
`dev-nurse.kim` (nurse), `dev-lab.chen` (laboratory),
`dev-privacy.wolf` (privacy officer). Development tokens work only against
seeded synthetic users and only when `WELLOS_ENV=development` and
`WELLOS_DEV_AUTH=true` (the server refuses to start with dev auth enabled in
any other environment). On sign-in the Next.js BFF exchanges the credential
for an opaque server-side session (`wss_`, stored hashed in PostgreSQL with
absolute + inactivity timeouts, rotation and logout revocation) held in an
HttpOnly cookie, plus a CSRF cookie for state-changing requests; access
tokens are never exposed to browser JavaScript, and signing out revokes the
server-side session.

Production human identity uses OIDC/OAuth 2.1: configure
`WELLOS_OIDC_ISSUER` and `WELLOS_OIDC_AUDIENCE`, then either a static JWKS
(`WELLOS_OIDC_JWKS_JSON`/`_PATH`) or OIDC discovery
(`WELLOS_OIDC_DISCOVERY=true`, with pinned issuer, HTTPS-only JWKS URI, and
a cached, auto-refreshing key set). The validated `(issuer, sub)` pair maps
to a local user, optional MFA enforcement reads validated `amr`/`acr`
claims (`WELLOS_OIDC_REQUIRE_MFA`), and tenant/roles are resolved only from
the database. Machines authenticate with hashed, scoped, expiring,
revocable `wsk_` service credentials (seeded for development, printed once
by `make seed`) administered via `/api/v1/admin/service-credentials`. See
`SECURITY.md` and `docs/operations/runbook.md`.

## Tests

```bash
make lint               # cargo fmt --check, clippy -D warnings, next lint
make test               # unit tests (domain rules, state machine, policy, gateway)
make test-integration   # API integration tests (requires running PostgreSQL)
```

## Limitations

- Synthetic data only; no real PHI anywhere (code, fixtures, tests, logs).
- No external identity provider is bundled; OIDC discovery/JWKS refresh only
  contacts the issuer you explicitly configure.
- Role assignments are enforced tenant-wide, not facility-scoped (documented
  in SECURITY.md).
- This remains a development system: no production deployment, compliance or
  clinical claims.
- The FHIR R4 endpoints are a minimal read-only facade, not a FHIR server.
- The AI provider is a deterministic offline fake; no external AI calls.
- No claims of HIPAA/GDPR compliance, clinical validation, or device
  certification are made or implied.
- Not production-deployable: no TLS termination, HA, or backup automation here.

See `docs/` for the architecture, decision records, clinical safety case
outline, threat model, and roadmap.
