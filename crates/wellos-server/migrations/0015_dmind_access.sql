-- dMind Access v1: open clinical/resource catalogs, schedulable resources
-- with availability, authoritative appointments (offers, holds, history),
-- access requests and deterministic matcher runs, patient-access grants,
-- privacy-preserving personal-calendar busy intervals, consented waitlists
-- and cancellation recovery, durable notifications, operational calendars
-- with capacity forecasts, and consented transport/location coordination.
--
-- Forward-only. Existing scheduled visits are migrated into confirmed
-- appointments (idempotently, one appointment per visit) so the operational
-- `visits` episode keeps its history while `appointments` becomes the
-- authoritative scheduling record.

-- Range exclusion over (resource, slot) needs btree_gist for the equality
-- operands. Postgres ships it as a contrib extension.
CREATE EXTENSION IF NOT EXISTS btree_gist;

-- ---------------------------------------------------------------------------
-- 1. Open catalogs
-- ---------------------------------------------------------------------------

-- One table, several *kinds* of catalog. The kinds are the closed vocabulary
-- of what a catalog *is*; the entries (specialties, professions, resource
-- types, ...) are open and tenant-configurable at runtime.
CREATE TABLE catalog_entries (
    id               uuid PRIMARY KEY,
    tenant_id        uuid NOT NULL REFERENCES tenants(id),
    kind             text NOT NULL CHECK (kind IN (
                        'clinical_service', 'specialty', 'profession',
                        'modality', 'resource_type', 'accessibility_capability',
                        'location', 'transport_resource')),
    code             text NOT NULL CHECK (code ~ '^[a-z0-9][a-z0-9_.-]{0,63}$'),
    parent_id        uuid REFERENCES catalog_entries(id),
    name_en          text NOT NULL,
    name_es          text NOT NULL,
    synonyms         text[] NOT NULL DEFAULT '{}',
    -- e.g. [{"system":"http://snomed.info/sct","code":"394579002"}]
    external_codings jsonb NOT NULL DEFAULT '[]'::jsonb,
    -- Kind-specific configuration (service duration/buffers/preparation
    -- instructions/age restrictions/required resource types; location
    -- coordinates and service area). Validated by the server, never by UI.
    config           jsonb NOT NULL DEFAULT '{}'::jsonb,
    active           boolean NOT NULL DEFAULT true,
    effective_from   date,
    effective_to     date,
    version          bigint NOT NULL DEFAULT 1,
    created_by       uuid NOT NULL REFERENCES users(id),
    created_at       timestamptz NOT NULL DEFAULT now(),
    updated_at       timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, kind, code),
    CHECK (effective_to IS NULL OR effective_from IS NULL OR effective_to >= effective_from)
);
CREATE INDEX catalog_entries_kind ON catalog_entries (tenant_id, kind, active);
CREATE INDEX catalog_entries_parent ON catalog_entries (parent_id) WHERE parent_id IS NOT NULL;

-- Append-only history: full snapshot per version.
CREATE TABLE catalog_entry_history (
    id            uuid PRIMARY KEY,
    tenant_id     uuid NOT NULL REFERENCES tenants(id),
    entry_id      uuid NOT NULL REFERENCES catalog_entries(id),
    version       bigint NOT NULL,
    snapshot      jsonb NOT NULL,
    change_reason text,
    changed_by    uuid NOT NULL REFERENCES users(id),
    recorded_at   timestamptz NOT NULL DEFAULT now(),
    UNIQUE (entry_id, version)
);

-- Facility availability of an entry. No rows = available tenant-wide.
CREATE TABLE catalog_entry_facilities (
    tenant_id   uuid NOT NULL REFERENCES tenants(id),
    entry_id    uuid NOT NULL REFERENCES catalog_entries(id),
    facility_id uuid NOT NULL REFERENCES facilities(id),
    PRIMARY KEY (entry_id, facility_id)
);

-- ---------------------------------------------------------------------------
-- 2. Scheduling configuration: tenant policy, facility hours/location
-- ---------------------------------------------------------------------------

