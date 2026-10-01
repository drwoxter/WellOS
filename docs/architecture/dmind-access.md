# dMind Access v1 — intelligent access, real scheduling and patient self-service

Status: implemented (migration `0015_dmind_access.sql`). Replaces the
staff-created "scheduled visit" approximation introduced with the access and
triage MVP (`docs/architecture/patient-access-and-triage.md`).

dMind Access helps patients, representatives and staff find the best **valid**
clinical resource. Deterministic rules are authoritative at every step; dMind
interprets, ranks and explains. No AI operation can invent an appointment, a
professional, a facility, a capability, a time, a transport resource or a
clinical fact, and nothing is booked, cancelled or dispatched without a human
(patient, representative or staff) decision.

## 1. Architectural boundary: Appointment versus Visit

| Record | Meaning | Owner | Created by |
| --- | --- | --- | --- |
| `appointments` | Authoritative scheduling record: patient, service, modality, facility, start/end, booked resources, status, history | Access | Confirming an offer (patient or staff), staff direct booking, legacy migration |
| `visits` | Operational care episode: arrival, triage, consultation, closure | Access & triage MVP | **Only** by a confirmed appointment (`scheduled` visit) or by walk-in / urgent / remote registration |

Rules enforced in `wellos_server::scheduling` and tested in
`access_scheduling_integration.rs`:

- Confirming an offer books the resources, inserts the appointment and creates
  or updates the linked `scheduled` visit **in one transaction**
  (`visits.appointment_id`, `appointments.visit_id`).
- Offers and temporary holds never create a visit. A hold only inserts
  `resource_bookings(kind='hold', expires_at)` rows.
- Reschedule marks the prior appointment `rescheduled` (`rescheduled_to`
  points forward), frees its bookings, inserts the new confirmed appointment
  and moves the same visit to the new time — the visit keeps its identity and
  history.
- Cancel / fulfil / no-show from the appointment side moves the visit; arrival,
  consultation closure, cancellation and no-show from the visit side
  (`routes/visits.rs`, encounter sign/cancel) move the appointment — always in
  the same transaction, with `appointment_history` rows and audit.
- `AppointmentStatus::occupies_slot()` is true only for `confirmed`; every
  other status is history. The patient-facing "Upcoming" list, the patient
  busy-time check in the matcher, cancellation recovery and capacity counting
  all use that single definition.

### Legacy migration

Section 14 of `0015` migrates every pre-existing `visits.arrival_kind='scheduled'`
row with `scheduled_at` into one confirmed (or `fulfilled` / `cancelled` /
`no_show`, derived from the visit status) appointment, links both records,
and writes an `appointment_history` row with
`reason_code='migrated_from_scheduled_visit'` (actor `system:migration-0015`).
It is idempotent (`WHERE appointment_id IS NULL`), creates no duplicate visits
and deletes nothing; `legacy_scheduled_visits_migrate_once_into_appointments`
proves a second run is a no-op. Like every WellOS migration it is
forward-only in CI; the manual rollback statements are documented at the end
of the file.

## 2. Open catalogs (`catalog_entries`)

Tenant-scoped, versioned, bilingual catalogs replace the closed `SERVICES`
list. Kinds: `clinical_service`, `specialty`, `profession`, `modality`,
`resource_type`, `accessibility_capability`, `location`, `transport_resource`
(emergency-response vehicles and teams are `transport_resource` /
`resource_type` entries with `emergency=true` on the transport request, not a
separate enum). Every entry has a stable code
(`^[a-z0-9][a-z0-9_.-]{0,63}$`), optional parent, `name_en` / `name_es`,
synonyms, external codings, kind-specific validated `config` (service
duration, buffers, preparation instructions per language, explicit age
restrictions, required resource types; location coordinates and service
area), `active` flag, effective dates, `version`, facility availability
(`catalog_entry_facilities`) and append-only `catalog_entry_history`.
Deactivation is the only "delete".

Specialties, professions and resource types are **data**: a clinical
administrator adds a previously unknown specialty through
`POST /api/v1/catalog` (or the `/scheduling/catalog` screen) and it becomes
matchable immediately (`unknown_specialty_added_at_runtime_becomes_schedulable`).
Catalog membership grants **no** permission: authorization remains the
functional-role matrix in `policy.rs` plus facility and care-team
relationships (`H-31`).

## 3. Schedulable resources and availability

`schedulable_resources` represent professionals, teams, rooms, dental chairs,
procedure rooms, laboratory stations, imaging equipment, rehabilitation
spaces, telehealth channels, home-visit teams, vehicles, accessible transport,
ambulances and tenant-defined types. Each resource has a facility, an IANA
time zone, capacity (`slots`), capability codes, deliverable service codes,
languages and accessibility capabilities. Services declare duration,
preparation/cleanup buffers and the resource types that must be booked
together (`service_resource_requirements`); per-resource deliverable
services live in `resource_services`.

