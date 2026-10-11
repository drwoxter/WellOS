-- dMind Live Intelligence & Evaluation v1: evaluation-only storage.
--
-- Additive. Nothing here is read by any clinical, scheduling, notification or
-- patient-facing path; the tables record how a configured model provider
-- behaved on a *synthetic* corpus so that quality and safety can be measured
-- before any real-patient use is ever considered. Rows are append-only except
-- for the tightly controlled run-state transitions enforced by trigger. No
-- raw audio, no unrestricted prompts, no provider secrets, no real PHI.
--
-- Rollback (manual, destructive to evaluation history only):
--   DROP TABLE dmind_eval_run_summaries, dmind_eval_reviews, dmind_eval_metrics,
--        dmind_eval_executions, dmind_eval_runs, dmind_eval_cases,
--        dmind_eval_provider_fingerprints, dmind_eval_suites;
--   DROP FUNCTION dmind_eval_forbid_mutation(), dmind_eval_run_transition(),
--        dmind_eval_require_synthetic_tenant();

CREATE FUNCTION dmind_eval_forbid_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'dmind evaluation table % is append-only', TG_TABLE_NAME
        USING ERRCODE = 'integrity_constraint_violation';
END $$;

-- Evaluation rows may only ever belong to a synthetic tenant.
CREATE FUNCTION dmind_eval_require_synthetic_tenant() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF NOT EXISTS (SELECT 1 FROM tenants t WHERE t.id = NEW.tenant_id AND t.data_class = 'synthetic') THEN
        RAISE EXCEPTION 'dmind evaluation refuses tenant %: data_class is not synthetic', NEW.tenant_id
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END $$;

-- Versioned synthetic suites (one row per suite key + version per tenant).
CREATE TABLE dmind_eval_suites (
    id             uuid PRIMARY KEY,
    tenant_id      uuid NOT NULL REFERENCES tenants(id),
    suite_key      text NOT NULL,
    suite_version  text NOT NULL,
    rubric_version text NOT NULL,
    languages      text[] NOT NULL,
    case_count     integer NOT NULL CHECK (case_count >= 0),
    corpus_hash    text NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, suite_key, suite_version)
);
CREATE TRIGGER dmind_eval_suites_synthetic BEFORE INSERT ON dmind_eval_suites
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_require_synthetic_tenant();
CREATE TRIGGER dmind_eval_suites_append_only BEFORE UPDATE OR DELETE ON dmind_eval_suites
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_forbid_mutation();

-- One fictional case: operation, language, category, hashed input, the
-- evidence references the provider may cite and the case invariants.
CREATE TABLE dmind_eval_cases (
    id               uuid PRIMARY KEY,
    tenant_id        uuid NOT NULL REFERENCES tenants(id),
    suite_id         uuid NOT NULL REFERENCES dmind_eval_suites(id),
    case_key         text NOT NULL,
    operation        text NOT NULL,
    category         text NOT NULL CHECK (category IN
                        ('typical', 'incomplete', 'high_risk', 'adversarial', 'edge')),
    language         text NOT NULL CHECK (language IN ('en', 'es')),
    title            text NOT NULL CHECK (char_length(title) <= 200),
    input_hash       text NOT NULL,
    expected_schema  text NOT NULL,
    allowed_evidence jsonb NOT NULL DEFAULT '[]'::jsonb,
    expectations     jsonb NOT NULL DEFAULT '{}'::jsonb,
    synthetic        boolean NOT NULL DEFAULT true CHECK (synthetic),
    created_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (suite_id, case_key)
);
CREATE INDEX dmind_eval_cases_suite_op ON dmind_eval_cases (suite_id, operation);
CREATE TRIGGER dmind_eval_cases_synthetic BEFORE INSERT ON dmind_eval_cases
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_require_synthetic_tenant();
CREATE TRIGGER dmind_eval_cases_append_only BEFORE UPDATE OR DELETE ON dmind_eval_cases
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_forbid_mutation();

-- Provider/model configuration identity (never the credential, never the
-- endpoint: only hashes and safe metadata) plus the pricing used for cost
-- estimates and its effective date.
CREATE TABLE dmind_eval_provider_fingerprints (
    id                     uuid PRIMARY KEY,
    tenant_id              uuid NOT NULL REFERENCES tenants(id),
    provider               text NOT NULL,
    model                  text NOT NULL,
    model_version          text NOT NULL,
    external               boolean NOT NULL,
    synthetic_provider     boolean NOT NULL,
    config_hash            text NOT NULL,
    pricing                jsonb,
    pricing_effective_date date,
    created_at             timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, config_hash)
);
CREATE TRIGGER dmind_eval_fingerprints_synthetic BEFORE INSERT ON dmind_eval_provider_fingerprints
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_require_synthetic_tenant();
CREATE TRIGGER dmind_eval_fingerprints_append_only BEFORE UPDATE OR DELETE ON dmind_eval_provider_fingerprints
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_forbid_mutation();

