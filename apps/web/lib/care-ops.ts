/**
 * Typed contracts and loaders for Care Operations & Command Center v1.
 *
 * Everything rendered by the operations surfaces is server state: the
 * deterministic snapshot, episodes, placement requests and their persisted
 * candidates, coordination tasks, escalations, theatre cases, workforce
 * shifts and forecasts. Capability flags gate display only; every action is
 * re-authorized, audited and version-checked by the API. dMind output is
 * advisory and stays `awaiting_review` until a human accepts or rejects it.
 */
import { apiFetch } from "./session";
import { opsFetch, postOps } from "../app/scheduling/shared";

export type CareOpsCapabilities = {
  can_read: boolean;
  can_manage_flow: boolean;
  can_manage_theatre: boolean;
  can_manage_workforce: boolean;
  can_review_ai: boolean;
  can_audit: boolean;
  self_service: boolean;
};

export const NO_CARE_OPS_CAPABILITIES: CareOpsCapabilities = {
  can_read: false,
  can_manage_flow: false,
  can_manage_theatre: false,
  can_manage_workforce: false,
  can_review_ai: false,
  can_audit: false,
  self_service: false,
};

/** Whether the staff `/operations` surfaces have anything to show. */
export function hasOperationsAccess(
  caps: CareOpsCapabilities | undefined,
): boolean {
  return Boolean(
    caps &&
    (caps.can_read ||
      caps.can_manage_flow ||
      caps.can_manage_theatre ||
      caps.can_manage_workforce),
  );
}

// ---------------------------------------------------------------------------
// Snapshot (deterministic command-center facts)
// ---------------------------------------------------------------------------

export type UnitCapacity = {
  unit_id: string;
  name_en?: string;
  name_es?: string;
  total: number;
  occupied: number;
  available: number;
  cleaning?: number;
  held?: number;
  blocked?: number;
};

export type Snapshot = {
  facility_id: string;
  generated_at: string;
  emergency: {
    awaiting_triage: number;
    awaiting_clinician: number;
    in_consultation: number;
    by_priority: Record<string, number>;
  };
  placement_queue: { pending: number; max_wait_minutes: number };
  capacity: {
    total: number;
    occupied: number;
    available: number;
    occupancy_ratio: number;
    units: UnitCapacity[];
  };
  episodes: Record<string, number>;
  discharge: {
    barriers: { barrier_category: string | null; n: number }[];
    overdue_tasks: number;
  };
  transport: { open: number; delayed: number };
  alerts: number;
  escalations: { kind: string; status: string; n: number }[];
  theatre: {
    cases_next_24h: number;
    ready: number;
    blocking_conflicts: number;
    missing_prerequisites: number;
  };
  workforce: { shifts_next_24h: number; gaps: unknown[] };
  forecasts: Forecast[];
};

// ---------------------------------------------------------------------------
// Rows
// ---------------------------------------------------------------------------

export type EpisodeStatus =
  | "planned"
  | "active"
  | "discharge_preparation"
  | "discharge_ready"
  | "completed"
  | "cancelled"
  | "error";

export type Episode = {
  id: string;
  facility_id: string;
  patient_id: string;
  episode_type_code: string;
  status: EpisodeStatus;
  responsible_service_code: string | null;
  current_placement_id: string | null;
  started_at: string | null;
  expected_end_at: string | null;
  ended_at: string | null;
  priority: string;
  barriers: string[];
  version: number;
  created_at: string;
  updated_at: string;
};

export type PlacementRequest = {
  id: string;
  facility_id: string;
  episode_id: string;
  patient_id: string;
  kind: string;
  status:
    | "requested"
    | "candidates_ready"
    | "held"
    | "placed"
    | "cancelled"
    | "expired";
  priority: string;
  requirements: Record<string, unknown>;
  requested_from: string;
  matcher_version: string | null;
  facts_hash: string | null;
  ranking_artifact_id: string | null;
  version: number;
  created_at: string;
};