CREATE TABLE tenant_scheduling_policies (
    tenant_id                   uuid PRIMARY KEY REFERENCES tenants(id),
    time_zone                   text NOT NULL DEFAULT 'UTC',
    hold_minutes                int NOT NULL DEFAULT 15 CHECK (hold_minutes BETWEEN 1 AND 1440),
    offer_ttl_minutes           int NOT NULL DEFAULT 120 CHECK (offer_ttl_minutes BETWEEN 5 AND 10080),
    cancellation_window_hours   int NOT NULL DEFAULT 24 CHECK (cancellation_window_hours BETWEEN 0 AND 720),
    reschedule_window_hours     int NOT NULL DEFAULT 24 CHECK (reschedule_window_hours BETWEEN 0 AND 720),
    min_notice_hours            int NOT NULL DEFAULT 2 CHECK (min_notice_hours BETWEEN 0 AND 720),
    horizon_days                int NOT NULL DEFAULT 90 CHECK (horizon_days BETWEEN 1 AND 365),
    patient_confirmation_required boolean NOT NULL DEFAULT false,
    confirmation_deadline_hours int NOT NULL DEFAULT 48 CHECK (confirmation_deadline_hours BETWEEN 1 AND 720),
    reminder_lead_hours         int[] NOT NULL DEFAULT '{48,3}',
    quiet_hours_start           time NOT NULL DEFAULT '21:00',
    quiet_hours_end             time NOT NULL DEFAULT '08:00',
    max_candidates              int NOT NULL DEFAULT 8 CHECK (max_candidates BETWEEN 1 AND 20),
    version                     bigint NOT NULL DEFAULT 1,
    updated_by                  uuid REFERENCES users(id),
    updated_at                  timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE facility_scheduling (
    facility_id        uuid PRIMARY KEY REFERENCES facilities(id),
    tenant_id          uuid NOT NULL REFERENCES tenants(id),
    time_zone          text NOT NULL DEFAULT 'UTC',
    -- [{"weekday":1,"open":"08:00","close":"20:00"}, ...] (ISO weekday 1-7)
    opening_hours      jsonb NOT NULL DEFAULT '[]'::jsonb,
    latitude           double precision,
    longitude          double precision,
    service_radius_km  double precision CHECK (service_radius_km IS NULL OR service_radius_km > 0),
    address_line       text,
    version            bigint NOT NULL DEFAULT 1,
    updated_by         uuid REFERENCES users(id),
    updated_at         timestamptz NOT NULL DEFAULT now(),
    CHECK ((latitude IS NULL) = (longitude IS NULL)),
    CHECK (latitude IS NULL OR (latitude BETWEEN -90 AND 90 AND longitude BETWEEN -180 AND 180))
);

-- ---------------------------------------------------------------------------
-- 3. Schedulable resources and availability
-- ---------------------------------------------------------------------------

CREATE TABLE schedulable_resources (
    id                  uuid PRIMARY KEY,
    tenant_id           uuid NOT NULL REFERENCES tenants(id),
    facility_id         uuid NOT NULL REFERENCES facilities(id),
    -- Open catalog code of kind 'resource_type' (professional, team, room,
    -- dental_chair, procedure_room, laboratory_station, imaging_equipment,
    -- rehabilitation_space, telehealth_channel, home_visit_team, vehicle,
    -- accessible_vehicle, ambulance, ... or tenant-defined).
    resource_type_code  text NOT NULL,
    name                text NOT NULL,
    -- Professional resources link to the human user delivering the care.
    -- Never an authorization grant: permissions stay in role_assignments.
    user_id             uuid REFERENCES users(id),
    profession_code     text,
    specialty_codes     text[] NOT NULL DEFAULT '{}',
    languages           text[] NOT NULL DEFAULT '{}',
    accessibility_codes text[] NOT NULL DEFAULT '{}',
    capacity            int NOT NULL DEFAULT 1 CHECK (capacity BETWEEN 1 AND 500),
    time_zone           text NOT NULL DEFAULT 'UTC',
    active              boolean NOT NULL DEFAULT true,
    metadata            jsonb NOT NULL DEFAULT '{}'::jsonb,
    version             bigint NOT NULL DEFAULT 1,
    created_by          uuid NOT NULL REFERENCES users(id),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX schedulable_resources_facility ON schedulable_resources (tenant_id, facility_id, active);
CREATE INDEX schedulable_resources_user ON schedulable_resources (tenant_id, user_id) WHERE user_id IS NOT NULL;

-- Services a resource can deliver (with optional duration/buffer overrides).
CREATE TABLE resource_services (
    tenant_id          uuid NOT NULL REFERENCES tenants(id),
    resource_id        uuid NOT NULL REFERENCES schedulable_resources(id),
    service_code       text NOT NULL,
    duration_minutes   int CHECK (duration_minutes IS NULL OR duration_minutes BETWEEN 5 AND 720),
    prep_minutes       int NOT NULL DEFAULT 0 CHECK (prep_minutes BETWEEN 0 AND 240),
    cleanup_minutes    int NOT NULL DEFAULT 0 CHECK (cleanup_minutes BETWEEN 0 AND 240),
    modality_codes     text[] NOT NULL DEFAULT '{}',
    PRIMARY KEY (resource_id, service_code)
);
CREATE INDEX resource_services_service ON resource_services (tenant_id, service_code);

-- Weekly recurring availability in the resource's local time zone.
-- kind 'available' opens capacity; 'break' closes it inside an open window.
CREATE TABLE resource_availability_rules (
    id             uuid PRIMARY KEY,
    tenant_id      uuid NOT NULL REFERENCES tenants(id),
    resource_id    uuid NOT NULL REFERENCES schedulable_resources(id),
    weekday        smallint NOT NULL CHECK (weekday BETWEEN 1 AND 7),
    start_local    time NOT NULL,
    end_local      time NOT NULL,
    kind           text NOT NULL DEFAULT 'available' CHECK (kind IN ('available', 'break')),
    capacity       int CHECK (capacity IS NULL OR capacity BETWEEN 1 AND 500),
    effective_from date,
    effective_to   date,
    created_by     uuid NOT NULL REFERENCES users(id),
    created_at     timestamptz NOT NULL DEFAULT now(),
    CHECK (end_local > start_local)
);
CREATE INDEX resource_availability_rules_resource ON resource_availability_rules (resource_id, weekday);

-- Dated exceptions: leave, sickness, blocked periods, temporary closures
-- and temporary extra capacity. Absolute instants, so DST is unambiguous.
CREATE TABLE resource_exceptions (
    id             uuid PRIMARY KEY,
    tenant_id      uuid NOT NULL REFERENCES tenants(id),
    resource_id    uuid NOT NULL REFERENCES schedulable_resources(id),
    kind           text NOT NULL CHECK (kind IN (
                      'leave', 'sickness', 'blocked', 'closure', 'extra_capacity', 'training')),
    starts_at      timestamptz NOT NULL,
    ends_at        timestamptz NOT NULL,
    capacity_delta int NOT NULL DEFAULT 0,
    reason_code    text,
    created_by     uuid NOT NULL REFERENCES users(id),
    created_at     timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at)
);
CREATE INDEX resource_exceptions_window ON resource_exceptions (resource_id, starts_at, ends_at);

-- Required resource combinations for a service beyond the primary
-- professional (e.g. dental_visit requires a dental_chair).
CREATE TABLE service_resource_requirements (
    tenant_id          uuid NOT NULL REFERENCES tenants(id),
    service_code       text NOT NULL,
    resource_type_code text NOT NULL,
    quantity           int NOT NULL DEFAULT 1 CHECK (quantity BETWEEN 1 AND 10),
    PRIMARY KEY (tenant_id, service_code, resource_type_code)
);

-- ---------------------------------------------------------------------------
-- 4. Bookings: the concurrency backbone
-- ---------------------------------------------------------------------------

-- Every active hold or appointment occupies exactly one capacity slot of a
-- resource over [starts_at, ends_at) including buffers. The exclusion
-- constraint makes overlapping active occupations of the same slot
-- impossible at database level, whatever the application does.
CREATE TABLE resource_bookings (
    id             uuid PRIMARY KEY,
    tenant_id      uuid NOT NULL REFERENCES tenants(id),
    resource_id    uuid NOT NULL REFERENCES schedulable_resources(id),
    slot_index     int NOT NULL CHECK (slot_index >= 0),
    starts_at      timestamptz NOT NULL,
    ends_at        timestamptz NOT NULL,
    kind           text NOT NULL CHECK (kind IN ('hold', 'appointment')),
    status         text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'released')),
    appointment_id uuid,
    offer_id       uuid,
    expires_at     timestamptz,
    created_by     uuid NOT NULL REFERENCES users(id),
    created_at     timestamptz NOT NULL DEFAULT now(),
    released_at    timestamptz,
    CHECK (ends_at > starts_at),
    CHECK (kind <> 'hold' OR expires_at IS NOT NULL),
    EXCLUDE USING gist (
        resource_id WITH =,
        slot_index WITH =,
        tstzrange(starts_at, ends_at, '[)') WITH &&
    ) WHERE (status = 'active')
);
CREATE INDEX resource_bookings_resource_time ON resource_bookings (resource_id, starts_at) WHERE status = 'active';
CREATE INDEX resource_bookings_expiry ON resource_bookings (expires_at) WHERE status = 'active' AND kind = 'hold';

