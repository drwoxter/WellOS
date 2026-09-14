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

WellOS has two explicit local modes. Neither is inherited by the other:

| Mode | Config file | Binary | What it enables |
| --- | --- | --- | --- |
| **Synthetic-fixture mode** (development/test) | `.env.development` from `.env.development.example` | `--features dev-fixtures` | Development sign-in for seeded synthetic users, deterministic offline `fake` AI providers (clearly labelled synthetic), synthetic seed |
| **Real-provider mode** (production-intent) | `.env` from `.env.example` | default features | OIDC only, `disabled` or `openai_compatible` AI providers, no fixtures compiled in |

`WELLOS_ENV` (`development` \| `test` \| `staging` \| `production`) is
required and typed; a missing or unknown value fails startup. In `staging`
and `production` the server refuses `WELLOS_DEV_AUTH=true`, `fake`
providers, `WELLOS_ALLOW_SYNTHETIC_SEED=true` and the development-user
endpoint unconditionally — even on a `dev-fixtures` build.

```bash
# Synthetic-fixture mode
cp .env.development.example .env.development
make up               # start PostgreSQL 16 in Docker
make migrate          # apply SQL migrations (idempotent)
make seed             # SYNTHETIC data: requires WELLOS_ENV=development|test,
                      # WELLOS_ALLOW_SYNTHETIC_SEED=true and a dev-fixtures build
make server-fixtures  # API on :8080 with dev sign-in + fake AI (dev-fixtures build)
make web              # clinician UI on :3000 (separate shell)

# Real-provider mode (no fixtures compiled in; .env must configure OIDC and,
# optionally, openai_compatible providers — see .env.example)
cp .env.example .env
make server
```

`make reset` drops all data and reloads the synthetic dataset (useful after
completing the demo workflow, which closes the seeded critical loop).

### Sign-in and screens

The landing page asks the backend (`GET /api/auth/providers`) which sign-in
methods exist. In synthetic-fixture mode it lists the seeded synthetic users
(served by the backend from the synthetic tenant, never bundled in the
client): **Dr. García** (physician), **Nurse Kim** (nurse), **Reg. Rivera**
(registration staff), **Privacy Officer Wolf** and the others. Everywhere
else only the configured OIDC provider is offered; no development
credentials or placeholder controls are rendered.

| URL | Screen |
| --- | --- |
| `/dashboard` | Consultation cockpit: prominent **Start consultation** (patient search → create or resume), role-aware widgets for patients ready for consultation (start/resume), alerts for you, the triage queue and today's appointments/arrivals, plus draft consultations, patients needing attention, critical/pending results, pending tasks and recent dMind activity (show/hide, reorder, density; layout-only browser storage) |
| `/access` | Access board: today's appointments and arrivals, walk-in / urgent / remote registration, mark arrived, cancel, no-show; Triage, Ready and Closed tabs by role |
| `/visits/[id]/triage` | Triage workspace: safety header, previous vitals, structured concerns, red flags, vital signs, deterministic safety floor, dMind triage proposal (assistive), priority, requested service, named professional, handoff summary, complete |
| `/patients` | Patient directory: search by name or identifier, register a patient |
| `/patients/[id]` | Patient workspace: demographics, allergies/alerts, tabs, clinical timeline, recent vital trends, today's visit (arrive / triage / start), start/resume consultation, order laboratory test |
| `/patients/[id]/360` | Patient 360: identity, care team and responsible professional, alerts, conditions, allergies and medications, recent encounters and notes, pending tasks/referrals/tests/follow-ups, diagnostic trends and abnormal results awaiting review, preventive gaps, explainable risk by domain with expandable technical evidence, risk evolution, dMind risk summary (assistive), start/resume consultation — all on one screen |
| `/risk` | Risk worklist: critical and high first, filters by domain / service / assigned professional / review status / trend, plain-language reason per item, links to Patient 360 and source evidence, acknowledge / assign / mark reviewed (audited) |
| `/encounters/[id]` | Consultation workspace: patient safety header, read-only arrival & triage handoff, “Patient 360 before you start” card (elevated risk domains, alerts, pending items, evidence links, dMind summary; re-read after confirmed changes, never blocks the note), sticky recording dock (consent → record → pause/resume → finish/discard → transcript + structured dMind scribe draft), Patient Brief, vital signs (validated, BMI), structured clinical note, diagnoses, laboratory order, dMind documentation aid, diagnostic history with deterministic trend commentary, draft save, sign-and-complete, addenda on signed notes |
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
Demopatient). Encounters carry an explicit `encounter_type`: `consultation`
holds the clinical note, while `order_only` is the laboratory-order context
used by the result loops (pre-documentation encounters are backfilled to it);
order-only encounters appear in the timeline as “Laboratory orders”, are never
offered as “Resume consultation” and refuse note, vitals, diagnosis, sign and
dMind mutations. For patient access it seeds facility service queues and
today's visits in every state — a scheduled appointment and a remote
appointment, a walk-in awaiting triage, an urgent arrival with an open
emergency-queue alert, a walk-in mid-triage with a pending dMind proposal, a
patient ready for consultation assigned to Dr. García with an open alert, the
in-consultation visit behind Alba's draft encounter and a cancelled
appointment from yesterday. `make reset` restores all demo states.

