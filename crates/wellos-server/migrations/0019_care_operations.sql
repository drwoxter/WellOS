-- Care Operations & Command Center v1: operational authority for episodes,
-- placements, multidisciplinary coordination, perioperative cases, workforce
-- coverage and deterministic operational forecasts.
--
-- Additive. Nothing here copies clinical content: episodes *link* the existing
-- visits, encounters, appointments, service requests, tasks, alerts and care
-- team assignments. Position occupancy and holds are mutually exclusive at the
-- database level (range exclusion). Histories are append-only by trigger.
-- Catalog kinds are extended so location types, episode types, capabilities,
-- barrier categories and procedures stay tenant-configurable data.
--
-- Rollback (manual, destructive to operational history only):
--   DROP TABLE care_operations_forecasts, workforce_coverage_requirements,
--        workforce_shift_absences, workforce_shift_assignments, workforce_shifts,
--        perioperative_case_history, perioperative_cases, theatre_sessions,
--        care_escalations, care_handoffs, care_coordination_task_history,
--        care_coordination_task_dependencies, care_coordination_tasks,
--        placement_history, placements, placement_candidates, placement_requests,
--        care_episode_history, care_episode_links, care_episodes,
--        operational_location_periods, operational_location_history,
--        operational_locations;
--   DROP FUNCTION care_ops_forbid_mutation();
--   ALTER TABLE ai_artifacts DROP COLUMN care_episode_id, DROP COLUMN placement_request_id,
--        DROP COLUMN perioperative_case_id, DROP COLUMN care_operations_forecast_id,
--        DROP COLUMN care_handoff_id, DROP COLUMN workforce_shift_id;
--   restore the catalog_entries kind, notifications kind and
--   ai_artifacts_patient_required CHECK constraints.

CREATE FUNCTION care_ops_forbid_mutation() RETURNS trigger
LANGUAGE plpgsql AS $$
BEGIN
    RAISE EXCEPTION 'care operations history table % is append-only', TG_TABLE_NAME
        USING ERRCODE = 'integrity_constraint_violation';
END $$;

-- ---------------------------------------------------------------------------
-- 1. Runtime-configurable vocabularies (no closed enums in code)
-- ---------------------------------------------------------------------------

ALTER TABLE catalog_entries DROP CONSTRAINT catalog_entries_kind_check;
ALTER TABLE catalog_entries ADD CONSTRAINT catalog_entries_kind_check CHECK (kind IN (
    'clinical_service', 'specialty', 'profession',
    'modality', 'resource_type', 'accessibility_capability',
    'location', 'transport_resource', 'diagnostic_orderable',
    'operational_location_type', 'episode_type', 'age_group',
    'isolation_capability', 'equipment_capability', 'professional_capability',
    'barrier_category', 'procedure', 'shift_pattern'));

-- ---------------------------------------------------------------------------
-- 2. Operational location hierarchy and positions
--    facility -> department/service area -> unit/ward -> room/bay -> position
-- ---------------------------------------------------------------------------

CREATE TABLE operational_locations (
    id                     uuid PRIMARY KEY,
    tenant_id              uuid NOT NULL REFERENCES tenants(id),
    facility_id            uuid NOT NULL REFERENCES facilities(id),
    parent_id              uuid REFERENCES operational_locations(id),
    -- Structural level; the *type* (ward, bay, bed, chair, theatre, treatment
    -- position, ...) is a tenant catalog code of kind operational_location_type.
    level                  text NOT NULL CHECK (level IN ('department', 'unit', 'room', 'position')),
    type_code              text NOT NULL,
    code                   text NOT NULL CHECK (code ~ '^[A-Za-z0-9][A-Za-z0-9_.-]{0,63}$'),
    name_en                text NOT NULL,
    name_es                text NOT NULL,
    specialty_codes        text[] NOT NULL DEFAULT '{}',
    service_line_codes     text[] NOT NULL DEFAULT '{}',
    -- [{"profession":"nurse","capabilities":["paediatric_nursing"],"minimum":1}]
    profession_requirements jsonb NOT NULL DEFAULT '[]'::jsonb,
    age_group_codes        text[] NOT NULL DEFAULT '{}',
    isolation_codes        text[] NOT NULL DEFAULT '{}',
    accessibility_codes    text[] NOT NULL DEFAULT '{}',
    equipment_codes        text[] NOT NULL DEFAULT '{}',
    -- Room compatibility policy (single, same_sex, cohort_isolation, mixed).
    room_policy            text NOT NULL DEFAULT 'mixed'
                           CHECK (room_policy IN ('mixed', 'single', 'same_sex', 'cohort_isolation')),
    capacity               int NOT NULL DEFAULT 1 CHECK (capacity BETWEEN 0 AND 10000),
    operational_status     text NOT NULL DEFAULT 'open'
                           CHECK (operational_status IN ('open', 'closed', 'maintenance')),
    -- Position-level occupancy state; NULL above position level.
    position_state         text CHECK (position_state IN
                           ('available', 'held', 'occupied', 'cleaning', 'blocked', 'out_of_service')),
    cleaning_minutes       int NOT NULL DEFAULT 30 CHECK (cleaning_minutes BETWEEN 0 AND 1440),
    -- Optional link to the scheduling engine resource (theatres, chairs).
    schedulable_resource_id uuid REFERENCES schedulable_resources(id),
    effective_from         date,
    effective_to           date,
    active                 boolean NOT NULL DEFAULT true,
    metadata               jsonb NOT NULL DEFAULT '{}'::jsonb,
    version                bigint NOT NULL DEFAULT 1,
    created_by             uuid NOT NULL REFERENCES users(id),
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, facility_id, code),
    CHECK ((level = 'position') = (position_state IS NOT NULL)),
    CHECK (effective_to IS NULL OR effective_from IS NULL OR effective_to >= effective_from)
);
CREATE INDEX operational_locations_parent ON operational_locations (parent_id) WHERE parent_id IS NOT NULL;
CREATE INDEX operational_locations_facility_level ON operational_locations (tenant_id, facility_id, level, active);
CREATE INDEX operational_locations_state ON operational_locations (tenant_id, facility_id, position_state)
    WHERE level = 'position';

