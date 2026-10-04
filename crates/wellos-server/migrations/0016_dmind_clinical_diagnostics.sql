-- dMind Clinical Orders & Diagnostics v1.
--
-- Additive, forward-only. `service_requests` stays the authoritative clinical
-- order and the existing result loop (`loop_state`: ordered -> received ->
-- reviewed -> notified -> closed, amendment reopening) stays the authoritative
-- review / notification / closure state. This migration adds, next to it:
--
--   1. an open `diagnostic_orderable` catalog kind (laboratory, imaging,
--      cardiology, pathology, dentistry, procedures and anything a tenant
--      configures at runtime), with the existing facility mapping, version
--      history and deactivation semantics;
--   2. a versioned diagnostic *order* state on `service_requests`
--      (placed -> accepted -> scheduled -> in_progress -> completed, with
--      on_hold / cancelled / rejected / entered_in_error), an explicit
--      fulfilment mode, priority, indication, requested window, performing
--      facility / service / professional / equipment and the link to the
--      existing Access request and appointment; order groups (panels);
--      append-only order history;
--   3. typed result components: `observations` gains value types
--      (quantity | text | coded | boolean | datetime | narrative), the report
--      it belongs to and a deterministic interpretation; numeric rows are
--      untouched and keep working with the existing critical rules;
--   4. diagnostic reports (preliminary | final | amended | corrected |
--      cancelled | entered_in_error) as immutable rows chained by `replaces`;
--   5. specimens with an append-only chain of custody;
--   6. deterministic safety evaluations with audited acknowledgements and
--      overrides;
--   7. professional reviews of a report version, release decisions and the
--      clinician-approved patient explanation;
--   8. clinical documents (object-store references, never bytes) and
--      external imaging-study references (DICOM identifiers, controlled
--      DICOMweb reference, never pixels);
--   9. dMind artifact bindings for the three new governed operations.
--
-- Legacy data: every existing (tenant, LOINC, display) laboratory order
-- becomes a `diagnostic_orderable` catalog entry (created once per tenant,
-- version 1, history recorded) and every existing `service_requests` row is
-- linked to it with an `immediate` fulfilment mode and an order status derived
-- from its loop state. No rows are deleted or re-keyed; `code_loinc`,
-- `display`, `loop_state` and `version` keep their values.
--
-- Rollback (manual, forward migration):
--   ALTER TABLE service_requests DROP COLUMN ... (every column added below);
--   ALTER TABLE observations DROP COLUMN ... ; ALTER COLUMN value_num SET NOT NULL;
--   ALTER TABLE alerts ALTER COLUMN observation_id SET NOT NULL, DROP COLUMN diagnostic_report_id;
--   DROP TABLE diagnostic_safety_evaluations, result_release_decisions,
--       diagnostic_reviews, imaging_studies, clinical_documents, specimen_events,
--       specimens, diagnostic_reports, service_request_history, diagnostic_order_groups;
--   ALTER TABLE ai_artifacts DROP COLUMN diagnostic_report_id, DROP COLUMN order_group_id;
--   DELETE FROM catalog_entries WHERE kind = 'diagnostic_orderable' (and history);
--   restore the catalog_entries kind and notifications kind CHECK constraints.

-- ---------------------------------------------------------------------------
-- 1. Catalog: open diagnostic orderables
-- ---------------------------------------------------------------------------

ALTER TABLE catalog_entries DROP CONSTRAINT catalog_entries_kind_check;
ALTER TABLE catalog_entries ADD CONSTRAINT catalog_entries_kind_check CHECK (kind IN (
    'clinical_service', 'specialty', 'profession',
    'modality', 'resource_type', 'accessibility_capability',
    'location', 'transport_resource', 'diagnostic_orderable'));

-- ---------------------------------------------------------------------------
-- 2. Orders: groups, generalized service requests, append-only history
-- ---------------------------------------------------------------------------

