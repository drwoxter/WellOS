-- Patient 360 + explainable risk engine + dMind Risk Agent.
--
-- Risk assessments are append-only snapshots of the deterministic engine
-- (wellos_domain::risk). Exactly one row per patient is `current`; older rows
-- remain for the evolution timeline and provenance. Nothing in this migration
-- rewrites or reinterprets existing clinical data: the engine reads the
-- existing chart, results, tasks, visits, alerts and care-team tables.

CREATE TABLE risk_assessments (
    id             uuid PRIMARY KEY,
    tenant_id      uuid NOT NULL REFERENCES tenants(id),
    facility_id    uuid NOT NULL REFERENCES facilities(id),
    patient_id     uuid NOT NULL REFERENCES patients(id),
    rules_version  text NOT NULL,
    -- Overall and per-domain levels are also stored denormalised for the
    -- worklist; `assessment` carries the complete explainable structure
    -- (factors, evidence references, gaps, trends, timestamps).
    overall_level  text NOT NULL, -- insufficient_data | low | moderate | high | critical
    safety_floor   text NOT NULL,
    overall_trend  text NOT NULL, -- improving | stable | worsening | unknown
    domain_levels  jsonb NOT NULL, -- {"acute_safety":"low", ...}
    assessment     jsonb NOT NULL,
    input_hash     text NOT NULL,
    -- What produced this snapshot: seed | manual | encounter.signed |
    -- result.reviewed | triage.completed | ...
    trigger        text NOT NULL,
    calculated_by  uuid REFERENCES users(id),
    calculated_at  timestamptz NOT NULL DEFAULT now(),
    is_current     boolean NOT NULL DEFAULT true
);
CREATE UNIQUE INDEX risk_assessments_current ON risk_assessments (tenant_id, patient_id) WHERE is_current;
CREATE INDEX risk_assessments_history ON risk_assessments (tenant_id, patient_id, calculated_at DESC);
CREATE INDEX risk_assessments_worklist ON risk_assessments (tenant_id, facility_id, overall_level) WHERE is_current;

-- Human review state per patient and risk domain ('overall' or one of the
-- seven domains). A review is bound to the level it was made against: when a
-- later calculation raises that domain above `level_at_review` the item is
-- shown as unreviewed again, so a worsening signal can never hide behind an
-- older acknowledgement.
CREATE TABLE risk_reviews (
    id              uuid PRIMARY KEY,
    tenant_id       uuid NOT NULL REFERENCES tenants(id),
    patient_id      uuid NOT NULL REFERENCES patients(id),
    domain          text NOT NULL,
    status          text NOT NULL, -- acknowledged | reviewed
    reviewer_id     uuid NOT NULL REFERENCES users(id),
    note            text,
    assessment_id   uuid NOT NULL REFERENCES risk_assessments(id),
    level_at_review text NOT NULL,
    reviewed_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, patient_id, domain)
);

-- Risk-summary proposals are AIArtifacts bound to the exact assessment they
-- explain; a newer assessment supersedes any awaiting proposal.
ALTER TABLE ai_artifacts
    ADD COLUMN risk_assessment_id uuid REFERENCES risk_assessments(id);
CREATE INDEX ai_artifacts_risk ON ai_artifacts (tenant_id, risk_assessment_id) WHERE risk_assessment_id IS NOT NULL;

-- Follow-up tasks created from a confirmed dMind suggestion no longer need a
-- laboratory request as their anchor; the confirming professional and the
-- artifact are recorded instead. Existing rows keep their request link.
ALTER TABLE follow_up_tasks
    ALTER COLUMN service_request_id DROP NOT NULL,
    ADD COLUMN source text NOT NULL DEFAULT 'result_loop',
    ADD COLUMN created_by uuid REFERENCES users(id),
    ADD COLUMN ai_artifact_id uuid REFERENCES ai_artifacts(id),
    ADD CONSTRAINT follow_up_tasks_anchor
        CHECK (service_request_id IS NOT NULL OR created_by IS NOT NULL);
CREATE INDEX follow_up_tasks_patient_status ON follow_up_tasks (tenant_id, patient_id, status);

-- Rollback (manual, no data reinterpretation was performed):
--   DROP INDEX follow_up_tasks_patient_status;
--   ALTER TABLE follow_up_tasks DROP CONSTRAINT follow_up_tasks_anchor,
--       DROP COLUMN ai_artifact_id, DROP COLUMN created_by, DROP COLUMN source;
--   -- only after deleting tasks with service_request_id IS NULL:
--   ALTER TABLE follow_up_tasks ALTER COLUMN service_request_id SET NOT NULL;
--   ALTER TABLE ai_artifacts DROP COLUMN risk_assessment_id;
--   DROP TABLE risk_reviews; DROP TABLE risk_assessments;
