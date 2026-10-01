import type { Lang, TKey } from "./i18n";
import { t } from "./i18n";
import { apiFetch } from "./session";

/**
 * Typed contracts and helpers for dMind Access: tenant catalogs, schedulable
 * resources, access requests, offers/holds, appointments, waitlist recovery,
 * capacity forecasts, transport and the patient self-service surface.
 *
 * Everything displayed here comes from the server. Catalog codes are open
 * tenant data (never a closed list in the UI); statuses are translated when
 * known and shown verbatim otherwise so an unknown server value is never
 * hidden.
 */

// ---------------------------------------------------------------------------
// Server-derived capability hints (display/gating only; authorization is
// enforced server-side on every call).
// ---------------------------------------------------------------------------

export type SchedulingCapabilities = {
  can_read: boolean;
  can_manage: boolean;
  can_manage_catalog: boolean;
  can_manage_resources: boolean;
  can_manage_waitlist: boolean;
  can_coordinate_transport: boolean;
  can_review_capacity: boolean;
  can_manage_grants: boolean;
  self_service: boolean;
};

export const NO_SCHEDULING_CAPABILITIES: SchedulingCapabilities = {
  can_read: false,
  can_manage: false,
  can_manage_catalog: false,
  can_manage_resources: false,
  can_manage_waitlist: false,
  can_coordinate_transport: false,
  can_review_capacity: false,
  can_manage_grants: false,
  self_service: false,
};

// ---------------------------------------------------------------------------
// Catalogs
// ---------------------------------------------------------------------------

export const CATALOG_KINDS = [
  "clinical_service",
  "specialty",
  "profession",
  "modality",
  "resource_type",
  "accessibility_capability",
  "location",
  "transport_resource",
] as const;
export type CatalogKind = (typeof CATALOG_KINDS)[number];

const CATALOG_KIND_KEY: Record<CatalogKind, TKey> = {
  clinical_service: "catalogKindClinicalService",
  specialty: "catalogKindSpecialty",
  profession: "catalogKindProfession",
  modality: "catalogKindModality",
  resource_type: "catalogKindResourceType",
  accessibility_capability: "catalogKindAccessibility",
  location: "catalogKindLocation",
  transport_resource: "catalogKindTransportResource",
};

export type CatalogEntry = {
  id: string;
  kind: string;
  code: string;
  parent_id: string | null;
  name_en: string;
  name_es: string;
  synonyms: string[];
  external_codings: unknown;
  config: Record<string, unknown> | null;
  active: boolean;
  effective_from: string | null;
  effective_to: string | null;
  version: number;
  created_at: string;
  updated_at: string;
};

export type Page<T> = { items: T[]; next: string | null };

function isKey<T extends readonly string[]>(
  list: T,
  value: string,
): value is T[number] {
  return (list as readonly string[]).includes(value);
}

export function catalogKindLabel(lang: Lang, kind: string): string {
  return isKey(CATALOG_KINDS, kind) ? t(lang, CATALOG_KIND_KEY[kind]) : kind;
}

/** Bilingual name of a catalog entry (or any `{name_en,name_es}` label). */
export function catalogName(
  lang: Lang,
  entry: { name_en: string; name_es: string } | null | undefined,
  fallback = "",
): string {
  if (!entry) return fallback;
  const name = lang === "es" ? entry.name_es : entry.name_en;
  return name || entry.name_en || entry.name_es || fallback;
}

/** Resolve a code against a loaded catalog list; the code itself otherwise. */
export function codeLabel(
  lang: Lang,
  entries: ReadonlyArray<CatalogEntry>,
  code: string | null | undefined,
): string {
  if (!code) return "";
  const e = entries.find((x) => x.code === code);
  return e ? catalogName(lang, e, code) : code;
}

export type CatalogQuery = {
  kind?: string;
  q?: string;
  facility_id?: string;
  include_inactive?: boolean;
  limit?: number;
  after?: string;
};

export type CatalogHistoryEntry = {
  version: number;
  snapshot: CatalogEntry & { facility_ids?: string[] };
  change_reason: string | null;
  changed_by: string;
  recorded_at: string;
};

export function loadCatalog(q: CatalogQuery): Promise<Page<CatalogEntry>> {
  return apiFetch<Page<CatalogEntry>>(`/api/v1/catalog${query(q)}`);
}