export type ScoreFactor = {
  code: string;
  detail: string;
  points: number;
  value: number;
  weight: number;
};

export type Candidate = {
  location_id: string;
  unit_id: string | null;
  label: string;
  feasible: boolean;
  rank: number | null;
  score: number | null;
  factors: ScoreFactor[];
  reasons: string[];
  rejection_codes: string[];
};

export type MatchResult = {
  matcher_version: string;
  facts_hash: string;
  feasible: Candidate[];
  rejected: Candidate[];
  rejected_by_code: Record<string, number>;
};

export type Task = {
  id: string;
  episode_id: string;
  patient_id: string;
  title_en: string;
  title_es: string;
  kind: string;
  status: string;
  profession_code: string | null;
  capability_codes: string[];
  due_at: string | null;
  escalate_at: string | null;
  barrier_category_code: string | null;
  patient_visible: boolean;
  version: number;
  depends_on: string[];
};

export type Escalation = {
  id: string;
  facility_id: string;
  episode_id: string | null;
  kind: string;
  threshold_minutes: number;
  started_at: string;
  due_at: string;
  status: string;
  version: number;
};

export type Handoff = {
  id: string;
  episode_id: string;
  shift_label: string;
  language: string;
  status: "draft" | "accepted" | "rejected" | "superseded";
  content: Record<string, unknown>;
  origin: "human" | "dmind_draft";
  artifact_id: string | null;
  version: number;
  created_at: string;
};

export type TheatreCase = {
  id: string;
  facility_id: string;
  episode_id: string | null;
  patient_id: string;
  procedure_code: string;
  responsible_service_code: string | null;
  status: string;
  priority: string;
  expected_duration_minutes: number;
  required_equipment_codes: string[];
  prerequisites: Record<string, unknown>;
  session_id: string | null;
  theatre_location_id: string | null;
  scheduled_start: string | null;
  scheduled_end: string | null;
  version: number;
};

export type TheatreConflict = {
  code: string;
  detail: string;
  blocking: boolean;
};

export type Readiness = {
  engine_version: string;
  feasible: boolean;
  conflicts: TheatreConflict[];
};

export type Shift = {
  id: string;
  facility_id: string;
  unit_location_id: string | null;
  label: string;
  local_date: string;
  starts_at: string;
  ends_at: string;
  status: string;
  version: number;
};

export type CoverageGap = {
  profession_code: string;
  capability_codes: string[];
  specialty_codes: string[];
  required: number;
  present: number;
  on_call: number;
  shortfall: number;
};

export type Forecast = {
  id: string;
  facility_id: string;
  forecast_version: string;
  horizon: "24h" | "7d";
  generated_at: string;
  horizon_start: string;
  horizon_end: string;
  status: "ready" | "insufficient_history";
  output: Record<string, unknown>;
  explanation_artifact_id: string | null;
};

// ---------------------------------------------------------------------------
// dMind outcomes (advisory)
// ---------------------------------------------------------------------------

export type AiOutcome<T = Record<string, unknown>> = {
  mode: "dmind" | "skipped";
  artifact_id: string | null;
  status: string | null;
  output: T | null;
  synthetic: boolean | null;
  reused: boolean;
  reason: string | null;
  input_hash: string;
};

export type Evidence = { text: string; cited_sources: string[] };

export type FlowBrief = {
  summary: string;
  pressures: { area: string; explanation: string; cited_sources: string[] }[];
  human_decisions: Evidence[];
  cited_sources: string[];
  confidence: string;
  limitations: string[];
  missing_information: string[];
};

export type PlacementRankingOut = {
  summary: string;
  ranked: {
    candidate_ref: string;
    rationale: string;
    cited_sources: string[];
  }[];
  cited_sources: string[];
  confidence: string;
  limitations: string[];
  missing_information: string[];
};