-- ---------------------------------------------------------------------------
-- 5. Access requests, matcher runs, offers, appointments
-- ---------------------------------------------------------------------------

CREATE TABLE access_requests (
    id                 uuid PRIMARY KEY,
    tenant_id          uuid NOT NULL REFERENCES tenants(id),
    patient_id         uuid NOT NULL REFERENCES patients(id),
    facility_id        uuid REFERENCES facilities(id),
    -- access-request.v1 state machine
    status             text NOT NULL CHECK (status IN (
                          'draft', 'submitted', 'needs_clinical_triage', 'options_ready',
                          'booked', 'closed', 'withdrawn')),
    channel            text NOT NULL CHECK (channel IN ('staff', 'patient', 'representative')),
    -- Natural-language need as entered (patient/staff wording). Read only
    -- by access-intent.v1 and the staff console; never logged or exported.
    free_text          text,
    -- Structured constraints (service_code, modality_codes, earliest,
    -- latest, preferred weekdays/times, language, accessibility needs,
    -- continuity_user_id, origin area, referral flags).
    constraints        jsonb NOT NULL DEFAULT '{}'::jsonb,
    missing_info       text[] NOT NULL DEFAULT '{}',
    -- Urgency is established by deterministic rules or a human only.
    urgency            text NOT NULL DEFAULT 'routine' CHECK (urgency IN ('routine', 'priority', 'urgent')),
    urgency_source     text NOT NULL DEFAULT 'default' CHECK (urgency_source IN ('default', 'deterministic', 'human')),
    triage_reason      text,
    intent_artifact_id uuid REFERENCES ai_artifacts(id),
    appointment_id     uuid,
    closed_reason      text,
    idempotency_key    text,
    version            bigint NOT NULL DEFAULT 1,
    created_by         uuid NOT NULL REFERENCES users(id),
    created_at         timestamptz NOT NULL DEFAULT now(),
    updated_at         timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX access_requests_patient ON access_requests (tenant_id, patient_id, created_at DESC);
CREATE INDEX access_requests_status ON access_requests (tenant_id, status, created_at DESC);
CREATE UNIQUE INDEX access_requests_idempotency ON access_requests (tenant_id, created_by, idempotency_key)
    WHERE idempotency_key IS NOT NULL;

CREATE TABLE access_request_history (
    id                uuid PRIMARY KEY,
    tenant_id         uuid NOT NULL REFERENCES tenants(id),
    access_request_id uuid NOT NULL REFERENCES access_requests(id),
    from_status       text,
    to_status         text NOT NULL,
    reason            text,
    actor             text NOT NULL,
    recorded_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX access_request_history_request ON access_request_history (access_request_id, recorded_at);

-- One deterministic candidate generation. Candidates, hard-constraint
-- trace, score decomposition and the source facts are persisted verbatim;
-- the optional dMind ranking artifact only reorders/explains them.
CREATE TABLE matcher_runs (
    id                  uuid PRIMARY KEY,
    tenant_id           uuid NOT NULL REFERENCES tenants(id),
    access_request_id   uuid NOT NULL REFERENCES access_requests(id),
    patient_id          uuid NOT NULL REFERENCES patients(id),
    matcher_version     text NOT NULL,
    source_facts        jsonb NOT NULL,
    candidates          jsonb NOT NULL,
    rejected_summary    jsonb NOT NULL DEFAULT '{}'::jsonb,
    ranking_mode        text NOT NULL CHECK (ranking_mode IN ('deterministic', 'dmind')),
    ranking_artifact_id uuid REFERENCES ai_artifacts(id),
    created_by          uuid NOT NULL REFERENCES users(id),
    created_at          timestamptz NOT NULL DEFAULT now(),
    expires_at          timestamptz NOT NULL
);
CREATE INDEX matcher_runs_request ON matcher_runs (access_request_id, created_at DESC);

CREATE TABLE appointments (
    id                    uuid PRIMARY KEY,
    tenant_id             uuid NOT NULL REFERENCES tenants(id),
    facility_id           uuid NOT NULL REFERENCES facilities(id),
    patient_id            uuid NOT NULL REFERENCES patients(id),
    service_code          text NOT NULL,
    modality_code         text NOT NULL DEFAULT 'in_person',
    -- appointment.v1 state machine
    status                text NOT NULL CHECK (status IN (
                             'confirmed', 'rescheduled', 'cancelled', 'fulfilled', 'no_show')),
    starts_at             timestamptz NOT NULL,
    ends_at               timestamptz NOT NULL,
    time_zone             text NOT NULL DEFAULT 'UTC',
    reason                text,
    access_request_id     uuid REFERENCES access_requests(id),
    offer_id              uuid,
    matcher_run_id        uuid REFERENCES matcher_runs(id),
    candidate_id          text,
    score                 jsonb,
    primary_resource_id   uuid REFERENCES schedulable_resources(id),
    -- Operational episode; created atomically on confirmation.
    visit_id              uuid REFERENCES visits(id),
    confirmation_required boolean NOT NULL DEFAULT false,
    patient_confirmed_at  timestamptz,
    confirmation_due_at   timestamptz,
    booked_via            text NOT NULL CHECK (booked_via IN (
                             'staff', 'patient', 'representative', 'waitlist', 'migration', 'staff_direct')),
    booked_by             uuid NOT NULL REFERENCES users(id),
    override_reason       text,
    rescheduled_from      uuid REFERENCES appointments(id),
    rescheduled_to        uuid REFERENCES appointments(id),
    cancellation_reason   text,
    cancellation_note     text,
    cancelled_by          uuid REFERENCES users(id),
    cancelled_at          timestamptz,
    fulfilled_at          timestamptz,
    no_show_at            timestamptz,
    idempotency_key       text,
    version               bigint NOT NULL DEFAULT 1,
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at)
);
CREATE UNIQUE INDEX appointments_visit ON appointments (visit_id) WHERE visit_id IS NOT NULL;
CREATE INDEX appointments_patient ON appointments (tenant_id, patient_id, starts_at DESC);
CREATE INDEX appointments_facility_time ON appointments (tenant_id, facility_id, starts_at);
CREATE INDEX appointments_status ON appointments (tenant_id, status, starts_at);
CREATE UNIQUE INDEX appointments_idempotency ON appointments (tenant_id, booked_by, idempotency_key)
    WHERE idempotency_key IS NOT NULL;
-- A patient holds at most one live appointment per service at a time.
CREATE UNIQUE INDEX appointments_one_live_per_patient_slot
    ON appointments (tenant_id, patient_id, starts_at)
    WHERE status IN ('confirmed', 'rescheduled');

ALTER TABLE access_requests
    ADD CONSTRAINT access_requests_appointment_fkey FOREIGN KEY (appointment_id) REFERENCES appointments(id);
ALTER TABLE resource_bookings
    ADD CONSTRAINT resource_bookings_appointment_fkey FOREIGN KEY (appointment_id) REFERENCES appointments(id);

-- Every status/time change is history; prior times are never overwritten
-- silently.
CREATE TABLE appointment_history (
    id              uuid PRIMARY KEY,
    tenant_id       uuid NOT NULL REFERENCES tenants(id),
    appointment_id  uuid NOT NULL REFERENCES appointments(id),
    from_status     text,
    to_status       text NOT NULL,
    starts_at_before timestamptz,
    starts_at_after  timestamptz,
    reason_code     text,
    note            text,
    override        boolean NOT NULL DEFAULT false,
    actor           text NOT NULL,
    version         bigint NOT NULL,
    recorded_at     timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX appointment_history_appointment ON appointment_history (appointment_id, recorded_at);

-- Resources occupied by an appointment (primary professional/team plus
-- required rooms, equipment, transport).
CREATE TABLE appointment_resources (
    appointment_id uuid NOT NULL REFERENCES appointments(id),
    resource_id    uuid NOT NULL REFERENCES schedulable_resources(id),
    booking_id     uuid NOT NULL REFERENCES resource_bookings(id),
    role           text NOT NULL,
    PRIMARY KEY (appointment_id, resource_id)
);

-- Offers (from a matcher run or a cancellation-recovery event). A hold is
-- an offer with active bookings and a `hold_expires_at`.
CREATE TABLE appointment_offers (
    id                    uuid PRIMARY KEY,
    tenant_id             uuid NOT NULL REFERENCES tenants(id),
    patient_id            uuid NOT NULL REFERENCES patients(id),
    facility_id           uuid NOT NULL REFERENCES facilities(id),
    access_request_id     uuid REFERENCES access_requests(id),
    matcher_run_id        uuid REFERENCES matcher_runs(id),
    candidate_id          text,
    cancellation_event_id uuid,
    waitlist_entry_id     uuid,
    -- appointment-offer.v1 state machine
    status                text NOT NULL CHECK (status IN (
                             'offered', 'held', 'accepted', 'declined', 'expired', 'revoked')),
    service_code          text NOT NULL,
    modality_code         text NOT NULL,
    starts_at             timestamptz NOT NULL,
    ends_at               timestamptz NOT NULL,
    -- [{"resource_id":..., "role":"primary"|"room"|...}]
    resources             jsonb NOT NULL DEFAULT '[]'::jsonb,
    score                 jsonb,
    explanation           jsonb,
    rank                  int,
    offered_to            text NOT NULL CHECK (offered_to IN ('staff', 'patient', 'waitlist')),
    offer_expires_at      timestamptz NOT NULL,
    hold_expires_at       timestamptz,
    appointment_id        uuid REFERENCES appointments(id),
    decline_reason        text,
    version               bigint NOT NULL DEFAULT 1,
    -- NULL when the recovery worker offered a freed slot to the waitlist.
    created_by            uuid REFERENCES users(id),
    created_at            timestamptz NOT NULL DEFAULT now(),
    updated_at            timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_at > starts_at)
);
CREATE INDEX appointment_offers_request ON appointment_offers (access_request_id, created_at DESC);
CREATE INDEX appointment_offers_patient ON appointment_offers (tenant_id, patient_id, status);
CREATE INDEX appointment_offers_expiry ON appointment_offers (offer_expires_at) WHERE status IN ('offered', 'held');
ALTER TABLE appointments
    ADD CONSTRAINT appointments_offer_fkey FOREIGN KEY (offer_id) REFERENCES appointment_offers(id);
ALTER TABLE resource_bookings
    ADD CONSTRAINT resource_bookings_offer_fkey FOREIGN KEY (offer_id) REFERENCES appointment_offers(id);

CREATE TABLE appointment_offer_history (
    id          uuid PRIMARY KEY,
    tenant_id   uuid NOT NULL REFERENCES tenants(id),
    offer_id    uuid NOT NULL REFERENCES appointment_offers(id),
    from_status text,
    to_status   text NOT NULL,
    reason      text,
    actor       text NOT NULL,
    recorded_at timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX appointment_offer_history_offer ON appointment_offer_history (offer_id, recorded_at);

-- Visits know their authoritative appointment.
ALTER TABLE visits ADD COLUMN appointment_id uuid REFERENCES appointments(id);
CREATE UNIQUE INDEX visits_appointment ON visits (appointment_id) WHERE appointment_id IS NOT NULL;

-- ---------------------------------------------------------------------------
-- 6. Patient access grants (self-service identity boundary)
-- ---------------------------------------------------------------------------

CREATE TABLE patient_access_grants (
    id                uuid PRIMARY KEY,
    tenant_id         uuid NOT NULL REFERENCES tenants(id),
    user_id           uuid NOT NULL REFERENCES users(id),
    patient_id        uuid NOT NULL REFERENCES patients(id),
    relationship      text NOT NULL CHECK (relationship IN ('self', 'parent_guardian', 'authorized_proxy')),
    status            text NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'revoked', 'expired')),
    verified_by       uuid NOT NULL REFERENCES users(id),
    verification_note text,
    granted_at        timestamptz NOT NULL DEFAULT now(),
    expires_at        timestamptz,
    revoked_at        timestamptz,
    revoked_by        uuid REFERENCES users(id),
    revoke_reason     text,
    version           bigint NOT NULL DEFAULT 1
);
CREATE UNIQUE INDEX patient_access_grants_active ON patient_access_grants (user_id, patient_id) WHERE status = 'active';
CREATE INDEX patient_access_grants_patient ON patient_access_grants (tenant_id, patient_id);

