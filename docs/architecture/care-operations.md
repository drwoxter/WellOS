# Care Operations & Command Center v1 — flow, placement, coordination, theatres, workforce, forecasts

Adds the hospital-operations layer around the clinical loop. Everything that
moves a patient, a bed, a theatre slot or a shift is a deterministic,
human-confirmed state transition; dMind only summarises, ranks already
feasible options, explains blockers and forecasts.

```
urgent arrival → deterministic triage → human admission decision →
placement request → deterministic feasible candidates (care-placement.v1) →
optional dMind explanation/ranking → human hold → human confirmation →
occupied placement → multidisciplinary coordination tasks →
transfer or discharge preparation → human discharge / completion →
placement released → cleaning → available capacity →
clinician-approved patient notification
```

## Runtime hierarchy (migration `0019`)

`facility → department/service area → unit/ward → room/bay → position`
(bed, chair, theatre, treatment position) lives in `operational_locations`
with bilingual names, catalog-driven `type_code`, specialty/service-line,
profession/capability, age-group, isolation, accessibility and equipment
codes, `room_policy`, capacity, `operational_status`, effective dates and an
optimistic `version`. Only positions carry a `position_state`
(`available | held | occupied | cleaning | blocked | out_of_service`,
`position-state.v1`); every change is recorded in
`operational_location_history` (append-only). Closures, maintenance and
cleaning windows are `operational_location_periods`. Nothing is backfilled
on upgrade: an operations manager configures the hierarchy at runtime, and
catalog or profession membership never grants permissions (central policy,
ADR-0006).

## Episodes, placement, coordination

* `care_episodes` (`care-episode.v1`:
  `planned → active → discharge_preparation → discharge_ready → completed`,
  `cancelled`/`error` terminal) link visits, encounters, appointments,
  diagnostic orders, tasks, alerts and care-team assignments by id only
  (`care_episode_links`) — no clinical content is copied. One open episode
  per type per patient is a database rule. `discharge_ready` is operational
  readiness, never medical clearance; completion requires the live
  placement to be released first.
* `placement_requests` (`placement-request.v1`) hold the requirements; the
  deterministic matcher `care-placement.v1` evaluates every position of the
  facility and persists each candidate's feasibility, score decomposition,
  rejection reasons, source facts and `facts_hash` in
  `placement_candidates`. Hold (`DEFAULT_HOLD_MINUTES` 30, max 240),
  confirmation and transfer re-load the position, the request and the
  candidate inside one transaction with row locks and refuse stale state
  with typed `409` conflicts (`position_unavailable`, `candidate_stale`,
  `version_conflict`). `placements` (`placement.v1`) move
  `held → occupied → released`; release puts the position into `cleaning`
  with the configured turnaround, and `available` is restored by a human
  state change or the cleaning worker tick. A confirmation that finds its
  hold expired (or the facility closed) releases the hold, returns the
  request to `candidates_ready` and commits that release before answering
  `409 hold_expired`/`facility_closed`, so the position is available again
  immediately rather than at the next expiry tick.
* `care_coordination_tasks` (`coordination-task.v1`) require an accountable
  user or queue, carry profession/capability requirements, due and
  escalation times, dependencies, barrier category, patient-visible text
  (separate from staff-only titles), completion evidence, reassignment with
  reason and append-only history. `care_handoffs` are drafts until a human
  accepts them. `care_escalations` (`escalation.v1`) are raised by the
  deterministic tick for overdue boarding, tasks and transport.

## Emergency flow, theatres, workforce, forecasts

* The flow view aggregates arrivals, triage, waiting, boarding, capacity,
  transport and escalation timers per facility from existing access/triage
  rows and the operational tables; it never changes clinical priority.
* `perioperative_cases` (`perioperative-case.v1`:
  `requested → ready_for_scheduling → scheduled → in_theatre → completed`,
  with `delayed`/`cancelled`) carry prerequisites, equipment and team
  capability requirements; `theatre-conflicts.v1` is a deterministic
  readiness/conflict engine over `theatre_sessions`, position state,
  equipment and team shifts. Overrides need a reason and are recorded.
  Scheduling an already `scheduled` case is a `reschedule`: the previous
  theatre booking is released and the new interval booked in one
  transaction, subject to the same conflict engine.
  No anaesthetic record, operative note or medication administration exists.
* `workforce_shifts`, `workforce_coverage_requirements`,
  `workforce_shift_assignments` and `workforce_shift_absences` feed
  `workforce-coverage.v1`, a capability-specific gap engine in the facility's
  IANA zone (DST-safe). Reallocation is a human action; moving an
  assignment to another shift authorises the target shift's facility as well
  as the source's.
* `care-operations-forecast.v1` is deterministic over ≥ 14 days of history
  (`insufficient_history` otherwise) and persists expected demand, capacity,
  gap, uncertainty, source facts and `facts_hash` for 24 h and 7 d horizons;
  outputs are aggregate by default.

## Governed dMind operations

`care-flow-brief.v1`, `placement-ranking.v1`, `discharge-coordination.v1`,
`theatre-plan-explanation.v1`, `operational-forecast-explanation.v1` and
`shift-handoff-draft.v1` run through the model gateway with the payload
whitelist, prompt-boundary policy, facts hash binding, deterministic
degradation and artifact review of ADR-0007/0008. Ranking may only permute
supplied feasible candidate ids; a draft handoff stays a draft until a human
accepts it; nothing here admits, places, transfers, discharges, schedules a
theatre, changes staffing or notifies a patient. Each operation has ≥ 4
bilingual synthetic evaluation cases (`eval-corpus/v1`) and the offline
evaluator must show zero safety failures.

## API, FHIR, web

* REST under `/api/v1/operations/*` (snapshot, locations/state/periods,
  episodes/transitions/links/handoffs, placement requests/candidates/hold/
  confirm/transfer/release, tasks/escalations, theatre sessions/cases,
  workforce shifts/absences/coverage, forecasts, artifact review, jobs tick)
  and `/api/v1/me/care` (patient active-care summary limited to released,
  patient-visible items). Permissions: `care_ops.read`, `manage_flow`,
  `manage_theatre`, `manage_workforce`, `review_ai`, `audit`; roles
  `operations_manager`, `bed_coordinator`, `theatre_coordinator`,
  `workforce_coordinator`.
* FHIR R4 read subset: `EpisodeOfCare`, linked `Encounter`, `Location`
  (hierarchy + v2-0116 bed status), `Task`. Operational readiness, linked
  encounters and coordination tasks travel as `urn:wellos:*` extensions.
  Facade only (ADR-0005): no search, Bundles or conformance claim.
* Web: `/operations` (command center), `/operations/flow`,
  `/operations/theatres`, `/operations/workforce`,
  `/episodes/[id]/coordination`, `/my/care`; EN/ES, 1440/1024/390 px,
  keyboard, 44 px targets, reduced motion, textual chart summaries, loading/
  empty/error/stale/permission-denied states.

## Limitations (honest)

Synthetic fixtures only; matcher weights, escalation timers and forecast
thresholds have no clinical or operational validation; no interface to a
real ADT, theatre or rostering system; forecasts recommend and never act;
no production readiness or regulatory compliance is claimed.