CREATE TABLE diagnostic_order_groups (
    id                     uuid PRIMARY KEY,
    tenant_id              uuid NOT NULL REFERENCES tenants(id),
    patient_id             uuid NOT NULL REFERENCES patients(id),
    encounter_id           uuid NOT NULL REFERENCES encounters(id),
    requester_id           uuid NOT NULL REFERENCES users(id),
    clinical_indication    text,
    clinical_question      text,
    priority               text NOT NULL DEFAULT 'routine' CHECK (priority IN ('routine', 'urgent', 'stat', 'timed')),
    -- Deterministic safety evaluation the clinician confirmed against.
    safety_evaluation_id   uuid,
    -- dMind suggestion the clinician looked at (never the author of the order).
    suggestion_artifact_id uuid REFERENCES ai_artifacts(id),
    idempotency_key        text,
    version                bigint NOT NULL DEFAULT 1,
    created_at             timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, idempotency_key)
);
CREATE INDEX diagnostic_order_groups_patient ON diagnostic_order_groups (tenant_id, patient_id, created_at DESC);
CREATE INDEX diagnostic_order_groups_encounter ON diagnostic_order_groups (tenant_id, encounter_id);

ALTER TABLE service_requests
    -- Non-laboratory orderables carry their primary coding in the catalog;
    -- LOINC stays populated whenever the orderable has one.
    ALTER COLUMN code_loinc DROP NOT NULL,
    ADD COLUMN orderable_id              uuid REFERENCES catalog_entries(id),
    ADD COLUMN orderable_code            text,
    ADD COLUMN orderable_version         bigint,
    ADD COLUMN order_group_id            uuid REFERENCES diagnostic_order_groups(id),
    ADD COLUMN category_code             text,
    ADD COLUMN modality_code             text,
    ADD COLUMN expected_result_type      text NOT NULL DEFAULT 'quantity' CHECK (expected_result_type IN (
                                             'quantity', 'text', 'coded', 'boolean', 'datetime', 'narrative', 'report')),
    ADD COLUMN order_status              text NOT NULL DEFAULT 'placed' CHECK (order_status IN (
                                             'placed', 'accepted', 'scheduled', 'in_progress', 'completed',
                                             'on_hold', 'cancelled', 'rejected', 'entered_in_error')),
    ADD COLUMN fulfilment_mode           text NOT NULL DEFAULT 'scheduled' CHECK (fulfilment_mode IN (
                                             'scheduled', 'immediate', 'inpatient', 'bedside', 'walk_in')),
    ADD COLUMN priority                  text NOT NULL DEFAULT 'routine' CHECK (priority IN ('routine', 'urgent', 'stat', 'timed')),
    ADD COLUMN clinical_indication       text,
    ADD COLUMN clinical_question         text,
    ADD COLUMN requested_window_start    timestamptz,
    ADD COLUMN requested_window_end      timestamptz,
    ADD COLUMN preparation_en            text,
    ADD COLUMN preparation_es            text,
    ADD COLUMN performing_facility_id    uuid REFERENCES facilities(id),
    ADD COLUMN performing_service_code   text,
    ADD COLUMN performing_professional_id uuid REFERENCES users(id),
    ADD COLUMN performing_resource_id    uuid REFERENCES schedulable_resources(id),
    ADD COLUMN access_request_id         uuid REFERENCES access_requests(id),
    ADD COLUMN appointment_id            uuid REFERENCES appointments(id),
    -- A cancelled / no-show appointment never cancels the order: the order
    -- is flagged for a human decision instead.
    ADD COLUMN schedule_conflict         text CHECK (schedule_conflict IN ('appointment_cancelled', 'appointment_no_show', 'facility_closed')),
    ADD COLUMN hold_reason               text,
    ADD COLUMN cancellation_reason       text,
    ADD COLUMN rejection_reason          text,
    ADD COLUMN accepted_at               timestamptz,
    ADD COLUMN started_at                timestamptz,
    ADD COLUMN completed_at              timestamptz,
    ADD COLUMN cancelled_at              timestamptz,
    ADD COLUMN idempotency_key           text,
    ADD COLUMN source_system             text,
    ADD COLUMN updated_at                timestamptz NOT NULL DEFAULT now(),
    ADD CONSTRAINT service_requests_window_check CHECK (
        requested_window_end IS NULL OR requested_window_start IS NULL
        OR requested_window_end >= requested_window_start),
    ADD CONSTRAINT service_requests_idempotency_key UNIQUE (tenant_id, idempotency_key);