/** Serialize defined, non-empty params into a query string. */
export function query(
  params: Record<string, string | number | boolean | null | undefined>,
): string {
  const sp = new URLSearchParams();
  for (const [k, v] of Object.entries(params)) {
    if (v === undefined || v === null || v === "") continue;
    sp.set(k, String(v));
  }
  const s = sp.toString();
  return s ? `?${s}` : "";
}

// ---------------------------------------------------------------------------
// Resources and availability
// ---------------------------------------------------------------------------

export type ResourceService = {
  service_code: string;
  duration_minutes: number | null;
  prep_minutes: number | null;
  cleanup_minutes: number | null;
  modality_codes: string[];
};

export type AvailabilityRule = {
  id: string;
  weekday: number;
  start_local: string;
  end_local: string;
  kind: string;
  capacity: number | null;
  effective_from: string | null;
  effective_to: string | null;
};

export type ResourceException = {
  id: string;
  kind: string;
  starts_at: string;
  ends_at: string;
  capacity_delta: number | null;
  reason_code: string | null;
  created_at: string;
};

export type SchedulableResource = {
  id: string;
  facility_id: string;
  resource_type_code: string;
  name: string;
  user_id: string | null;
  profession_code: string | null;
  specialty_codes: string[];
  languages: string[];
  accessibility_codes: string[];
  capacity: number;
  time_zone: string;
  active: boolean;
  metadata: Record<string, unknown> | null;
  version: number;
  services?: ResourceService[];
  availability_rules?: AvailabilityRule[];
  exceptions?: ResourceException[];
};

export const EXCEPTION_KINDS = [
  "leave",
  "sickness",
  "blocked",
  "closure",
  "extra_capacity",
  "training",
] as const;

const EXCEPTION_KEY: Record<(typeof EXCEPTION_KINDS)[number], TKey> = {
  leave: "exceptionLeave",
  sickness: "exceptionSickness",
  blocked: "exceptionBlocked",
  closure: "exceptionClosure",
  extra_capacity: "exceptionExtraCapacity",
  training: "exceptionTraining",
};

export function exceptionKindLabel(lang: Lang, kind: string): string {
  return isKey(EXCEPTION_KINDS, kind) ? t(lang, EXCEPTION_KEY[kind]) : kind;
}

const WEEKDAY_KEY: TKey[] = [
  "weekdayMon",
  "weekdayTue",
  "weekdayWed",
  "weekdayThu",
  "weekdayFri",
  "weekdaySat",
  "weekdaySun",
];

/** ISO weekday 1 (Monday) .. 7 (Sunday). */
export function weekdayLabel(lang: Lang, weekday: number): string {
  const key = WEEKDAY_KEY[weekday - 1];
  return key ? t(lang, key) : String(weekday);
}

// ---------------------------------------------------------------------------
// Access requests, offers, appointments
// ---------------------------------------------------------------------------

export type WeeklyWindow = { weekday: number; start: string; end: string };

export type Constraints = {
  service_code: string | null;
  specialty_code: string | null;
  modality_codes: string[];
  facility_ids: string[];
  earliest: string | null;
  latest: string | null;
  preferred_windows: WeeklyWindow[];
  max_travel_minutes: number | null;
  continuity_required: boolean;
  accessibility_codes: string[];
  language: string | null;
  transport_requested: boolean;
  has_referral: boolean;
  reschedule_of: string | null;
};

/** Partial constraint update accepted by create/amend endpoints. */
export type ConstraintsInput = Partial<{
  service_code: string | null;
  specialty_code: string | null;
  modality_codes: string[];
  facility_ids: string[];
  earliest: string | null;
  latest: string | null;
  preferred_windows: WeeklyWindow[];
  max_travel_minutes: number | null;
  continuity_required: boolean;
  accessibility_codes: string[];
  language: string | null;
  transport_requested: boolean;
  has_referral: boolean;
}>;

export type PatientSummary = {
  id: string;
  family_name: string;
  given_name: string;
  identifier?: string;
};

export const REQUEST_STATUSES = [
  "draft",
  "submitted",
  "needs_clinical_triage",
  "options_ready",
  "booked",
  "closed",
  "withdrawn",
] as const;

const REQUEST_STATUS_KEY: Record<(typeof REQUEST_STATUSES)[number], TKey> = {
  draft: "reqStatusDraft",
  submitted: "reqStatusSubmitted",
  needs_clinical_triage: "reqStatusNeedsTriage",
  options_ready: "reqStatusOptionsReady",
  booked: "reqStatusBooked",
  closed: "reqStatusClosed",
  withdrawn: "reqStatusWithdrawn",
};