-- One evaluation run. The only mutable rows in this schema, restricted to
-- the state machine planned -> running -> completed|aborted|failed (and
-- planned -> aborted when the gate refuses before any execution).
CREATE TABLE dmind_eval_runs (
    id                uuid PRIMARY KEY,
    tenant_id         uuid NOT NULL REFERENCES tenants(id),
    suite_id          uuid NOT NULL REFERENCES dmind_eval_suites(id),
    fingerprint_id    uuid NOT NULL REFERENCES dmind_eval_provider_fingerprints(id),
    mode              text NOT NULL CHECK (mode IN ('dry_run', 'offline', 'live')),
    environment       text NOT NULL,
    data_class        text NOT NULL DEFAULT 'synthetic' CHECK (data_class = 'synthetic'),
    requested_by      uuid REFERENCES users(id),
    requested_via     text NOT NULL CHECK (requested_via IN ('cli', 'api')),
    operations        jsonb NOT NULL DEFAULT '[]'::jsonb,
    budget            jsonb NOT NULL,
    gate              jsonb NOT NULL,
    stop_on_high_severity boolean NOT NULL DEFAULT true,
    status            text NOT NULL CHECK (status IN ('planned', 'running', 'completed', 'aborted', 'failed')),
    abort_reason      text CHECK (char_length(abort_reason) <= 400),
    version           integer NOT NULL DEFAULT 1,
    created_at        timestamptz NOT NULL DEFAULT now(),
    started_at        timestamptz,
    finished_at       timestamptz
);
CREATE INDEX dmind_eval_runs_tenant_created ON dmind_eval_runs (tenant_id, created_at DESC);
CREATE TRIGGER dmind_eval_runs_synthetic BEFORE INSERT ON dmind_eval_runs
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_require_synthetic_tenant();

CREATE FUNCTION dmind_eval_run_transition() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'dmind evaluation runs cannot be deleted'
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.id <> OLD.id OR NEW.tenant_id <> OLD.tenant_id OR NEW.suite_id <> OLD.suite_id
       OR NEW.fingerprint_id <> OLD.fingerprint_id OR NEW.mode <> OLD.mode
       OR NEW.environment <> OLD.environment OR NEW.requested_by IS DISTINCT FROM OLD.requested_by
       OR NEW.requested_via <> OLD.requested_via OR NEW.operations <> OLD.operations
       OR NEW.budget <> OLD.budget OR NEW.gate <> OLD.gate
       OR NEW.stop_on_high_severity <> OLD.stop_on_high_severity
       OR NEW.created_at <> OLD.created_at THEN
        RAISE EXCEPTION 'dmind evaluation run % is immutable except for its state', OLD.id
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    IF NEW.version <> OLD.version + 1 THEN
        RAISE EXCEPTION 'dmind evaluation run % state change requires version % (got %)',
            OLD.id, OLD.version + 1, NEW.version
            USING ERRCODE = 'serialization_failure';
    END IF;
    IF NOT (
        (OLD.status = 'planned' AND NEW.status IN ('running', 'aborted'))
        OR (OLD.status = 'running' AND NEW.status IN ('completed', 'aborted', 'failed'))
    ) THEN
        RAISE EXCEPTION 'dmind evaluation run % cannot move from % to %', OLD.id, OLD.status, NEW.status
            USING ERRCODE = 'integrity_constraint_violation';
    END IF;
    RETURN NEW;
END $$;
CREATE TRIGGER dmind_eval_runs_transition BEFORE UPDATE OR DELETE ON dmind_eval_runs
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_run_transition();

-- One provider execution (or one refused/aborted/reused slot) per case per
-- run. Stores the bounded typed output and the deterministic baseline, never
-- the rendered prompt.
CREATE TABLE dmind_eval_executions (
    id                uuid PRIMARY KEY,
    tenant_id         uuid NOT NULL REFERENCES tenants(id),
    run_id            uuid NOT NULL REFERENCES dmind_eval_runs(id),
    case_id           uuid NOT NULL REFERENCES dmind_eval_cases(id),
    operation         text NOT NULL,
    language          text NOT NULL,
    prompt_version    text NOT NULL,
    schema_version    text NOT NULL,
    provider          text NOT NULL,
    model             text NOT NULL,
    model_version     text NOT NULL,
    input_hash        text NOT NULL,
    evidence_refs     jsonb NOT NULL DEFAULT '[]'::jsonb,
    baseline_hash     text NOT NULL,
    baseline_output   jsonb,
    provider_output   jsonb,
    execution_kind    text NOT NULL CHECK (execution_kind IN ('fresh', 'retried', 'reused', 'cached', 'skipped')),
    retry_count       integer NOT NULL DEFAULT 0 CHECK (retry_count >= 0),
    degraded          boolean NOT NULL DEFAULT false,
    fallback_verified boolean NOT NULL DEFAULT false,
    status            text NOT NULL CHECK (status IN
                        ('passed', 'needs_review', 'failed', 'aborted', 'provider_unavailable_fallback_verified')),
    failure_code      text CHECK (char_length(failure_code) <= 80),
    latency_ms        integer CHECK (latency_ms >= 0),
    input_tokens      bigint CHECK (input_tokens >= 0),
    output_tokens     bigint CHECK (output_tokens >= 0),
    estimated_cost    numeric(12, 6) CHECK (estimated_cost >= 0),
    currency          text CHECK (char_length(currency) = 3),
    started_at        timestamptz NOT NULL DEFAULT now(),
    finished_at       timestamptz,
    UNIQUE (run_id, case_id)
);
CREATE INDEX dmind_eval_executions_run ON dmind_eval_executions (run_id, operation);
CREATE TRIGGER dmind_eval_executions_synthetic BEFORE INSERT ON dmind_eval_executions
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_require_synthetic_tenant();
CREATE TRIGGER dmind_eval_executions_append_only BEFORE UPDATE OR DELETE ON dmind_eval_executions
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_forbid_mutation();

