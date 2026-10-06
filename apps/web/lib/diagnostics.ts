/**
 * Typed contracts and helpers for dMind Clinical Orders & Diagnostics v1.
 *
 * Everything shown by the diagnostics surfaces comes from the server: the
 * runtime catalog, deterministic safety findings, order state, specimens,
 * typed report components, professional reviews and release decisions.
 * Nothing here is authoritative — capability flags gate display only and
 * every action is re-authorized server-side.
 */
import { t, type Lang, type TKey } from "./i18n";
import { apiFetch } from "./session";

// ---------------------------------------------------------------------------
// Server-derived capability hints
// ---------------------------------------------------------------------------

export type DiagnosticsCapabilities = {
  can_read: boolean;
  can_order: boolean;
  can_override_safety: boolean;
  can_fulfil: boolean;
  can_handle_specimens: boolean;
  can_write_reports: boolean;
  can_review: boolean;
  can_release: boolean;
  can_manage_catalog: boolean;
  self_service: boolean;
};

export const NO_DIAGNOSTICS_CAPABILITIES: DiagnosticsCapabilities = {
  can_read: false,
  can_order: false,
  can_override_safety: false,
  can_fulfil: false,
  can_handle_specimens: false,
  can_write_reports: false,
  can_review: false,
  can_release: false,
  can_manage_catalog: false,
  self_service: false,
};

/** Whether the staff `/diagnostics` workspace has anything to show. */
export function hasDiagnosticsWorkspaceAccess(
  caps: DiagnosticsCapabilities | undefined,
): boolean {
  return Boolean(caps && (caps.can_read || caps.can_review));
}

// ---------------------------------------------------------------------------
// Catalog (runtime-administered diagnostic orderables)
// ---------------------------------------------------------------------------

export const FULFILMENT_MODES = [
  "scheduled",
  "immediate",
  "inpatient",
  "bedside",
  "walk_in",
] as const;
export type FulfilmentMode = (typeof FULFILMENT_MODES)[number];

export const PRIORITIES = ["routine", "urgent", "stat", "timed"] as const;
export type Priority = (typeof PRIORITIES)[number];

export const RESULT_TYPES = [
  "quantity",
  "text",
  "coded",
  "boolean",
  "datetime",
  "narrative",
] as const;
export type ResultType = (typeof RESULT_TYPES)[number];

export type ComponentSpec = {
  code: string;
  system?: string;
  display: string;
  result_type: ResultType;
  unit?: string | null;
  reference_range?: string | null;
};

export type SpecimenSpec = {
  type_code: string;
  container_code?: string | null;
  minimum_volume_ml?: number | null;
  fasting_hours?: number | null;
};

export type SafetyRuleSpec = {
  id: string;
  kind: "question" | "fact" | "prerequisite";
  text_en: string;
  text_es: string;
  severity: "warning" | "hard_stop";
  fact_key?: string | null;
};

export type Orderable = {
  id: string;
  code: string;
  name: string;
  name_en: string;
  name_es: string;
  synonyms: string[];
  external_codings: unknown;
  category_code: string;
  modality_code: string | null;
  result_type: ResultType;
  components: ComponentSpec[];
  panel_member_codes: string[];
  specimen: SpecimenSpec | null;
  preparation: string | null;
  preparation_en: string | null;
  preparation_es: string | null;
  scheduling_service_code: string | null;
  required_resource_types: string[];
  fulfilment_modes: FulfilmentMode[];
  safety_rules: SafetyRuleSpec[];
  duplicate_window_days: number;
  redundant_with_codes: string[];
  requires_specimen: boolean;
  expects_imaging_study: boolean;
  active: boolean;
  version: number;
  facility_ids: string[];
};

export type CatalogSearch = {
  q?: string;
  category?: string;
  modality?: string;
  facility_id?: string;
  lang?: Lang;
  include_inactive?: boolean;
  limit?: number;
};

export function query(params: Record<string, unknown>): string {
  const sp = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v === undefined || v === null || v === "" || v === false) continue;
    sp.set(k, String(v));
  }
  const s = sp.toString();
  return s ? `?${s}` : "";
}

export function searchCatalog(
  q: CatalogSearch,
): Promise<{ items: Orderable[] }> {
  return apiFetch(`/api/v1/diagnostics/catalog${query(q)}`);
}

// ---------------------------------------------------------------------------
// Composer: deterministic preflight, dMind suggestions, confirmation
// ---------------------------------------------------------------------------

export type ItemInput = {
  orderable_id: string;
  fulfilment_mode?: FulfilmentMode;
  priority?: Priority;
  requested_window_start?: string;
  requested_window_end?: string;
};

