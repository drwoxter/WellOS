-- Patient access, arrival, triage, care-team assignment and internal alerts.
--
-- A visit is the operational episode from registration/appointment through
-- arrival, triage, assignment and consultation. Its status follows the
-- explicit state machine in wellos_domain::triage (scheduled → arrived →
-- triage_in_progress → ready_for_consultation → in_consultation → completed,
-- with cancelled / no_show terminals). Every transition is server-enforced
-- under a row lock and bound to the caller's expected version.

-- Explicit destination queues per facility. Unassigned patients route here
-- so nobody silently disappears from every worklist.
CREATE TABLE service_queues (
    id          uuid PRIMARY KEY,
    tenant_id   uuid NOT NULL REFERENCES tenants(id),
    facility_id uuid NOT NULL REFERENCES facilities(id),
    code        text NOT NULL, -- general_medicine | emergency | nursing | telehealth
    name        text NOT NULL,
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, facility_id, code)
);

CREATE TABLE visits (
    id                      uuid PRIMARY KEY,
    tenant_id               uuid NOT NULL REFERENCES tenants(id),
    facility_id             uuid NOT NULL REFERENCES facilities(id),
    patient_id              uuid NOT NULL REFERENCES patients(id),
    status                  text NOT NULL,
    arrival_kind            text NOT NULL, -- scheduled | walk_in | urgent | remote
    service                 text NOT NULL, -- requested service code
    reason                  text,
    scheduled_at            timestamptz,
    arrived_at              timestamptz,
    triage_started_at       timestamptz,
    ready_at                timestamptz,
    consultation_started_at timestamptz,
    completed_at            timestamptz,
    closed_at               timestamptz,   -- cancelled / no_show
    closed_reason           text,
    -- Operational priority set at triage (never a diagnosis).
    priority                text,
    handoff_summary         text,
    encounter_id            uuid REFERENCES encounters(id),
    version                 bigint NOT NULL DEFAULT 1,
    created_by              uuid NOT NULL REFERENCES users(id),
    created_at              timestamptz NOT NULL DEFAULT now(),
    updated_at              timestamptz NOT NULL DEFAULT now()
);
-- A patient can be present in at most one open visit at a time.
CREATE UNIQUE INDEX visits_one_open_per_patient ON visits (tenant_id, patient_id)
    WHERE status IN ('arrived', 'triage_in_progress', 'ready_for_consultation', 'in_consultation');
CREATE INDEX visits_facility_status ON visits (tenant_id, facility_id, status, arrived_at);
CREATE INDEX visits_scheduled ON visits (tenant_id, facility_id, scheduled_at) WHERE status = 'scheduled';
CREATE UNIQUE INDEX visits_encounter ON visits (tenant_id, encounter_id) WHERE encounter_id IS NOT NULL;

-- Care-team membership is explicit and patient-specific. Holding a system
-- role (e.g. physician) never implies membership; membership never grants a
-- system permission. Exactly one of assignee_user_id / queue_id is set.
CREATE TABLE care_team_assignments (
    id               uuid PRIMARY KEY,
    tenant_id        uuid NOT NULL REFERENCES tenants(id),
    facility_id      uuid NOT NULL REFERENCES facilities(id),
    patient_id       uuid NOT NULL REFERENCES patients(id),
    visit_id         uuid REFERENCES visits(id),
    assignee_user_id uuid REFERENCES users(id),
    queue_id         uuid REFERENCES service_queues(id),
    -- treating_physician | triage_nurse | care_coordinator | nursing | ...
    function         text NOT NULL,
    active           boolean NOT NULL DEFAULT true,
    starts_at        timestamptz NOT NULL DEFAULT now(),
    ends_at          timestamptz,
    source           text NOT NULL, -- registration | triage | handoff | seed
    assigned_by      uuid NOT NULL REFERENCES users(id),
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    CHECK ((assignee_user_id IS NULL) <> (queue_id IS NULL))
);
CREATE INDEX care_team_patient ON care_team_assignments (tenant_id, patient_id) WHERE active;
CREATE INDEX care_team_assignee ON care_team_assignments (tenant_id, assignee_user_id) WHERE active;
CREATE INDEX care_team_queue ON care_team_assignments (tenant_id, queue_id) WHERE active;
CREATE INDEX care_team_visit ON care_team_assignments (tenant_id, visit_id);