export type DischargeCoordinationOut = {
  summary: string;
  barriers: {
    task_ref: string;
    category: string;
    explanation: string;
    cited_sources: string[];
  }[];
  suggested_steps: {
    text: string;
    requires_confirmation: boolean;
    cited_sources: string[];
  }[];
  operational_only: boolean;
  cited_sources: string[];
  confidence: string;
  limitations: string[];
  missing_information: string[];
};

export type ShiftHandoffDraftOut = {
  situation: string;
  background: string;
  pending: Evidence[];
  risks: Evidence[];
  barriers: Evidence[];
  next_steps: Evidence[];
  cited_sources: string[];
  confidence: string;
  limitations: string[];
  missing_information: string[];
};

export type TheatrePlanExplanationOut = {
  summary: string;
  cases: {
    case_ref: string;
    readiness: string;
    explanation: string;
    cited_sources: string[];
  }[];
  conflicts: { code: string; explanation: string; cited_sources: string[] }[];
  cited_sources: string[];
  confidence: string;
  limitations: string[];
  missing_information: string[];
};

export type ForecastExplanationOut = {
  summary: string;
  pressure_points: {
    date: string;
    explanation: string;
    cited_sources: string[];
  }[];
  recommendations: {
    category: string;
    text: string;
    requires_confirmation: boolean;
  }[];
  cited_sources: string[];
  confidence: string;
  limitations: string[];
  missing_information: string[];
};

// ---------------------------------------------------------------------------
// Loaders
// ---------------------------------------------------------------------------

function q(params: Record<string, string | number | boolean | undefined>) {
  const sp = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v !== undefined && v !== "") sp.set(k, String(v));
  }
  const s = sp.toString();
  return s ? `?${s}` : "";
}

export function loadSnapshot(facilityId: string): Promise<Snapshot> {
  return opsFetch(
    `/api/v1/operations/snapshot${q({ facility_id: facilityId })}`,
  );
}

export function loadEpisodes(
  facilityId: string,
  openOnly = true,
): Promise<{ episodes: Episode[] }> {
  return opsFetch(
    `/api/v1/operations/episodes${q({ facility_id: facilityId, open_only: openOnly })}`,
  );
}

export type EpisodeDetail = {
  episode: Episode;
  links: { kind: string; target_id: string; created_at: string }[];
  placements: unknown[];
  placement_requests: PlacementRequest[];
  tasks: Task[];
  handoffs: Handoff[];
  escalations: Escalation[];
  theatre_cases: TheatreCase[];
  history: {
    id: string;
    from_status: string | null;
    to_status: string;
    action: string;
    reason: string | null;
    actor_id: string | null;
    version: number;
    created_at: string;
  }[];
  artifacts: {
    id: string;
    artifact_type: string;
    status: string;
    synthetic: boolean;
    created_at: string;
  }[];
};

export function loadEpisode(id: string): Promise<EpisodeDetail> {
  return opsFetch(`/api/v1/operations/episodes/${id}`);
}

export function loadRequests(
  facilityId: string,
  status?: string,
): Promise<{ requests: PlacementRequest[] }> {
  return opsFetch(
    `/api/v1/operations/placement-requests${q({ facility_id: facilityId, status })}`,
  );
}

export function loadRequest(id: string): Promise<{
  request: PlacementRequest;
  match: MatchResult | null;
  considered?: number;
}> {
  return opsFetch(`/api/v1/operations/placement-requests/${id}`);
}

export function generateCandidates(
  id: string,
  body: { expected_version?: number } = {},
) {
  return postOps<{ request: PlacementRequest; match: MatchResult }>(
    `/api/v1/operations/placement-requests/${id}/candidates`,
    body,
  );
}

export function holdCandidate(
  id: string,
  body: {
    location_id: string;
    expected_version: number;
    facts_hash: string;
    hold_minutes?: number;
  },
) {
  return postOps<{ request: PlacementRequest; placement: unknown }>(
    `/api/v1/operations/placement-requests/${id}/hold`,
    body,
  );
}