export function requestStatusLabel(lang: Lang, status: string): string {
  return isKey(REQUEST_STATUSES, status)
    ? t(lang, REQUEST_STATUS_KEY[status])
    : status;
}

export const URGENCIES = ["routine", "priority", "urgent"] as const;
const URGENCY_KEY: Record<(typeof URGENCIES)[number], TKey> = {
  routine: "urgencyRoutine",
  priority: "urgencyPriority",
  urgent: "urgencyUrgent",
};
export function urgencyLabel(lang: Lang, urgency: string): string {
  return isKey(URGENCIES, urgency) ? t(lang, URGENCY_KEY[urgency]) : urgency;
}

export type AccessRequest = {
  id: string;
  patient_id: string;
  facility_id: string | null;
  status: string;
  channel: string;
  free_text: string | null;
  constraints: Constraints;
  missing_info: string[];
  urgency: string;
  urgency_source: string;
  triage_reason: string | null;
  intent_artifact_id: string | null;
  appointment_id: string | null;
  closed_reason: string | null;
  version: number;
  created_at: string;
  updated_at: string;
  patient?: PatientSummary;
};

export type OfferResource = {
  resource_id: string;
  role: string;
  slot_index?: number;
  name?: string;
  resource_type_code?: string;
};

export type ScoreFactor = {
  code: string;
  weight: number;
  value: number;
  points: number;
  detail: string;
};

export type TravelEstimate = {
  distance_km: number;
  minutes: number;
  provenance: string;
};

export type OfferScore = {
  score: number;
  factors: ScoreFactor[];
  travel: TravelEstimate | null;
  reasons: string[];
  supportive_actions: string[];
  resources?: OfferResource[];
};

export type OfferExplanation = {
  artifact_id: string;
  text: string;
  cited_sources: string[];
  synthetic: boolean;
};

export const OFFER_STATUSES = [
  "offered",
  "held",
  "accepted",
  "declined",
  "expired",
  "revoked",
] as const;

const OFFER_STATUS_KEY: Record<(typeof OFFER_STATUSES)[number], TKey> = {
  offered: "offerStatusOffered",
  held: "offerStatusHeld",
  accepted: "offerStatusAccepted",
  declined: "offerStatusDeclined",
  expired: "offerStatusExpired",
  revoked: "offerStatusRevoked",
};

export function offerStatusLabel(lang: Lang, status: string): string {
  return isKey(OFFER_STATUSES, status)
    ? t(lang, OFFER_STATUS_KEY[status])
    : status;
}

export type ServiceLabel = {
  code: string;
  name_en: string;
  name_es: string;
  /** Preparation instructions from the service catalog config, if any. */
  preparation_en?: string | null;
  preparation_es?: string | null;
};

export type Offer = {
  id: string;
  patient_id: string;
  facility_id: string;
  facility_name?: string;
  access_request_id: string | null;
  matcher_run_id: string | null;
  candidate_id: string;
  cancellation_event_id: string | null;
  waitlist_entry_id: string | null;
  status: string;
  service_code: string;
  service?: ServiceLabel | null;
  modality_code: string;
  starts_at: string;
  ends_at: string;
  resources: OfferResource[];
  score: OfferScore | null;
  explanation: OfferExplanation | null;
  rank: number | null;
  offered_to: string;
  offer_expires_at: string | null;
  hold_expires_at: string | null;
  appointment_id: string | null;
  version: number;
  patient?: PatientSummary;
};

export type RankingOutcome = {
  mode: "deterministic" | "dmind" | string;
  artifact_id: string | null;
  synthetic: boolean | null;
  reused: boolean;
  reason: string | null;
};

export type MatchResult = {
  request: AccessRequest;
  matcher_run_id: string;
  matcher_version: string;
  offers: Offer[];
  rejected_summary: Record<string, number>;
  ranking: RankingOutcome;
};

export const APPOINTMENT_STATUSES = [
  "confirmed",
  "rescheduled",
  "cancelled",
  "fulfilled",
  "no_show",
] as const;

const APPOINTMENT_STATUS_KEY: Record<
  (typeof APPOINTMENT_STATUSES)[number],
  TKey
> = {
  confirmed: "apptStatusConfirmed",
  rescheduled: "apptStatusRescheduled",
  cancelled: "apptStatusCancelled",
  fulfilled: "apptStatusFulfilled",
  no_show: "apptStatusNoShow",
};