-- ---------------------------------------------------------------------------
-- 7. Personal calendars and scheduling preferences (privacy-preserving)
-- ---------------------------------------------------------------------------

CREATE TABLE patient_calendar_sources (
    id             uuid PRIMARY KEY,
    tenant_id      uuid NOT NULL REFERENCES tenants(id),
    patient_id     uuid NOT NULL REFERENCES patients(id),
    source_type    text NOT NULL CHECK (source_type IN ('ics_import', 'device_sync')),
    time_zone      text NOT NULL,
    integrity_hash text NOT NULL,
    horizon_start  timestamptz NOT NULL,
    horizon_end    timestamptz NOT NULL,
    interval_count int NOT NULL DEFAULT 0,
    status         text NOT NULL DEFAULT 'connected' CHECK (status IN ('connected', 'disconnected')),
    connected_by   uuid NOT NULL REFERENCES users(id),
    connected_at   timestamptz NOT NULL DEFAULT now(),
    last_synced_at timestamptz,
    disconnected_at timestamptz
);
CREATE INDEX patient_calendar_sources_patient ON patient_calendar_sources (tenant_id, patient_id, status);

-- Only normalized busy intervals are kept: no titles, descriptions,
-- attendees, links or raw files ever reach the database.
CREATE TABLE patient_busy_intervals (
    id         uuid PRIMARY KEY,
    tenant_id  uuid NOT NULL REFERENCES tenants(id),
    patient_id uuid NOT NULL REFERENCES patients(id),
    source_id  uuid NOT NULL REFERENCES patient_calendar_sources(id) ON DELETE CASCADE,
    starts_at  timestamptz NOT NULL,
    ends_at    timestamptz NOT NULL,
    CHECK (ends_at > starts_at)
);
CREATE INDEX patient_busy_intervals_patient ON patient_busy_intervals (tenant_id, patient_id, starts_at);

