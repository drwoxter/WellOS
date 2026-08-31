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

`make reset` drops all data and reloads the synthetic demo dataset (useful
after completing the demo workflow, which closes the seeded critical loop).

### Demo sign-in and screens

Open http://localhost:3000 and pick a demo role card (development builds
only): **Dr. García** (physician), **Nurse Kim** (nurse), **Reg. Rivera**
(registration staff) or **Privacy Officer Wolf**. The cards use the seeded
synthetic users' development tokens (`dev-<username>`) under the hood; no
token needs to be typed.

| URL | Screen |
| --- | --- |
| `/dashboard` | Home: facility context, workload counts, prioritized pending results, quick actions |
| `/patients` | Patient directory: search by name or identifier, register a patient |
| `/patients/[id]` | Patient workspace: demographics, allergies/alerts, tabs, clinical timeline, recent vital trends, start/resume consultation, order laboratory test |
| `/encounters/[id]` | Consultation workspace: patient safety header, vital signs (validated, BMI), structured clinical note, diagnoses, laboratory order, dMind documentation aid, draft save, sign-and-complete, addenda on signed notes |
| `/results` | Results worklist: priority-first, criticality/state filters, patient search (`/worklist` redirects here) |
| `/requests/[id]` | Result detail: workflow stepper, critical banner, deterministic rule evaluation, advisory dMind summary, review → notification → closure |

The seed includes a critical potassium result awaiting review (Carlos
Demopatient), a reviewed glucose result awaiting patient notification (Marta
Demopatient) and a closed potassium loop (Jonás Demopatient), plus patients
with encounters, allergies, medications and laboratory history. For
consultation documentation it also seeds an in-progress draft consultation
(Alba Demopatient), a signed encounter with vital signs, a diagnosis and a
plan (Carlos Demopatient), a signed encounter with a later addendum (Marta
Demopatient) and a patient ready for a fresh consultation (Jonás
Demopatient). `make reset` restores all demo states.

Development tokens work only against
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
by `make seed`) administered via `/api/v1/admin/service-credentials`.

Browser login in production uses OIDC Authorization Code + PKCE (S256)
through the BFF: set `WELLOS_OIDC_CLIENT_ID`, the exact
`WELLOS_OIDC_REDIRECT_URI` (the BFF callback, e.g.
`https://app.example.org/api/auth/oidc/callback`), optionally
`WELLOS_OIDC_CLIENT_SECRET`, and `WELLOS_OIDC_DISCOVERY=true` so the
authorization/token endpoints come from validated, issuer-pinned metadata.
Login state lives in server-side single-use transactions (≤ 10 minutes);
the browser only ever receives the opaque session and CSRF cookies. Local
logout always revokes the WellOS session; provider logout is optional via a
discovery-validated end-session endpoint.

Authorization is facility-scoped: clinicians act only within their assigned
facilities (seeded: `dev-dr.garcia` at both tenant-A facilities,
`dev-dr.annex` at North Annex only), while allowlisted
administrative/oversight roles may hold tenant-wide (NULL-facility)
assignments. Shared PostgreSQL-backed rate limits protect login, patient
search, credential administration and general API traffic
(`WELLOS_RATE_*_PER_MIN`). See `SECURITY.md` and
`docs/operations/runbook.md`.

## Tests

```bash
make lint               # cargo fmt --check, clippy -D warnings, next lint
make test               # unit tests (domain rules, state machine, policy, gateway)
make test-integration   # API integration tests (requires running PostgreSQL)
```

Frontend tests (from `apps/web`):

```bash
npm run test       # component tests (Vitest + Testing Library)
npm run test:e2e   # browser tests (Playwright; requires Postgres, seeds mutated — run `make reset` after)
```

## Limitations

- Synthetic data only; no real PHI anywhere (code, fixtures, tests, logs).
- No external identity provider is bundled; OIDC discovery/JWKS refresh only
  contacts the issuer you explicitly configure.
- User provisioning and role/facility assignment are direct database
  operations; SCIM/IdP-driven provisioning is future work.
- This remains a development system: no production deployment, compliance or
  clinical claims.
- The FHIR R4 endpoints are a minimal read-only facade, not a FHIR server.
- The AI provider is a deterministic offline fake; no external AI calls.
- No claims of HIPAA/GDPR compliance, clinical validation, or device
  certification are made or implied.
- Not production-deployable: no TLS termination, HA, or backup automation here.
- The workspace UI covers the closed-loop result slice only; scheduling,
  documentation, orders beyond the two seeded laboratory tests, and care-team
  based notification permissions are future work.
- Completing the demo workflow mutates the seed data; use `make reset` to
  restore the demo states.

See `docs/` for the architecture, decision records, clinical safety case
outline, threat model, and roadmap.
