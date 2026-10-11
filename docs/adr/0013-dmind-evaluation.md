# ADR-0013: dMind Evaluation Is a Governed, Synthetic, Budgeted Shadow Path

Status: Accepted · Date: 2026-10-11

## Context

dMind operations (`Operation::ALL` in `dmind-gateway`) are assistive: every
clinical decision stays with an accountable professional and every workflow
keeps a deterministic baseline. Measuring provider quality and safety still
requires calling real providers with realistic inputs, which creates three
risks: real patient data reaching a provider, evaluation traffic mutating
clinical state, and uncontrolled spend.

## Decision

- Evaluation is a **shadow path**: the runner (`crates/wellos-server/src/eval`)
  calls the same gateway, validators and deterministic baselines as production
  but writes only to the `dmind_eval_*` tables (migration 0018). No clinical
  table changes during a run; an integration test asserts the footprint.
- Inputs are a **versioned synthetic corpus** compiled into the binary and
  hashed; identifiers must belong to reserved synthetic UUID namespaces and
  the run refuses to start otherwise. Evaluation tables accept synthetic
  tenants only (database trigger).
- Quality is scored by a **versioned rubric** that separates deterministic,
  heuristic, human-review-required and not-validated metrics; any
  high-severity safety-boundary violation fails the case regardless of score.
  Heuristic metrics are engineering signals, never clinical truth.
- **Live** provider calls require every gate (`WELLOS_ALLOW_EXTERNAL_AI`,
  `WELLOS_EVAL_LIVE`, test/staging environment, synthetic tenant, verified
  synthetic corpus, explicit `--confirm-live`) and are reachable from the
  CLI only. Budgets (calls incl. retries, tokens, cost, concurrency) are
  reserved worst-case before dispatch and settled afterwards.
- Reuse is provenance-keyed: input hash + prompt version + secret-free
  provider configuration fingerprint. Only successful, schema-valid outputs
  are reusable; a provider failure is never replayed as a result.
- Human review of executions is append-only and records the rubric version
  that scored the execution, under the existing central policy
  (`dmind_eval.read` / `dmind_eval.manage`) and audit path.

## Consequences

- A new `Operation` variant is registered automatically (single declaration
  macro) and must gain corpus cases or the corpus coverage test fails.
- Evaluation results describe behaviour on synthetic cases under a specific
  provider fingerprint; they are not a clinical validation and make no
  production-readiness or regulatory claim.
- Transport failure modes are tested against a controlled local HTTP server
  rather than modelled as corpus cases (see
  `docs/architecture/dmind-evaluation.md`).