export function appointmentStatusLabel(lang: Lang, status: string): string {
  return isKey(APPOINTMENT_STATUSES, status)
    ? t(lang, APPOINTMENT_STATUS_KEY[status])
    : status;
}

export type Appointment = {
  id: string;
  facility_id: string;
  facility_name?: string;
  patient_id: string;
  service_code: string;
  service?: ServiceLabel | null;
  modality_code: string;
  status: string;
  starts_at: string;
  ends_at: string;
  time_zone: string;
  reason: string | null;
  access_request_id: string | null;
  offer_id: string | null;
  matcher_run_id: string | null;
  candidate_id: string | null;
  score: OfferScore | null;
  primary_resource_id: string | null;
  primary_resource?: { name: string; resource_type_code: string };
  resources?: OfferResource[];
  visit_id: string | null;
  confirmation_required: boolean;
  patient_confirmed_at: string | null;
  confirmation_due_at: string | null;
  booked_via: string;
  override_reason: string | null;
  rescheduled_from: string | null;
  rescheduled_to: string | null;
  cancellation_reason: string | null;
  cancellation_note: string | null;
  cancelled_at: string | null;
  fulfilled_at: string | null;
  no_show_at: string | null;
  version: number;
  created_at: string;
  updated_at: string;
  patient?: PatientSummary;
  explanation?: OfferExplanation | null;
};

export type HistoryEntry = {
  from_status: string | null;
  to_status: string;
  reason: string | null;
  actor: string | null;
  recorded_at: string;
};

export type AppointmentHistoryEntry = {
  from_status: string | null;
  to_status: string;
  starts_at_before: string | null;
  starts_at_after: string | null;
  reason_code: string | null;
  note: string | null;
  override: boolean;
  actor: string;
  version: number;
  recorded_at: string;
};

export type CursorPage<T> = { items: T[]; next_after: string | null };

// ---------------------------------------------------------------------------
// Explanations
// ---------------------------------------------------------------------------

const REASON_KEY: Record<string, TKey> = {
  continuity_of_care: "reasonContinuity",
  early_slot_for_urgency: "reasonEarlySlot",
  fills_cancellation_gap: "reasonFillsGap",
  low_travel_burden: "reasonLowTravel",
  matches_preferred_time: "reasonPreferredTime",
};

export function reasonLabel(lang: Lang, code: string): string {
  const key = REASON_KEY[code];
  return key ? t(lang, key) : code;
}

const FACTOR_KEY: Record<string, TKey> = {
  urgency_soonness: "factorUrgency",
  waiting_time: "factorWaiting",
  preferred_time: "factorPreferredTime",
  travel_burden: "factorTravel",
  continuity: "factorContinuity",
  utilization: "factorUtilization",
  gap_fill: "factorGapFill",
  waitlist_fairness: "factorFairness",
  seasonal_demand: "factorSeasonal",
  confirmation_support: "factorConfirmationSupport",
};

export function factorLabel(lang: Lang, code: string): string {
  const key = FACTOR_KEY[code];
  return key ? t(lang, key) : code;
}

const SUPPORTIVE_KEY: Record<string, TKey> = {
  extra_reminder: "supportExtraReminder",
  confirmation_request: "supportConfirmationRequest",
  convenient_option: "supportConvenientOption",
};

export function supportiveActionLabel(lang: Lang, code: string): string {
  const key = SUPPORTIVE_KEY[code];
  return key ? t(lang, key) : code;
}

const REJECTION_KEY: Record<string, TKey> = {
  accessibility_not_met: "rejectAccessibility",
  continuity_not_met: "rejectContinuity",
  empty_window: "rejectEmptyWindow",
  facility_not_requested: "rejectFacility",
  language_not_supported: "rejectLanguage",
  modality_not_available: "rejectModality",
  no_open_slot_for_resource: "rejectNoOpenSlot",
  outside_patient_availability: "rejectOutsideAvailability",
  patient_calendar_conflict: "rejectCalendarConflict",
  patient_unavailable: "rejectPatientUnavailable",
  referral_required: "rejectReferral",
  required_resource_unavailable: "rejectRequiredResource",
  resource_booked: "rejectResourceBooked",
  service_age_restriction: "rejectAgeRestriction",
  travel_too_long: "rejectTravel",
};

export function rejectionLabel(lang: Lang, code: string): string {
  const key = REJECTION_KEY[code];
  return key ? t(lang, key) : code;
}

/**
 * Why an offer was ranked where it is. dMind's explanation (when present)
 * is shown first; the deterministic reasons always follow so ranking is
 * never opaque, and the matcher's reasons are the authority when AI is off.
 */
