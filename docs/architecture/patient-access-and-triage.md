# Patient access, arrival, triage and care-team routing

This slice connects patient registration or appointment → arrival → triage →
care-team assignment → internal professional alert → start/resume
consultation, so WellOS behaves as one operational system rather than a set of
separate patient and consultation screens. Everything runs on synthetic data;
the dMind triage assistant is assistive only and is **not clinically validated
for production** (see [Clinical governance before production
use](#clinical-governance-before-production-use)).

## Aggregates (migration `0011_patient_access_triage.sql`)

| Table | Purpose |
| --- | --- |
| `visits` | One planned or actual presentation of a patient at a facility: `arrival_kind` (`scheduled`, `walk_in`, `urgent`, `remote`), requested `service`, `status`, `scheduled_at` / `arrived_at`, free-text `reason`, operational `priority`, `handoff_summary`, `encounter_id` once a consultation exists, optimistic `version`. Exactly one open visit per patient per tenant. |
| `service_queues` | Facility-level destinations (`general_medicine`, `emergency`, `nursing`, `telehealth`) used for routing when no named professional is assigned. |
| `care_team_assignments` | A patient-specific, time-bounded (`starts_at`, `ends_at`, `active`) link between a visit and a named professional and/or a queue. Written by triage completion and reassignment. |
| `triage_assessments` | Append-only structured triage records per visit: concerns, red flags, vital signs (also written to `vital_signs` with `visit_id` and a nullable `encounter_id`), free-text note, deterministic `safety_floor`, `triggered_rules`, `rules_version`, author and time. |
| `internal_alerts` | Directed, in-system notifications (`kind`, `priority`, `target_user_id` xor `target_queue_id`, `open` → `acknowledged` → `resolved`). Alerts never leave WellOS: no SMS, e-mail or push. |
| `ai_artifacts.visit_id` | Triage proposals are A2 `AIArtifact`s bound to a visit and to the exact triage version they were generated from. |

## Visit state machine

Defined once in `crates/wellos-domain/src/triage.rs` (`VisitStatus::apply`)
and enforced by every mutation route inside a `SELECT … FOR UPDATE`
transaction with the client's `version`; a stale version or an invalid
transition returns `409` and the UI reloads the current state.

```text
scheduled ──arrive──▶ arrived ──start_triage──▶ triage_in_progress
    │                    │                             │
    │ no_show            │ cancel                      │ complete_triage
    ▼                    ▼                             ▼
 no_show            cancelled ◀──cancel── ready_for_consultation
                                              │              ▲
                                              │ start        │ release
                                              ▼              │
                                        in_consultation ─────┘
                                              │ complete (encounter signed)
                                              ▼
                                          completed
```

- Walk-in, urgent and remote visits are created directly as `arrived`;
  scheduled visits require `scheduled_at` and are arrived explicitly.
- Registration is bounded so one compromised account cannot flood the
  fixed-size worklists: `scheduled_at` must lie between one hour ago and one
  year ahead (`400 validation_failed`), a patient may hold at most five
  pending appointments (`409 too_many_pending_appointments`; walk-ins are
  never blocked by this cap), and `POST /visits` has its own per-principal
  rate-limit family (`WELLOS_RATE_VISIT_CREATE_PER_MIN`, default 30).
- `cancel` is allowed from every open state before a consultation starts;
  `no_show` only from `scheduled`.
- `in_consultation` is entered only through `POST /visits/:id/start-consultation`,
  which creates or resumes the consultation encounter for the calling
  clinician in the same transaction (patient and visit rows locked). Signing
  the encounter completes the visit; cancelling the encounter releases the
  visit back to `ready_for_consultation`.

## Deterministic triage safety floor

`safety_floor(arrival_kind, red_flags, vitals)` (`triage-safety@1.0.0`)
returns the minimum operational priority and the rules that fired. It is
recomputed server-side on every triage save and completion, stored with the
assessment, and shown to the triage professional. Priority order is
`non_urgent < standard < urgent < immediate`.

| Input | Rule | Floor |
| --- | --- | --- |
| Arrival kind `urgent` | `arrival:urgent` | urgent |
| Red flags (explicit checkboxes, never inferred from free text) | `airway_compromise`, `unresponsive`, `severe_bleeding`, `stroke_signs`, `anaphylaxis_signs` | immediate |
| | `chest_pain`, `severe_breathing_difficulty`, `severe_pain`, `suicidal_ideation`, `pregnancy_complication` | urgent |
| SpO₂ | `< 90 %` / `< 94 %` | immediate / urgent |
| Systolic BP | `< 90 mmHg` / `> 180 mmHg` | immediate / urgent |
| Heart rate | `< 40` or `> 130 bpm` | urgent |
| Respiratory rate | `< 8` or `> 30 /min` | urgent |
| Temperature | `< 35 °C` or `≥ 39.5 °C` | urgent |

Missing values never raise or lower the floor. The professional may choose any
priority **at or above** the floor; the UI disables lower options and the
server rejects them. Structured concerns and free text supplement the record
but never drive a rule.

## dMind triage assistant: human-review boundary

`POST /visits/:id/triage/proposal` asks the dMind gateway (deterministic
offline fake provider by default and in CI) for a `triage-proposal.v1`
artifact: proposed priority and destination service, important facts, missing
information, contradictions, handoff summary, rationale, confidence,
limitations and cited source fields. Boundaries:

- **Floor first.** The proposal is clamped to the deterministic safety floor
  after generation (`clamp_to_floor`); `raised_to_floor` records when that
  happened. dMind can never lower a rule-based or red-flag priority.
- **Assistive only (A2).** The artifact is bound to the visit and the triage
  version it read. Nothing is applied until the professional explicitly
  accepts or overrides it via `POST …/triage/proposal/:artifact_id/review`; a
  proposal generated from an older triage version is refused as stale.
- **Never autonomous.** dMind does not diagnose, finalise acuity, assign a
  professional, start an encounter or notify a patient. Those remain human
  actions with their own policy checks and audit events.
- **Labelled.** The UI marks the output as an assistive draft that is not
  clinically validated and shows the facts used and the limitations.

## Care team versus system roles

System roles (RBAC) say what a *kind* of user may attempt: registration staff
manage visits, nurses and physicians triage and assign, physicians start
consultations, laboratory professionals see none of the access board. The
policy matrix is in `crates/wellos-server/src/policy.rs` and every route goes
through the central `guard` (authentication → tenant → RBAC → service scopes →
purpose-of-use → facility scope → care relationship).

Care-team membership is *patient-specific* and separate from roles:

- A `care_team_assignments` row (or an encounter as practitioner) is what
  establishes a care relationship for reading a patient's chart. Holding the
  `physician` role does not make someone part of a patient's care team.
- Triage completion or reassignment writes the assignment; starting the
  consultation creates the encounter relationship. Neither is inferred from
  role alone.
- Facility scope still applies: clinical roles act only within their assigned
  facilities, and a visit can only be routed to queues and professionals of
  its own facility.

Frontend capability hints (`can_arrive`, `can_triage`, `can_start_consultation`,
`can_resume_consultation`, …) are computed server-side per visit and only
decide which controls are drawn; the server re-authorises every action.

## Notification routing (internal alerts)

- An `urgent` arrival immediately raises an `urgent_arrival` alert to the
  requested service's queue; it is superseded when triage completes.
- Completing triage raises one `patient_ready` alert for the visit's target:
  the named professional if one was assigned, otherwise the facility queue of
  the requested service. Reassigning a waiting patient resolves the earlier
  alert and raises a new one for the new target, so a re-route never leaves a
  stale item.
- `GET /api/v1/alerts` returns unresolved alerts addressed to the caller
  personally plus queue alerts for queues the caller may serve (nursing queues
  for triage roles, clinical queues for roles allowed to start encounters) in
  facilities within their scope, ordered by priority then age. Visibility is
  part of the query itself, so the 100-item cap is applied to alerts the
  caller may see, never to a broader candidate set.
- Acknowledging (`POST /alerts/:id/acknowledge`) is audited and requires the
  alert to be visible to the caller. The lookup applies the same visibility
  predicate, so an alert outside the caller's scope answers `404 not_found`
  exactly like an unknown id, before any patient or facility data is read.
  Alerts resolve automatically when the consultation starts or the visit
  closes.
- Alerts are internal professional notifications only. Patient-facing
  notification is a separate, deliberately unimplemented capability.

## API (`/api/v1`)

All routes pass through the central policy guard; mutations take the current
`version` and return the new one; a stale version or invalid transition is
`409`. Not-found and forbidden responses follow the existing anti-probing
conventions.

| Route | Action | Purpose |
| --- | --- | --- |
| `GET /visits?view=access\|triage\|ready\|closed\|all[&facility_id]` | `visit.read` | Today's board for the caller's facilities (appointments within ±24 h, closures in the last 24 h; later appointments are reachable from the patient chart) with per-visit capability hints |
| `POST /visits` | `visit.manage` | Register an appointment (`scheduled` + `scheduled_at`) or a walk-in / urgent / remote arrival |
| `GET /visits/:id` | `visit.read` | Visit detail with patient safety data, previous vitals, latest triage, proposal and routing |
| `POST /visits/:id/arrive` · `/cancel` · `/no-show` | `visit.manage` | Arrival and closure transitions |
| `POST /visits/:id/triage` | `triage.write` | Save/replace the triage assessment; recomputes and returns the safety floor |
| `POST /visits/:id/triage/proposal` | `triage.write` | Ask dMind for a `triage-proposal.v1` artifact (A2, floor-clamped) |
| `POST /visits/:id/triage/proposal/:artifact_id/review` | `triage.write` | Explicit accept / override / reject of the proposal; stale versions refused |
| `POST /visits/:id/triage/complete` | `triage.write` | Set priority (≥ floor), requested service, optional named professional, handoff summary; routes and alerts |
| `POST /visits/:id/assign` | `care_team.assign` | Re-route a waiting patient to another professional or queue |
| `POST /visits/:id/start-consultation` | `encounter.start` | Create or resume the consultation encounter for the caller; returns `encounter_id` |
| `GET /alerts` | `visit.read` | Unresolved internal alerts addressed to the caller or their queues |
| `POST /alerts/:id/acknowledge` | `alert.acknowledge` | Audited acknowledgement |

## Screens

- `/access` — access board (Arrivals / Triage / Ready / Closed today tabs),
  registration of appointments, walk-ins, urgent and remote arrivals with
  human-readable patient search, arrive/cancel/no-show with confirmation,
  facility filter when the user has several facilities.
- `/visits/[id]/triage` — triage workspace: patient safety header, previous
  vitals, structured concerns, red flags, vitals with usual-range confirmation,
  live safety floor, dMind proposal panel, priority (floor-limited), requested
  service, named professional, handoff summary, complete.
- `/dashboard` — role-aware widgets on the existing cockpit: today's
  appointments and arrivals, triage queue, ready for consultation (start /
  resume from the card), alerts for you.
- `/patients/[id]` — today's visit card with the same actions.
- `/encounters/[id]` — read-only "Arrival and triage" handoff card (arrival
  kind, service, visit status, times, reason, concerns, red flags, safety
  floor, triggered rules, triage note, handoff summary).

## Clinical governance before production use

The triage rules and the dMind assistant are engineering safeguards, not a
validated triage instrument. Before any real-patient use the following is
required at minimum: adoption or licensing of a recognised triage scale and
mapping of `Priority` to it; clinical review and sign-off of every threshold
in `triage-safety@1.0.0` with a versioned change process; prospective
evaluation of the dMind proposals against clinician decisions (accuracy,
under-triage rate, bias across age/sex/language); human-factors testing of the
floor and override UI; documented escalation paths for `immediate`; and
inclusion of the hazards `H-13`–`H-17` in the safety case
(`docs/clinical-safety/hazard-log.md`).

## Limitations and deferred capabilities

- No appointment book: "appointment" here is a scheduled visit for today
  created by staff; no slots, calendars, reminders or patient self-service.
- No patient-facing notifications (SMS/e-mail/push), no waiting-room displays,
  no paging integration; alerts are visible only inside WellOS.
- Queues are static facility destinations; no load balancing, capacity or
  escalation timers on waiting patients.
- Triage vitals are entered manually; no device integration.
- Care-team assignments are written by triage routing only; there is no
  general care-team management UI and they do not yet extend
  notification-of-patient permissions (still encounter-practitioner only).
- The triage scale is an internal four-level operational priority, not
  Manchester/ESI/CTAS; no reassessment timers or re-triage workflow.
- One open visit per patient at a time; no multi-day admissions or bed
  management.
