# ADR-0009: Audit, Provenance, and Tamper Evidence

Status: Accepted · Date: 2026-08-29

## Context

Every access, rule execution, AI generation, review, consent decision, and
closure must be reconstructable — for clinical safety review, privacy law, and
incident response.

## Decision

- Audit events are written **in the same transaction** as the state change
  they describe: actor, tenant, action, resource, purpose of use, correlation
  id, outcome (including denials), timestamps.
- Observations and audit events are append-only; amendments link to what they
  amend. Break-glass produces additional enhanced records.
- Rule evaluations persist rule id + version + input + outcome; AI artifacts
  persist input hashes and citations — provenance is queryable, not inferred
  from logs.
- Audit reads are themselves authorized (`audit.read`, privacy/security roles)
  and audited.

## Consequences

- No state change without its audit row (atomicity by construction).
- Tamper evidence (hash-chained audit, WORM storage) is planned, not present —
  application convention is the current integrity layer (roadmap #2).