CREATE INDEX service_requests_order_status ON service_requests (tenant_id, order_status, priority, created_at);
CREATE INDEX service_requests_group ON service_requests (order_group_id) WHERE order_group_id IS NOT NULL;
CREATE INDEX service_requests_appointment ON service_requests (appointment_id) WHERE appointment_id IS NOT NULL;
CREATE INDEX service_requests_access_request ON service_requests (access_request_id) WHERE access_request_id IS NOT NULL;
CREATE INDEX service_requests_performing_facility ON service_requests (tenant_id, performing_facility_id, order_status);
CREATE INDEX service_requests_patient_status ON service_requests (tenant_id, patient_id, order_status);

CREATE TABLE service_request_history (
    id                 uuid PRIMARY KEY,
    tenant_id          uuid NOT NULL REFERENCES tenants(id),
    service_request_id uuid NOT NULL REFERENCES service_requests(id),
    from_status        text,
    to_status          text NOT NULL,
    -- Order version after the transition.
    version            bigint NOT NULL,
    reason             text,
    actor              text NOT NULL,
    actor_user_id      uuid REFERENCES users(id),
    details            jsonb NOT NULL DEFAULT '{}'::jsonb,
    recorded_at        timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX service_request_history_order ON service_request_history (service_request_id, recorded_at);

-- ---------------------------------------------------------------------------
-- 3. Diagnostic reports (immutable rows chained by `replaces`)
-- ---------------------------------------------------------------------------

CREATE TABLE diagnostic_reports (
    id                     uuid PRIMARY KEY,
    tenant_id              uuid NOT NULL REFERENCES tenants(id),
    patient_id             uuid NOT NULL REFERENCES patients(id),
    service_request_id     uuid NOT NULL REFERENCES service_requests(id),
    status                 text NOT NULL CHECK (status IN (
                              'preliminary', 'final', 'amended', 'corrected', 'cancelled', 'entered_in_error')),
    -- Monotonic per order; the row this one supersedes (amendment /
    -- correction / finalization of a preliminary report) is never mutated.
    version                bigint NOT NULL,
    replaces               uuid REFERENCES diagnostic_reports(id),
    category_code          text,
    conclusion             text,
    -- [{"system": "...", "code": "...", "display": "..."}]
    conclusion_codes       jsonb NOT NULL DEFAULT '[]'::jsonb,
    -- Deterministic: the worst component interpretation plus configured
    -- report-level rules. Never produced by dMind.
    criticality            text NOT NULL DEFAULT 'unknown' CHECK (criticality IN ('normal', 'abnormal', 'critical', 'unknown')),
    criticality_rules      jsonb NOT NULL DEFAULT '[]'::jsonb,
    performer_id           uuid REFERENCES users(id),
    performing_facility_id uuid REFERENCES facilities(id),
    performing_service_code text,
    signed_by              uuid REFERENCES users(id),
    signed_at              timestamptz,
    issued_at              timestamptz NOT NULL DEFAULT now(),
    effective_at           timestamptz,
    source_system          text NOT NULL,
    external_report_id     text,
    idempotency_key        text NOT NULL,
    change_reason          text,
    created_by             uuid REFERENCES users(id),
    created_at             timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, idempotency_key),
    UNIQUE (service_request_id, version),
    CHECK (replaces IS NULL OR replaces <> id),
    CHECK (status NOT IN ('amended', 'corrected') OR replaces IS NOT NULL)
);
CREATE INDEX diagnostic_reports_order ON diagnostic_reports (service_request_id, version DESC);
CREATE INDEX diagnostic_reports_patient ON diagnostic_reports (tenant_id, patient_id, issued_at DESC);
CREATE INDEX diagnostic_reports_replaces ON diagnostic_reports (replaces) WHERE replaces IS NOT NULL;

