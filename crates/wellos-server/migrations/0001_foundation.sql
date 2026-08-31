-- WellOS foundation schema.
-- Forward-only. All timestamps are timestamptz. All ids are UUIDv7 (opaque,
-- sortable). Tenant isolation is enforced in every query; UI filtering is
-- never a security boundary.

CREATE TABLE tenants (
    id          uuid PRIMARY KEY,
    cell        text NOT NULL,
    name        text NOT NULL,
    -- Brand Studio tokens (colors, logos, naming). Safety semantics
    -- (severity colors) are not tenant-configurable.
    brand       jsonb NOT NULL DEFAULT '{}'::jsonb,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE facilities (
    id          uuid PRIMARY KEY,
    tenant_id   uuid NOT NULL REFERENCES tenants(id),
    name        text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE users (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL REFERENCES tenants(id),
    username     text NOT NULL UNIQUE,
    display_name text NOT NULL,
    -- Nonhuman principals (service agents) are users with is_service = true.
    is_service   boolean NOT NULL DEFAULT false,
    created_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE role_assignments (
    id          uuid PRIMARY KEY,
    tenant_id   uuid NOT NULL REFERENCES tenants(id),
    user_id     uuid NOT NULL REFERENCES users(id),
    role        text NOT NULL,
    facility_id uuid REFERENCES facilities(id),
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (user_id, role, facility_id)
);

CREATE TABLE patients (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL REFERENCES tenants(id),
    facility_id  uuid NOT NULL REFERENCES facilities(id),
    -- Synthetic demographic data only. No real PHI in this repository.
    family_name  text NOT NULL,
    given_name   text NOT NULL,
    birth_date   date NOT NULL,
    sex          text NOT NULL,
    identifier   text NOT NULL, -- synthetic MRN
    created_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, identifier)
);
CREATE INDEX patients_tenant_name ON patients (tenant_id, family_name, given_name);

CREATE TABLE allergies (
    id          uuid PRIMARY KEY,
    tenant_id   uuid NOT NULL REFERENCES tenants(id),
    patient_id  uuid NOT NULL REFERENCES patients(id),
    substance   text NOT NULL,
    criticality text NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE medications (
    id          uuid PRIMARY KEY,
    tenant_id   uuid NOT NULL REFERENCES tenants(id),
    patient_id  uuid NOT NULL REFERENCES patients(id),
    name        text NOT NULL,
    status      text NOT NULL DEFAULT 'active',
    recorded_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE conditions (
    id          uuid PRIMARY KEY,
    tenant_id   uuid NOT NULL REFERENCES tenants(id),
    patient_id  uuid NOT NULL REFERENCES patients(id),
    code        text NOT NULL,      -- e.g. ICD-10 / SNOMED code (synthetic)
    display     text NOT NULL,
    clinical_status text NOT NULL DEFAULT 'active',
    recorded_at timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE encounters (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL REFERENCES tenants(id),
    facility_id  uuid NOT NULL REFERENCES facilities(id),
    patient_id   uuid NOT NULL REFERENCES patients(id),
    practitioner_id uuid NOT NULL REFERENCES users(id),
    status       text NOT NULL DEFAULT 'in_progress',
    started_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX encounters_patient ON encounters (tenant_id, patient_id);

CREATE TABLE service_requests (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL REFERENCES tenants(id),
    encounter_id uuid NOT NULL REFERENCES encounters(id),
    patient_id   uuid NOT NULL REFERENCES patients(id),
    requester_id uuid NOT NULL REFERENCES users(id),
    code_loinc   text NOT NULL,
    display      text NOT NULL,
    status       text NOT NULL DEFAULT 'active',
    -- Closed-loop state: ordered -> received -> reviewed -> notified -> closed
    loop_state   text NOT NULL DEFAULT 'ordered',
    version      bigint NOT NULL DEFAULT 1, -- optimistic concurrency
    created_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX service_requests_worklist ON service_requests (tenant_id, loop_state, created_at);

-- Observations are append-only. Amendments create a new row referencing the
-- amended row; clinical history is never destructively overwritten.
CREATE TABLE observations (
    id              uuid PRIMARY KEY,
    tenant_id       uuid NOT NULL REFERENCES tenants(id),
    service_request_id uuid NOT NULL REFERENCES service_requests(id),
    patient_id      uuid NOT NULL REFERENCES patients(id),
    code_loinc      text NOT NULL,
    value_num       numeric NOT NULL,
    unit            text NOT NULL,
    reference_range text,
    status          text NOT NULL DEFAULT 'final', -- final | amended-superseded | corrected
    amends          uuid REFERENCES observations(id),
    source_system   text NOT NULL,
    idempotency_key text NOT NULL,
    effective_at    timestamptz NOT NULL,
    received_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, idempotency_key)
);
CREATE INDEX observations_sr ON observations (tenant_id, service_request_id);

CREATE TABLE rule_evaluations (
    id             uuid PRIMARY KEY,
    tenant_id      uuid NOT NULL REFERENCES tenants(id),
    observation_id uuid NOT NULL REFERENCES observations(id),
    rule_id        text NOT NULL,
    rule_version   text NOT NULL,
    outcome        jsonb NOT NULL,
    evaluated_at   timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE alerts (
    id             uuid PRIMARY KEY,
    tenant_id      uuid NOT NULL REFERENCES tenants(id),
    patient_id     uuid NOT NULL REFERENCES patients(id),
    observation_id uuid NOT NULL REFERENCES observations(id),
    severity       text NOT NULL,
    message        text NOT NULL,
    status         text NOT NULL DEFAULT 'open',
    created_at     timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE follow_up_tasks (
    id                 uuid PRIMARY KEY,
    tenant_id          uuid NOT NULL REFERENCES tenants(id),
    patient_id         uuid NOT NULL REFERENCES patients(id),
    service_request_id uuid NOT NULL REFERENCES service_requests(id),
    description        text NOT NULL,
    priority           text NOT NULL DEFAULT 'routine',
    status             text NOT NULL DEFAULT 'open',
    due_at             timestamptz,
    completed_by       uuid REFERENCES users(id),
    created_at         timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE ai_artifacts (
    id                 uuid PRIMARY KEY,
    tenant_id          uuid NOT NULL REFERENCES tenants(id),
    patient_id         uuid NOT NULL REFERENCES patients(id),
    service_request_id uuid NOT NULL REFERENCES service_requests(id),
    observation_id     uuid REFERENCES observations(id),
    artifact_type      text NOT NULL,
    autonomy_level     text NOT NULL,
    status             text NOT NULL,
    model              text,
    model_version      text,
    route              text,
    template           text,
    input_hash         text,
    output             jsonb,          -- validated structured output
    output_schema      text,
    citations          jsonb NOT NULL DEFAULT '[]'::jsonb,
    limitations        jsonb NOT NULL DEFAULT '[]'::jsonb,
    reviewer_id        uuid REFERENCES users(id),
    review_decision    text,
    review_note        text,
    reviewed_at        timestamptz,
    generated_at       timestamptz,
    created_at         timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE consents (
    id          uuid PRIMARY KEY,
    tenant_id   uuid NOT NULL REFERENCES tenants(id),
    patient_id  uuid NOT NULL REFERENCES patients(id),
    -- purpose e.g. 'ai_external_processing', 'care_delivery'
    purpose     text NOT NULL,
    status      text NOT NULL, -- active | revoked
    version     int NOT NULL DEFAULT 1,
    recorded_at timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, patient_id, purpose)
);

CREATE TABLE data_quality_issues (
    id             uuid PRIMARY KEY,
    tenant_id      uuid NOT NULL REFERENCES tenants(id),
    resource_type  text NOT NULL,
    resource_id    uuid NOT NULL,
    issue          text NOT NULL,
    created_at     timestamptz NOT NULL DEFAULT now()
);

-- Append-only audit log. No UPDATE/DELETE is ever issued by the application;
-- suitable for export to WORM storage.
CREATE TABLE audit_events (
    id             uuid PRIMARY KEY,
    tenant_id      uuid NOT NULL,
    actor          text NOT NULL,
    action         text NOT NULL,
    resource_type  text,
    resource_id    text,
    decision       text NOT NULL, -- allow | deny
    reason         text,
    purpose_of_use text,
    break_glass    boolean NOT NULL DEFAULT false,
    break_glass_reason text,
    correlation_id uuid,
    recorded_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX audit_tenant_time ON audit_events (tenant_id, recorded_at);

-- Transactional outbox: events are written in the same transaction as state
-- changes and dispatched by a relay. Payloads contain ids/metadata only.
CREATE TABLE outbox_events (
    id             uuid PRIMARY KEY,
    event_type     text NOT NULL,
    schema_version text NOT NULL,
    tenant_id      uuid NOT NULL,
    cell           text NOT NULL,
    actor          text NOT NULL,
    correlation_id uuid NOT NULL,
    causation_id   uuid,
    occurred_at    timestamptz NOT NULL,
    recorded_at    timestamptz NOT NULL DEFAULT now(),
    source         text NOT NULL,
    resource_refs  jsonb NOT NULL,
    dispatched_at  timestamptz
);
CREATE INDEX outbox_pending ON outbox_events (recorded_at) WHERE dispatched_at IS NULL;

CREATE TABLE break_glass_events (
    id          uuid PRIMARY KEY,
    tenant_id   uuid NOT NULL REFERENCES tenants(id),
    user_id     uuid NOT NULL REFERENCES users(id),
    patient_id  uuid NOT NULL,
    reason      text NOT NULL,
    reviewed    boolean NOT NULL DEFAULT false,
    created_at  timestamptz NOT NULL DEFAULT now()
);