CREATE TABLE operational_location_history (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL REFERENCES tenants(id),
    location_id  uuid NOT NULL REFERENCES operational_locations(id),
    from_state   text,
    to_state     text NOT NULL,
    reason_code  text,
    note         text,
    actor        text NOT NULL,
    version      bigint NOT NULL,
    recorded_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX operational_location_history_location ON operational_location_history (location_id, recorded_at);
CREATE TRIGGER operational_location_history_append_only BEFORE UPDATE OR DELETE ON operational_location_history
    FOR EACH ROW EXECUTE FUNCTION care_ops_forbid_mutation();

-- Temporary closures, maintenance, out-of-service and cleaning windows at any
-- level of the hierarchy (a unit closure blocks every position beneath it).
CREATE TABLE operational_location_periods (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL REFERENCES tenants(id),
    location_id  uuid NOT NULL REFERENCES operational_locations(id),
    kind         text NOT NULL CHECK (kind IN ('closure', 'maintenance', 'out_of_service', 'cleaning')),
    starts_at    timestamptz NOT NULL,
    ends_at      timestamptz,
    reason_code  text,
    active       boolean NOT NULL DEFAULT true,
    created_by   uuid NOT NULL REFERENCES users(id),
    created_at   timestamptz NOT NULL DEFAULT now(),
    deactivated_at timestamptz,
    CHECK (ends_at IS NULL OR ends_at > starts_at)
);
CREATE INDEX operational_location_periods_window ON operational_location_periods (location_id, starts_at, ends_at)
    WHERE active;

-- ---------------------------------------------------------------------------
-- 3. Care episodes (operational grouping authority, links only)
-- ---------------------------------------------------------------------------

CREATE TABLE care_episodes (
    id                     uuid PRIMARY KEY,
    tenant_id              uuid NOT NULL REFERENCES tenants(id),
    facility_id            uuid NOT NULL REFERENCES facilities(id),
    patient_id             uuid NOT NULL REFERENCES patients(id),
    -- Tenant catalog code of kind episode_type (emergency, inpatient, ...).
    episode_type_code      text NOT NULL,
    status                 text NOT NULL DEFAULT 'planned' CHECK (status IN (
                           'planned', 'active', 'discharge_preparation', 'discharge_ready',
                           'completed', 'cancelled', 'error')),
    responsible_service_code text,
    responsible_queue_id   uuid REFERENCES service_queues(id),
    responsible_user_id    uuid REFERENCES users(id),
    current_placement_id   uuid,
    started_at             timestamptz,
    expected_end_at        timestamptz,
    ended_at               timestamptz,
    priority               text NOT NULL DEFAULT 'routine'
                           CHECK (priority IN ('routine', 'priority', 'urgent', 'emergency')),
    -- [{"code":"mobilise","text_en":...,"text_es":...,"owner_profession":...,"status":"open|met"}]
    goals                  jsonb NOT NULL DEFAULT '[]'::jsonb,
    -- [{"category_code":"transport","text_en":...,"text_es":...,"status":"open|resolved"}]
    barriers               jsonb NOT NULL DEFAULT '[]'::jsonb,
    provenance             jsonb NOT NULL DEFAULT '{}'::jsonb,
    version                bigint NOT NULL DEFAULT 1,
    created_by             uuid NOT NULL REFERENCES users(id),
    created_at             timestamptz NOT NULL DEFAULT now(),
    updated_at             timestamptz NOT NULL DEFAULT now(),
    CHECK (expected_end_at IS NULL OR started_at IS NULL OR expected_end_at >= started_at)
);
CREATE INDEX care_episodes_patient ON care_episodes (tenant_id, patient_id, created_at DESC);
CREATE INDEX care_episodes_facility_status ON care_episodes (tenant_id, facility_id, status);
-- One non-terminal episode per patient and type at a time.
CREATE UNIQUE INDEX care_episodes_one_open_per_type ON care_episodes (tenant_id, patient_id, episode_type_code)
    WHERE status IN ('planned', 'active', 'discharge_preparation', 'discharge_ready');

CREATE TABLE care_episode_links (
    id          uuid PRIMARY KEY,
    tenant_id   uuid NOT NULL REFERENCES tenants(id),
    episode_id  uuid NOT NULL REFERENCES care_episodes(id),
    link_type   text NOT NULL CHECK (link_type IN (
                'visit', 'encounter', 'appointment', 'service_request', 'follow_up_task',
                'internal_alert', 'care_team_assignment', 'transport_request', 'diagnostic_report')),
    link_id     uuid NOT NULL,
    created_by  uuid NOT NULL REFERENCES users(id),
    created_at  timestamptz NOT NULL DEFAULT now(),
    UNIQUE (episode_id, link_type, link_id)
);
CREATE INDEX care_episode_links_target ON care_episode_links (tenant_id, link_type, link_id);

CREATE TABLE care_episode_history (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL REFERENCES tenants(id),
    episode_id   uuid NOT NULL REFERENCES care_episodes(id),
    from_status  text,
    to_status    text NOT NULL,
    change_kind  text NOT NULL,
    reason_code  text,
    note         text,
    actor        text NOT NULL,
    version      bigint NOT NULL,
    recorded_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX care_episode_history_episode ON care_episode_history (episode_id, recorded_at);
CREATE TRIGGER care_episode_history_append_only BEFORE UPDATE OR DELETE ON care_episode_history
    FOR EACH ROW EXECUTE FUNCTION care_ops_forbid_mutation();

-- ---------------------------------------------------------------------------
-- 4. Placement workflow: request -> candidates -> hold -> occupancy -> release
-- ---------------------------------------------------------------------------

CREATE TABLE placement_requests (
    id                 uuid PRIMARY KEY,
    tenant_id          uuid NOT NULL REFERENCES tenants(id),
    facility_id        uuid NOT NULL REFERENCES facilities(id),
    episode_id         uuid NOT NULL REFERENCES care_episodes(id),
    patient_id         uuid NOT NULL REFERENCES patients(id),
    kind               text NOT NULL DEFAULT 'admission' CHECK (kind IN ('admission', 'transfer')),
    status             text NOT NULL DEFAULT 'requested' CHECK (status IN (
                       'requested', 'candidates_ready', 'held', 'placed', 'cancelled', 'expired')),
    priority           text NOT NULL DEFAULT 'routine'
                       CHECK (priority IN ('routine', 'priority', 'urgent', 'emergency')),
    -- Deterministic constraints: {"service_code","specialty_codes","age_group_code",
    --  "isolation_codes","accessibility_codes","equipment_codes","room_policy",
    --  "sex","preferred_unit_ids","earliest_from"}
    requirements       jsonb NOT NULL DEFAULT '{}'::jsonb,
    requested_from     timestamptz NOT NULL DEFAULT now(),
    reason_code        text,
    note               text,
    matcher_version    text,
    facts_hash         text,
    ranking_artifact_id uuid REFERENCES ai_artifacts(id),
    version            bigint NOT NULL DEFAULT 1,
    created_by         uuid NOT NULL REFERENCES users(id),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX placement_requests_episode ON placement_requests (episode_id, created_at DESC);
CREATE INDEX placement_requests_open ON placement_requests (tenant_id, facility_id, status, requested_from)
    WHERE status IN ('requested', 'candidates_ready', 'held');

-- Every proposed (and rejected) candidate with its decomposition and facts.
CREATE TABLE placement_candidates (
    id               uuid PRIMARY KEY,
    tenant_id        uuid NOT NULL REFERENCES tenants(id),
    request_id       uuid NOT NULL REFERENCES placement_requests(id),
    location_id      uuid NOT NULL REFERENCES operational_locations(id),
    matcher_version  text NOT NULL,
    generation       int NOT NULL CHECK (generation >= 1),
    rank             int CHECK (rank IS NULL OR rank >= 1),
    feasible         boolean NOT NULL,
    score            double precision,
    score_factors    jsonb NOT NULL DEFAULT '[]'::jsonb,
    rejection_codes  text[] NOT NULL DEFAULT '{}',
    source_facts     jsonb NOT NULL DEFAULT '{}'::jsonb,
    facts_hash       text NOT NULL,
    created_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (request_id, generation, location_id)
);
CREATE INDEX placement_candidates_request ON placement_candidates (request_id, generation, rank);

CREATE TABLE placements (
    id            uuid PRIMARY KEY,
    tenant_id     uuid NOT NULL REFERENCES tenants(id),
    facility_id   uuid NOT NULL REFERENCES facilities(id),
    episode_id    uuid NOT NULL REFERENCES care_episodes(id),
    request_id    uuid REFERENCES placement_requests(id),
    candidate_id  uuid REFERENCES placement_candidates(id),
    location_id   uuid NOT NULL REFERENCES operational_locations(id),
    patient_id    uuid NOT NULL REFERENCES patients(id),
    status        text NOT NULL CHECK (status IN ('held', 'occupied', 'released', 'expired', 'revoked')),
    starts_at     timestamptz NOT NULL,
    ends_at       timestamptz,
    expires_at    timestamptz,
    facts_hash    text,
    version       bigint NOT NULL DEFAULT 1,
    created_by    uuid NOT NULL REFERENCES users(id),
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_at IS NULL OR ends_at >= starts_at),
    CHECK (status <> 'held' OR expires_at IS NOT NULL),
    -- A position holds at most one live hold or occupancy at any instant.
    EXCLUDE USING gist (
        location_id WITH =,
        tstzrange(starts_at, COALESCE(ends_at, 'infinity'::timestamptz), '[)') WITH &&
    ) WHERE (status IN ('held', 'occupied'))
);
CREATE INDEX placements_episode ON placements (episode_id, starts_at DESC);
CREATE INDEX placements_location_live ON placements (location_id) WHERE status IN ('held', 'occupied');
CREATE INDEX placements_hold_expiry ON placements (expires_at) WHERE status = 'held';
ALTER TABLE care_episodes
    ADD CONSTRAINT care_episodes_current_placement_fkey
        FOREIGN KEY (current_placement_id) REFERENCES placements(id);

CREATE TABLE placement_history (
    id            uuid PRIMARY KEY,
    tenant_id     uuid NOT NULL REFERENCES tenants(id),
    placement_id  uuid REFERENCES placements(id),
    request_id    uuid REFERENCES placement_requests(id),
    from_status   text,
    to_status     text NOT NULL,
    reason_code   text,
    note          text,
    actor         text NOT NULL,
    version       bigint NOT NULL,
    recorded_at   timestamptz NOT NULL DEFAULT now(),
    CHECK (placement_id IS NOT NULL OR request_id IS NOT NULL)
);
CREATE INDEX placement_history_placement ON placement_history (placement_id, recorded_at) WHERE placement_id IS NOT NULL;
CREATE INDEX placement_history_request ON placement_history (request_id, recorded_at) WHERE request_id IS NOT NULL;
CREATE TRIGGER placement_history_append_only BEFORE UPDATE OR DELETE ON placement_history
    FOR EACH ROW EXECUTE FUNCTION care_ops_forbid_mutation();

-- ---------------------------------------------------------------------------
-- 5. Multidisciplinary coordination: episode tasks, dependencies, handoffs
--    (accountability reuses users, service_queues and care_team_assignments;
--    laboratory follow_up_tasks stay the result-loop authority and are linked)
-- ---------------------------------------------------------------------------

CREATE TABLE care_coordination_tasks (
    id                    uuid PRIMARY KEY,
    tenant_id             uuid NOT NULL REFERENCES tenants(id),
    facility_id           uuid NOT NULL REFERENCES facilities(id),
    episode_id            uuid NOT NULL REFERENCES care_episodes(id),
    patient_id            uuid NOT NULL REFERENCES patients(id),
    title_en              text NOT NULL CHECK (char_length(title_en) <= 200),
    title_es              text NOT NULL CHECK (char_length(title_es) <= 200),
    kind                  text NOT NULL DEFAULT 'coordination'
                          CHECK (kind IN ('coordination', 'discharge_preparation', 'transfer', 'assessment',
                                          'education', 'equipment', 'transport', 'documentation', 'other')),
    status                text NOT NULL DEFAULT 'open'
                          CHECK (status IN ('open', 'in_progress', 'blocked', 'handed_off', 'completed', 'cancelled')),
    accountable_user_id   uuid REFERENCES users(id),
    accountable_queue_id  uuid REFERENCES service_queues(id),
    care_team_assignment_id uuid REFERENCES care_team_assignments(id),
    profession_code       text,
    capability_codes      text[] NOT NULL DEFAULT '{}',
    due_at                timestamptz,
    escalate_at           timestamptz,
    barrier_category_code text,
    patient_visible       boolean NOT NULL DEFAULT false,
    -- Patient-facing wording is separate and only shown when patient_visible.
    patient_text_en       text,
    patient_text_es       text,
    completion_evidence   jsonb,
    completed_at          timestamptz,
    completed_by          uuid REFERENCES users(id),
    version               bigint NOT NULL DEFAULT 1,
    created_by            uuid NOT NULL REFERENCES users(id),
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    CHECK (accountable_user_id IS NOT NULL OR accountable_queue_id IS NOT NULL),
    CHECK (escalate_at IS NULL OR due_at IS NULL OR escalate_at >= due_at)
);
CREATE INDEX care_coordination_tasks_episode ON care_coordination_tasks (episode_id, status, due_at);
CREATE INDEX care_coordination_tasks_due ON care_coordination_tasks (tenant_id, facility_id, status, due_at)
    WHERE status IN ('open', 'in_progress', 'blocked');

CREATE TABLE care_coordination_task_dependencies (
    task_id        uuid NOT NULL REFERENCES care_coordination_tasks(id),
    depends_on_id  uuid NOT NULL REFERENCES care_coordination_tasks(id),
    PRIMARY KEY (task_id, depends_on_id),
    CHECK (task_id <> depends_on_id)
);

CREATE TABLE care_coordination_task_history (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL REFERENCES tenants(id),
    task_id      uuid NOT NULL REFERENCES care_coordination_tasks(id),
    from_status  text,
    to_status    text NOT NULL,
    change_kind  text NOT NULL,
    reason_code  text,
    note         text,
    actor        text NOT NULL,
    version      bigint NOT NULL,
    recorded_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX care_coordination_task_history_task ON care_coordination_task_history (task_id, recorded_at);
CREATE TRIGGER care_coordination_task_history_append_only BEFORE UPDATE OR DELETE ON care_coordination_task_history
    FOR EACH ROW EXECUTE FUNCTION care_ops_forbid_mutation();

-- Durable shift handoffs; AI drafts stay drafts until a human accepts them.
CREATE TABLE care_handoffs (
    id            uuid PRIMARY KEY,
    tenant_id     uuid NOT NULL REFERENCES tenants(id),
    facility_id   uuid NOT NULL REFERENCES facilities(id),
    episode_id    uuid NOT NULL REFERENCES care_episodes(id),
    patient_id    uuid NOT NULL REFERENCES patients(id),
    shift_label   text NOT NULL CHECK (char_length(shift_label) <= 80),
    language      text NOT NULL DEFAULT 'en' CHECK (language IN ('en', 'es')),
    status        text NOT NULL DEFAULT 'draft'
                  CHECK (status IN ('draft', 'accepted', 'rejected', 'superseded')),
    -- Structured sections (situation, background, pending, risks, barriers,
    -- next steps); never free clinical narrative copied from notes.
    content       jsonb NOT NULL,
    source_facts  jsonb NOT NULL DEFAULT '{}'::jsonb,
    facts_hash    text NOT NULL,
    origin        text NOT NULL CHECK (origin IN ('human', 'dmind_draft')),
    artifact_id   uuid REFERENCES ai_artifacts(id),
    author_id     uuid NOT NULL REFERENCES users(id),
    decided_by    uuid REFERENCES users(id),
    decided_at    timestamptz,
    decision_note text,
    version       bigint NOT NULL DEFAULT 1,
    created_at    timestamptz NOT NULL DEFAULT now(),
    updated_at    timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX care_handoffs_episode ON care_handoffs (episode_id, created_at DESC);

-- ---------------------------------------------------------------------------
-- 6. Escalation timers (waiting, boarding, placement, discharge delays)
-- ---------------------------------------------------------------------------

CREATE TABLE care_escalations (
    id             uuid PRIMARY KEY,
    tenant_id      uuid NOT NULL REFERENCES tenants(id),
    facility_id    uuid NOT NULL REFERENCES facilities(id),
    patient_id     uuid NOT NULL REFERENCES patients(id),
    episode_id     uuid REFERENCES care_episodes(id),
    visit_id       uuid REFERENCES visits(id),
    placement_request_id uuid REFERENCES placement_requests(id),
    kind           text NOT NULL CHECK (kind IN ('waiting_triage', 'waiting_clinician', 'boarding',
                                                 'placement_delay', 'discharge_delay', 'transport_delay')),
    threshold_minutes int NOT NULL CHECK (threshold_minutes BETWEEN 1 AND 100000),
    started_at     timestamptz NOT NULL,
    due_at         timestamptz NOT NULL,
    status         text NOT NULL DEFAULT 'armed'
                   CHECK (status IN ('armed', 'escalated', 'acknowledged', 'resolved', 'cancelled')),
    escalated_at   timestamptz,
    resolved_at    timestamptz,
    internal_alert_id uuid REFERENCES internal_alerts(id),
    version        bigint NOT NULL DEFAULT 1,
    created_by     uuid NOT NULL REFERENCES users(id),
    created_at     timestamptz NOT NULL DEFAULT now(),
    updated_at     timestamptz NOT NULL DEFAULT now(),
    CHECK (due_at > started_at),
    CHECK (episode_id IS NOT NULL OR visit_id IS NOT NULL)
);
CREATE INDEX care_escalations_due ON care_escalations (tenant_id, status, due_at) WHERE status = 'armed';
CREATE INDEX care_escalations_facility ON care_escalations (tenant_id, facility_id, status, kind);

-- ---------------------------------------------------------------------------
-- 7. Perioperative cases and theatre sessions (time/resource reservations go
--    through resource_bookings on the theatre's schedulable resource)
-- ---------------------------------------------------------------------------

-- Theatre cases book the theatre's schedulable resource through the existing
-- exclusion-constrained bookings table.
ALTER TABLE resource_bookings DROP CONSTRAINT resource_bookings_kind_check;
ALTER TABLE resource_bookings ADD CONSTRAINT resource_bookings_kind_check
    CHECK (kind IN ('hold', 'appointment', 'theatre_case'));

CREATE TABLE theatre_sessions (
    id                   uuid PRIMARY KEY,
    tenant_id            uuid NOT NULL REFERENCES tenants(id),
    facility_id          uuid NOT NULL REFERENCES facilities(id),
    theatre_location_id  uuid NOT NULL REFERENCES operational_locations(id),
    service_code         text,
    specialty_codes      text[] NOT NULL DEFAULT '{}',
    starts_at            timestamptz NOT NULL,
    ends_at              timestamptz NOT NULL,
    turnaround_minutes   int NOT NULL DEFAULT 30 CHECK (turnaround_minutes BETWEEN 0 AND 600),
    status               text NOT NULL DEFAULT 'planned' CHECK (status IN ('planned', 'released', 'cancelled')),
    version              bigint NOT NULL DEFAULT 1,
    created_by           uuid NOT NULL REFERENCES users(id),
    created_at           timestamptz NOT NULL DEFAULT now(),
    updated_at           timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at),
    EXCLUDE USING gist (theatre_location_id WITH =, tstzrange(starts_at, ends_at, '[)') WITH &&)
        WHERE (status = 'planned')
);
CREATE INDEX theatre_sessions_theatre_time ON theatre_sessions (theatre_location_id, starts_at);

CREATE TABLE perioperative_cases (
    id                        uuid PRIMARY KEY,
    tenant_id                 uuid NOT NULL REFERENCES tenants(id),
    facility_id               uuid NOT NULL REFERENCES facilities(id),
    episode_id                uuid NOT NULL REFERENCES care_episodes(id),
    patient_id                uuid NOT NULL REFERENCES patients(id),
    procedure_code            text NOT NULL,
    responsible_service_code  text NOT NULL,
    responsible_user_id       uuid REFERENCES users(id),
    status                    text NOT NULL DEFAULT 'requested' CHECK (status IN (
                              'requested', 'ready_for_scheduling', 'scheduled', 'in_theatre',
                              'completed', 'delayed', 'cancelled')),
    priority                  text NOT NULL DEFAULT 'routine'
                              CHECK (priority IN ('routine', 'priority', 'urgent', 'emergency')),
    expected_duration_minutes int NOT NULL CHECK (expected_duration_minutes BETWEEN 5 AND 1440),
    -- [{"profession":"anaesthetist","capabilities":["paediatric_anaesthesia"],"count":1}]
    required_professionals    jsonb NOT NULL DEFAULT '[]'::jsonb,
    required_equipment_codes  text[] NOT NULL DEFAULT '{}',
    -- Externally supplied prerequisites; never an AI decision.
    -- [{"code":"anaesthetic_review","status":"pending|met|not_required","evidence_ref":...}]
    prerequisites             jsonb NOT NULL DEFAULT '[]'::jsonb,
    required_service_request_ids uuid[] NOT NULL DEFAULT '{}',
    checklist                 jsonb NOT NULL DEFAULT '[]'::jsonb,
    post_procedure_requirements jsonb NOT NULL DEFAULT '{}'::jsonb,
    session_id                uuid REFERENCES theatre_sessions(id),
    theatre_location_id       uuid REFERENCES operational_locations(id),
    scheduled_start           timestamptz,
    scheduled_end             timestamptz,
    booking_id                uuid REFERENCES resource_bookings(id),
    team_user_ids             uuid[] NOT NULL DEFAULT '{}',
    delay_reason_code         text,
    cancellation_reason_code  text,
    override_reason           text,
    version                   bigint NOT NULL DEFAULT 1,
    created_by                uuid NOT NULL REFERENCES users(id),
    created_at                timestamptz NOT NULL DEFAULT now(),
    updated_at                timestamptz NOT NULL DEFAULT now(),
    CHECK (scheduled_end IS NULL OR scheduled_start IS NULL OR scheduled_end > scheduled_start),
    CHECK (status <> 'scheduled' OR (scheduled_start IS NOT NULL AND theatre_location_id IS NOT NULL))
);
CREATE INDEX perioperative_cases_episode ON perioperative_cases (episode_id);
CREATE INDEX perioperative_cases_theatre_time ON perioperative_cases (theatre_location_id, scheduled_start)
    WHERE status IN ('scheduled', 'in_theatre');
CREATE INDEX perioperative_cases_facility_status ON perioperative_cases (tenant_id, facility_id, status, scheduled_start);

CREATE TABLE perioperative_case_history (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL REFERENCES tenants(id),
    case_id      uuid NOT NULL REFERENCES perioperative_cases(id),
    from_status  text,
    to_status    text NOT NULL,
    change_kind  text NOT NULL,
    reason_code  text,
    override     boolean NOT NULL DEFAULT false,
    note         text,
    actor        text NOT NULL,
    version      bigint NOT NULL,
    recorded_at  timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX perioperative_case_history_case ON perioperative_case_history (case_id, recorded_at);
CREATE TRIGGER perioperative_case_history_append_only BEFORE UPDATE OR DELETE ON perioperative_case_history
    FOR EACH ROW EXECUTE FUNCTION care_ops_forbid_mutation();

-- ---------------------------------------------------------------------------
-- 8. Workforce coverage (operational only: no payroll, no HR records)
-- ---------------------------------------------------------------------------

CREATE TABLE workforce_shifts (
    id              uuid PRIMARY KEY,
    tenant_id       uuid NOT NULL REFERENCES tenants(id),
    facility_id     uuid NOT NULL REFERENCES facilities(id),
    unit_location_id uuid REFERENCES operational_locations(id),
    label           text NOT NULL CHECK (char_length(label) <= 80),
    -- Local wall-clock definition; UTC instants derived with the facility zone.
    local_date      date NOT NULL,
    starts_at       timestamptz NOT NULL,
    ends_at         timestamptz NOT NULL,
    status          text NOT NULL DEFAULT 'planned' CHECK (status IN ('planned', 'active', 'closed', 'cancelled')),
    version         bigint NOT NULL DEFAULT 1,
    created_by      uuid NOT NULL REFERENCES users(id),
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at)
);
CREATE INDEX workforce_shifts_facility_date ON workforce_shifts (tenant_id, facility_id, local_date);

-- Minimum configured coverage per shift: profession/capability/count.
CREATE TABLE workforce_coverage_requirements (
    id               uuid PRIMARY KEY,
    tenant_id        uuid NOT NULL REFERENCES tenants(id),
    shift_id         uuid NOT NULL REFERENCES workforce_shifts(id),
    profession_code  text NOT NULL,
    capability_codes text[] NOT NULL DEFAULT '{}',
    specialty_codes  text[] NOT NULL DEFAULT '{}',
    minimum_count    int NOT NULL CHECK (minimum_count BETWEEN 0 AND 1000),
    UNIQUE (shift_id, profession_code, capability_codes, specialty_codes)
);

CREATE TABLE workforce_shift_assignments (
    id               uuid PRIMARY KEY,
    tenant_id        uuid NOT NULL REFERENCES tenants(id),
    shift_id         uuid NOT NULL REFERENCES workforce_shifts(id),
    -- A professional (user) or a team resource; profession/capabilities are
    -- catalog data, never permissions.
    user_id          uuid REFERENCES users(id),
    resource_id      uuid REFERENCES schedulable_resources(id),
    profession_code  text NOT NULL,
    capability_codes text[] NOT NULL DEFAULT '{}',
    specialty_codes  text[] NOT NULL DEFAULT '{}',
    role_in_shift    text NOT NULL DEFAULT 'assigned' CHECK (role_in_shift IN ('assigned', 'on_call', 'float')),
    status           text NOT NULL DEFAULT 'planned'
                     CHECK (status IN ('planned', 'confirmed', 'absent', 'reassigned', 'cancelled')),
    override_reason  text,
    version          bigint NOT NULL DEFAULT 1,
    created_by       uuid NOT NULL REFERENCES users(id),
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    CHECK ((user_id IS NULL) <> (resource_id IS NULL))
);
CREATE INDEX workforce_shift_assignments_shift ON workforce_shift_assignments (shift_id, status);
CREATE INDEX workforce_shift_assignments_user ON workforce_shift_assignments (tenant_id, user_id) WHERE user_id IS NOT NULL;

CREATE TABLE workforce_shift_absences (
    id           uuid PRIMARY KEY,
    tenant_id    uuid NOT NULL REFERENCES tenants(id),
    user_id      uuid NOT NULL REFERENCES users(id),
    starts_at    timestamptz NOT NULL,
    ends_at      timestamptz NOT NULL,
    -- Operational reason category only (no medical or HR detail).
    reason_code  text NOT NULL CHECK (reason_code IN ('planned_leave', 'unplanned_absence', 'training', 'other')),
    created_by   uuid NOT NULL REFERENCES users(id),
    created_at   timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at)
);
CREATE INDEX workforce_shift_absences_user ON workforce_shift_absences (tenant_id, user_id, starts_at);

-- ---------------------------------------------------------------------------
-- 9. Deterministic operational forecasts (care-operations-forecast.v1)
-- ---------------------------------------------------------------------------

CREATE TABLE care_operations_forecasts (
    id                     uuid PRIMARY KEY,
    tenant_id              uuid NOT NULL REFERENCES tenants(id),
    facility_id            uuid NOT NULL REFERENCES facilities(id),
    forecast_version       text NOT NULL,
    horizon                text NOT NULL CHECK (horizon IN ('24h', '7d')),
    generated_at           timestamptz NOT NULL DEFAULT now(),
    horizon_start          timestamptz NOT NULL,
    horizon_end            timestamptz NOT NULL,
    status                 text NOT NULL CHECK (status IN ('ready', 'insufficient_history')),
    source_facts           jsonb NOT NULL,
    facts_hash             text NOT NULL,
    output                 jsonb NOT NULL,
    explanation_artifact_id uuid REFERENCES ai_artifacts(id),
    created_by             uuid NOT NULL REFERENCES users(id),
    CHECK (horizon_end > horizon_start)
);
CREATE INDEX care_operations_forecasts_facility ON care_operations_forecasts (tenant_id, facility_id, horizon, generated_at DESC);

-- ---------------------------------------------------------------------------
-- 10. AI artifacts and notifications for care operations
-- ---------------------------------------------------------------------------

ALTER TABLE ai_artifacts
    ADD COLUMN care_episode_id            uuid REFERENCES care_episodes(id),
    ADD COLUMN placement_request_id       uuid REFERENCES placement_requests(id),
    ADD COLUMN perioperative_case_id      uuid REFERENCES perioperative_cases(id),
    ADD COLUMN care_operations_forecast_id uuid REFERENCES care_operations_forecasts(id),
    ADD COLUMN care_handoff_id            uuid REFERENCES care_handoffs(id),
    ADD COLUMN workforce_shift_id         uuid REFERENCES workforce_shifts(id);
CREATE INDEX ai_artifacts_care_episode ON ai_artifacts (tenant_id, care_episode_id) WHERE care_episode_id IS NOT NULL;
-- Facility-level operational artifacts have no single patient.
ALTER TABLE ai_artifacts DROP CONSTRAINT ai_artifacts_patient_required;
ALTER TABLE ai_artifacts ADD CONSTRAINT ai_artifacts_patient_required
    CHECK (patient_id IS NOT NULL OR artifact_type IN (
        'capacity_explanation', 'care_flow_brief', 'operational_forecast_explanation'));

ALTER TABLE notifications DROP CONSTRAINT notifications_kind_check;
ALTER TABLE notifications ADD CONSTRAINT notifications_kind_check CHECK (kind IN (
    'booking_confirmation', 'reschedule', 'cancellation', 'reminder',
    'preparation', 'confirmation_request', 'confirmation_follow_up',
    'waitlist_offer', 'waitlist_offer_expired', 'transport_status',
    'no_response_follow_up',
    'diagnostic_result_released', 'diagnostic_report_review', 'diagnostic_order_conflict',
    'diagnostic_follow_up',
    'care_plan_update', 'care_location_update', 'discharge_preparation'));
ALTER TABLE notifications ADD COLUMN care_episode_id uuid REFERENCES care_episodes(id);
