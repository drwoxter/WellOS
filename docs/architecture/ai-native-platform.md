# AI-Native Platform (dMind)

## Governance model

AI in WellOS is **governed by construction**:

1. **Every AI output is an AIArtifact** — a first-class, persisted, versioned
   record with status lifecycle, autonomy level, model metadata, input hash,
   and structured output. There is no path from model output directly into
   clinical fields.
2. **Autonomy levels A0–A4** bound what a route may do:
   - A0 — none; A1 — retrieval/summarization of existing record data;
   - A2 — assistive draft requiring human review (current result summary);
   - A3 — proposed action requiring explicit human approval;
   - A4 — reserved; no autonomous clinical action is permitted anywhere.
3. **Hard prohibitions**: AI never orders, prescribes, diagnoses, or changes
   treatment. Deterministic versioned rules — not models — decide criticality
   and thresholds.
4. **Asynchrony**: AI generation runs after the clinical transaction commits.
   Provider failure yields an `unavailable` artifact; the clinical workflow is
   never blocked (graceful degradation, tested).
5. **Consent and policy gates**: external AI processing requires
   purpose-specific consent and the tenant/policy flag `allow_external_ai`
   (default `false`), plus the deployment-level opt-in
   `WELLOS_ALLOW_EXTERNAL_AI=true` before an external provider can even be
   configured.
6. **Provenance**: artifacts cite the exact source facts used, list
   limitations, and record the input hash for reproducibility; generation and
   review are audit events.
7. **Nothing is fabricated**: when no provider is configured, or the
   configured provider fails, the affected dMind action is unavailable and
   says why. There is no fallback from a real provider to a fixture, in any
   environment.

## Runtime boundary: product, configured provider, fixture, future

| Category | What it means | Where it runs |
| --- | --- | --- |
| **Production-intent functionality** | Deterministic clinical workflows, authorization, audit, consent, AIArtifact lifecycle, capability reporting, the `disabled` provider. | Every `WELLOS_ENV`. |
| **Configured real-provider functionality** | `DMIND_MODEL_PROVIDER=openai_compatible` and `WELLOS_SCRIBE_PROVIDER=openai_compatible` against an operator-configured, allowlisted HTTPS endpoint. Implemented and tested against a controlled local HTTP server; **not** validated against a live vendor from this repository. | Any environment once configured and `WELLOS_ALLOW_EXTERNAL_AI=true`. |
| **Development/test fixtures** | `fake` providers, development sign-in, synthetic seed, development-user discovery. Compiled only with `--features dev-fixtures`; refused at startup in `staging`/`production` even when compiled in. Every fixture artifact is stored with `synthetic=true` and shown with a synthetic-provider notice. | `development`, `test`. |
| **Planned** | dMind Access operations (intent interpretation, resource matching, appointment ranking, capacity forecasting, cancellation recovery, attendance support, transport coordination). No code, routes, enums or UI exist for them yet; they will be added as further typed `Operation`s and schema-versioned contracts on the same gateway. | — |

Fixture execution is never described as product readiness: `/ready` reports
the fixture provider as `ready` **with `synthetic: true`**, and the UI
labels its output as deterministic test data.

## Runtime environment

`WELLOS_ENV` is parsed once into a typed `RuntimeEnv`
(`development | test | staging | production`); missing or unknown values
fail startup. `staging` and `production` refuse `WELLOS_DEV_AUTH=true`,
`fake` providers, `WELLOS_ALLOW_SYNTHETIC_SEED=true`, development-token
authentication and the development-user endpoint. Development sign-in is
server-controlled: the web app asks `GET /api/auth/providers` which methods
exist and never decides authorization from a `NEXT_PUBLIC_*` variable.

## Model Gateway

`dmind-gateway` exposes a provider-neutral `ModelGateway` trait with typed
operations for every implemented dMind workflow — diagnostic-result and
encounter summary (`result-summary.v1`), triage proposal
(`triage-proposal.v1`), risk summary (`risk-summary.v1`) and structured
consultation-note draft (`scribe-draft.v1`, fed by a genuine transcript). Each operation has a
prompt/template version and an output-schema version recorded on the
artifact. `DMIND_MODEL_PROVIDER` selects the implementation:

| Provider | Behaviour |
| --- | --- |
| `disabled` (default) | Every operation returns `disabled`; `/ready` reports the capability as disabled by configuration. |
| `fake` | Deterministic, offline, EN/ES, failure-injectable. `dev-fixtures` builds in `development`/`test` only. |
| `openai_compatible` | Real external chat-completions endpoint. Requires `WELLOS_ALLOW_EXTERNAL_AI=true`, `DMIND_MODEL_ENDPOINT`, `DMIND_MODEL_NAME`, `DMIND_MODEL_API_KEY` and, outside development, `DMIND_MODEL_ALLOWED_HOSTS`. |

