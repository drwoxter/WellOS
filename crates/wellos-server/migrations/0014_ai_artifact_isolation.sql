-- AI artifact isolation and synthetic provenance hotfix.
--
-- 1. An artifact's output may only be reused for the same tenant, patient,
--    task and clinical resource it was generated for. Existing reuse links
--    that crossed tenants, patients, tasks or clinical resources (another
--    encounter, observation, visit or risk assessment of the same patient)
--    are severed (the link is dropped, the output is untouched; the
--    `ai.artifact.generated` audit event keeps the original `reused_from`
--    value) and, if the output was never professionally decided on, the
--    artifact is invalidated so it is neither presented as a current
--    proposal nor eligible to seed further reuse. Approved/rejected
--    artifacts keep the recorded professional decision.
-- 2. `reused_from` is guaranteed by the database to point at an artifact of
--    the same tenant and patient: the reference is a composite foreign key
--    over (id, tenant_id, patient_id). Resource-scope agreement is enforced
--    transactionally by `aigov::annotate`.
-- 3. The deduplication index covers the full reuse key (patient, provider,
--    model version included).
-- 4. Artifacts whose stored provenance or Scribe JSON proves the fixture
--    provider (`dmind-fake` / `local-fake`) took part are marked synthetic.
--    Artifacts with real-provider provenance are not touched.
--
-- Non-destructive: no rows are deleted, no columns dropped, no output
-- altered. Rollback (forward migration, documented in
-- docs/architecture/ai-native-platform.md):
--     ALTER TABLE ai_artifacts DROP CONSTRAINT ai_artifacts_reused_from_same_patient_fkey;
--     ALTER TABLE ai_artifacts ADD CONSTRAINT ai_artifacts_reused_from_fkey
--         FOREIGN KEY (reused_from) REFERENCES ai_artifacts(id);
--     ALTER TABLE ai_artifacts DROP CONSTRAINT ai_artifacts_id_tenant_patient_key;
--     DROP INDEX ai_artifacts_dedup;
--     CREATE INDEX ai_artifacts_dedup
--         ON ai_artifacts (tenant_id, artifact_type, input_hash, model, prompt_version, output_schema)
--         WHERE input_hash IS NOT NULL AND output IS NOT NULL;
-- The severed links, invalidations and synthetic flags are corrections of
-- incorrect data and are intentionally not reverted.

-- 1. Sever reuse links that crossed tenant, patient, task or clinical
--    resource boundaries. The resource is the column the artifact type is
--    bound to (see `aigov::ReuseScope`). Artifacts that reused a crossed
--    artifact (even within the correct scope) carry the same foreign output
--    and are treated the same way, transitively.
WITH RECURSIVE crossed AS (
    SELECT a.id
    FROM ai_artifacts a
    JOIN ai_artifacts p ON p.id = a.reused_from
    WHERE p.tenant_id <> a.tenant_id
       OR p.patient_id <> a.patient_id
       OR p.artifact_type <> a.artifact_type
       OR CASE a.artifact_type
            WHEN 'result_summary'    THEN p.observation_id     IS DISTINCT FROM a.observation_id
            WHEN 'encounter_summary' THEN p.encounter_id       IS DISTINCT FROM a.encounter_id
            WHEN 'scribe_draft'      THEN p.encounter_id       IS DISTINCT FROM a.encounter_id
            WHEN 'triage_proposal'   THEN p.visit_id           IS DISTINCT FROM a.visit_id
            WHEN 'risk_summary'      THEN p.risk_assessment_id IS DISTINCT FROM a.risk_assessment_id
            ELSE false
          END
    UNION
    SELECT a.id
    FROM ai_artifacts a
    JOIN crossed c ON c.id = a.reused_from
)
UPDATE ai_artifacts a
SET reused_from = NULL,
    status = CASE WHEN a.status IN ('draft', 'awaiting_review') THEN 'invalidated' ELSE a.status END
FROM crossed c
WHERE a.id = c.id;

-- 2. Composite reference: a reuse link must name an artifact of the same
--    tenant and patient.
ALTER TABLE ai_artifacts
    ADD CONSTRAINT ai_artifacts_id_tenant_patient_key UNIQUE (id, tenant_id, patient_id);
ALTER TABLE ai_artifacts
    DROP CONSTRAINT ai_artifacts_reused_from_fkey;
ALTER TABLE ai_artifacts
    ADD CONSTRAINT ai_artifacts_reused_from_same_patient_fkey
        FOREIGN KEY (reused_from, tenant_id, patient_id)
        REFERENCES ai_artifacts (id, tenant_id, patient_id);

-- 3. Deduplication index over the complete reuse key. The bound clinical
--    resource (observation / encounter / visit / risk assessment) is
--    filtered from the handful of rows this key selects.
DROP INDEX IF EXISTS ai_artifacts_dedup;
CREATE INDEX ai_artifacts_dedup
    ON ai_artifacts (tenant_id, patient_id, artifact_type, input_hash, provider, model,
                     model_version, prompt_version, output_schema)
    WHERE input_hash IS NOT NULL AND output IS NOT NULL;

-- 4. Repair synthetic provenance where the fixture provider provably took
--    part: model provenance columns, or the Scribe draft's embedded
--    transcription / extraction provider records.
UPDATE ai_artifacts
SET synthetic = true
WHERE synthetic = false
  AND (
        provider IN ('local-fake', 'dmind-fake')
     OR route IN ('local-fake', 'dmind-fake')
     OR model IN ('dmind-fake', 'fake-transcribe')
     OR (artifact_type = 'scribe_draft' AND (
            output->'transcription'->>'provider' IN ('local-fake', 'dmind-fake')
         OR output->'transcription'->>'model' IN ('dmind-fake', 'fake-transcribe')
         OR output->'extraction'->>'provider' IN ('local-fake', 'dmind-fake')
         OR output->'extraction'->>'model' = 'dmind-fake'))
  );