export type Composition = {
  items: ItemInput[];
  answers: Record<string, boolean>;
  performing_facility_id: string;
  priority: Priority;
  lang: Lang;
};

export type SafetyFinding = {
  id: string;
  kind: string;
  severity: "warning" | "hard_stop";
  orderable_id: string;
  orderable_code: string;
  text: string;
  text_en: string;
  text_es: string;
  evidence: string[];
  answerable: boolean;
};

export type SafetyCandidate = {
  orderable_id: string;
  code: string;
  name: string;
  fulfilment_mode: FulfilmentMode;
  priority: Priority;
  requested_window_start: string | null;
  requested_window_end: string | null;
  requires_appointment: boolean;
  scheduling_service_code: string | null;
  needs_specimen: boolean;
  preparation: string | null;
};

export type SafetyEvaluation = {
  id: string;
  engine_version: string;
  input_hash: string;
  evaluated_at: string;
  performing_facility_id: string;
  candidates: SafetyCandidate[];
  findings: SafetyFinding[];
  warnings: number;
  hard_stops: number;
  requires_acknowledgement: boolean;
  requires_override: boolean;
};

export function preflight(
  encounterId: string,
  body: Composition,
): Promise<SafetyEvaluation> {
  return apiFetch<{ evaluation: SafetyEvaluation }>(
    `/api/v1/encounters/${encounterId}/diagnostic-orders/preflight`,
    { method: "POST", body: JSON.stringify(body) },
  ).then((r) => r.evaluation);
}

export type OrderSuggestion = {
  orderable_id: string;
  code: string;
  name: string;
  category_code: string;
  default_fulfilment_mode: FulfilmentMode;
  rationale: string;
  cited_sources: string[];
  proposed_timing: string | null;
  preparation_note: string | null;
};

export type SuggestionDraft = {
  artifact_id: string;
  encounter_id: string;
  patient_id: string;
  template: string;
  prompt_version: string;
  model: string;
  input_hash: string;
  reused_from: string | null;
  synthetic: boolean;
  suggestions: OrderSuggestion[];
  duplicate_warnings: {
    orderable_id: string;
    reason: string;
    cited_sources: string[];
  }[];
  missing_information: string[];
  cited_sources: string[];
  confidence: number | null;
  limitations: string[];
  autonomy_level: string;
  status: string;
};

export function suggestOrders(
  encounterId: string,
  body: { q?: string; lang: Lang; facility_id: string },
): Promise<SuggestionDraft> {
  return apiFetch(
    `/api/v1/encounters/${encounterId}/diagnostic-orders/suggest`,
    { method: "POST", body: JSON.stringify(body) },
  );
}

export type ConfirmBody = Composition & {
  safety_evaluation_id: string;
  acknowledged_ids: string[];
  override_reason?: string;
  clinical_indication: string;
  clinical_question?: string;
  suggestion_artifact_id?: string;
  idempotency_key: string;
  schedule: boolean;
};

export type OrderGroup = {
  id: string;
  priority: Priority;
  clinical_indication: string | null;
  clinical_question: string | null;
  safety_evaluation_id: string;
  suggestion_artifact_id: string | null;
  created_at: string;
  orders: DiagnosticOrder[];
  replayed?: boolean;
};

export function confirmOrders(
  encounterId: string,
  body: ConfirmBody,
): Promise<{ group: OrderGroup }> {
  return apiFetch(`/api/v1/encounters/${encounterId}/diagnostic-orders`, {
    method: "POST",
    body: JSON.stringify(body),
  });
}