-- ---------------------------------------------------------------------------
-- 4. Typed result components: `observations` generalized
-- ---------------------------------------------------------------------------

ALTER TABLE observations
    ALTER COLUMN value_num DROP NOT NULL,
    ALTER COLUMN unit DROP NOT NULL,
    ADD COLUMN value_type         text NOT NULL DEFAULT 'quantity' CHECK (value_type IN (
                                      'quantity', 'text', 'coded', 'boolean', 'datetime', 'narrative')),
    ADD COLUMN code_system        text NOT NULL DEFAULT 'http://loinc.org',
    ADD COLUMN display            text,
    ADD COLUMN value_text         text,
    ADD COLUMN value_code         text,
    ADD COLUMN value_code_system  text,
    ADD COLUMN value_code_display text,
    ADD COLUMN value_bool         boolean,
    ADD COLUMN value_datetime     timestamptz,
    ADD COLUMN value_narrative    text,
    -- Deterministic interpretation (configured rules / reference range);
    -- `unknown` when no rule applies.
    ADD COLUMN interpretation     text NOT NULL DEFAULT 'unknown' CHECK (interpretation IN (
                                      'normal', 'abnormal', 'critical', 'unknown')),
    ADD COLUMN diagnostic_report_id uuid REFERENCES diagnostic_reports(id),
    ADD COLUMN sequence           int NOT NULL DEFAULT 0,
    ADD CONSTRAINT observations_typed_value CHECK (
        CASE value_type
            WHEN 'quantity'  THEN value_num IS NOT NULL AND unit IS NOT NULL
            WHEN 'text'      THEN value_text IS NOT NULL
            WHEN 'coded'     THEN value_code IS NOT NULL AND value_code_system IS NOT NULL
            WHEN 'boolean'   THEN value_bool IS NOT NULL
            WHEN 'datetime'  THEN value_datetime IS NOT NULL
            WHEN 'narrative' THEN value_narrative IS NOT NULL
        END);
CREATE INDEX observations_report ON observations (diagnostic_report_id, sequence) WHERE diagnostic_report_id IS NOT NULL;
CREATE INDEX observations_patient_code ON observations (tenant_id, patient_id, code_loinc, effective_at DESC);

-- Critical alerts may now anchor on a report (coded / narrative criticality)
-- as well as on a numeric observation.
ALTER TABLE alerts
    ALTER COLUMN observation_id DROP NOT NULL,
    ADD COLUMN diagnostic_report_id uuid REFERENCES diagnostic_reports(id),
    ADD CONSTRAINT alerts_anchor CHECK (observation_id IS NOT NULL OR diagnostic_report_id IS NOT NULL);
CREATE INDEX alerts_report ON alerts (diagnostic_report_id) WHERE diagnostic_report_id IS NOT NULL;

-- ---------------------------------------------------------------------------
-- 5. Specimens and chain of custody
-- ---------------------------------------------------------------------------