export function confirmPlacement(
  id: string,
  body: {
    expected_version: number;
    location_id?: string;
    facts_hash?: string;
    note?: string;
  },
) {
  return postOps<{ request: PlacementRequest; placement: unknown }>(
    `/api/v1/operations/placement-requests/${id}/confirm`,
    body,
  );
}

export function loadTasks(
  facilityId: string,
  episodeId?: string,
): Promise<{ tasks: Task[] }> {
  return opsFetch(
    `/api/v1/operations/tasks${q({ facility_id: facilityId, episode_id: episodeId, open_only: true })}`,
  );
}

export function loadEscalations(
  facilityId: string,
): Promise<{ escalations: Escalation[] }> {
  return opsFetch(
    `/api/v1/operations/escalations${q({ facility_id: facilityId, open_only: true })}`,
  );
}

export function loadCases(
  facilityId: string,
): Promise<{ cases: TheatreCase[] }> {
  return opsFetch(
    `/api/v1/operations/theatre/cases${q({ facility_id: facilityId, open_only: true })}`,
  );
}

export function loadCase(
  id: string,
): Promise<{ case: TheatreCase; readiness: Readiness; history?: unknown[] }> {
  return opsFetch(`/api/v1/operations/theatre/cases/${id}`);
}

export function loadShifts(facilityId: string): Promise<{ shifts: Shift[] }> {
  return opsFetch(
    `/api/v1/operations/workforce/shifts${q({ facility_id: facilityId })}`,
  );
}

export function loadShift(id: string): Promise<{
  shift: Shift;
  requirements: unknown[];
  assignments: unknown[];
  coverage: {
    shift_id: string;
    covered: boolean;
    gaps: CoverageGap[];
  };
  reallocation_candidates: unknown[];
}> {
  return opsFetch(`/api/v1/operations/workforce/shifts/${id}`);
}

export function loadForecasts(
  facilityId: string,
): Promise<{ forecasts: Forecast[] }> {
  return opsFetch(
    `/api/v1/operations/forecasts${q({ facility_id: facilityId })}`,
  );
}

export function requestFlowBrief(facilityId: string, language: string) {
  return postOps<{
    snapshot_id: string;
    snapshot: Snapshot;
    brief: AiOutcome<FlowBrief>;
  }>("/api/v1/operations/snapshot/brief", {
    facility_id: facilityId,
    language,
  });
}

export function requestRanking(requestId: string, language: string) {
  return postOps<{
    request_id: string;
    ranking: AiOutcome<PlacementRankingOut>;
  }>(`/api/v1/operations/placement-requests/${requestId}/ranking`, {
    language,
  });
}

export function requestDischargeCoordination(
  episodeId: string,
  language: string,
) {
  return postOps<{
    episode_id: string;
    coordination: AiOutcome<DischargeCoordinationOut>;
  }>(`/api/v1/operations/episodes/${episodeId}/discharge-coordination`, {
    language,
  });
}

export function requestHandoffDraft(
  episodeId: string,
  shiftLabel: string,
  language: string,
) {
  return postOps<{
    draft: AiOutcome<ShiftHandoffDraftOut>;
    handoff: Handoff | null;
  }>(`/api/v1/operations/episodes/${episodeId}/handoffs/draft`, {
    shift_label: shiftLabel,
    language,
  });
}

export function decideHandoff(
  id: string,
  body: {
    decision: "accept" | "reject";
    expected_version: number;
    note?: string;
  },
) {
  return postOps<{ handoff: Handoff }>(
    `/api/v1/operations/handoffs/${id}/decision`,
    body,
  );
}

export function reviewArtifact(
  id: string,
  decision: "approved" | "rejected",
  note?: string,
) {
  return postOps<{ artifact_id: string; status: string }>(
    `/api/v1/operations/artifacts/${id}/review`,
    { decision, note },
  );
}

export type PlacementRequirements = {
  service_code?: string;
  specialty_codes?: string[];
  age_group_code?: string;
  isolation_codes?: string[];
  accessibility_codes?: string[];
  equipment_codes?: string[];
  room_policy?: string;
  sex?: string;
  preferred_unit_ids?: string[];
};