Availability = weekly rules (`resource_availability_rules`, `available` or
`break`, local wall-clock time) minus exceptions
(`resource_exceptions`: leave, sickness, closure, temporary capacity change)
minus facility closures from the operational calendar. Local times are
projected to UTC per day, so DST transitions keep local wall time
(`dst_transition_keeps_local_wall_time`).

Double booking is prevented at database level:

```sql
resource_bookings ... EXCLUDE USING gist (
    resource_id WITH =, slot_index WITH =,
    tstzrange(starts_at, ends_at, '[)') WITH &&) WHERE (status = 'active')
```

Holds and appointments share this table, so a hold blocks a concurrent
confirmation and vice versa; expired holds are released by the scheduling
worker. One partial unique index also prevents two live appointments for the
same patient at the same start time. Everything the constraint cannot express
(multi-resource combinations, patient busy time, policy) is checked inside
the booking transaction with row locks.

## 4. State machines (`wellos_domain::access`)

Each machine is a versioned, exhaustive `apply(transition)` function
(`access-request.v1`, `appointment-offer.v1`, `appointment.v1`); anything
not listed is rejected with `409 invalid_transition`. A stale optimistic
`version` is `409 stale_version` and a reused `Idempotency-Key` with a
different body is `409 idempotency_conflict`.

**Access request** — `draft`, `submitted`, `needs_clinical_triage`,
`options_ready`, `booked`, `closed`, `withdrawn`

| From | Transition | To |
| --- | --- | --- |
| `draft` | `Submit` | `submitted` |
| `submitted`, `options_ready` | `RouteToTriage` (deterministic red flag or staff; dMind may only suggest) | `needs_clinical_triage` |
| `needs_clinical_triage` | `TriageCleared` | `submitted` |
| `submitted`, `options_ready` | `OptionsGenerated` (matcher run) | `options_ready` |
| `submitted`, `options_ready`, `needs_clinical_triage` | `Amend` (answers supplied; options regenerate) | `submitted` |
| `submitted`, `options_ready` | `Book` | `booked` |
| `submitted`, `needs_clinical_triage`, `options_ready`, `booked` | `Close` | `closed` |
| `draft`, `submitted`, `needs_clinical_triage`, `options_ready` | `Withdraw` | `withdrawn` |

**Appointment offer** — `offered`, `held`, `accepted`, `declined`,
`expired`, `revoked`

| From | Transition | To |
| --- | --- | --- |
| `offered` | `Hold` (atomic `resource_bookings kind='hold'`, `hold_expires_at` from policy `hold_minutes`) | `held` |
| `held` | `ReleaseHold` (patient release or hold expiry; the offer stays valid until its own expiry) | `offered` |
| `held` | `Accept` (books, confirms, creates/updates the visit) | `accepted` |
| `offered`, `held` | `Decline` | `declined` |
| `offered`, `held` | `Expire` | `expired` |
| `offered`, `held` | `Revoke` (new matcher run, staff, booked elsewhere) | `revoked` |

A hold belongs to the principal that placed it; only `offered`/`held` offers
are live.

**Appointment** — `confirmed`, `rescheduled`, `cancelled`, `fulfilled`,
`no_show`. Only `confirmed` moves: `Reschedule → rescheduled`
(`rescheduled_to` → the new confirmed appointment), `Cancel → cancelled`,
`Fulfil → fulfilled` (consultation signed), `MarkNoShow → no_show`. Every move
appends `appointment_history` (previous time and status, reason code, actor,
override reason) and an audit record.

**Waitlist entry** — `active ⇄ paused` (`Pause`/`Resume`), `active →
offered` (`Offer`), `offered → active` (`OfferClosed`: declined or expired),
`offered → fulfilled` (`Fulfil`), `active | paused | offered → left`
(`Leave`).

**Cancellation recovery event** — `open → offered` (one live offer at a
time; a declined or expired offer clears `current_offer_id` and the event
immediately advances to the next eligible candidate), `→ filled` on
acceptance, `→ exhausted` when no eligible candidate remains or
`MAX_OFFERS_PER_EVENT` is reached, `→ closed` by staff, when the slot is too
close to offer, or when it is rebooked another way.

**Transport request** (`transport-request.v1`) — `requested → scheduled →
en_route → picked_up → completed`; `cancelled` from `requested`, `scheduled`
or `en_route` (not after pickup), `failed` from any live state; live location
is accepted only in `scheduled`, `en_route` and `picked_up`.

## 5. Deterministic matching — `access-matcher.v1` (`wellos_domain::matcher`)

Input: a frozen `MatcherFacts` snapshot (request constraints, service
configuration, resources with rules/exceptions/bookings, facility hours and
coordinates, patient busy intervals from imported calendars and other
appointments, patient preferences, continuity team, policy). The snapshot is
hashed (`facts_hash`) and persisted with the run (`matcher_runs`).