export function offerReasons(lang: Lang, offer: Offer): string[] {
  const out: string[] = [];
  if (offer.explanation?.text) out.push(offer.explanation.text);
  for (const r of offer.score?.reasons ?? []) out.push(reasonLabel(lang, r));
  return out;
}

export function rankingNotice(lang: Lang, ranking: RankingOutcome): string {
  if (ranking.mode === "dmind") {
    return ranking.synthetic
      ? t(lang, "rankingDmindSynthetic")
      : t(lang, "rankingDmind");
  }
  switch (ranking.reason) {
    case "disabled":
      return t(lang, "rankingDeterministicDisabled");
    case "unavailable":
    case "invalid_output":
    case "quota_exceeded":
      return t(lang, "rankingDeterministicUnavailable");
    case "consent_required":
      return t(lang, "rankingDeterministicConsent");
    default:
      return t(lang, "rankingDeterministic");
  }
}

// ---------------------------------------------------------------------------
// Time helpers
// ---------------------------------------------------------------------------

export function formatRange(
  lang: Lang,
  startIso: string,
  endIso: string,
  timeZone?: string,
): string {
  const locale = lang === "es" ? "es-ES" : "en-GB";
  const start = new Date(startIso);
  const end = new Date(endIso);
  const day = new Intl.DateTimeFormat(locale, {
    weekday: "short",
    day: "numeric",
    month: "short",
    year: "numeric",
    timeZone,
  }).format(start);
  const time = new Intl.DateTimeFormat(locale, {
    hour: "2-digit",
    minute: "2-digit",
    timeZone,
  });
  return `${day}, ${time.format(start)}–${time.format(end)}`;
}

export function formatTime(lang: Lang, iso: string, timeZone?: string): string {
  return new Intl.DateTimeFormat(lang === "es" ? "es-ES" : "en-GB", {
    hour: "2-digit",
    minute: "2-digit",
    timeZone,
  }).format(new Date(iso));
}

export function formatDay(lang: Lang, iso: string, timeZone?: string): string {
  return new Intl.DateTimeFormat(lang === "es" ? "es-ES" : "en-GB", {
    weekday: "short",
    day: "numeric",
    month: "short",
    timeZone,
  }).format(new Date(iso));
}

/** Whole minutes until `iso`, never negative; null when absent. */
export function minutesUntil(
  iso: string | null,
  now: Date = new Date(),
): number | null {
  if (!iso) return null;
  const ms = new Date(iso).getTime() - now.getTime();
  return Math.max(0, Math.round(ms / 60_000));
}

/** `YYYY-MM-DD` of `d` in the browser's local zone. */
export function localDateKey(d: Date): string {
  const y = d.getFullYear();
  const m = String(d.getMonth() + 1).padStart(2, "0");
  const day = String(d.getDate()).padStart(2, "0");
  return `${y}-${m}-${day}`;
}

export type DateRange = { from: string; to: string };

/** Local calendar day containing `day` (`YYYY-MM-DD`), as ISO instants. */
export function dayRange(day: string): DateRange {
  const start = new Date(`${day}T00:00:00`);
  const end = new Date(start);
  end.setDate(end.getDate() + 1);
  return { from: start.toISOString(), to: end.toISOString() };
}

/** Monday-start local week containing `day`. */
export function weekRange(day: string): DateRange {
  const start = new Date(`${day}T00:00:00`);
  const dow = (start.getDay() + 6) % 7;
  start.setDate(start.getDate() - dow);
  const end = new Date(start);
  end.setDate(end.getDate() + 7);
  return { from: start.toISOString(), to: end.toISOString() };
}

export function weekDays(day: string): string[] {
  const { from } = weekRange(day);
  const start = new Date(from);
  return Array.from({ length: 7 }, (_, i) => {
    const d = new Date(start);
    d.setDate(d.getDate() + i);
    return localDateKey(d);
  });
}

export function shiftDay(day: string, days: number): string {
  const d = new Date(`${day}T00:00:00`);
  d.setDate(d.getDate() + days);
  return localDateKey(d);
}

/** Convert a `datetime-local` input value to an ISO instant. */
export function localInputToIso(value: string): string | null {
  if (!value) return null;
  const d = new Date(value);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}

// ---------------------------------------------------------------------------
// Agenda lanes
// ---------------------------------------------------------------------------