Development tokens work only against seeded synthetic users, only when
`WELLOS_ENV=development|test` **and** `WELLOS_DEV_AUTH=true`, and only on a
`dev-fixtures` build (the server refuses to start with dev auth enabled in
any other environment; `WELLOS_DEV_AUTH` defaults to `false`). On sign-in the Next.js BFF exchanges the credential
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

### AI scribe demo (deterministic, offline)

1. Sign in as **Dr. García**, click **Start consultation** on the dashboard,
   search “Jonás” and choose **Start consultation** (or **Resume
   consultation** if a draft already exists).
2. In the recording dock, press **Record consultation**, confirm **Patient
   consented — start recording** (the consent is audited) and grant the
   browser microphone permission. Pause/resume as you like, then **Finish**.
   In synthetic-fixture mode (`WELLOS_SCRIBE_PROVIDER=fake`) what you say is
   irrelevant: the server returns the same clearly-labelled synthetic
   transcript for any recording of a given duration and language, so the
   demo is reproducible and no audio ever leaves the machine. With
   `openai_compatible` the actual audio is transcribed by the configured
   provider and the transcript is passed to the structured-note dMind
   operation; if either capability is disabled or unavailable the record
   button is disabled with the real reason and nothing is fabricated.
3. Review the transcript (timecoded, with speaker labels and per-segment
   confidence) and the structured draft mapped to the note sections, each
   with confidence, review-needed reasons and contradiction / uncertainty
   flags.
4. **Insert into empty section** per section, or **Insert all into empty
   sections**; text already typed is never overwritten — the only
   alternative for a filled section is an explicit **Append below my text**.
   Edit, save and sign through the normal note lifecycle. Every applied
   artifact is bound to the exact note version it was reviewed against.

Raw audio is held in server memory only for the duration of the request;
only a hash, size, MIME type and duration are persisted. See
`docs/architecture/ai-scribe.md` for the provider configuration, privacy
boundaries, structured-output contract and failure recovery.

### Patient access and triage demo (registration → nurse → physician)

1. Sign in as **Reg. Rivera**. The access board opens on **Arrivals**: add
   an appointment or walk-in with **New visit** (search the patient by name;
   no identifiers to type), then **Mark arrived** when the patient presents.
   Urgent arrivals immediately alert the emergency queue.
2. Sign in as **Nurse Kim**. The board opens on **Triage**; open the patient
   and record concerns, explicit red flags and vital signs. The deterministic
   **safety floor** (e.g. SpO₂ below 94 % → Urgent) is shown with the rules
   that fired; priorities below it are disabled. Optionally **Ask dMind**: the
   proposal is an assistive draft citing the facts it used and can never lower
   the floor — accept or override it explicitly. Choose the service and,
   optionally, a named professional, write the handoff summary and
   **Complete triage**.