The `openai_compatible` adapter enforces: HTTPS on a DNS host outside
development (loopback `http://` accepted in development only, for a
controlled local test server); exact hostname allowlist (`host` or
`host:port`, no wildcards); no embedded URL credentials; redirects never
followed; connect and request timeouts; a response-size cap
(`DMIND_MODEL_MAX_RESPONSE_KIB`); bounded retries only on transient
failures (`DMIND_MODEL_MAX_RETRIES`, 0..=5); a per-replica concurrency
limit with a queue timeout; secret-safe errors (the key, prompts, responses
and transcripts never appear in errors or logs); structured JSON output
validated against the operation's schema, with every evidence reference
checked against the facts, results or transcript segments actually supplied
— an unknown citation, a missing field or free-form text where structure
is required rejects the whole response as `invalid_output`. Raw model text
is never written to an authoritative field.

Capability state is tracked per provider (`ready`, `degraded` after
consecutive failures, `disabled`, `invalid_configuration`) and exposed on
`/ready` and in tenant metadata for the model, the transcription provider
and the derived structured-note capability, together with the configured
BCP-47 transcription languages (`WELLOS_SCRIBE_LANGUAGES`).

## Governed execution (`aigov`)

Every dMind route plans its execution through one shared module before the
provider is called:

1. capability callable; external processing permitted by tenant policy and
   the patient's `ai_external_processing` consent when the provider is
   external;
2. **reuse**: the newest decided artifact with the same tenant, task, input
   hash, model, prompt version and output-schema version controls the
   decision. If it is `awaiting_review`, `approved` or `superseded` it is
   returned instead (recorded via `reused_from`; no provider call, no quota
   consumption); if it is `rejected` or `withdrawn` nothing is reused and a
   fresh execution runs — an older approved copy never outranks a newer
   professional rejection. `draft`, `invalidated` and `unavailable` rows
   never qualify;
3. **quotas**: hourly per-tenant (`DMIND_QUOTA_TENANT_PER_HOUR`) and
   per-task (`DMIND_QUOTA_TASK_PER_HOUR`) execution counts are checked under
   an advisory lock and a row is reserved in `ai_executions`, so concurrent
   requests cannot both pass the same check; exhaustion answers `429
   ai_quota_exceeded` with `Retry-After` derived from the window that is
   actually exhausted (the later of the two when both are).

A successful execution persists on the AIArtifact: tenant, patient/episode
scope, task type, authorized input references (`input_refs`), input hash,
provider, model, prompt version, output-schema version, timestamps and
status, citations, limitations, token usage when the provider reports it,
`synthetic`, review status and the reviewer/disposition once reviewed.
Output stays a proposal until an authorized professional accepts it;
acceptance, rejection and edits are audited. Automated CI configures the
fixture providers explicitly and never contacts an external model.

## Real-provider smoke test with synthetic patients

No live vendor credential is used in CI or was used to validate this
repository. To smoke-test the real adapters against synthetic patients, run
a `dev-fixtures` build in `development` with the fixtures otherwise off:

```bash
export WELLOS_ENV=development WELLOS_DEV_AUTH=true WELLOS_ALLOW_SYNTHETIC_SEED=true
export DMIND_MODEL_PROVIDER=fake WELLOS_SCRIBE_PROVIDER=fake
cargo run -p wellos-server --bin migrate
cargo run -p wellos-server --bin seed --features dev-fixtures     # synthetic patients only

export WELLOS_ALLOW_EXTERNAL_AI=true
export DMIND_MODEL_PROVIDER=openai_compatible
export DMIND_MODEL_ENDPOINT=https://<vendor-host>/v1/chat/completions
export DMIND_MODEL_ALLOWED_HOSTS=<vendor-host>
export DMIND_MODEL_NAME=<model>
export DMIND_MODEL_API_KEY=<key>            # never commit; never logged
export WELLOS_SCRIBE_PROVIDER=openai_compatible
export WELLOS_SCRIBE_ENDPOINT=https://<vendor-host>/v1/audio/transcriptions
export WELLOS_SCRIBE_ALLOWED_HOSTS=<vendor-host>
export WELLOS_SCRIBE_MODEL=<transcription-model>
export WELLOS_SCRIBE_API_KEY=<key>
cargo run -p wellos-server --features dev-fixtures
```

PowerShell: replace each `export NAME=value` with `$env:NAME = 'value'`.
Then sign in as a seeded synthetic clinician, open a synthetic patient's
consultation and record; confirm `/ready` shows `model`, `transcription`
and `structured_note` as `ready` with `synthetic: false`, and that the
resulting artifact carries the real provider/model and `synthetic=false`.
The seeded tenant is `data_class='synthetic'`, so no real patient data is
involved.

## Structured outputs

Outputs are schema-versioned (e.g. `ResultSummaryV1`: summary, trend,
citations, limitations, suggested next-step categories). The UI renders the
structure with a mandatory disclaimer; free-form generation is not exposed to
clinicians as a primary interface.