Hard constraints (candidate is not produced at all): service capability,
required resource combination, facility and modality, duration + buffers,
availability and exceptions, explicit service age restriction, accessibility
capabilities, language, earliest/latest window, referral/preparation
prerequisites, patient availability and personal-calendar busy intervals,
travel feasibility (only with location consent), continuity when requested,
tenant policy (horizon, minimum notice). Rejections are counted per reason
and stored.

Soft factors produce a transparent score decomposition per candidate:
urgency (set by deterministic or human triage, never by the matcher),
waiting time, preferred days/times, travel burden, continuity, utilisation,
cancellation-gap fill, fair waitlist position, seasonal demand, confirmation
history. No-show history can only **add** supportive actions (extra reminder,
closer option); `no_show_history_never_lowers_score` is a unit test. Protected
characteristics are not inputs.

Output: `appointment_offers` with `candidate_id`, resources, `score`,
`score_factors`, `travel` (with provenance), `reasons`; the run stores the
version, facts hash, rejection summary and the candidate list dMind may see.

## 6. dMind Access operations (governed gateway, no parallel path)

Four typed operations extend `ModelGateway` (`dmind_gateway::access`) and the
AIArtifact lifecycle (`aigov::plan` with typed reuse scopes bound through
`ai_artifacts.access_request_id`, `matcher_run_id`,
`cancellation_event_id`, `capacity_forecast_id`):

| Operation | Input | Output constraints | Reuse scope |
| --- | --- | --- | --- |
| `access-intent.v1` | patient/staff free text + catalog codes | structured constraints using **supplied codes only**, missing-information questions, `needs_clinical_triage` flag; cannot set urgency or clear the deterministic red-flag floor | access request |
| `appointment-ranking.v1` | at most `MAX_RANKING_CANDIDATES` deterministic candidates (reduced first) | a **permutation** of the supplied candidate IDs with cited facts; cannot add, drop or edit a candidate | matcher run |
| `cancellation-recovery.v1` | eligible, consented waitlist entries | an order covering every entry exactly once; urgency and waiting-time floors are re-applied deterministically afterwards | cancellation event |
| `capacity-explanation.v1` | a `capacity-forecast.v1` result | explanation bound to the forecast dates and factor categories; no schedule or staffing change | forecast |

All outputs pass schema validation, evidence validation and provenance
recording; quotas and token usage are shared with the rest of dMind. One
bounded request per run — never one call per slot.

AI disabled, degraded or over quota: the matcher result is used as is, the
API reports `ranking.mode = "deterministic"` with the reason, the UI says so,
and holding/confirming is unaffected
(`matching_stays_deterministic_when_dmind_is_disabled_or_degraded`). Nothing
synthetic replaces a missing provider outside `dev-fixtures` test mode.

## 7. Personal calendars (`wellos_domain::ics`, `/api/v1/me/calendars`)

- **Import** (`POST /me/calendars/ics`): RFC 5545 parsed in memory; only
  normalised busy intervals (`patient_busy_intervals`), time zone, source
  type and a SHA-256 integrity hash are stored. Titles, descriptions,
  attendees, locations, URLs and the raw file are never written. Bounds:
  1 MB, 200 000 lines, 2 000 events, 400 occurrences per event, 5 000
  intervals, 180-day horizon; malformed lines are skipped, not fatal.
- **Device free/busy sync** (`POST /me/calendars/device-sync`): the same
  interval model for mobile clients.
- **Disconnect** deletes the connection and all derived intervals.
- **Export**: `GET /me/appointments/{id}/ics` and the staff equivalent emit
  a minimal VEVENT (service, facility, time) — no diagnoses or notes.
- Requires `scheduling_calendar` consent; connect, sync and disconnect are
  audited. No Google / Microsoft buttons exist.

## 8. Patient and representative self-service

`patient_access_grants` link an authenticated user (OIDC identity,
`patient_representative` role) to a patient with relationship `self`,
`parent_guardian` or `authorized_proxy`, optional expiry, revocation and
staff verification (audited). `/api/v1/me/...` derives the patient from the
caller's **active grants**; `patient_id` in a request selects among them and
anything else is `404` (anti-enumeration). No name/birth-date/MRN claim path
exists. The `patient_representative` role has `patient.self_service` only —
no chart, notes, results or risk.

`/my/appointments` (mobile-first, EN/ES): dependant switcher, upcoming and
history, request → answer questions → ranked options with a concise
explanation → hold → confirm, reschedule, cancel within policy, waitlist
join/pause/leave, availability and notification preferences, calendar
import/disconnect, accessibility and transport support, ICS download.

## 9. Cancellation recovery and waitlist (`wellos_server::recovery`)