-- Triage vitals belong to the visit, before any encounter exists.
ALTER TABLE vital_signs
    ALTER COLUMN encounter_id DROP NOT NULL,
    ADD COLUMN visit_id uuid REFERENCES visits(id),
    ADD CONSTRAINT vital_signs_context CHECK (encounter_id IS NOT NULL OR visit_id IS NOT NULL);
CREATE INDEX vital_signs_visit ON vital_signs (tenant_id, visit_id) WHERE visit_id IS NOT NULL;

-- One editable triage assessment per visit; completing triage freezes it.
-- The deterministic safety floor and the rules that produced it are stored
-- with the human-decided priority so precedence is auditable.
CREATE TABLE triage_assessments (
    id                uuid PRIMARY KEY,
    tenant_id         uuid NOT NULL REFERENCES tenants(id),
    visit_id          uuid NOT NULL REFERENCES visits(id),
    patient_id        uuid NOT NULL REFERENCES patients(id),
    author_id         uuid NOT NULL REFERENCES users(id),
    version           bigint NOT NULL DEFAULT 1,
    reason            text,
    concerns          jsonb NOT NULL DEFAULT '[]'::jsonb,
    onset             text,
    red_flags         jsonb NOT NULL DEFAULT '[]'::jsonb,
    note              text,
    vital_signs_id    uuid REFERENCES vital_signs(id),
    priority          text,
    safety_floor      text NOT NULL DEFAULT 'non_urgent',
    safety_rules      jsonb NOT NULL DEFAULT '[]'::jsonb,
    rules_version     text NOT NULL,
    requested_service text,
    -- Human decision on the latest dMind proposal: accepted | overridden |
    -- rejected. Null when no proposal was reviewed.
    ai_artifact_id    uuid REFERENCES ai_artifacts(id),
    ai_decision       text,
    completed_at      timestamptz,
    created_at        timestamptz NOT NULL DEFAULT now(),
    updated_at        timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, visit_id)
);

-- Triage proposals are AIArtifacts bound to the visit and the exact triage
-- assessment version they were generated from.
ALTER TABLE ai_artifacts
    ADD COLUMN visit_id uuid REFERENCES visits(id),
    ADD COLUMN triage_version bigint;
CREATE INDEX ai_artifacts_visit ON ai_artifacts (tenant_id, visit_id) WHERE visit_id IS NOT NULL;

-- Directed internal work items. Each targets exactly one professional or one
-- explicit queue; nothing here sends SMS, email or push. Alerts resolve
-- automatically when the visit moves on.
CREATE TABLE internal_alerts (
    id              uuid PRIMARY KEY,
    tenant_id       uuid NOT NULL REFERENCES tenants(id),
    facility_id     uuid NOT NULL REFERENCES facilities(id),
    patient_id      uuid NOT NULL REFERENCES patients(id),
    visit_id        uuid NOT NULL REFERENCES visits(id),
    kind            text NOT NULL, -- patient_ready | urgent_arrival
    priority        text NOT NULL,
    target_user_id  uuid REFERENCES users(id),
    target_queue_id uuid REFERENCES service_queues(id),
    status          text NOT NULL DEFAULT 'open', -- open | acknowledged | resolved
    acknowledged_by uuid REFERENCES users(id),
    acknowledged_at timestamptz,
    resolved_at     timestamptz,
    created_by      uuid NOT NULL REFERENCES users(id),
    created_at      timestamptz NOT NULL DEFAULT now(),
    CHECK ((target_user_id IS NULL) <> (target_queue_id IS NULL))
);
CREATE INDEX internal_alerts_user ON internal_alerts (tenant_id, target_user_id, created_at)
    WHERE status <> 'resolved';
CREATE INDEX internal_alerts_queue ON internal_alerts (tenant_id, target_queue_id, created_at)
    WHERE status <> 'resolved';
CREATE INDEX internal_alerts_visit ON internal_alerts (tenant_id, visit_id);