CREATE TABLE specimens (
    id                  uuid PRIMARY KEY,
    tenant_id           uuid NOT NULL REFERENCES tenants(id),
    patient_id          uuid NOT NULL REFERENCES patients(id),
    service_request_id  uuid NOT NULL REFERENCES service_requests(id),
    -- Human-readable accession / barcode identifier, unique per tenant.
    identifier          text NOT NULL,
    specimen_type_code  text NOT NULL,
    container_code      text,
    body_site           text,
    status              text NOT NULL DEFAULT 'planned' CHECK (status IN (
                           'planned', 'collected', 'in_transit', 'received', 'processing',
                           'processed', 'rejected', 'consumed')),
    collected_at        timestamptz,
    collected_by        uuid REFERENCES users(id),
    collection_facility_id uuid REFERENCES facilities(id),
    rejection_reason    text,
    -- The specimen this one replaces after a rejection.
    recollection_of     uuid REFERENCES specimens(id),
    version             bigint NOT NULL DEFAULT 1,
    created_by          uuid NOT NULL REFERENCES users(id),
    created_at          timestamptz NOT NULL DEFAULT now(),
    updated_at          timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, identifier)
);
CREATE INDEX specimens_order ON specimens (service_request_id, created_at);

CREATE TABLE specimen_events (
    id            uuid PRIMARY KEY,
    tenant_id     uuid NOT NULL REFERENCES tenants(id),
    specimen_id   uuid NOT NULL REFERENCES specimens(id),
    event         text NOT NULL CHECK (event IN (
                     'planned', 'collected', 'dispatched', 'received', 'processing_started',
                     'processed', 'rejected', 'recollection_requested', 'consumed')),
    from_status   text,
    to_status     text NOT NULL,
    facility_id   uuid REFERENCES facilities(id),
    location_note text,
    reason        text,
    actor_user_id uuid REFERENCES users(id),
    actor         text NOT NULL,
    recorded_at   timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX specimen_events_specimen ON specimen_events (specimen_id, recorded_at);

-- ---------------------------------------------------------------------------
-- 6. Deterministic safety evaluations (diagnostic-safety.v1)
-- ---------------------------------------------------------------------------

CREATE TABLE diagnostic_safety_evaluations (
    id                 uuid PRIMARY KEY,
    tenant_id          uuid NOT NULL REFERENCES tenants(id),
    patient_id         uuid NOT NULL REFERENCES patients(id),
    encounter_id       uuid NOT NULL REFERENCES encounters(id),
    engine_version     text NOT NULL,
    -- Hash of the exact (orderables, answers, facts) input the findings
    -- were produced from; confirmation must present the same hash.
    input_hash         text NOT NULL,
    orderable_ids      uuid[] NOT NULL,
    findings           jsonb NOT NULL,
    hard_stops         int NOT NULL DEFAULT 0,
    warnings           int NOT NULL DEFAULT 0,
    acknowledged_ids   text[] NOT NULL DEFAULT '{}',
    override_reason    text,
    overridden_by      uuid REFERENCES users(id),
    overridden_at      timestamptz,
    evaluated_by       uuid NOT NULL REFERENCES users(id),
    evaluated_at       timestamptz NOT NULL DEFAULT now()
);
CREATE INDEX diagnostic_safety_evaluations_encounter ON diagnostic_safety_evaluations (tenant_id, encounter_id, evaluated_at DESC);

ALTER TABLE diagnostic_order_groups
    ADD CONSTRAINT diagnostic_order_groups_safety_fkey
        FOREIGN KEY (safety_evaluation_id) REFERENCES diagnostic_safety_evaluations(id);

-- ---------------------------------------------------------------------------
-- 7. Professional review, release decisions, approved explanation
-- ---------------------------------------------------------------------------

CREATE TABLE diagnostic_reviews (
    id                   uuid PRIMARY KEY,
    tenant_id            uuid NOT NULL REFERENCES tenants(id),
    patient_id           uuid NOT NULL REFERENCES patients(id),
    service_request_id   uuid NOT NULL REFERENCES service_requests(id),
    -- Exact report row (and therefore version) the professional reviewed.
    diagnostic_report_id uuid NOT NULL REFERENCES diagnostic_reports(id),
    report_version       bigint NOT NULL,
    reviewer_id          uuid NOT NULL REFERENCES users(id),
    clinical_assessment  text NOT NULL,
    disposition          text NOT NULL CHECK (disposition IN (
                            'no_action', 'routine_follow_up', 'urgent_follow_up', 'repeat_test',
                            'referral', 'immediate_contact', 'other')),
    disposition_note     text,
    -- Follow-up tasks and orders the human created from this review.
    follow_up_task_ids   uuid[] NOT NULL DEFAULT '{}',
    synthesis_artifact_id uuid REFERENCES ai_artifacts(id),
    reviewed_at          timestamptz NOT NULL DEFAULT now(),
    UNIQUE (diagnostic_report_id, reviewer_id)
);
CREATE INDEX diagnostic_reviews_order ON diagnostic_reviews (service_request_id, reviewed_at DESC);

CREATE TABLE result_release_decisions (
    id                     uuid PRIMARY KEY,
    tenant_id              uuid NOT NULL REFERENCES tenants(id),
    patient_id             uuid NOT NULL REFERENCES patients(id),
    service_request_id     uuid NOT NULL REFERENCES service_requests(id),
    diagnostic_report_id   uuid NOT NULL REFERENCES diagnostic_reports(id),
    report_version         bigint NOT NULL,
    review_id              uuid NOT NULL REFERENCES diagnostic_reviews(id),
    decision               text NOT NULL CHECK (decision IN ('release', 'withhold')),
    withhold_reason        text,
    -- Clinician-approved (possibly edited) plain-language explanation; the
    -- dMind draft it was derived from, when any.
    explanation_en         text,
    explanation_es         text,
    explanation_artifact_id uuid REFERENCES ai_artifacts(id),
    notify_patient         boolean NOT NULL DEFAULT false,
    notification_id        uuid REFERENCES notifications(id),
    decided_by             uuid NOT NULL REFERENCES users(id),
    decided_at             timestamptz NOT NULL DEFAULT now(),
    -- A later decision on a newer report version supersedes this one.
    superseded_at          timestamptz,
    CHECK (decision <> 'withhold' OR withhold_reason IS NOT NULL)
);
CREATE INDEX result_release_decisions_order ON result_release_decisions (service_request_id, decided_at DESC);
CREATE INDEX result_release_decisions_patient ON result_release_decisions (tenant_id, patient_id, decided_at DESC)
    WHERE decision = 'release' AND superseded_at IS NULL;

-- ---------------------------------------------------------------------------
-- 8. Clinical documents and external imaging-study references
-- ---------------------------------------------------------------------------

CREATE TABLE clinical_documents (
    id                   uuid PRIMARY KEY,
    tenant_id            uuid NOT NULL REFERENCES tenants(id),
    patient_id           uuid NOT NULL REFERENCES patients(id),
    service_request_id   uuid REFERENCES service_requests(id),
    diagnostic_report_id uuid REFERENCES diagnostic_reports(id),
    kind                 text NOT NULL CHECK (kind IN (
                            'report_pdf', 'image', 'tracing', 'referral', 'consent', 'other')),
    title                text NOT NULL,
    mime_type            text NOT NULL,
    size_bytes           bigint NOT NULL CHECK (size_bytes >= 0),
    checksum_sha256      text NOT NULL,
    -- Tenant-scoped object key in the configured store; never a URL.
    object_key           text NOT NULL,
    store_kind           text NOT NULL,
    -- Documents stay quarantined until the configured scanner clears them;
    -- only `clean` documents can be read or released.
    status               text NOT NULL DEFAULT 'quarantined' CHECK (status IN (
                            'quarantined', 'scanning', 'clean', 'rejected', 'deleted')),
    scan_verdict         text,
    scanned_at           timestamptz,
    released             boolean NOT NULL DEFAULT false,
    uploaded_by          uuid REFERENCES users(id),
    source_system        text NOT NULL,
    created_at           timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, object_key)
);
CREATE INDEX clinical_documents_order ON clinical_documents (service_request_id) WHERE service_request_id IS NOT NULL;
CREATE INDEX clinical_documents_report ON clinical_documents (diagnostic_report_id) WHERE diagnostic_report_id IS NOT NULL;