-- Automated metric results. `verification` separates what the rubric can
-- prove deterministically from heuristics, from checks that require a human
-- and from metrics that cannot yet be validated at all.
CREATE TABLE dmind_eval_metrics (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL REFERENCES tenants(id),
    execution_id uuid NOT NULL REFERENCES dmind_eval_executions(id),
    metric       text NOT NULL,
    verification text NOT NULL CHECK (verification IN
                    ('deterministic', 'heuristic', 'human_review_required', 'not_validated')),
    severity     text NOT NULL CHECK (severity IN ('info', 'low', 'medium', 'high')),
    outcome      text NOT NULL CHECK (outcome IN ('pass', 'fail', 'review', 'not_applicable', 'not_validated')),
    score        numeric(8, 4),
    detail       text CHECK (char_length(detail) <= 600),
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX dmind_eval_metrics_execution ON dmind_eval_metrics (execution_id);
CREATE TRIGGER dmind_eval_metrics_synthetic BEFORE INSERT ON dmind_eval_metrics
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_require_synthetic_tenant();
CREATE TRIGGER dmind_eval_metrics_append_only BEFORE UPDATE OR DELETE ON dmind_eval_metrics
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_forbid_mutation();

-- Append-only, versioned human review decisions on one execution. Accepting
-- an evaluation result is a statement about the evaluation, never an
-- approval for clinical use.
CREATE TABLE dmind_eval_reviews (
    id             uuid PRIMARY KEY,
    tenant_id      uuid NOT NULL REFERENCES tenants(id),
    execution_id   uuid NOT NULL REFERENCES dmind_eval_executions(id),
    reviewer_id    uuid NOT NULL REFERENCES users(id),
    rubric_version text NOT NULL,
    decision       text NOT NULL CHECK (decision IN
                      ('accept_result', 'flag_quality', 'flag_safety', 'request_revision')),
    comment        text NOT NULL DEFAULT '' CHECK (char_length(comment) <= 1000),
    version        integer NOT NULL CHECK (version >= 1),
    created_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (execution_id, version)
);
CREATE TRIGGER dmind_eval_reviews_synthetic BEFORE INSERT ON dmind_eval_reviews
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_require_synthetic_tenant();
CREATE TRIGGER dmind_eval_reviews_append_only BEFORE UPDATE OR DELETE ON dmind_eval_reviews
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_forbid_mutation();

-- Aggregate written once when a run reaches a terminal state.
CREATE TABLE dmind_eval_run_summaries (
    id              uuid PRIMARY KEY,
    tenant_id       uuid NOT NULL REFERENCES tenants(id),
    run_id          uuid NOT NULL UNIQUE REFERENCES dmind_eval_runs(id),
    totals          jsonb NOT NULL,
    operations      jsonb NOT NULL,
    languages       jsonb NOT NULL,
    safety_failures jsonb NOT NULL DEFAULT '[]'::jsonb,
    calls           integer NOT NULL CHECK (calls >= 0),
    input_tokens    bigint NOT NULL DEFAULT 0,
    output_tokens   bigint NOT NULL DEFAULT 0,
    estimated_cost  numeric(12, 6) NOT NULL DEFAULT 0,
    currency        text,
    p50_latency_ms  integer,
    p95_latency_ms  integer,
    created_at      timestamptz NOT NULL DEFAULT now()
);
CREATE TRIGGER dmind_eval_summaries_synthetic BEFORE INSERT ON dmind_eval_run_summaries
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_require_synthetic_tenant();
CREATE TRIGGER dmind_eval_summaries_append_only BEFORE UPDATE OR DELETE ON dmind_eval_run_summaries
    FOR EACH ROW EXECUTE FUNCTION dmind_eval_forbid_mutation();
