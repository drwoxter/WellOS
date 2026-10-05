# dMind Clinical Orders & Diagnostics v1 — orders, safety, fulfilment, reports and release

Replaces the single "potassium/glucose result loop" with a general,
AI-native but human-accountable diagnostic pathway:

```
consultation → dMind-assisted order proposal → clinician confirmation →
deterministic preflight → scheduling (dMind Access) or immediate fulfilment →
specimen / acquisition → typed results → report (preliminary/final/amended/
corrected) → deterministic criticality → dMind synthesis draft →
professional review → clinician-approved release + notification →
follow-up and closed loop
```

Every order, review, release and follow-up decision belongs to an
accountable human professional. dMind is assistive, bounded, auditable and
never authoritative; every workflow below behaves identically with AI
`disabled`, `degraded` or `invalid_configuration`.

## 1. Data model (migration `0016_dmind_clinical_diagnostics.sql`)

- `diagnostic_order_groups` — one confirmation = one group (encounter,
  ordering practitioner, indication, clinical question, priority,
  performing facility, bound `safety_evaluation_id`, idempotency key).
- `diagnostic_orders` — versioned orders (`version`, optimistic
  concurrency), `fulfilment_mode`
  (`scheduled | immediate | inpatient | bedside | walk_in`), per-order
  priority (`routine | urgent | stat | timed`), preparation and
  accessibility requirements, `access_request_id` / `appointment_id`
  links, `legacy_service_request_id` for migrated rows.
- `diagnostic_order_history` — append-only transitions with actor, reason
  and version.
- `diagnostic_safety_evaluations` — persisted preflight results: engine
  version, exact `input_hash`, findings, acknowledgements, override reason
  and actor.
- `specimens` + `specimen_custody_events` — type, container, collector,
  collected-at, label identifiers, custody chain (`collected`,
  `labelled`, `in_transit`, `received`, `accessioned`, `rejected`, …).
- `diagnostic_reports` — version chain (`version`, `replaces_report_id`),
  status `preliminary | final | amended | corrected | cancelled |
  entered_in_error`, conclusion + coded conclusions, deterministic
  `criticality` (`normal | abnormal | critical`) with the rule set that
  produced it.
- `diagnostic_report_components` — append-only typed observations
  (`quantity`, `coded`, `text`, `ratio`, `range`, `boolean`, `date_time`)
  with UCUM units, reference ranges, interpretation and provenance
  (device/analyser, performer, source message).
- `diagnostic_reviews` — professional review bound to an exact report
  version: disposition (`no_action`, `routine_follow_up`,
  `urgent_follow_up`, `repeat_test`, `referral`, `immediate_contact`,
  `other`), assessment, follow-ups.
- `diagnostic_release_decisions` — `release | withhold` bound to report
  version + review id, patient-facing explanation (EN/ES), optional
  approved explanation artifact, released documents, notification state.
- `clinical_documents` + `imaging_studies` — ObjectStore-backed documents
  (status `quarantined → clean | rejected`; only `clean` documents are
  downloadable) and
  imaging study references (DICOM study/series identifiers only; no pixel
  data) attached to an order/report.
- Legacy potassium/glucose `service_requests` and `observations` are
  migrated once into orders/reports/components (idempotent; verified by
  `upgrade_migration_0016_integration`).

## 2. Runtime catalog (`catalog_entries.kind = 'diagnostic_orderable'`)

Orderables are tenant data, never code: EN/ES names, synonyms, external
codings (LOINC/SNOMED/CPT-like), validated `OrderableConfig`
(`category_code`, `modality_code`, `result_type`, `components`,
`panel_member_codes`, `specimen`, `preparation_en/es`,
`scheduling_service_code`, `required_resource_types`, `fulfilment_modes`,
`safety_rules`, `duplicate_window_days`, `redundant_with_codes`,
`critical_conclusion_codes`, `requires_specimen`, `expects_imaging_study`),
facility mappings, effective dates and version history
(`catalog_entry_history`). Administration reuses the Access catalog
routes (`POST /api/v1/catalog`, `POST /api/v1/catalog/:id`,
`POST /api/v1/catalog/:id/deactivate`, `GET /api/v1/catalog/:id/history`);
search is `GET /api/v1/diagnostics/catalog?q=&facility_id=&lang=` and is
the only source of orderables for the composer and for dMind.

## 3. Order lifecycle (`wellos_domain::diagnostics`)

```
placed → accepted → scheduled → in_progress → completed
          ↕ on_hold (resume returns to accepted)
any open state → cancelled | rejected | entered_in_error
```

Scheduling is an API operation (`POST /orders/:id/schedule`, which
creates an Access request / appointment link), not a transition; the
matcher, holds and bookings are the dMind Access ones. `immediate`,
`inpatient`, `bedside` and `walk_in` orders may start without an
appointment only when the mode is recorded explicitly on the order.
The fulfilment mode cannot change while an appointment is linked
(`409 appointment_linked`), and appointment reschedule/cancel/no-show never
mutates the order silently: the divergence is exposed as a `schedule_conflict` on the order detail and in
the worklist for a human decision.

## 4. Deterministic safety — `diagnostic-safety.v1`

