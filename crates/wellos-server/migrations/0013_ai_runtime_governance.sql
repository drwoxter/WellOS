-- Productization gate v1: governed AI execution provenance, credit
-- deduplication, per-tenant/per-task quotas and machine-identifiable
-- synthetic data.

-- Tenants carry an explicit data class. Synthetic (fixture) tenants can
-- never be mistaken for production tenants, and the seeder refuses to run in
-- a database that holds any production tenant.
ALTER TABLE tenants
    ADD COLUMN data_class text NOT NULL DEFAULT 'production'
        CHECK (data_class IN ('production', 'synthetic'));
-- Backfill: the only tenants that exist before this migration were created
-- by the synthetic seeder, whose human users carry a `synthetic|` OIDC
-- subject that no real identity provider issues (test suites add further
-- users to those tenants without a subject). A tenant holding any such
-- identity is synthetic; any other tenant keeps the production class.
UPDATE tenants t SET data_class = 'synthetic'
WHERE EXISTS (
    SELECT 1 FROM users u
    WHERE u.tenant_id = t.id AND u.oidc_subject LIKE 'synthetic|%'
);

-- Full provenance on every artifact: which provider produced it, under
-- which prompt family, from which authorized input references, with which
-- usage, and whether the output is synthetic (fixture provider). Artifacts
-- generated from a byte-identical prior execution record their source.
ALTER TABLE ai_artifacts
    ADD COLUMN provider       text,
    ADD COLUMN prompt_version text,
    ADD COLUMN input_refs     jsonb NOT NULL DEFAULT '[]'::jsonb,
    ADD COLUMN usage          jsonb,
    ADD COLUMN synthetic      boolean NOT NULL DEFAULT false,
    ADD COLUMN reused_from    uuid REFERENCES ai_artifacts(id);
-- Backfill: every artifact generated before this migration came from the
-- deterministic fixture provider (the only provider that existed).
UPDATE ai_artifacts SET provider = route, synthetic = true WHERE route IS NOT NULL;
CREATE INDEX ai_artifacts_dedup
    ON ai_artifacts (tenant_id, artifact_type, input_hash, model, prompt_version, output_schema)
    WHERE input_hash IS NOT NULL AND output IS NOT NULL;

-- One row per provider execution attempt (never per reuse), the basis for
-- hourly per-tenant and per-task quotas. Holds no request content.
CREATE TABLE ai_executions (
    id            uuid PRIMARY KEY,
    tenant_id     uuid NOT NULL REFERENCES tenants(id),
    artifact_type text NOT NULL,
    artifact_id   uuid REFERENCES ai_artifacts(id),
    provider      text NOT NULL,
    model         text NOT NULL,
    external      boolean NOT NULL,
    executed_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX ai_executions_tenant_time ON ai_executions (tenant_id, executed_at);
CREATE INDEX ai_executions_task_time ON ai_executions (tenant_id, artifact_type, executed_at);

-- Rollback (manual; the backfills above only add classification columns):
--   DROP TABLE ai_executions;
--   DROP INDEX ai_artifacts_dedup;
--   ALTER TABLE ai_artifacts DROP COLUMN reused_from, DROP COLUMN synthetic,
--       DROP COLUMN usage, DROP COLUMN input_refs, DROP COLUMN prompt_version,
--       DROP COLUMN provider;
--   ALTER TABLE tenants DROP COLUMN data_class;