3. Sign in as **Dr. García**. The dashboard shows the internal **Patient
   ready** alert and the **Ready for consultation** card; **Acknowledge**,
   then **Start consultation**. The consultation workspace opens with the
   read-only arrival and triage handoff; sign the note to complete the visit.

See `docs/architecture/patient-access-and-triage.md` for the visit state
machine, the safety rules, the care-team versus system-role distinction and
internal alert routing.

### Patient 360 and explainable risk demo

The seed adds six clearly synthetic `Riskdemo` patients (`SYN-0101` …
`SYN-0106`): Lucía (stable low risk, consented to the insurer projection),
Ramón (worsening chronic complexity), Teresa (critical unreviewed potassium),
Hugo (penicillin allergy with active amoxicillin), Nora (preventive-care
gaps, no responsible professional) and Iván (registered with a treating
professional but no clinical data yet — insufficient data).

1. Sign in as **Dr. García** and open **Risk** in the navigation. Teresa and
   Hugo (▲ Critical) lead, then Ramón (High, worsening) and Nora (High);
   Iván is listed as *Insufficient data* and Lucía only appears with
   **Include low risk**. Every item states *why* in plain language; expand
   **Technical evidence** for rule codes, `risk-rules.v1`, timestamps and
   links to the source records. Filter by domain, service, assignee, review
   status or trend; **Acknowledge**, **Assign follow-up** and **Mark as
   reviewed** are audited and bound to the assessment version shown.
2. Open **Patient 360** for Teresa: identity, care team, alerts, conditions,
   allergies/medications, encounters, pending work, diagnostic trends,
   preventive gaps, risk by domain and risk evolution are on one screen.
   Press **Generate dMind summary**: the proposal is labelled
   AI-generated, explains each domain, cites the records used, lists missing
   or contradictory information and suggests follow-ups. Levels are always
   the deterministic ones — dMind cannot lower a critical signal. **Approve**
   the summary, then **Create follow-up task…** on a suggestion: the task is
   created only after your explicit confirmation.
3. **Start / Resume consultation** from Patient 360. The cockpit shows the
   Patient 360 card before the note; record vitals or sign the note and the
   risk assessment is recalculated in the same transaction without touching
   the consultation.
4. Insurer projection (contract only): as **Carla Silva** (clinical
   administrator) call `GET /api/v1/patients/{id}/risk/projection` with
   `X-Purpose-Of-Use: operations`. Lucía (consented) returns levels,
   versions, review state and record provenance; the other patients return
   the envelope with `risk: null`; every call is audited.

See `docs/architecture/patient-360-and-risk.md` for the rules, contracts,
permissions and non-goals.

#### Windows PowerShell

Equivalent commands for the Makefile targets (run from the repository root,
Rust, Node 20+ and Docker Desktop installed):

```powershell
# Synthetic-fixture mode (development/test only)
Copy-Item .env.development.example .env.development
Get-Content .env.development | Where-Object { $_ -match '^\s*[^#].*=' } | ForEach-Object {
  $name, $value = $_ -split '=', 2
  [Environment]::SetEnvironmentVariable($name.Trim(), $value.Trim(), 'Process')
}
docker compose -f infra/docker-compose.yml up -d                     # PostgreSQL 16
cargo run -p wellos-server --bin migrate                             # migrations (idempotent)
cargo run -p wellos-server --bin seed --features dev-fixtures        # SYNTHETIC data (refused outside development/test)
cargo run -p wellos-server --features dev-fixtures                   # API with dev sign-in + fake AI

# Real-provider mode instead: load .env (OIDC + disabled/openai_compatible providers)
# and run the default build, which has no fixtures compiled in.
#   Get-Content .env | ... (same loop) ; cargo run -p wellos-server

# Web app (second PowerShell window)
Set-Location apps/web; npm install; npm run dev            # http://localhost:3000

# Demo: sign in as Dr. García, then open
Start-Process http://localhost:3000/risk

# Reset the demo dataset after exercising the workflows
docker compose -f infra/docker-compose.yml exec postgres psql -U wellos -d wellos -c 'DROP SCHEMA public CASCADE; CREATE SCHEMA public;'
cargo run -p wellos-server --bin migrate; cargo run -p wellos-server --bin seed --features dev-fixtures

# Insurer projection contract (dev auth; Carla Silva = clinical administrator)
$patientId = '<uuid of Lucía Riskdemo from /patients search>'
Invoke-RestMethod "http://127.0.0.1:8080/api/v1/patients/$patientId/risk/projection" `
  -Headers @{ Authorization = 'Bearer dev-admin.silva'; 'X-Purpose-Of-Use' = 'operations' }