export type LaneItem = {
  kind: "appointment" | "hold" | "offer";
  id: string;
  starts_at: string;
  ends_at: string;
  status: string;
  service_code: string;
  service?: ServiceLabel | null;
  modality_code: string;
  patient?: PatientSummary;
  resource_id: string;
};

export type Lane = { resource: SchedulableResource; items: LaneItem[] };

/**
 * Group confirmed appointments and live offers/holds into one lane per
 * resource. A multi-resource booking appears in every lane it occupies.
 * Resources without items are kept so unfilled capacity stays visible.
 */
export function buildLanes(
  resources: SchedulableResource[],
  appointments: Appointment[],
  offers: Offer[],
): Lane[] {
  const lanes = new Map<string, Lane>();
  for (const r of resources) lanes.set(r.id, { resource: r, items: [] });
  const push = (resourceId: string, item: Omit<LaneItem, "resource_id">) => {
    const lane = lanes.get(resourceId);
    if (lane) lane.items.push({ ...item, resource_id: resourceId });
  };
  for (const a of appointments) {
    if (a.status !== "confirmed" && a.status !== "fulfilled") continue;
    const ids = new Set<string>(
      [...(a.resources ?? []), ...(a.score?.resources ?? [])].map(
        (r) => r.resource_id,
      ),
    );
    if (a.primary_resource_id) ids.add(a.primary_resource_id);
    for (const id of ids) {
      push(id, {
        kind: "appointment",
        id: a.id,
        starts_at: a.starts_at,
        ends_at: a.ends_at,
        status: a.status,
        service_code: a.service_code,
        service: a.service,
        modality_code: a.modality_code,
        patient: a.patient,
      });
    }
  }
  for (const o of offers) {
    if (o.status !== "held" && o.status !== "offered") continue;
    for (const r of o.resources) {
      push(r.resource_id, {
        kind: o.status === "held" ? "hold" : "offer",
        id: o.id,
        starts_at: o.starts_at,
        ends_at: o.ends_at,
        status: o.status,
        service_code: o.service_code,
        service: o.service,
        modality_code: o.modality_code,
        patient: o.patient,
      });
    }
  }
  for (const lane of lanes.values()) {
    lane.items.sort((a, b) => a.starts_at.localeCompare(b.starts_at));
  }
  return [...lanes.values()];
}

// ---------------------------------------------------------------------------
// Waitlist and cancellation recovery
// ---------------------------------------------------------------------------

export const WAITLIST_STATUSES = [
  "active",
  "paused",
  "offered",
  "fulfilled",
  "left",
] as const;
const WAITLIST_STATUS_KEY: Record<(typeof WAITLIST_STATUSES)[number], TKey> = {
  active: "waitlistActive",
  paused: "waitlistPaused",
  offered: "waitlistOffered",
  fulfilled: "waitlistFulfilled",
  left: "waitlistLeft",
};
export function waitlistStatusLabel(lang: Lang, status: string): string {
  return isKey(WAITLIST_STATUSES, status)
    ? t(lang, WAITLIST_STATUS_KEY[status])
    : status;
}

export type WaitlistEntry = {
  id: string;
  patient_id: string;
  service_code: string;
  facility_ids: string[];
  modality_codes: string[];
  acceptable_windows: WeeklyWindow[];
  earliest: string | null;
  latest: string | null;
  min_notice_hours: number;
  status: string;
  urgency: string;
  urgency_source: string;
  access_request_id: string | null;
  current_appointment_id: string | null;
  fulfilled_appointment_id: string | null;
  offers_declined: number;
  joined_at: string;
  paused_at: string | null;
  left_at: string | null;
  version: number;
  updated_at: string;
  patient?: PatientSummary;
  current_offer?: Offer | null;
};

export const RECOVERY_STATUSES = [
  "open",
  "offered",
  "filled",
  "exhausted",
  "closed",
] as const;
const RECOVERY_STATUS_KEY: Record<(typeof RECOVERY_STATUSES)[number], TKey> = {
  open: "recoveryOpen",
  offered: "recoveryOffered",
  filled: "recoveryFilled",
  exhausted: "recoveryExhausted",
  closed: "recoveryClosed",
};
export function recoveryStatusLabel(lang: Lang, status: string): string {
  return isKey(RECOVERY_STATUSES, status)
    ? t(lang, RECOVERY_STATUS_KEY[status])
    : status;
}

export type EligibleRecord = {
  entry_id: string;
  patient_id: string;
  urgency: string;
  waited_hours: number;
  position: number;
  rank: number | null;
  reasons: string[];
  explanation: string | null;
  outcome: string | null;
  offer_id: string | null;
  patient?: PatientSummary;
};