CREATE TABLE patient_scheduling_preferences (
    patient_id             uuid PRIMARY KEY REFERENCES patients(id),
    tenant_id              uuid NOT NULL REFERENCES tenants(id),
    -- Recurring availability: [{"weekday":1,"start":"08:00","end":"12:00"}]
    available_windows      jsonb NOT NULL DEFAULT '[]'::jsonb,
    unavailable_windows    jsonb NOT NULL DEFAULT '[]'::jsonb,
    preferred_modalities   text[] NOT NULL DEFAULT '{}',
    preferred_facility_ids uuid[] NOT NULL DEFAULT '{}',
    language               text,
    accessibility_needs    text[] NOT NULL DEFAULT '{}',
    time_zone              text,
    -- Channels the patient opted into: in_app is always available; email
    -- and push additionally require the corresponding consent and adapter.
    channels               text[] NOT NULL DEFAULT '{in_app}',
    quiet_hours_start      time,
    quiet_hours_end        time,
    -- Encrypted contact for external delivery (application-level, keyed).
    contact_email_enc      bytea,
    push_endpoint_enc      bytea,
    version                bigint NOT NULL DEFAULT 1,
    updated_by             uuid REFERENCES users(id),
    updated_at             timestamptz NOT NULL DEFAULT now()
);

-- ---------------------------------------------------------------------------
-- 8. Waitlist and cancellation recovery
-- ---------------------------------------------------------------------------

