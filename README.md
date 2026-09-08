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
| `/dashboard` | Consultation cockpit: prominent **Start consultation** (patient search → create or resume), role-aware widgets for patients ready for consultation (start/resume), alerts for you, the triage queue and today's appointments/arrivals, plus draft consultations, patients needing attention, critical/pending results, pending tasks and recent dMind activity (show/hide, reorder, density; layout-only browser storage) |
| `/access` | Access board: today's appointments and arrivals, walk-in / urgent / remote registration, mark arrived, cancel, no-show; Triage, Ready and Closed tabs by role |
| `/visits/[id]/triage` | Triage workspace: safety header, previous vitals, structured concerns, red flags, vital signs, deterministic safety floor, dMind triage proposal (assistive), priority, requested service, named professional, handoff summary, complete |
| `/patients` | Patient directory: search by name or identifier, register a patient |
| `/patients/[id]` | Patient workspace: demographics, allergies/alerts, tabs, clinical timeline, recent vital trends, today's visit (arrive / triage / start), start/resume consultation, order laboratory test |
| `/encounters/[id]` | Consultation workspace: patient safety header, read-only arrival & triage handoff, sticky recording dock (consent → record → pause/resume → finish/discard → transcript + structured dMind scribe draft), Patient Brief, vital signs (validated, BMI), structured clinical note, diagnoses, laboratory order, dMind documentation aid, diagnostic history with deterministic trend commentary, draft save, sign-and-complete, addenda on signed notes |
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

### AI scribe demo (deterministic, offline)

1. Sign in as **Dr. García**, click **Start consultation** on the dashboard,
   search “Jonás” and choose **Start consultation** (or **Resume
   consultation** if a draft already exists).
2. In the recording dock, press **Record consultation**, confirm **Patient
   consented — start recording** (the consent is audited) and grant the
   browser microphone permission. Pause/resume as you like, then **Finish**.
   What you say is irrelevant: with the default `WELLOS_SCRIBE_PROVIDER=fake`
   the server returns the same synthetic transcript for any recording of a
   given duration and language, so the demo is reproducible and no audio
   ever leaves the machine.
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
- The AI provider is a deterministic offline fake; no external AI calls by
  default. The optional OpenAI-compatible transcription adapter is opt-in and
  not exercised in CI (a mocked HTTP server covers its contract).
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
- Completing the demo workflow mutates the seed data; use `make reset` to
  restore the demo states.

See `docs/` for the architecture, decision records, clinical safety case
outline, threat model, and roadmap.