Preflight (`POST /api/v1/encounters/:id/diagnostic-orders/preflight`)
evaluates the exact composition against server facts only: duplicate /
pending orders within the orderable's window, specimen and preparation
requirements, prerequisites, contraindications (`allergy:*`,
`medication:*`, `condition:*` facts), timing windows, redundant
combinations and fulfilment restrictions. Findings are `info`, `warning`
(must be acknowledged by id) or `hard_stop` (requires correction or an
authorised override — `diagnostic_safety.override` plus a reason ≥ 10
characters). Confirmation (`POST .../diagnostic-orders`) binds to the
persisted evaluation id and its `input_hash`; any change to the
composition, facts or catalog since the preflight yields
`409 stale_safety_evaluation` and a fresh preflight is required. The
same `Idempotency-Key` returns the original group; a different body under
the same key is `409 idempotency_conflict`.

## 5. dMind diagnostic operations (gateway, governed by `aigov`)

| Template | Input (server-supplied only) | Output | Bound to |
| --- | --- | --- | --- |
| `diagnostic-order-suggestion@1.0.0` | ≤ 60 catalog candidates, ≤ 120 patient/encounter facts, indication | suggested orderable ids ∈ candidates, rationale, cited facts, limitations | encounter + facts hash |
| `diagnostic-result-synthesis@1.0.0` | report version, typed components, deterministic criticality, order context | clinician-facing draft synthesis with citations; cannot lower criticality | exact report version |
| `patient-result-explanation@1.0.0` | released-eligible report version, approved synthesis | plain-language EN/ES draft | exact report version; requires `approved` review before use |

dMind cannot invent orderables, create orders, set final urgency, bypass
preflight, sign or release reports, close critical loops or notify
patients. Drafts are `AIArtifact`s with the standard lifecycle
(reviewed/approved/rejected/superseded/invalidated); artifacts of a
previous report version are superseded on amendment/correction.

## 6. Results, reports, review and release

- Typed ingestion (`POST /orders/:id/reports`) appends components and
  creates/extends the report chain; criticality is recomputed
  deterministically per component and conclusion code on every version.
- Amendments and corrections create a new version that `replaces` the
  previous one and reopen review and release; `entered_in_error`
  withdraws the report from every patient view.
- `POST /reports/:id/review` requires `diagnostic_report.review`, binds to
  the exact version and records disposition and follow-ups. Critical
  reports additionally appear on the review worklist
  (`GET /api/v1/diagnostics/reviews`) until reviewed.
- `POST /reports/:id/release` requires `diagnostic_result.release`, the
  review id of the same version and an explicit `release | withhold`
  decision; abnormal/critical results need a bilingual patient
  explanation (approved artifact or typed text). Nothing is released or
  notified automatically; patient notification is created only inside the
  release transaction.
- Patient self-service (`GET /api/v1/me/diagnostics`,
  `/me/diagnostics/:report_id`, document download) is derived from active
  `patient_access_grants` and shows pending orders, "under review" status
  without values, and released reports with the approved explanation only.

## 7. Documents, imaging and interoperability

- `ObjectStore` (`WELLOS_OBJECT_STORE=disabled|fixture|s3`): upload
  intents, completion, scan status and short-lived download URLs
  (`WELLOS_OBJECT_URL_TTL_SECS`, ≤ 900 s; `WELLOS_OBJECT_MAX_BYTES`).
  `disabled` answers `503 object_store_unavailable` for documents while
  every other workflow continues; `fixture` is refused outside
  development/test builds; `s3` requires credentials and an HTTPS
  endpoint outside local environments.
- Imaging studies are references (study/series identifiers, modality,
  description; ≤ 200 series); no pixel data is stored.
- FHIR R4 subset: `ServiceRequest`, `Specimen`, `Observation`,
  `DiagnosticReport` (with `urn:wellos:replaces`), `DocumentReference`,
  `ImagingStudy`; inbound `POST /fhir/r4/DiagnosticReport` from a scoped
  service credential is idempotent, validated, maps to typed components and
  runs the same criticality/review path (never released).

## 8. Authorization

Permissions: `diagnostic_order.manage`, `diagnostic_safety.override`,
`diagnostic.read`, `diagnostic.fulfil`, `specimen.handle`,
`diagnostic_report.write`, `diagnostic_report.review`,
`diagnostic_result.release`, `catalog.manage`. Roles:
physician (order, override, review, release), nurse (read, fulfil,
specimens), `diagnostic_professional` and `laboratory_professional`
(fulfil, specimens, write reports), `clinical_admin` (catalog),
`patient_representative` (self-service only). Every route enforces tenant,
facility and care relationship through the central policy and
purpose-of-use; capability flags in UI payloads are presentation hints only.

## 9. Web

`/diagnostics` (orders + review worklists, filters, keyboard tabs),
`/diagnostics/orders/:id` (transitions, scheduling, specimens, documents,
result entry, reports), `/diagnostics/reports/:id` (critical banner,
components, synthesis draft, review, release with explicit confirmation),
`/diagnostics/catalog` (runtime orderables, history), order composer in the
consultation and Patient 360, pending diagnostics and trends in the Patient
Brief/360, and `/my/diagnostics` for patients. EN/ES, 390 px, AI-state
aware (dMind actions disabled when the capability is not `ready`).

## 10. Non-goals and limitations

No autonomous ordering, release or notification; no DICOM storage or
viewer; no HL7v2 interface; safety rules are tenant data and not a
validated clinical knowledge base; criticality rules are deterministic
thresholds that require clinical sign-off before any real use. Hazards
H-34–H-42 in `docs/clinical-safety/hazard-log.md`.