CREATE TABLE waitlist_entries (
    id                 uuid PRIMARY KEY,
    tenant_id          uuid NOT NULL REFERENCES tenants(id),
    patient_id         uuid NOT NULL REFERENCES patients(id),
    service_code       text NOT NULL,
    facility_ids       uuid[] NOT NULL DEFAULT '{}',
    modality_codes     text[] NOT NULL DEFAULT '{}',
    -- [{"weekday":1,"start":"08:00","end":"18:00"}]; empty = any time
    acceptable_windows jsonb NOT NULL DEFAULT '[]'::jsonb,
    earliest           timestamptz,
    latest             timestamptz,
    min_notice_hours   int NOT NULL DEFAULT 2 CHECK (min_notice_hours BETWEEN 0 AND 720),
    status             text NOT NULL DEFAULT 'active' CHECK (status IN (
                          'active', 'paused', 'offered', 'fulfilled', 'left')),
    urgency            text NOT NULL DEFAULT 'routine' CHECK (urgency IN ('routine', 'priority', 'urgent')),
    urgency_source     text NOT NULL DEFAULT 'default' CHECK (urgency_source IN ('default', 'deterministic', 'human')),
    access_request_id  uuid REFERENCES access_requests(id),
    current_appointment_id uuid REFERENCES appointments(id),
    fulfilled_appointment_id uuid REFERENCES appointments(id),
    offers_declined    int NOT NULL DEFAULT 0,
    joined_at          timestamptz NOT NULL DEFAULT now(),
    paused_at          timestamptz,
    left_at            timestamptz,
    version            bigint NOT NULL DEFAULT 1,
    created_by         uuid NOT NULL REFERENCES users(id),
    updated_at         timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX waitlist_entries_service ON waitlist_entries (tenant_id, service_code, status, joined_at);
CREATE INDEX waitlist_entries_patient ON waitlist_entries (tenant_id, patient_id, status);
ALTER TABLE appointment_offers
    ADD CONSTRAINT appointment_offers_waitlist_fkey FOREIGN KEY (waitlist_entry_id) REFERENCES waitlist_entries(id);

CREATE TABLE cancellation_events (
    id                  uuid PRIMARY KEY,
    tenant_id           uuid NOT NULL REFERENCES tenants(id),
    appointment_id      uuid NOT NULL REFERENCES appointments(id),
    facility_id         uuid NOT NULL REFERENCES facilities(id),
    service_code        text NOT NULL,
    modality_code       text NOT NULL,
    starts_at           timestamptz NOT NULL,
    ends_at             timestamptz NOT NULL,
    resources           jsonb NOT NULL DEFAULT '[]'::jsonb,
    status              text NOT NULL DEFAULT 'open' CHECK (status IN (
                           'open', 'offered', 'filled', 'exhausted', 'closed')),
    -- Deterministic eligibility + fairness ordering, persisted verbatim.
    eligible            jsonb NOT NULL DEFAULT '[]'::jsonb,
    -- Entries considered but excluded, with the deterministic reason.
    excluded            jsonb NOT NULL DEFAULT '[]'::jsonb,
    ranking_mode        text NOT NULL DEFAULT 'deterministic' CHECK (ranking_mode IN ('deterministic', 'dmind', 'human_override')),
    ranking_artifact_id uuid REFERENCES ai_artifacts(id),
    current_offer_id    uuid REFERENCES appointment_offers(id),
    offers_made         int NOT NULL DEFAULT 0,
    override_by         uuid REFERENCES users(id),
    override_reason     text,
    closed_reason       text,
    version             bigint NOT NULL DEFAULT 1,
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX cancellation_events_open ON cancellation_events (tenant_id, status, starts_at);
ALTER TABLE appointment_offers
    ADD CONSTRAINT appointment_offers_cancellation_fkey FOREIGN KEY (cancellation_event_id) REFERENCES cancellation_events(id);

-- ---------------------------------------------------------------------------
-- 9. Durable notifications
-- ---------------------------------------------------------------------------

CREATE TABLE notifications (
    id              uuid PRIMARY KEY,
    tenant_id       uuid NOT NULL REFERENCES tenants(id),
    patient_id      uuid REFERENCES patients(id),
    -- Recipient user for in-app delivery (self-service account or staff).
    user_id         uuid REFERENCES users(id),
    kind            text NOT NULL CHECK (kind IN (
                       'booking_confirmation', 'reschedule', 'cancellation', 'reminder',
                       'preparation', 'confirmation_request', 'confirmation_follow_up',
                       'waitlist_offer', 'waitlist_offer_expired', 'transport_status',
                       'no_response_follow_up')),
    appointment_id  uuid REFERENCES appointments(id),
    offer_id        uuid REFERENCES appointment_offers(id),
    transport_request_id uuid,
    language        text NOT NULL DEFAULT 'en',
    time_zone       text NOT NULL DEFAULT 'UTC',
    channels        text[] NOT NULL DEFAULT '{in_app}',
    -- Identifiers and template parameters only; no names, no free text.
    payload         jsonb NOT NULL DEFAULT '{}'::jsonb,
    dedupe_key      text NOT NULL,
    scheduled_for   timestamptz NOT NULL,
    status          text NOT NULL DEFAULT 'scheduled' CHECK (status IN (
                       'scheduled', 'delivering', 'delivered', 'partially_delivered',
                       'failed', 'dead', 'cancelled')),
    attempts        int NOT NULL DEFAULT 0,
    next_attempt_at timestamptz,
    last_error_code text,
    locked_by       text,
    locked_until    timestamptz,
    delivered_at    timestamptz,
    read_at         timestamptz,
    created_at      timestamptz NOT NULL DEFAULT now(),
    updated_at      timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, dedupe_key)
);
CREATE INDEX notifications_due ON notifications (scheduled_for) WHERE status IN ('scheduled', 'failed');
CREATE INDEX notifications_inbox ON notifications (tenant_id, user_id, created_at DESC) WHERE user_id IS NOT NULL;
CREATE INDEX notifications_patient ON notifications (tenant_id, patient_id, created_at DESC) WHERE patient_id IS NOT NULL;
CREATE INDEX notifications_appointment ON notifications (appointment_id) WHERE appointment_id IS NOT NULL;

-- One row per delivery attempt per channel (idempotent per attempt).
CREATE TABLE notification_deliveries (
    id              uuid PRIMARY KEY,
    tenant_id       uuid NOT NULL REFERENCES tenants(id),
    notification_id uuid NOT NULL REFERENCES notifications(id),
    channel         text NOT NULL CHECK (channel IN ('in_app', 'email', 'webhook', 'dev_sink')),
    attempt         int NOT NULL,
    status          text NOT NULL CHECK (status IN ('delivered', 'failed', 'skipped')),
    error_code      text,
    recorded_at     timestamptz NOT NULL DEFAULT now(),
    UNIQUE (notification_id, channel, attempt)
);

-- ---------------------------------------------------------------------------
-- 10. Operational calendar and capacity forecasts
-- ---------------------------------------------------------------------------

-- Tenant-configured holidays, school breaks, local events, seasonal periods
-- and closures. Nothing about a specific country or island is hardcoded.
CREATE TABLE operational_calendar_events (
    id                  uuid PRIMARY KEY,
    tenant_id           uuid NOT NULL REFERENCES tenants(id),
    facility_id         uuid REFERENCES facilities(id),
    kind                text NOT NULL CHECK (kind IN (
                           'holiday', 'school_break', 'local_event', 'seasonal_period', 'closure')),
    name                text NOT NULL,
    starts_on           date NOT NULL,
    ends_on             date NOT NULL,
    -- Expected demand relative to baseline (1.0 = unchanged).
    demand_multiplier   numeric(5,2) NOT NULL DEFAULT 1.00 CHECK (demand_multiplier BETWEEN 0 AND 10),
    -- Expected capacity relative to plan (0 for a closure).
    capacity_multiplier numeric(5,2) NOT NULL DEFAULT 1.00 CHECK (capacity_multiplier BETWEEN 0 AND 10),
    active              boolean NOT NULL DEFAULT true,
    created_by          uuid NOT NULL REFERENCES users(id),
    created_at          timestamptz NOT NULL DEFAULT now(),
    CHECK (ends_on >= starts_on)
);
CREATE INDEX operational_calendar_events_window ON operational_calendar_events (tenant_id, starts_on, ends_on);

CREATE TABLE capacity_forecasts (
    id                     uuid PRIMARY KEY,
    tenant_id              uuid NOT NULL REFERENCES tenants(id),
    facility_id            uuid NOT NULL REFERENCES facilities(id),
    service_code           text NOT NULL,
    forecast_version       text NOT NULL,
    horizon_start          date NOT NULL,
    horizon_end            date NOT NULL,
    status                 text NOT NULL CHECK (status IN ('ready', 'insufficient_history')),
    inputs_hash            text NOT NULL,
    output                 jsonb NOT NULL,
    explanation_artifact_id uuid REFERENCES ai_artifacts(id),
    created_by             uuid NOT NULL REFERENCES users(id),
    created_at             timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX capacity_forecasts_lookup ON capacity_forecasts (tenant_id, facility_id, service_code, created_at DESC);

-- ---------------------------------------------------------------------------
-- 11. Transport and location coordination
-- ---------------------------------------------------------------------------

CREATE TABLE transport_requests (
    id                 uuid PRIMARY KEY,
    tenant_id          uuid NOT NULL REFERENCES tenants(id),
    patient_id         uuid NOT NULL REFERENCES patients(id),
    appointment_id     uuid NOT NULL REFERENCES appointments(id),
    status             text NOT NULL DEFAULT 'requested' CHECK (status IN (
                          'requested', 'scheduled', 'en_route', 'picked_up', 'completed',
                          'cancelled', 'failed')),
    requirements       text[] NOT NULL DEFAULT '{}',
    emergency          boolean NOT NULL DEFAULT false,
    -- Approximate origin (service-area code) for matching; the exact
    -- address is encrypted at application level when retained.
    origin_area_code   text,
    pickup_address_enc bytea,
    pickup_window_start timestamptz,
    pickup_window_end   timestamptz,
    vehicle_resource_id uuid REFERENCES schedulable_resources(id),
    booking_id          uuid REFERENCES resource_bookings(id),
    operator_user_id    uuid REFERENCES users(id),
    -- Emergency dispatch is a human decision: who authorized it.
    authorized_by       uuid REFERENCES users(id),
    failure_reason      text,
    version             bigint NOT NULL DEFAULT 1,
    created_by          uuid NOT NULL REFERENCES users(id),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    CHECK (NOT emergency OR authorized_by IS NOT NULL),
    CHECK (pickup_window_end IS NULL OR pickup_window_start IS NULL OR pickup_window_end > pickup_window_start)
);
CREATE INDEX transport_requests_appointment ON transport_requests (appointment_id);
CREATE INDEX transport_requests_status ON transport_requests (tenant_id, status, pickup_window_start);
ALTER TABLE notifications
    ADD CONSTRAINT notifications_transport_fkey FOREIGN KEY (transport_request_id) REFERENCES transport_requests(id);

CREATE TABLE transport_request_history (
    id                   uuid PRIMARY KEY,
    tenant_id            uuid NOT NULL REFERENCES tenants(id),
    transport_request_id uuid NOT NULL REFERENCES transport_requests(id),
    from_status          text,
    to_status            text NOT NULL,
    note                 text,
    actor                text NOT NULL,
    recorded_at          timestamptz NOT NULL DEFAULT now()
);

-- Short-lived live positions during an active transport episode. Encrypted
-- at rest, expire automatically and are purged by the maintenance job.
CREATE TABLE transport_live_locations (
    id                   uuid PRIMARY KEY,
    tenant_id            uuid NOT NULL REFERENCES tenants(id),
    transport_request_id uuid NOT NULL REFERENCES transport_requests(id),
    coordinates_enc      bytea NOT NULL,
    shared_by            uuid NOT NULL REFERENCES users(id),
    recorded_at          timestamptz NOT NULL DEFAULT now(),
    expires_at           timestamptz NOT NULL
);
CREATE INDEX transport_live_locations_expiry ON transport_live_locations (expires_at);
CREATE INDEX transport_live_locations_request ON transport_live_locations (transport_request_id, recorded_at DESC);

-- ---------------------------------------------------------------------------
-- 12. AI artifact scopes for dMind Access
-- ---------------------------------------------------------------------------

-- Access-intent and ranking artifacts bind to the access request /
-- matcher run of one patient; cancellation-recovery artifacts bind to the
-- cancellation event (the patient column carries the cancelled appointment's
-- patient); capacity explanations are tenant-level operational artifacts
-- with no patient.
ALTER TABLE ai_artifacts
    ADD COLUMN access_request_id     uuid REFERENCES access_requests(id),
    ADD COLUMN matcher_run_id        uuid REFERENCES matcher_runs(id),
    ADD COLUMN cancellation_event_id uuid REFERENCES cancellation_events(id),
    ADD COLUMN capacity_forecast_id  uuid REFERENCES capacity_forecasts(id);
ALTER TABLE ai_artifacts ALTER COLUMN patient_id DROP NOT NULL;
ALTER TABLE ai_artifacts
    ADD CONSTRAINT ai_artifacts_patient_required
        CHECK (patient_id IS NOT NULL OR artifact_type = 'capacity_explanation');
-- Tenant agreement for reuse links is enforced even when no patient is
-- bound (the patient-aware composite FK from 0014 is skipped by SQL for
-- NULL patient columns).
ALTER TABLE ai_artifacts ADD CONSTRAINT ai_artifacts_id_tenant_key UNIQUE (id, tenant_id);
ALTER TABLE ai_artifacts
    ADD CONSTRAINT ai_artifacts_reused_from_same_tenant_fkey
        FOREIGN KEY (reused_from, tenant_id) REFERENCES ai_artifacts (id, tenant_id);
CREATE INDEX ai_artifacts_access_request ON ai_artifacts (tenant_id, access_request_id) WHERE access_request_id IS NOT NULL;
CREATE INDEX ai_artifacts_matcher_run ON ai_artifacts (tenant_id, matcher_run_id) WHERE matcher_run_id IS NOT NULL;
CREATE INDEX ai_artifacts_cancellation ON ai_artifacts (tenant_id, cancellation_event_id) WHERE cancellation_event_id IS NOT NULL;
CREATE INDEX ai_artifacts_capacity ON ai_artifacts (tenant_id, capacity_forecast_id) WHERE capacity_forecast_id IS NOT NULL;

-- ---------------------------------------------------------------------------
-- 13. Baseline catalog for existing tenants
-- ---------------------------------------------------------------------------

-- The closed service list validated by the visit API before this migration
-- becomes ordinary catalog data, plus the modalities and resource types the
-- scheduler needs to book anything at all. Everything else (specialties,
-- professions, accessibility, locations, transport) is tenant-authored at
-- runtime. Idempotent; tenants without any user cannot own entries yet and
-- are skipped (they have no visits either).
INSERT INTO catalog_entries (id, tenant_id, kind, code, name_en, name_es, config, created_by)
SELECT md5('wellos-0015-catalog:' || t.id::text || ':' || b.kind || ':' || b.code)::uuid,
       t.id, b.kind, b.code, b.name_en, b.name_es, b.config::jsonb, u.id
FROM tenants t
JOIN LATERAL (
    SELECT id FROM users WHERE tenant_id = t.id AND NOT is_service ORDER BY created_at, id LIMIT 1
) u ON true
CROSS JOIN (VALUES
    ('clinical_service', 'general_medicine', 'General medicine consultation', 'Consulta de medicina general',
        '{"duration_minutes":20,"modality_codes":["in_person","telehealth"],"required_resource_types":["professional"]}'),
    ('clinical_service', 'emergency', 'Emergency attendance', 'Atención de urgencias',
        '{"duration_minutes":30,"modality_codes":["in_person"],"required_resource_types":["professional"]}'),
    ('clinical_service', 'nursing', 'Nursing care', 'Atención de enfermería',
        '{"duration_minutes":15,"modality_codes":["in_person","home_visit"],"required_resource_types":["professional"]}'),
    ('clinical_service', 'telehealth', 'Telehealth consultation', 'Consulta de telesalud',
        '{"duration_minutes":15,"modality_codes":["telehealth"],"required_resource_types":["professional"]}'),
    ('modality', 'in_person', 'In person', 'Presencial', '{}'),
    ('modality', 'telehealth', 'Telehealth', 'Telesalud', '{}'),
    ('modality', 'home_visit', 'Home visit', 'Visita domiciliaria', '{}'),
    ('resource_type', 'professional', 'Professional', 'Profesional', '{}'),
    ('resource_type', 'team', 'Multidisciplinary team', 'Equipo multidisciplinar', '{}'),
    ('resource_type', 'room', 'Consultation room', 'Consulta', '{}'),
    ('resource_type', 'telehealth_channel', 'Telehealth channel', 'Canal de telesalud', '{}'),
    ('resource_type', 'vehicle', 'Vehicle', 'Vehículo', '{}'),
    ('resource_type', 'accessible_vehicle', 'Accessible vehicle', 'Vehículo adaptado', '{}'),
    ('resource_type', 'ambulance', 'Ambulance', 'Ambulancia', '{}')
) AS b(kind, code, name_en, name_es, config)
ON CONFLICT (tenant_id, kind, code) DO NOTHING;

INSERT INTO catalog_entry_history (id, tenant_id, entry_id, version, snapshot, change_reason, changed_by)
SELECT md5('wellos-0015-catalog-history:' || e.id::text)::uuid, e.tenant_id, e.id, e.version,
       jsonb_build_object('kind', e.kind, 'code', e.code, 'name_en', e.name_en, 'name_es', e.name_es,
                          'config', e.config, 'active', e.active),
       'baseline_catalog_migration_0015', e.created_by
FROM catalog_entries e
WHERE NOT EXISTS (SELECT 1 FROM catalog_entry_history h WHERE h.entry_id = e.id AND h.version = e.version);

-- ---------------------------------------------------------------------------
-- 14. Migrate existing scheduled visits into authoritative appointments
-- ---------------------------------------------------------------------------

-- Every visit registered as a scheduled appointment before this migration
-- gets exactly one appointment carrying its history (status mapped from the
-- visit's operational state), and the visit is linked back. Idempotent:
-- visits that already have an appointment are skipped. No visit row is
-- modified except for the new link column.
WITH candidates AS (
    SELECT v.*
    FROM visits v
    WHERE v.arrival_kind = 'scheduled'
      AND v.scheduled_at IS NOT NULL
      AND v.appointment_id IS NULL
      AND NOT EXISTS (SELECT 1 FROM appointments a WHERE a.visit_id = v.id)
), inserted AS (
    INSERT INTO appointments (
        id, tenant_id, facility_id, patient_id, service_code, modality_code, status,
        starts_at, ends_at, time_zone, reason, visit_id, booked_via, booked_by,
        cancellation_reason, cancelled_at, fulfilled_at, no_show_at, version, created_at, updated_at)
    SELECT
        -- Deterministic id derived from the visit id (idempotent re-runs
        -- cannot mint a second appointment for the same visit).
        md5('wellos-0015-appointment:' || c.id::text)::uuid,
        c.tenant_id, c.facility_id, c.patient_id, c.service, 'in_person',
        CASE c.status
            WHEN 'cancelled' THEN 'cancelled'
            WHEN 'no_show'   THEN 'no_show'
            WHEN 'completed' THEN 'fulfilled'
            WHEN 'closed'    THEN CASE WHEN c.closed_reason = 'no_show' THEN 'no_show'
                                       WHEN c.closed_reason = 'cancelled' THEN 'cancelled'
                                       ELSE 'fulfilled' END
            ELSE 'confirmed'
        END,
        c.scheduled_at, c.scheduled_at + interval '30 minutes', 'UTC', c.reason, c.id,
        'migration', c.created_by,
        CASE WHEN c.status IN ('cancelled') OR c.closed_reason = 'cancelled' THEN 'migrated_visit_cancelled' END,
        CASE WHEN c.status IN ('cancelled') OR c.closed_reason = 'cancelled' THEN COALESCE(c.closed_at, c.updated_at) END,
        CASE WHEN c.status = 'completed' OR (c.status = 'closed' AND COALESCE(c.closed_reason,'') NOT IN ('no_show','cancelled'))
             THEN COALESCE(c.completed_at, c.closed_at, c.updated_at) END,
        CASE WHEN c.status = 'no_show' OR c.closed_reason = 'no_show' THEN COALESCE(c.closed_at, c.updated_at) END,
        1, c.created_at, c.updated_at
    FROM candidates c
    RETURNING id, tenant_id, visit_id, status, starts_at, created_at
)
INSERT INTO appointment_history (id, tenant_id, appointment_id, from_status, to_status,
                                 starts_at_before, starts_at_after, reason_code, actor, version, recorded_at)
SELECT md5('wellos-0015-history:' || i.id::text)::uuid, i.tenant_id, i.id, NULL, i.status, NULL, i.starts_at,
       'migrated_from_scheduled_visit', 'system:migration-0015', 1, i.created_at
FROM inserted i;

UPDATE visits v
SET appointment_id = a.id
FROM appointments a
WHERE a.visit_id = v.id AND v.appointment_id IS NULL;

-- Rollback (manual; forward-only in CI):
--   UPDATE visits SET appointment_id = NULL;
--   ALTER TABLE visits DROP COLUMN appointment_id;
--   ALTER TABLE ai_artifacts DROP CONSTRAINT ai_artifacts_reused_from_same_tenant_fkey,
--       DROP CONSTRAINT ai_artifacts_id_tenant_key, DROP CONSTRAINT ai_artifacts_patient_required,
--       DROP COLUMN capacity_forecast_id, DROP COLUMN cancellation_event_id,
--       DROP COLUMN matcher_run_id, DROP COLUMN access_request_id;
--   ALTER TABLE ai_artifacts ALTER COLUMN patient_id SET NOT NULL;
--   DROP TABLE transport_live_locations, transport_request_history, transport_requests,
--       capacity_forecasts, operational_calendar_events, notification_deliveries, notifications,
--       cancellation_events, waitlist_entries, patient_scheduling_preferences,
--       patient_busy_intervals, patient_calendar_sources, patient_access_grants,
--       appointment_offer_history, appointment_offers, appointment_resources, appointment_history,
--       appointments, matcher_runs, access_request_history, access_requests, resource_bookings,
--       service_resource_requirements, resource_exceptions, resource_availability_rules,
--       resource_services, schedulable_resources, facility_scheduling, tenant_scheduling_policies,
--       catalog_entry_facilities, catalog_entry_history, catalog_entries CASCADE;
