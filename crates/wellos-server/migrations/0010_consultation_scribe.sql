-- Smart consultation cockpit: recording consent and structured AI-scribe
-- drafts.
--
-- Raw audio is never stored: the browser holds the recording in memory until
-- transcription succeeds (or the clinician discards it), and the server
-- processes bytes in memory only. What persists is the structured,
-- reviewable output (transcript segments + proposed note sections), bound to
-- the exact note version it was proposed against, as an unsigned AIArtifact.

-- Explicit, per-encounter recording consent recorded by the treating
-- practitioner before any audio is captured. Append-only.
CREATE TABLE encounter_recording_consents (
    id              uuid PRIMARY KEY,
    tenant_id       uuid NOT NULL REFERENCES tenants(id),
    encounter_id    uuid NOT NULL REFERENCES encounters(id),
    patient_id      uuid NOT NULL REFERENCES patients(id),
    practitioner_id uuid NOT NULL REFERENCES users(id),
    -- Consent is recorded as given; withdrawal is a new row with granted=false.
    granted         boolean NOT NULL,
    recorded_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX encounter_recording_consents_encounter
    ON encounter_recording_consents (tenant_id, encounter_id, recorded_at DESC);

-- Structured, per-section review state for scribe drafts (which sections the
-- clinician applied, how, and when). Result summaries and encounter
-- summaries leave it empty.
ALTER TABLE ai_artifacts
    ADD COLUMN review_detail jsonb NOT NULL DEFAULT '{}'::jsonb;

-- Dashboard cockpit preferences are browser-local (widget ids/order/display
-- only); nothing is stored server-side for them.
