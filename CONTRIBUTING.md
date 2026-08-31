# Contributing to WellOS

## Ground rules

- **No real patient data, ever.** Synthetic data only — in code, fixtures,
  tests, logs, prompts, screenshots, and analytics.
- **Clinical safety before convenience.** Deterministic calculators and
  versioned rules handle medical arithmetic and thresholds; AI output is always
  an artifact requiring human review, never silently converted to clinical
  truth.
- **Human accountability.** Consequential actions (ordering, prescribing,
  closing result loops) require an authenticated, authorized human.
- **Least privilege.** New endpoints must go through the central policy module
  (`wellos-server/src/policy.rs`) and write audit events.

## Workflow

1. Branch from `main`; open a PR for every change.
2. `make check` must pass locally (fmt, clippy `-D warnings`, unit tests,
   web lint/typecheck).
3. Integration tests (`make test-integration`) require the local PostgreSQL
   from `make up` with migrations and seed applied.
4. Schema changes are new numbered files in `crates/wellos-server/migrations/`;
   never edit an applied migration.
5. Significant technical decisions get an ADR in `docs/adr/`.

## Code conventions

- Rust: rustfmt defaults, clippy clean, no `unwrap()` in request paths.
- Observations and audit events are append-only; amendments link to the record
  they amend.
- Events written to the outbox carry identifiers and metadata, not full
  clinical payloads.
- UI strings live in `apps/web/lib/i18n.ts` and must exist in English and
  Spanish; interactive elements must be keyboard-accessible and labeled
  (WCAG 2.2 AA target).

## Security

Report vulnerabilities per [SECURITY.md](SECURITY.md). Never commit secrets;
`.env` is gitignored and `.env.example` documents required variables.