CREATE TABLE imaging_studies (
    id                   uuid PRIMARY KEY,
    tenant_id            uuid NOT NULL REFERENCES tenants(id),
    patient_id           uuid NOT NULL REFERENCES patients(id),
    service_request_id   uuid NOT NULL REFERENCES service_requests(id),
    diagnostic_report_id uuid REFERENCES diagnostic_reports(id),
    study_instance_uid   text NOT NULL,
    accession_number     text,
    modality_code        text NOT NULL,
    description          text,
    -- [{"series_instance_uid": "...", "modality": "CT", "number_of_instances": 120, "description": "..."}]
    series               jsonb NOT NULL DEFAULT '[]'::jsonb,
    number_of_series     int NOT NULL DEFAULT 0,
    number_of_instances  int NOT NULL DEFAULT 0,
    -- Controlled reference: the configured PACS endpoint code, never a raw
    -- URL chosen by the sender. Pixels never enter PostgreSQL.
    pacs_endpoint_code   text,
    status               text NOT NULL DEFAULT 'available' CHECK (status IN (
                            'registered', 'available', 'cancelled', 'entered_in_error')),
    started_at           timestamptz,
    source_system        text NOT NULL,
    idempotency_key      text NOT NULL,
    created_at           timestamptz NOT NULL DEFAULT now(),
    UNIQUE (tenant_id, idempotency_key),
    UNIQUE (tenant_id, study_instance_uid)
);
CREATE INDEX imaging_studies_order ON imaging_studies (service_request_id);