A cancellation inserts a `cancellation_events` row and the scheduling worker
computes eligibility deterministically (`wellos_domain::recovery`): same
service and facility/modality preferences, acceptable days/times, minimum
notice, calendar conflicts, `waitlist_offers` consent (recorded when the
patient joins), not paused.
Ordering is urgency, then waiting time, then preference fit; dMind may
reorder **within** the floors (`floors_reject_demotion_of_urgent_or_long_waiters`).
The top candidate receives a time-limited offer (notification
`waitlist_offer`); acceptance is an atomic hold + confirmation
(`concurrent_acceptance_of_one_waitlist_offer_books_exactly_once`); decline or
expiry advances to the next candidate until `exhausted`. Staff may override
the order only with a reason, and only inside their scope. Every offer,
expiry, acceptance, override and lost race is audited. The flow is identical
with AI disabled.

## 10. Notifications (`wellos_server::notify`)

Durable `notifications` rows (kinds: booking confirmation, reschedule,
cancellation, reminder, preparation, confirmation request, confirmation
follow-up, waitlist offer / expired, transport status, no-response follow-up)
with a per-kind idempotency key, patient language and time zone, channel
preferences and consent. The worker claims due rows with
`FOR UPDATE SKIP LOCKED` (safe across replicas), respects quiet hours, retries
with bounded backoff (`WELLOS_NOTIFICATION_*`) and parks exhausted rows in
status `dead` (audit `notification.dead_lettered`); each attempt is recorded
in `notification_deliveries`. Channels: in-app (always), SMTP and signed webhook adapters —
both **disabled by default**, host-allowlisted, with PHI-minimised subjects
and payloads (identifiers and template parameters only). The development sink
exists only in `dev-fixtures` builds and refuses to start outside
development/test.

## 11. Capacity forecasting — `capacity-forecast.v1` (`wellos_domain::capacity`)

Inputs: historical demand per service/facility/day, confirmed appointments,
cancellations and lead time, no-shows, resource exceptions, weekday and
month, and the tenant `operational_calendar_events` (`holiday`,
`school_break`, `local_event`, `seasonal_period`, `closure`, each with demand
and capacity multipliers). Spain/Ibiza appear only in synthetic fixtures.
Outputs per day: expected demand, planned capacity, gap/surplus, confidence
and contributing factors, or `insufficient_history` when evidence is
inadequate (`insufficient_history_is_explicit`). Forecasts recommend; they
never cancel care, remove availability or change clinical priority. The staff
capacity panel shows evidence and uncertainty, with the optional dMind
explanation.

## 12. Location and transport (`wellos_server::transport`, `crypto`)

Facility coordinates and service areas live in the `location` catalog.
Patient origin is either an approximate area code or one-time coordinates
used in memory for travel estimation (`scheduling_location` consent;
provenance `haversine-urban-estimate.v1`, clearly labelled as an estimate). Transport requests
(`transport_coordination` consent) link to an appointment, book vehicle/team
resources, carry pickup window, status and responsible operator, and send
`transport_status` notifications. Retained addresses and live coordinates
are AES-256-GCM encrypted with the `WELLOS_LOCATION_ENCRYPTION_KEYS` keyring
(rotation-ready, active key id); in staging/production the capability **fails
closed** without a key. Live locations expire (`WELLOS_LIVE_LOCATION_TTL_SECS`)
and are purged by the worker; every read is audited. Transport coordinators
see logistics only. `emergency=true` requires an authorized human coordinator
(`authorized_by` is a database constraint) — dMind never initiates transport.

## 13. Staff console `/scheduling`

Agenda (day/week, resource lanes, filters by service/specialty/profession/
facility/modality), requests, holds, appointments, cancellations and
unfilled capacity, waitlist recovery, capacity panel, transport status,
catalog and resource administration, override reasons, synthetic notice.
Primary action **Find the best appointment** → need and constraints → best
valid options with reasons → hold and confirmation. Confirmed appointments
appear on the existing `/access` arrival board through the linked visit;
there is no second operational state.

## 14. Security summary

Actions `catalog.read/manage`, `resource.manage`, `scheduling.read/manage`,
`patient.self_service`, `patient_grant.manage`, `waitlist.manage`,
`transport.coordinate`, `capacity.review`, `notification.read`; purposes
`operations` (admin), `treatment`/`operations` (scheduling). Tenant,
facility and grant isolation with indistinguishable `404`s; CSRF via the BFF
session; `WELLOS_RATE_SCHEDULING_PER_MIN` on writes; bounded pagination;
capability hints derived server-side; audited reads of calendars, locations
and grants; no PHI in logs, outbox or notification payloads; no browser
storage of appointment, calendar or location data. Threats and controls:
`docs/security/threat-model.md`; hazards H-23 – H-33:
`docs/clinical-safety/hazard-log.md`.