export type CancellationEvent = {
  id: string;
  appointment_id: string;
  facility_id: string;
  service_code: string;
  modality_code: string;
  starts_at: string;
  ends_at: string;
  status: string;
  eligible: EligibleRecord[];
  excluded_count: number;
  excluded: Record<string, number>;
  ranking_mode: string | null;
  ranking_artifact_id: string | null;
  current_offer_id: string | null;
  offers_made: number;
  override_by: string | null;
  override_reason: string | null;
  closed_reason: string | null;
  version: number;
  created_at: string;
  updated_at: string;
};

// ---------------------------------------------------------------------------
// Capacity
// ---------------------------------------------------------------------------

export type ForecastFactor = { code: string; effect: number; detail: string };

export type DayForecast = {
  date: string;
  expected_demand: number;
  available_capacity: number;
  gap: number;
  demand_low: number;
  demand_high: number;
  factors: ForecastFactor[];
};

export type ForecastOutput =
  | {
      status: "ready";
      version: string;
      days: DayForecast[];
      confidence: number;
      history_weeks: number;
      recommendations: string[];
      pressure_days: string[];
    }
  | {
      status: "insufficient_history";
      version: string;
      history_weeks: number;
      required_weeks: number;
      detail: string;
    };

export type CapacityForecast = {
  id: string;
  facility_id: string;
  service_code: string;
  forecast_version: string;
  horizon_start: string;
  horizon_end: string;
  status: string;
  inputs_hash: string;
  forecast: ForecastOutput;
  explanation_artifact_id: string | null;
  created_at: string;
};

export type CapacityExplanation = {
  artifact_id: string;
  provider: string;
  model: string | null;
  synthetic: boolean;
  reused: boolean;
  explanation: {
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
    limitations: string[];
  };
};

// ---------------------------------------------------------------------------
// Transport
// ---------------------------------------------------------------------------

export const TRANSPORT_STATUSES = [
  "requested",
  "scheduled",
  "en_route",
  "picked_up",
  "completed",
  "cancelled",
  "failed",
] as const;
const TRANSPORT_STATUS_KEY: Record<(typeof TRANSPORT_STATUSES)[number], TKey> =
  {
    requested: "transportRequested",
    scheduled: "transportScheduled",
    en_route: "transportEnRoute",
    picked_up: "transportPickedUp",
    completed: "transportCompleted",
    cancelled: "transportCancelled",
    failed: "transportFailed",
  };
export function transportStatusLabel(lang: Lang, status: string): string {
  return isKey(TRANSPORT_STATUSES, status)
    ? t(lang, TRANSPORT_STATUS_KEY[status])
    : status;
}

/** `transport-request.v1` forward transitions available from `from`. */
export function transportNextStatuses(from: string): string[] {
  switch (from) {
    case "requested":
      return ["scheduled", "cancelled", "failed"];
    case "scheduled":
      return ["en_route", "cancelled", "failed"];
    case "en_route":
      return ["picked_up", "cancelled", "failed"];
    case "picked_up":
      return ["completed", "failed"];
    default:
      return [];
  }
}

export type TransportRequest = {
  id: string;
  patient_id: string;
  appointment_id: string;
  facility_id: string;
  appointment_starts_at: string;
  status: string;
  requirements: string[];
  emergency: boolean;
  origin_area_code: string | null;
  has_pickup_address: boolean;
  pickup_window_start: string | null;
  pickup_window_end: string | null;
  vehicle_resource_id: string | null;
  vehicle_name: string | null;
  operator_user_id: string | null;
  authorized_by: string | null;
  failure_reason: string | null;
  location_sharing_open: boolean;
  version: number;
  created_at: string;
  updated_at: string;
  patient?: PatientSummary;
};

// ---------------------------------------------------------------------------
// Patient self-service
// ---------------------------------------------------------------------------

export type MePatient = {
  grant_id: string;
  patient_id: string;
  relationship: string;
  patient: { given_name: string; family_name: string };
  expires_at: string | null;
};

export type Me = {
  user_id: string;
  display_name: string;
  patients: MePatient[];
};

const RELATIONSHIP_KEY: Record<string, TKey> = {
  self: "relationshipSelf",
  parent_guardian: "relationshipParentGuardian",
  authorized_proxy: "relationshipAuthorizedProxy",
};
export function relationshipLabel(lang: Lang, rel: string): string {
  const key = RELATIONSHIP_KEY[rel];
  return key ? t(lang, key) : rel;
}

