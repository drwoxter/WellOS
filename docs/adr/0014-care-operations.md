# ADR-0014: Care Operations Are Deterministic, Human-Confirmed State Machines; dMind Advises Only

Status: Accepted · Date: 2026-10-04

## Context

Hospital operations (admission, bed placement, transfer, discharge, theatre
scheduling, staffing) are consequential and contested. A model that
"optimises" them silently would be unsafe, unauditable and impossible to
challenge. WellOS already treats clinical decisions as human-accountable
with deterministic baselines (ADR-0007, ADR-0008, ADR-0013).

## Decision

- Every operational object is a versioned state machine in PostgreSQL
  (`care-episode.v1`, `placement-request.v1`, `placement.v1`,
  `position-state.v1`, `coordination-task.v1`, `perioperative-case.v1`,
  `escalation.v1`) with optimistic `version`, typed `409` conflicts and
  append-only history. Transitions are human actions through authorised
  routes; a worker tick may only expire holds, finish cleaning and raise
  escalations.
- Feasibility is deterministic and authoritative: `care-placement.v1`,
  `theatre-conflicts.v1`, `workforce-coverage.v1` and
  `care-operations-forecast.v1` persist their inputs, decomposition and
  `facts_hash`. Hold, confirmation and transfer revalidate inside one
  transaction with row locks.
- dMind operations (`care-flow-brief.v1`, `placement-ranking.v1`,
  `discharge-coordination.v1`, `theatre-plan-explanation.v1`,
  `operational-forecast-explanation.v1`, `shift-handoff-draft.v1`) may
  summarise, rank supplied feasible candidates, explain and draft. They
  cannot admit, place, transfer, discharge, schedule, staff or notify, and
  their artifacts need human review. Ranking is a permutation of supplied
  ids or is discarded.
- The hierarchy, services, professions and position types are runtime
  catalog data, bilingual, with effective dates; catalog membership is not
  authorisation (ADR-0006). Nothing is backfilled on upgrade.
- `discharge_ready` is operational readiness only; medical clearance stays
  in the clinical record. Episodes link clinical objects by id and copy no
  clinical content.
- FHIR exposure is a read facade (`EpisodeOfCare`, `Encounter`, `Location`,
  `Task`) under ADR-0005.

## Consequences

- Operational throughput depends on humans confirming each step; the UI
  therefore optimises for fast confirmation, not automation.
- Deterministic engines need operational validation before any real use;
  weights and thresholds are synthetic fixtures today.
- Adding an operational object means adding a state machine, history table,
  typed conflicts, tests and a hazard entry — not a model prompt.
