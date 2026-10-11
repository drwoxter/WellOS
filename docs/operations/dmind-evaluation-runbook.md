# dMind evaluation runbook

Synthetic data only. Nothing here is clinical validation or a release gate for
real-patient use. Read `docs/architecture/dmind-evaluation.md` first.

## Environment

| Variable | Purpose |
|---|---|
| `DATABASE_URL` | Required for `--persist` and for the API; the tenant used must have `data_class = 'synthetic'` |
| `WELLOS_ENV` | `test` or `staging` for live mode; `production` refuses live evaluation |
| `DMIND_MODEL_PROVIDER` | `fake` (deterministic offline fixture) or the OpenAI-compatible provider configured for the runtime |
| `DMIND_MODEL_*` | Endpoint, model, timeout, host allowlist — the same variables the runtime uses; the evaluation does not add provider configuration of its own |
| `WELLOS_ALLOW_EXTERNAL_AI=true` | Runtime gate for any external provider call |
| `WELLOS_EVAL_LIVE=true` | Second, evaluation-specific gate for live runs |

Secrets (provider API keys) are read by the runtime configuration loader only.
The CLI has no argument that accepts a secret; passing one makes the run abort
with a usage error. Never commit `.env` files with real keys; use the secret
store of the environment running the evaluation.

## Offline evaluation (default, no network)

```
cargo run -p wellos-server --bin dmind-eval --features dev-fixtures -- \
  --mode offline --provider fake --json target/eval.json --html target/eval.html
```

* Exercises every corpus case against the deterministic fixture provider and
  the deterministic baselines; exit code is nonzero when any high-severity
  metric fails or a mandatory threshold is unmet.
* `--operations result_summary,triage_proposal` restricts the registry subset;
  `--list-operations` prints the authoritative list.
* `--persist --tenant-id <synthetic tenant>` records the run in the
  `dmind_eval_*` tables so it appears in the cockpit.
* CI runs this command on every push (`rust` job) and uploads the reports.

## Dry run

`--mode dry-run` validates gates, resolves the suite and prints the planned
calls, token ceilings and the maximum budget without contacting any provider.
Use it before every live run and attach the output to the change record.

## Controlled live evaluation

Preconditions, all enforced by the CLI before the first network call:

1. `WELLOS_ENV` is `test` or `staging`;
2. `WELLOS_ALLOW_EXTERNAL_AI=true` and `WELLOS_EVAL_LIVE=true`;
3. `--provider env` with a configured provider; `--confirm-live` passed;
4. budget: `--max-calls`, `--max-cost` with `--currency`, `--price-in`,
   `--price-out` (per 1k tokens) and `--price-date YYYY-MM-DD`;
5. ceilings: `--max-input-tokens`, `--max-output-tokens`, `--max-concurrency`;
6. tenant (`--tenant-id`) is synthetic when persisting.

```
WELLOS_ENV=staging WELLOS_ALLOW_EXTERNAL_AI=true WELLOS_EVAL_LIVE=true \
cargo run -p wellos-server --bin dmind-eval --features dev-fixtures -- \
  --mode live --provider env --confirm-live \
  --max-calls 60 --max-concurrency 2 --max-input-tokens 4000 --max-output-tokens 800 \
  --max-cost 5 --currency EUR --price-in 0.0025 --price-out 0.01 --price-date 2026-10-01 \
  --persist --tenant-id <synthetic tenant> --json target/live.json --html target/live.html
```

The runner prints planned calls and maximum budget, then stops at the first
call that would exceed a ceiling (`aborted`, reason recorded). With the default
`stop_on_high_severity` the run also stops at the first high-severity safety
failure; `--no-stop-on-high-severity` continues to collect all failures but
the exit code stays nonzero.

Live mode is **not** reachable through the HTTP API or the cockpit; the API
accepts `dry-run` and `offline` only.

## Interpreting a run

| Status | Meaning |
|---|---|
| `passed` | all deterministic and heuristic metrics passed; human review may still be pending |
| `needs_review` | a medium/low metric or a human-review metric asks for a person to look |
| `failed` | at least one high-severity metric failed — treat as a safety-boundary defect of the provider/prompt, not of the corpus |
| `provider_unavailable_fallback_verified` | the provider failed and the deterministic baseline was verified; never counted as provider success |
| `aborted` | a ceiling, budget or stop-on-failure rule halted the run |

* Compare runs only with the same corpus version, rubric version and provider
  fingerprint.
* Pass rates on 55 cases are indicative; look at the failing metric and case
  before drawing conclusions.
* Tokens, cost and latency are per execution and aggregated per run; costs are
  estimates from the supplied pricing.

## Human review

Clinical administrators record append-only reviews per execution from the run
detail page (`accept_result`, `flag_quality`, `flag_safety`,
`request_revision`; flags and revision requests require a comment). Reviews
are versioned, never edited or deleted, attributed to the reviewer and
recorded in the audit trail. Security auditors see everything read-only.

## Incidents and failures

| Symptom | Action |
|---|---|
| Run `aborted` with `CallCeiling` / `CostCeiling` / token ceilings | Expected ceiling; review the partial results, adjust the budget deliberately, rerun |
| Many `provider_unavailable_fallback_verified` | Provider outage or misconfiguration; the production fallback is confirmed working; check provider health (consecutive-failure gating) before rerunning |
| `failed` with `prohibited_claims` / `prohibited_autonomous_action` | Prompt-boundary regression: do not deploy the provider/prompt version; open a safety finding |
| `schema_validity` failures after a model upgrade | Provider output drift; the typed validator is correct — do not relax it |
| HTTP 409 on review submission | Another reviewer appended concurrently; reload and re-submit |
| Live run refused | One of the six preconditions failed; the error names it |

## Adding cases / new operations

Follow the steps in the architecture document; every new case must be
synthetic, bilingual where the operation is patient-facing, and must pass the
corpus tests before it is used in a live run.