export function createEpisode(body: {
  facility_id: string;
  patient_id: string;
  episode_type_code: string;
  responsible_service_code?: string;
  priority?: string;
  activate?: boolean;
  links?: { link_type: string; link_id: string }[];
}) {
  return postOps<{ episode: Episode }>(`/api/v1/operations/episodes`, body);
}

export function createPlacementRequest(body: {
  episode_id: string;
  kind?: string;
  priority?: string;
  requirements: PlacementRequirements;
  reason_code?: string;
  note?: string;
}) {
  return postOps<{ request: PlacementRequest }>(
    `/api/v1/operations/placement-requests`,
    body,
  );
}

export function createTask(body: {
  episode_id: string;
  title_en: string;
  title_es: string;
  kind?: string;
  profession_code?: string;
  capability_codes?: string[];
  due_at?: string;
  escalate_at?: string;
  barrier_category_code?: string;
  patient_visible?: boolean;
  patient_text_en?: string;
  patient_text_es?: string;
}) {
  return postOps<{ task: Task }>(`/api/v1/operations/tasks`, body);
}

export function requestTheatreExplanation(sessionId: string, language: string) {
  return postOps<{
    session_id: string;
    explanation: AiOutcome<TheatrePlanExplanationOut>;
  }>(`/api/v1/operations/theatre/sessions/${sessionId}/explanation`, {
    language,
  });
}

export function requestForecastExplanation(
  forecastId: string,
  language: string,
) {
  return postOps<{
    forecast_id: string;
    explanation: AiOutcome<ForecastExplanationOut>;
  }>(`/api/v1/operations/forecasts/${forecastId}/explanation`, { language });
}

export function transitionEpisode(
  id: string,
  body: {
    transition: string;
    expected_version: number;
    reason_code?: string;
    note?: string;
  },
) {
  return postOps<{ episode: Episode }>(
    `/api/v1/operations/episodes/${id}/transition`,
    body,
  );
}

export function transitionTask(
  id: string,
  body: {
    transition: string;
    expected_version: number;
    reason_code?: string;
    note?: string;
    completion_evidence?: unknown;
  },
) {
  return postOps<{ task: Task }>(
    `/api/v1/operations/tasks/${id}/transition`,
    body,
  );
}

// ---------------------------------------------------------------------------
// Patient active-care view (released, patient-appropriate facts only)
// ---------------------------------------------------------------------------

export type MyCare = {
  generated_at: string;
  episodes: {
    episode_id: string;
    episode_type_code: string;
    status: string;
    facility_name: string;
    time_zone: string;
    started_at: string | null;
    expected_end_at: string | null;
    location: {
      unit_en: string;
      unit_es: string;
      position_en: string;
      position_es: string;
    } | null;
    pending_tasks: {
      id: string;
      text_en: string;
      text_es: string;
      due_at: string | null;
    }[];
    appointments: {
      id: string;
      starts_at: string;
      ends_at: string;
      status: string;
    }[];
    procedures: {
      id: string;
      procedure_code: string;
      scheduled_start: string | null;
      status: string;
    }[];
    transport: {
      status: string;
      pickup_window_start: string | null;
      pickup_window_end: string | null;
    } | null;
    discharge_preparation: boolean;
    next_step: { text_en: string; text_es: string } | null;
  }[];
  notifications: {
    id: string;
    kind: string;
    text_en: string;
    text_es: string;
    delivered_at: string | null;
    created_at: string;
  }[];
  contact: unknown;
};

export function loadMyCare(patientId?: string): Promise<MyCare> {
  return apiFetch(`/api/v1/me/care${q({ patient_id: patientId })}`);
}

/** Minutes since a server timestamp, for honest freshness labels. */
export function ageMinutes(iso: string, now = Date.now()): number {
  return Math.max(0, Math.round((now - Date.parse(iso)) / 60000));
}
