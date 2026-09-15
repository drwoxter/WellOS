-- AI artifact isolation and synthetic provenance hotfix.
--
-- 1. An artifact's output may only be reused for the same tenant, patient,
--    task and clinical resource it was generated for. Existing reuse links
--    that crossed patients are severed (the link is dropped, the output is
--    untouched; the `ai.artifact.generated` audit event keeps the original
--    `reused_from` value) and, if the output was never professionally
--    decided on, the artifact is invalidated so it is not presented as a
--    current proposal. Approved/rejected artifacts keep the recorded
--    professional decision.
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

-- 1. Sever reuse links that crossed patient (or tenant) boundaries.
WITH crossed AS (
    SELECT a.id, a.status
    FROM ai_artifacts a
    JOIN ai_artifacts p ON p.id = a.reused_from
    WHERE p.tenant_id <> a.tenant_id OR p.patient_id <> a.patient_id
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