-- ---------------------------------------------------------------------------
-- 9. dMind bindings and notification kinds
-- ---------------------------------------------------------------------------

ALTER TABLE ai_artifacts
    ADD COLUMN diagnostic_report_id uuid REFERENCES diagnostic_reports(id),
    ADD COLUMN order_group_id       uuid REFERENCES diagnostic_order_groups(id);
CREATE INDEX ai_artifacts_report ON ai_artifacts (tenant_id, diagnostic_report_id) WHERE diagnostic_report_id IS NOT NULL;
CREATE INDEX ai_artifacts_order_group ON ai_artifacts (tenant_id, order_group_id) WHERE order_group_id IS NOT NULL;

ALTER TABLE notifications DROP CONSTRAINT notifications_kind_check;
ALTER TABLE notifications ADD CONSTRAINT notifications_kind_check CHECK (kind IN (
    'booking_confirmation', 'reschedule', 'cancellation', 'reminder',
    'preparation', 'confirmation_request', 'confirmation_follow_up',
    'waitlist_offer', 'waitlist_offer_expired', 'transport_status',
    'no_response_follow_up',
    'diagnostic_result_released', 'diagnostic_report_review', 'diagnostic_order_conflict',
    'diagnostic_follow_up'));
ALTER TABLE notifications
    ADD COLUMN service_request_id   uuid REFERENCES service_requests(id),
    ADD COLUMN diagnostic_report_id uuid REFERENCES diagnostic_reports(id);

-- ---------------------------------------------------------------------------
-- 10. Legacy laboratory orders -> generalized model (lossless)
-- ---------------------------------------------------------------------------

-- One `diagnostic_orderable` per (tenant, LOINC, display) in use, created by
-- the earliest requester of that test. Codes are derived from the LOINC
-- code (`loinc-2823-3`); tenants may rename them later through the normal
-- versioned catalog administration.
INSERT INTO catalog_entries (id, tenant_id, kind, code, name_en, name_es, synonyms, external_codings,
                             config, active, version, created_by, created_at, updated_at)
