-- Consent versions must stay unique and ordered per decision key; writers
-- serialize allocation by locking the patient row.
CREATE UNIQUE INDEX consents_version_unique_idx
    ON consents (tenant_id, patient_id, purpose, version);
