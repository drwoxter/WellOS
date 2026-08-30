-- Consent decisions become an append-only history: each grant or revocation
-- is a new immutable version; readers select the highest version per
-- (tenant, patient, purpose).
ALTER TABLE consents DROP CONSTRAINT consents_tenant_id_patient_id_purpose_key;
CREATE INDEX consents_current_idx
    ON consents (tenant_id, patient_id, purpose, version DESC);

-- Clinical documentation recorded with closed-loop transitions (review,
-- patient notification, closure follow-up disposition).
CREATE TABLE loop_notes (
    id                 uuid PRIMARY KEY,
    tenant_id          uuid NOT NULL REFERENCES tenants(id),
    service_request_id uuid NOT NULL REFERENCES service_requests(id),
    kind               text NOT NULL, -- review | notification | closure
    note               text NOT NULL,
    created_by         uuid NOT NULL REFERENCES users(id),
    created_at         timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX loop_notes_sr_idx ON loop_notes (tenant_id, service_request_id);