SELECT gen_random_uuid(),
       l.tenant_id,
       'diagnostic_orderable',
       'loinc-' || lower(regexp_replace(l.code_loinc, '[^A-Za-z0-9.-]', '-', 'g')),
       l.display,
       l.display,
       '{}',
       jsonb_build_array(jsonb_build_object('system', 'http://loinc.org', 'code', l.code_loinc)),
       jsonb_build_object(
           'category_code', 'laboratory',
           'modality_code', 'laboratory',
           'result_type', 'quantity',
           'fulfilment_modes', jsonb_build_array('immediate', 'scheduled', 'inpatient', 'bedside'),
           'specimen', jsonb_build_object('type_code', 'blood_venous', 'container_code', 'serum_tube'),
           'components', jsonb_build_array(jsonb_build_object(
               'code', l.code_loinc, 'system', 'http://loinc.org', 'display', l.display, 'result_type', 'quantity')),
           'duplicate_window_days', 1,
           'legacy_migrated', true),
       true,
       1,
       l.requester_id,
       now(),
       now()
FROM (
    SELECT DISTINCT ON (sr.tenant_id, sr.code_loinc)
           sr.tenant_id, sr.code_loinc, sr.display, sr.requester_id
    FROM service_requests sr
    WHERE sr.code_loinc IS NOT NULL
    ORDER BY sr.tenant_id, sr.code_loinc, sr.created_at, sr.id
) l
ON CONFLICT (tenant_id, kind, code) DO NOTHING;

INSERT INTO catalog_entry_history (id, tenant_id, entry_id, version, snapshot, change_reason, changed_by, recorded_at)
SELECT gen_random_uuid(), c.tenant_id, c.id, 1, to_jsonb(c) - 'created_by',
       'migrated from legacy laboratory order', c.created_by, now()
FROM catalog_entries c
WHERE c.kind = 'diagnostic_orderable'
  AND (c.config->>'legacy_migrated')::boolean IS TRUE
  AND NOT EXISTS (SELECT 1 FROM catalog_entry_history h WHERE h.entry_id = c.id AND h.version = 1);

UPDATE service_requests sr
SET orderable_id         = c.id,
    orderable_code       = c.code,
    orderable_version    = c.version,
    category_code        = 'laboratory',
    modality_code        = 'laboratory',
    expected_result_type = 'quantity',
    fulfilment_mode      = 'immediate',
    order_status         = CASE
                               WHEN sr.status <> 'active' THEN 'cancelled'
                               WHEN sr.loop_state = 'ordered' THEN 'placed'
                               ELSE 'completed'
                           END,
    completed_at         = CASE WHEN sr.status = 'active' AND sr.loop_state <> 'ordered'
                                THEN (SELECT MIN(o.received_at) FROM observations o WHERE o.service_request_id = sr.id)
                           END,
    source_system        = 'legacy_lab_order'
FROM catalog_entries c
WHERE sr.orderable_id IS NULL
  AND c.tenant_id = sr.tenant_id
  AND c.kind = 'diagnostic_orderable'
  AND c.code = 'loinc-' || lower(regexp_replace(sr.code_loinc, '[^A-Za-z0-9.-]', '-', 'g'));

INSERT INTO service_request_history (id, tenant_id, service_request_id, from_status, to_status, version, reason, actor, details, recorded_at)
SELECT gen_random_uuid(), sr.tenant_id, sr.id, NULL, sr.order_status, sr.version,
       'migrated from legacy laboratory order', 'migration:0016',
       jsonb_build_object('loop_state', sr.loop_state, 'code_loinc', sr.code_loinc), sr.created_at
FROM service_requests sr
WHERE sr.source_system = 'legacy_lab_order'
  AND NOT EXISTS (SELECT 1 FROM service_request_history h WHERE h.service_request_id = sr.id);

-- Legacy numeric observations are quantity components; a recorded critical
-- rule evaluation is the only deterministic interpretation available.
UPDATE observations o
SET interpretation = 'critical'
WHERE o.value_type = 'quantity' AND o.interpretation = 'unknown'
  AND EXISTS (SELECT 1 FROM rule_evaluations re WHERE re.observation_id = o.id AND re.outcome ? 'Critical');
