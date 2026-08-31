-- Clinical encounter documentation: structured consultation notes, vital
-- signs, and encounter-linked diagnoses.
--
-- Note integrity model: a draft note may be edited (with optimistic
-- concurrency); signing makes the note immutable. Post-signature corrections
-- are dated addenda linked to the original note — a signed note row is never
-- updated again.

ALTER TABLE encounters
    ADD COLUMN encounter_type text NOT NULL DEFAULT 'consultation',
    ADD COLUMN completed_at timestamptz;

CREATE TABLE encounter_notes (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL REFERENCES tenants(id),
    encounter_id uuid NOT NULL REFERENCES encounters(id),
    patient_id   uuid NOT NULL REFERENCES patients(id),
    author_id    uuid NOT NULL REFERENCES users(id),
    status       text NOT NULL DEFAULT 'draft', -- draft | signed
    version      bigint NOT NULL DEFAULT 1,     -- optimistic concurrency
    -- Structured consultation sections (synthetic data only).
    reason_for_encounter    text,
    history_present_illness text,
    medical_history         text,
    review_of_systems       text,
    physical_exam           text,
    assessment              text,
    plan                    text,
    follow_up               text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    updated_at   timestamptz NOT NULL DEFAULT now(),
    signed_at    timestamptz,
    signed_by    uuid REFERENCES users(id),
    UNIQUE (tenant_id, encounter_id)
);

CREATE TABLE encounter_note_addenda (
    id         uuid PRIMARY KEY,
    tenant_id  uuid NOT NULL REFERENCES tenants(id),
    note_id    uuid NOT NULL REFERENCES encounter_notes(id),
    author_id  uuid NOT NULL REFERENCES users(id),
    body       text NOT NULL,
    created_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX encounter_note_addenda_note ON encounter_note_addenda (tenant_id, note_id, created_at);

-- Structured vital-sign sets. Units are fixed per column; LOINC codes are the
-- internal vocabulary (systolic 8480-6, diastolic 8462-4, heart rate 8867-4,
-- respiratory rate 9279-1, temperature 8310-5, SpO2 2708-6, weight 29463-7,
-- height 8302-2, BMI 39156-5). BMI is always server-calculated.
CREATE TABLE vital_signs (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL REFERENCES tenants(id),
    encounter_id uuid NOT NULL REFERENCES encounters(id),
    patient_id   uuid NOT NULL REFERENCES patients(id),
    recorded_by  uuid NOT NULL REFERENCES users(id),
    systolic_mmhg        numeric,
    diastolic_mmhg       numeric,
    heart_rate_bpm       numeric,
    respiratory_rate_bpm numeric,
    temperature_c        numeric,
    spo2_percent         numeric,
    weight_kg            numeric,
    height_cm            numeric,
    bmi                  numeric,
    recorded_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX vital_signs_patient ON vital_signs (tenant_id, patient_id, recorded_at DESC);
CREATE INDEX vital_signs_encounter ON vital_signs (tenant_id, encounter_id);

-- Diagnoses recorded during an encounter integrate into the existing
-- conditions chart; the encounter link makes them part of the visit record.
ALTER TABLE conditions
    ADD COLUMN encounter_id uuid REFERENCES encounters(id),
    ADD COLUMN recorded_by  uuid REFERENCES users(id);

-- dMind documentation drafts attach to an encounter rather than a service
-- request; existing result summaries keep their service_request link.
ALTER TABLE ai_artifacts
    ALTER COLUMN service_request_id DROP NOT NULL,
    ADD COLUMN encounter_id uuid REFERENCES encounters(id);