```

## Tests

```bash
make lint               # cargo fmt --check, clippy -D warnings (production feature set), next lint
make lint-fixtures      # clippy with dev-fixtures compiled in
make test               # unit tests (domain rules, state machine, policy, gateway)
make test-integration   # API integration tests in test/fixture mode (requires running PostgreSQL)
```

Complete validation, exactly as CI runs it (Linux; PowerShell users run the
same commands, setting the variables with `$env:WELLOS_ENV = 'test'` etc.):

```bash
cargo fmt --all --check
cargo clippy --workspace --all-targets -- -D warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo build -p wellos-server --bins                     # production build: no fixtures compiled in
cargo test --workspace --lib
export WELLOS_ENV=test WELLOS_ALLOW_SYNTHETIC_SEED=true DMIND_MODEL_PROVIDER=fake WELLOS_SCRIBE_PROVIDER=fake
cargo run -p wellos-server --bin migrate
cargo run -p wellos-server --bin seed --features dev-fixtures
cargo test --workspace --test '*'
cargo audit
gitleaks detect --source . --no-banner --redact
cd apps/web && npm run format:check && npm run lint && npm run typecheck && npm run test && npm run build
```

CI never calls an external AI provider: the `openai_compatible` adapters are
exercised against a controlled local HTTP test server only.

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
- AI providers default to `disabled`. The real `openai_compatible` model and
  transcription adapters are implemented and hardened but have only been
  exercised against a controlled local HTTP test server, never against a
  live vendor from this repository; a real-provider smoke test with
  synthetic patients is documented in `docs/architecture/ai-native-platform.md`.
  The deterministic `fake` providers are development/test fixtures and
  cannot be selected in staging or production.
- The AI scribe is an assistive drafting aid: it cannot diagnose, prescribe,
  order, sign or alter signed records, and its output requires explicit
  clinician review. Speaker labels and confidence come from the provider and
  are not clinically validated.
- No claims of HIPAA/GDPR compliance, clinical validation, or device
  certification are made or implied.
- Not production-deployable: no TLS termination, HA, or backup automation here.
- The workspace UI covers the diagnostic-result loop, patient access and
  triage, and consultation documentation; a real appointment book (slots,
  calendars, reminders), patient-facing notifications, orders beyond the two
  seeded laboratory tests, and care-team based notification permissions are
  future work.
- Triage uses an internal four-level operational priority with deterministic
  safety rules; it is not a validated triage scale (Manchester/ESI/CTAS) and
  the dMind triage proposal is assistive only. Internal alerts stay inside
  WellOS (no SMS, e-mail, push or paging) and have no escalation timers.
- The dashboard cockpit stores only widget layout (order, hidden, density)
  in the browser; no patient or clinical data is ever placed in browser
  storage.
- The risk engine (`risk-rules.v1`) is an explainable prioritisation aid,
  not a validated clinical risk score; it produces domain levels and
  evidence, never a numeric score, and its thresholds await clinical
  sign-off. The dMind risk summary is assistive and cannot change a level.
  The insurer projection is a governed read-only contract for future
  integrations: there is no insurer identity, delivery, pricing,
  underwriting, coverage, authorization or denial logic anywhere in WellOS.
- Completing the demo workflow mutates the seed data; use `make reset` to
  restore the demo states.

See `docs/` for the architecture, decision records, clinical safety case
outline, threat model, and roadmap.