export type Preferences = {
  patient_id: string;
  version: number;
  available_windows: WeeklyWindow[];
  unavailable_windows: WeeklyWindow[];
  preferred_modalities: string[];
  preferred_facility_ids: string[];
  language: string | null;
  accessibility_needs: string[];
  time_zone: string | null;
  channels: string[];
  quiet_hours_start: string | null;
  quiet_hours_end: string | null;
  has_contact_email: boolean;
  has_push_endpoint: boolean;
};

export type Consent = {
  purpose: string;
  status: string;
  recorded_at?: string;
};

export type CalendarSource = {
  id: string;
  source_type: string;
  time_zone: string;
  status: string;
  integrity_hash: string | null;
  interval_count: number;
  horizon_start: string | null;
  horizon_end: string | null;
  connected_at: string;
  last_synced_at: string | null;
  disconnected_at: string | null;
};

export const NOTIFICATION_KINDS = [
  "booking_confirmation",
  "reschedule",
  "cancellation",
  "reminder",
  "preparation",
  "confirmation_request",
  "confirmation_follow_up",
  "no_response_follow_up",
  "waitlist_offer",
  "waitlist_offer_expired",
  "transport_status",
] as const;
const NOTIFICATION_KIND_KEY: Record<(typeof NOTIFICATION_KINDS)[number], TKey> =
  {
    booking_confirmation: "notifBookingConfirmation",
    reschedule: "notifReschedule",
    cancellation: "notifCancellation",
    reminder: "notifReminder",
    preparation: "notifPreparation",
    confirmation_request: "notifConfirmationRequest",
    confirmation_follow_up: "notifConfirmationFollowUp",
    no_response_follow_up: "notifNoResponseFollowUp",
    waitlist_offer: "notifWaitlistOffer",
    waitlist_offer_expired: "notifWaitlistOfferExpired",
    transport_status: "notifTransportStatus",
  };
export function notificationKindLabel(lang: Lang, kind: string): string {
  return isKey(NOTIFICATION_KINDS, kind)
    ? t(lang, NOTIFICATION_KIND_KEY[kind])
    : kind;
}

export type AppNotification = {
  id: string;
  patient_id: string | null;
  kind: string;
  appointment_id: string | null;
  offer_id: string | null;
  payload: Record<string, unknown>;
  subject: string;
  body: string;
  language: string;
  status: string;
  delivered_at: string | null;
  read_at: string | null;
};

export const SELF_SERVICE_CONSENTS = [
  "scheduling_calendar",
  "scheduling_location",
  "transport_coordination",
] as const;

const CONSENT_KEY: Record<(typeof SELF_SERVICE_CONSENTS)[number], TKey> = {
  scheduling_calendar: "consentSchedulingCalendar",
  scheduling_location: "consentSchedulingLocation",
  transport_coordination: "consentTransportCoordination",
};

const CONSENT_HELP_KEY: Record<(typeof SELF_SERVICE_CONSENTS)[number], TKey> = {
  scheduling_calendar: "consentSchedulingCalendarHelp",
  scheduling_location: "consentSchedulingLocationHelp",
  transport_coordination: "consentTransportCoordinationHelp",
};
export function consentHelp(lang: Lang, purpose: string): string {
  return isKey(SELF_SERVICE_CONSENTS, purpose)
    ? t(lang, CONSENT_HELP_KEY[purpose])
    : "";
}

export function consentLabel(lang: Lang, purpose: string): string {
  return isKey(SELF_SERVICE_CONSENTS, purpose)
    ? t(lang, CONSENT_KEY[purpose])
    : purpose;
}

/** A patient-facing summary of the appointment's current state. */
export function appointmentNeedsConfirmation(a: Appointment): boolean {
  return (
    a.status === "confirmed" &&
    a.confirmation_required &&
    a.patient_confirmed_at === null
  );
}

export function isUpcoming(a: Appointment, now: Date = new Date()): boolean {
  return a.status === "confirmed" && new Date(a.starts_at) >= now;
}

/** Service preparation instructions in the viewer's language, if configured. */
export function preparationText(
  lang: Lang,
  service: ServiceLabel | null | undefined,
): string | null {
  if (!service) return null;
  const preferred =
    lang === "es" ? service.preparation_es : service.preparation_en;
  return preferred ?? service.preparation_en ?? service.preparation_es ?? null;
}