/** Browser-side idempotency key: one per confirmation attempt, reused on retry. */
export function newIdempotencyKey(): string {
  if (typeof crypto !== "undefined" && "randomUUID" in crypto) {
    return crypto.randomUUID();
  }
  return `web-${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

// ---------------------------------------------------------------------------
// Orders
// ---------------------------------------------------------------------------

export const ORDER_STATUSES = [
  "placed",
  "accepted",
  "scheduled",
  "in_progress",
  "completed",
  "on_hold",
  "cancelled",
  "rejected",
  "entered_in_error",
] as const;
export type OrderStatus = (typeof ORDER_STATUSES)[number];

export const OPEN_ORDER_STATUSES: readonly OrderStatus[] = [
  "placed",
  "accepted",
  "scheduled",
  "in_progress",
  "on_hold",
];

export type PatientSummary = {
  id: string;
  given_name: string;
  family_name: string;
};

export type DiagnosticOrder = {
  id: string;
  patient_id: string;
  encounter_id: string | null;
  requester_id: string;
  order_group_id: string | null;
  orderable_id: string | null;
  orderable_code: string | null;
  orderable_version: number | null;
  code_loinc: string | null;
  display: string;
  category_code: string | null;
  modality_code: string | null;
  expected_result_type: ResultType | null;
  order_status: OrderStatus;
  loop_state: string;
  fulfilment_mode: FulfilmentMode;
  priority: Priority;
  clinical_indication: string | null;
  clinical_question: string | null;
  requested_window_start: string | null;
  requested_window_end: string | null;
  preparation_en: string | null;
  preparation_es: string | null;
  performing_facility_id: string | null;
  performing_service_code: string | null;
  performing_professional_id: string | null;
  performing_resource_id: string | null;
  access_request_id: string | null;
  appointment_id: string | null;
  schedule_conflict: string | null;
  hold_reason: string | null;
  cancellation_reason: string | null;
  rejection_reason: string | null;
  accepted_at: string | null;
  started_at: string | null;
  completed_at: string | null;
  cancelled_at: string | null;
  source_system: string | null;
  version: number;
  created_at: string;
  updated_at: string;
  latest_report_status?: string | null;
  patient?: PatientSummary | null;
};

export type OrderHistoryEntry = {
  id: string;
  from_status: string | null;
  to_status: string;
  actor_user_id: string | null;
  actor: string | null;
  reason: string | null;
  details: unknown;
  recorded_at: string;
};

export type Specimen = {
  id: string;
  service_request_id: string;
  patient_id: string;
  identifier: string;
  specimen_type_code: string;
  container_code: string | null;
  body_site: string | null;
  status: string;
  collected_at: string | null;
  collected_by: string | null;
  collection_facility_id: string | null;
  rejection_reason: string | null;
  recollection_of: string | null;
  version: number;
  created_at: string;
  updated_at: string;
};

export type ClinicalDocument = {
  id: string;
  patient_id: string;
  service_request_id: string | null;
  diagnostic_report_id: string | null;
  kind: string;
  title: string;
  mime_type: string;
  size_bytes: number | null;
  store_kind: string;
  status: string;
  scan_verdict: string | null;
  scanned_at: string | null;
  released: boolean;
  uploaded_by: string | null;
  source_system: string | null;
  created_at: string;
  downloadable: boolean;
};

export type ImagingStudy = {
  id: string;
  service_request_id: string;
  patient_id: string;
  diagnostic_report_id: string | null;
  study_instance_uid: string;
  accession_number: string | null;
  modality_code: string;
  description: string | null;
  number_of_series: number | null;
  number_of_instances: number | null;
  series: unknown;
  pacs_endpoint_code: string | null;
  status: string;
  started_at: string | null;
  source_system: string | null;
  created_at: string;
};

export type AppointmentSummary = {
  id: string;
  status: string;
  starts_at: string;
  ends_at: string;
  time_zone: string;
  facility_id: string;
  facility_name: string;
  service_code: string;
};

export type GroupSafety = {
  id: string;
  engine_version: string;
  input_hash: string;
  evaluated_at: string;
  evaluated_by: string | null;
  findings: SafetyFinding[];
  warnings: number;
  hard_stops: number;
  acknowledged_ids: string[];
  override_reason: string | null;
  overridden_by: string | null;
  overridden_at: string | null;
};

export type OrderDetail = DiagnosticOrder & {
  history: OrderHistoryEntry[];
  specimens: Specimen[];
  reports: DiagnosticReport[];
  documents: ClinicalDocument[];
  imaging_studies: ImagingStudy[];
  appointment: AppointmentSummary | null;
  safety: GroupSafety | null;
};

export type WorklistQuery = {
  status?: string;
  category?: string;
  modality?: string;
  facility_id?: string;
  patient_id?: string;
  conflicts_only?: boolean;
  q?: string;
  cursor?: string;
  limit?: number;
};

export type Worklist = {
  items: DiagnosticOrder[];
  has_more: boolean;
  next_cursor: string | null;
};

export function loadWorklist(q: WorklistQuery): Promise<Worklist> {
  return apiFetch(`/api/v1/diagnostics/orders${query(q)}`);
}

export function loadOrder(id: string): Promise<OrderDetail> {
  return apiFetch(`/api/v1/diagnostics/orders/${id}`);
}

export type PatientDiagnostics = {
  pending_orders: DiagnosticOrder[];
  completed_orders: DiagnosticOrder[];
  recent_reports: DiagnosticReport[];
};

export function loadPatientDiagnostics(
  patientId: string,
): Promise<PatientDiagnostics> {
  return apiFetch(`/api/v1/patients/${patientId}/diagnostics`);
}

export const TRANSITIONS = [
  "accept",
  "start",
  "complete",
  "hold",
  "resume",
  "cancel",
  "reject",
  "enter_in_error",
] as const;
export type Transition = (typeof TRANSITIONS)[number];

/** Transitions the deterministic state machine accepts from each status. The
 * server is authoritative; this only decides which buttons to offer. */
export function availableTransitions(status: OrderStatus): Transition[] {
  switch (status) {
    case "placed":
      return ["accept", "hold", "cancel", "reject", "enter_in_error"];
    case "accepted":
      return ["start", "hold", "cancel", "reject", "enter_in_error"];
    case "scheduled":
      return ["start", "hold", "cancel", "enter_in_error"];
    case "in_progress":
      return ["complete", "hold", "cancel", "enter_in_error"];
    case "on_hold":
      return ["resume", "cancel", "enter_in_error"];
    default:
      return [];
  }
}

/** Transitions that always require a human-readable reason. */
export function transitionNeedsReason(tr: Transition): boolean {
  return (
    tr === "hold" ||
    tr === "cancel" ||
    tr === "reject" ||
    tr === "enter_in_error"
  );
}

export function transitionOrder(
  id: string,
  body: {
    transition: Transition;
    version: number;
    reason?: string;
    fulfilment_mode?: FulfilmentMode;
  },
): Promise<DiagnosticOrder> {
  return apiFetch(`/api/v1/diagnostics/orders/${id}/transition`, {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function scheduleOrder(
  id: string,
  body: { version: number; facility_id?: string; lang: Lang },
): Promise<unknown> {
  return apiFetch(`/api/v1/diagnostics/orders/${id}/schedule`, {
    method: "POST",
    body: JSON.stringify(body),
  });
}

// ---------------------------------------------------------------------------
// Specimens
// ---------------------------------------------------------------------------

export const SPECIMEN_EVENTS = [
  "collected",
  "dispatched",
  "received",
  "processing_started",
  "processed",
  "rejected",
  "consumed",
] as const;
export type SpecimenEvent = (typeof SPECIMEN_EVENTS)[number];

export function specimenEventsFor(status: string): SpecimenEvent[] {
  switch (status) {
    case "planned":
      return ["collected", "rejected"];
    case "collected":
      return ["dispatched", "received", "rejected"];
    case "in_transit":
      return ["received", "rejected"];
    case "received":
      return ["processing_started", "rejected"];
    case "processing":
      return ["processed", "rejected"];
    case "processed":
      return ["consumed"];
    default:
      return [];
  }
}

export function recordSpecimen(
  orderId: string,
  body: {
    identifier?: string;
    specimen_type_code: string;
    container_code?: string;
    body_site?: string;
    collected: boolean;
    collection_facility_id?: string;
  },
): Promise<Specimen> {
  return apiFetch(`/api/v1/diagnostics/orders/${orderId}/specimens`, {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function specimenEvent(
  specimenId: string,
  body: {
    event: SpecimenEvent;
    version: number;
    facility_id?: string;
    location_note?: string;
    reason?: string;
  },
): Promise<Specimen> {
  return apiFetch(`/api/v1/diagnostics/specimens/${specimenId}/events`, {
    method: "POST",
    body: JSON.stringify(body),
  });
}

// ---------------------------------------------------------------------------
// Reports, components, review, explanation and release
// ---------------------------------------------------------------------------

export type ResultValue =
  | { type: "quantity"; value: string; unit: string }
  | { type: "text"; text: string }
  | { type: "coded"; code: string; system: string; display?: string | null }
  | { type: "boolean"; value: boolean }
  | { type: "datetime"; value: string }
  | { type: "narrative"; text: string };

export type ReportComponent = {
  id: string;
  code: string;
  system?: string | null;
  display: string | null;
  value: ResultValue;
  value_text: string;
  unit?: string | null;
  reference_range: string | null;
  interpretation: string | null;
  effective_at: string | null;
  received_at: string | null;
  source_system: string | null;
  status: string;
  superseded: boolean;
  amends: string | null;
};

export type Review = {
  id: string;
  report_version: number;
  reviewer_id: string;
  reviewer_name: string | null;
  clinical_assessment: string;
  disposition: string;
  disposition_note: string | null;
  follow_up_task_ids: string[];
  synthesis_artifact_id: string | null;
  reviewed_at: string;
};

export type ReleaseDecision = {
  id: string;
  report_version: number;
  review_id: string | null;
  decision: "release" | "withhold";
  withhold_reason: string | null;
  explanation_en: string | null;
  explanation_es: string | null;
  explanation_artifact_id: string | null;
  notify_patient: boolean;
  notification_id: string | null;
  decided_by: string | null;
  decided_at: string;
  superseded_at: string | null;
};

export type SynthesisOutput = {
  schema_version: string;
  report_ref: string;
  summary: string;
  criticality: string;
  components: {
    component_ref: string;
    interpretation: string;
    statement: string;
    change_from_prior: string | null;
    cited_sources: string[];
  }[];
  changes: string[];
  contradictions: string[];
  missing_information: string[];
  cited_sources: string[];
  confidence: string;
  limitations: string[];
};

export type ExplanationOutput = {
  schema_version: string;
  report_ref: string;
  report_version: number;
  explanation_en: string;
  explanation_es: string;
  cited_sources: string[];
  next_steps_en: string;
  next_steps_es: string;
  confidence: string;
  limitations: string[];
};

export type ReportArtifact = {
  id: string;
  artifact_type: string;
  template: string;
  prompt_version: string | null;
  model: string | null;
  model_version: string | null;
  route: string | null;
  status: string;
  autonomy_level: string | null;
  output: SynthesisOutput | ExplanationOutput | null;
  citations: string[];
  limitations: string[];
  synthetic: boolean;
  generated_at: string | null;
  review_decision: string | null;
  review_note: string | null;
  reviewed_at: string | null;
  reviewer_id: string | null;
  report_version?: number | null;
};

export type CriticalityRule = {
  component_ref?: string;
  code?: string;
  conclusion_code?: string;
  interpretation: string;
  reference_range?: string | null;
  source?: string;
};

export function criticalityRuleText(rule: CriticalityRule): string {
  const subject = rule.code ?? rule.conclusion_code ?? rule.component_ref ?? "";
  const range = rule.reference_range ? ` (${rule.reference_range})` : "";
  return `${subject} ${rule.interpretation}${range}`.trim();
}

export type DiagnosticReport = {
  id: string;
  service_request_id: string;
  patient_id: string;
  status: string;
  version: number;
  replaces: string | null;
  category_code: string | null;
  conclusion: string | null;
  conclusion_codes: { system: string; code: string; display?: string | null }[];
  criticality: string;
  criticality_rules: CriticalityRule[];
  performer_id: string | null;
  performing_facility_id: string | null;
  performing_service_code: string | null;
  signed_by: string | null;
  signed_at: string | null;
  issued_at: string | null;
  effective_at: string | null;
  source_system: string | null;
  external_report_id: string | null;
  change_reason: string | null;
  created_at: string;
  reviewable: boolean;
  order_display?: string;
  priority?: Priority;
  order_category?: string | null;
  modality_code?: string | null;
  facility_id?: string;
  reviewed?: boolean;
  released?: boolean;
  patient?: PatientSummary | null;
};

export type ReportDetail = DiagnosticReport & {
  components: ReportComponent[];
  replaced_by: string | null;
  reviews: Review[];
  release_decisions: ReleaseDecision[];
  synthesis: ReportArtifact[];
  explanations: ReportArtifact[];
  documents: ClinicalDocument[];
  imaging_studies: ImagingStudy[];
  order: DiagnosticOrder;
};

export function loadReport(id: string): Promise<ReportDetail> {
  return apiFetch(`/api/v1/diagnostics/reports/${id}`);
}

export type ReviewWorklistQuery = {
  state?: "pending" | "reviewed" | "released" | "all";
  criticality?: string;
  facility_id?: string;
  patient_id?: string;
  category?: string;
  mine?: boolean;
  limit?: number;
};

export function loadReviewWorklist(
  q: ReviewWorklistQuery,
): Promise<{ items: DiagnosticReport[] }> {
  return apiFetch(`/api/v1/diagnostics/reviews${query(q)}`);
}

export type ComponentInput = {
  code: string;
  system?: string;
  display?: string;
  value: ResultValue;
  reference_range?: string;
  effective_at?: string;
  amends_observation_id?: string;
};

export type IssueBody = {
  status:
    | "preliminary"
    | "final"
    | "amended"
    | "corrected"
    | "cancelled"
    | "entered_in_error";
  components: ComponentInput[];
  conclusion?: string;
  conclusion_codes: { system: string; code: string; display?: string }[];
  change_reason?: string;
  idempotency_key: string;
  source_system?: string;
  effective_at?: string;
  external_report_id?: string;
  sign?: boolean;
};

export function issueReport(
  orderId: string,
  body: IssueBody,
): Promise<DiagnosticReport> {
  return apiFetch(`/api/v1/diagnostics/orders/${orderId}/reports`, {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function synthesizeReport(
  reportId: string,
  lang: Lang,
): Promise<
  ReportArtifact & { report_version: number; reused_from: string | null }
> {
  return apiFetch(`/api/v1/diagnostics/reports/${reportId}/synthesis`, {
    method: "POST",
    body: JSON.stringify({ lang }),
  });
}

export const DISPOSITIONS = [
  "no_action",
  "routine_follow_up",
  "urgent_follow_up",
  "repeat_test",
  "referral",
  "immediate_contact",
  "other",
] as const;
export type Disposition = (typeof DISPOSITIONS)[number];

export type ReviewBody = {
  report_version: number;
  clinical_assessment: string;
  disposition: Disposition;
  disposition_note?: string;
  follow_ups: {
    description: string;
    priority?: string;
    due_in_hours?: number;
  }[];
  synthesis_artifact_id?: string;
  synthesis_decision?: "approved" | "rejected";
  synthesis_note?: string;
};

export function reviewReport(
  reportId: string,
  body: ReviewBody,
): Promise<Review> {
  return apiFetch(`/api/v1/diagnostics/reports/${reportId}/review`, {
    method: "POST",
    body: JSON.stringify(body),
  });
}

export function draftExplanation(
  reportId: string,
): Promise<
  ReportArtifact & { report_version: number; reused_from: string | null }
> {
  return apiFetch(`/api/v1/diagnostics/reports/${reportId}/explanation`, {
    method: "POST",
    body: "{}",
  });
}

export function isSynthesisOutput(
  o: SynthesisOutput | ExplanationOutput | null,
): o is SynthesisOutput {
  return o !== null && "summary" in o;
}

export function isExplanationOutput(
  o: SynthesisOutput | ExplanationOutput | null,
): o is ExplanationOutput {
  return o !== null && "explanation_en" in o;
}

export function reviewExplanation(
  reportId: string,
  artifactId: string,
  body: { decision: "approved" | "rejected"; note?: string },
): Promise<ReportArtifact> {
  return apiFetch(
    `/api/v1/diagnostics/reports/${reportId}/explanation/${artifactId}/review`,
    { method: "POST", body: JSON.stringify(body) },
  );
}

export type ReleaseBody = {
  report_version: number;
  review_id: string;
  decision: "release" | "withhold";
  withhold_reason?: string;
  explanation_en?: string;
  explanation_es?: string;
  explanation_artifact_id?: string;
  notify_patient: boolean;
  release_documents?: boolean;
};

export function releaseReport(
  reportId: string,
  body: ReleaseBody,
): Promise<ReleaseDecision> {
  return apiFetch(`/api/v1/diagnostics/reports/${reportId}/release`, {
    method: "POST",
    body: JSON.stringify(body),
  });
}

// ---------------------------------------------------------------------------
// Patient self-service
// ---------------------------------------------------------------------------

export type MyReleasedComponent = {
  id: string;
  code: string;
  display: string;
  value: ResultValue | null;
  value_text: string | null;
  reference_range: string | null;
  interpretation: string;
  effective_at: string | null;
};

/**
 * One released report as `GET /api/v1/me/diagnostics` emits it: the order
 * title, the conclusion and the approved explanation. Component values are
 * only part of the single-report view (`components`).
 */
export type MyReleasedResult = {
  id: string;
  service_request_id: string;
  patient_id: string;
  order_display: string;
  category_code: string | null;
  modality_code: string | null;
  status: string;
  version: number;
  criticality: string;
  conclusion: string | null;
  issued_at: string | null;
  effective_at: string | null;
  released_at: string;
  notified: boolean;
  explanation_en: string | null;
  explanation_es: string | null;
  components?: MyReleasedComponent[];
  documents?: ClinicalDocument[];
};

export type MyPendingOrder = {
  service_request_id: string;
  order_display: string;
  status: OrderStatus;
  category_code: string | null;
  modality_code: string | null;
  fulfilment_mode: FulfilmentMode;
  appointment_id: string | null;
  starts_at: string | null;
  ends_at: string | null;
  time_zone: string | null;
  facility_id: string | null;
  facility_name: string | null;
  preparation_en: string | null;
  preparation_es: string | null;
};

export type MyDiagnostics = {
  patient_id: string;
  relationship: string;
  released: MyReleasedResult[];
  under_review: { service_request_id: string; order_display: string }[];
  pending: MyPendingOrder[];
};

export function loadMyDiagnostics(patientId?: string): Promise<MyDiagnostics> {
  return apiFetch(`/api/v1/me/diagnostics${query({ patient_id: patientId })}`);
}

export function myDocumentDownloadPath(
  reportId: string,
  documentId: string,
  patientId?: string,
): string {
  return `/api/v1/me/diagnostics/${reportId}/documents/${documentId}/download${query({ patient_id: patientId })}`;
}

export function staffDocumentDownloadPath(documentId: string): string {
  return `/api/v1/diagnostics/documents/${documentId}/download`;
}

// ---------------------------------------------------------------------------
// Labels (server codes → bilingual text; unknown codes fall back to the code)
// ---------------------------------------------------------------------------

function labelFrom(
  lang: Lang,
  map: Record<string, TKey>,
  code: string | null | undefined,
): string {
  if (!code) return "—";
  const key = map[code];
  return key ? t(lang, key) : code;
}

const ORDER_STATUS_KEY: Record<string, TKey> = {
  placed: "dxStatusPlaced",
  accepted: "dxStatusAccepted",
  scheduled: "dxStatusScheduled",
  in_progress: "dxStatusInProgress",
  completed: "dxStatusCompleted",
  on_hold: "dxStatusOnHold",
  cancelled: "dxStatusCancelled",
  rejected: "dxStatusRejected",
  entered_in_error: "dxStatusEnteredInError",
};
export function orderStatusLabel(
  lang: Lang,
  s: string | null | undefined,
): string {
  return labelFrom(lang, ORDER_STATUS_KEY, s);
}
export function orderStatusTone(
  s: string,
): "ok" | "warn" | "critical" | "neutral" {
  switch (s) {
    case "completed":
      return "ok";
    case "on_hold":
    case "placed":
      return "warn";
    case "rejected":
    case "cancelled":
    case "entered_in_error":
      return "critical";
    default:
      return "neutral";
  }
}

const PRIORITY_KEY: Record<string, TKey> = {
  routine: "dxPriorityRoutine",
  urgent: "dxPriorityUrgent",
  stat: "dxPriorityStat",
  timed: "dxPriorityTimed",
};
export function priorityLabel(
  lang: Lang,
  p: string | null | undefined,
): string {
  return labelFrom(lang, PRIORITY_KEY, p);
}

const MODE_KEY: Record<string, TKey> = {
  scheduled: "dxModeScheduled",
  immediate: "dxModeImmediate",
  inpatient: "dxModeInpatient",
  bedside: "dxModeBedside",
  walk_in: "dxModeWalkIn",
};
export function fulfilmentModeLabel(
  lang: Lang,
  m: string | null | undefined,
): string {
  return labelFrom(lang, MODE_KEY, m);
}

const TRANSITION_KEY: Record<Transition, TKey> = {
  accept: "dxTrAccept",
  start: "dxTrStart",
  complete: "dxTrComplete",
  hold: "dxTrHold",
  resume: "dxTrResume",
  cancel: "dxTrCancel",
  reject: "dxTrReject",
  enter_in_error: "dxTrEnterInError",
};
export function transitionLabel(lang: Lang, tr: Transition): string {
  return t(lang, TRANSITION_KEY[tr]);
}

const REPORT_STATUS_KEY: Record<string, TKey> = {
  preliminary: "dxReportPreliminary",
  final: "dxReportFinal",
  amended: "dxReportAmended",
  corrected: "dxReportCorrected",
  cancelled: "dxReportCancelled",
  entered_in_error: "dxStatusEnteredInError",
};
export function reportStatusLabel(
  lang: Lang,
  s: string | null | undefined,
): string {
  return labelFrom(lang, REPORT_STATUS_KEY, s);
}

const CRITICALITY_KEY: Record<string, TKey> = {
  normal: "dxCritNormal",
  abnormal: "dxCritAbnormal",
  critical: "dxCritCritical",
  unknown: "dxCritUnknown",
};
export function criticalityLabel(
  lang: Lang,
  c: string | null | undefined,
): string {
  return labelFrom(lang, CRITICALITY_KEY, c);
}
export function criticalityTone(
  c: string | null | undefined,
): "ok" | "warn" | "critical" | "neutral" {
  switch (c) {
    case "normal":
      return "ok";
    case "abnormal":
      return "warn";
    case "critical":
      return "critical";
    default:
      return "neutral";
  }
}

const SPECIMEN_STATUS_KEY: Record<string, TKey> = {
  planned: "dxSpecPlanned",
  collected: "dxSpecCollected",
  in_transit: "dxSpecInTransit",
  received: "dxSpecReceived",
  processing: "dxSpecProcessing",
  processed: "dxSpecProcessed",
  rejected: "dxStatusRejected",
  consumed: "dxSpecConsumed",
};
export function specimenStatusLabel(lang: Lang, s: string): string {
  return labelFrom(lang, SPECIMEN_STATUS_KEY, s);
}

const SPECIMEN_EVENT_KEY: Record<SpecimenEvent, TKey> = {
  collected: "dxEvCollected",
  dispatched: "dxEvDispatched",
  received: "dxEvReceived",
  processing_started: "dxEvProcessingStarted",
  processed: "dxEvProcessed",
  rejected: "dxEvRejected",
  consumed: "dxEvConsumed",
};
export function specimenEventLabel(lang: Lang, e: SpecimenEvent): string {
  return t(lang, SPECIMEN_EVENT_KEY[e]);
}

const DISPOSITION_KEY: Record<Disposition, TKey> = {
  no_action: "dxDispNoAction",
  routine_follow_up: "dxDispRoutineFollowUp",
  urgent_follow_up: "dxDispUrgentFollowUp",
  repeat_test: "dxDispRepeatTest",
  referral: "dxDispReferral",
  immediate_contact: "dxDispImmediateContact",
  other: "dxDispOther",
};
export function dispositionLabel(lang: Lang, d: string): string {
  return labelFrom(lang, DISPOSITION_KEY, d);
}

const FINDING_KIND_KEY: Record<string, TKey> = {
  duplicate_recent: "dxFindingDuplicate",
  pending_equivalent: "dxFindingPending",
  specimen_requirement: "dxFindingSpecimen",
  preparation: "dxFindingPreparation",
  prerequisite: "dxFindingPrerequisite",
  contraindication: "dxFindingContraindication",
  unanswered_question: "dxFindingQuestion",
  timing: "dxFindingTiming",
  redundant_combination: "dxFindingRedundant",
  fulfilment_mode_not_allowed: "dxFindingModeNotAllowed",
};
export function findingKindLabel(lang: Lang, k: string): string {
  return labelFrom(lang, FINDING_KIND_KEY, k);
}

const CATEGORY_KEY: Record<string, TKey> = {
  laboratory: "dxCatLaboratory",
  imaging: "dxCatImaging",
  cardiology: "dxCatCardiology",
  respiratory: "dxCatRespiratory",
  pathology: "dxCatPathology",
  procedure: "dxCatProcedure",
  dental: "dxCatDental",
};
export function categoryLabel(
  lang: Lang,
  c: string | null | undefined,
): string {
  return labelFrom(lang, CATEGORY_KEY, c);
}

export function interpretationTone(
  i: string | null | undefined,
): "ok" | "warn" | "critical" | "neutral" {
  switch (i) {
    case "normal":
      return "ok";
    case "abnormal":
      return "warn";
    case "critical":
      return "critical";
    default:
      return "neutral";
  }
}

/** Human-readable rendering of a typed result value. */
export function valueText(
  lang: Lang,
  v: ResultValue | null | undefined,
): string {
  if (!v) return "—";
  switch (v.type) {
    case "quantity":
      return `${v.value} ${v.unit}`.trim();
    case "text":
    case "narrative":
      return v.text;
    case "coded":
      return v.display ? `${v.display} (${v.code})` : v.code;
    case "boolean":
      return t(lang, v.value ? "yes" : "no");
    case "datetime":
      return new Date(v.value).toLocaleString(
        lang === "es" ? "es-ES" : "en-GB",
      );
  }
}

/** Build a typed component value from a form string for the expected type. */
export function parseValue(
  type: ResultType,
  raw: string,
  unit: string,
): ResultValue | null {
  const s = raw.trim();
  if (!s) return null;
  switch (type) {
    case "quantity":
      return /^-?\d+(\.\d+)?$/.test(s)
        ? { type: "quantity", value: s, unit: unit.trim() }
        : null;
    case "text":
      return { type: "text", text: s };
    case "narrative":
      return { type: "narrative", text: s };
    case "boolean":
      if (s === "true") return { type: "boolean", value: true };
      if (s === "false") return { type: "boolean", value: false };
      return null;
    case "datetime": {
      const d = new Date(s);
      return Number.isNaN(d.getTime())
        ? null
        : { type: "datetime", value: d.toISOString() };
    }
    case "coded": {
      // "system|code|display" or "code" against the component's own system.
      const [a, b, c] = s.split("|").map((x) => x.trim());
      if (b) return { type: "coded", system: a, code: b, display: c || null };
      return {
        type: "coded",
        system: "http://loinc.org",
        code: a,
        display: null,
      };
    }
  }
}

/** Patient-facing explanation in the current language, falling back to the other one. */
export function explanationFor(
  lang: Lang,
  r: { explanation_en: string | null; explanation_es: string | null },
): string | null {
  return lang === "es"
    ? (r.explanation_es ?? r.explanation_en)
    : (r.explanation_en ?? r.explanation_es);
}

export function preparationFor(
  lang: Lang,
  r: { preparation_en: string | null; preparation_es: string | null },
): string | null {
  return lang === "es"
    ? (r.preparation_es ?? r.preparation_en)
    : (r.preparation_en ?? r.preparation_es);
}
