// Patient access, arrival and triage vocabulary shared by the access board,
// the triage workspace, the dashboard and the chart. Codes mirror
// `wellos_domain::triage`; every label goes through the dictionary so EN/ES
// parity is enforced by the i18n completeness test.

import { t } from "./i18n";
import type { Lang, TKey } from "./i18n";

export const VISIT_STATUSES = [
  "scheduled",
  "arrived",
  "triage_in_progress",
  "ready_for_consultation",
  "in_consultation",
  "completed",
  "cancelled",
  "no_show",
] as const;
export type VisitStatus = (typeof VISIT_STATUSES)[number];

export const ARRIVAL_KINDS = [
  "scheduled",
  "walk_in",
  "urgent",
  "remote",
] as const;
export type ArrivalKind = (typeof ARRIVAL_KINDS)[number];

export const SERVICES = [
  "general_medicine",
  "emergency",
  "nursing",
  "telehealth",
] as const;
export type Service = (typeof SERVICES)[number];

export const PRIORITIES = [
  "non_urgent",
  "standard",
  "urgent",
  "immediate",
] as const;
export type Priority = (typeof PRIORITIES)[number];

export const CONCERNS = [
  "fever",
  "cough",
  "shortness_of_breath",
  "chest_pain",
  "abdominal_pain",
  "headache",
  "dizziness",
  "vomiting",
  "injury",
  "rash",
  "urinary_symptoms",
  "mental_health",
  "medication_review",
  "follow_up",
  "other",
] as const;
export type Concern = (typeof CONCERNS)[number];

export const RED_FLAGS = [
  "airway_compromise",
  "unresponsive",
  "severe_bleeding",
  "chest_pain",
  "stroke_signs",
  "severe_breathing_difficulty",
  "anaphylaxis_signs",
  "severe_pain",
  "suicidal_ideation",
  "pregnancy_complication",
] as const;
export type RedFlag = (typeof RED_FLAGS)[number];

const STATUS_KEY: Record<VisitStatus, TKey> = {
  scheduled: "visitScheduled",
  arrived: "visitArrived",
  triage_in_progress: "visitTriageInProgress",
  ready_for_consultation: "visitReady",
  in_consultation: "visitInConsultation",
  completed: "visitCompleted",
  cancelled: "visitCancelled",
  no_show: "visitNoShow",
};

const ARRIVAL_KEY: Record<ArrivalKind, TKey> = {
  scheduled: "arrivalScheduled",
  walk_in: "arrivalWalkIn",
  urgent: "arrivalUrgent",
  remote: "arrivalRemote",
};

const SERVICE_KEY: Record<Service, TKey> = {
  general_medicine: "serviceGeneralMedicine",
  emergency: "serviceEmergency",
  nursing: "serviceNursing",
  telehealth: "serviceTelehealth",
};

const PRIORITY_KEY: Record<Priority, TKey> = {
  non_urgent: "priorityNonUrgent",
  standard: "priorityStandard",
  urgent: "priorityUrgent",
  immediate: "priorityImmediate",
};

const CONCERN_KEY: Record<Concern, TKey> = {
  fever: "concernFever",
  cough: "concernCough",
  shortness_of_breath: "concernShortnessOfBreath",
  chest_pain: "concernChestPain",
  abdominal_pain: "concernAbdominalPain",
  headache: "concernHeadache",
  dizziness: "concernDizziness",
  vomiting: "concernVomiting",
  injury: "concernInjury",
  rash: "concernRash",
  urinary_symptoms: "concernUrinarySymptoms",
  mental_health: "concernMentalHealth",
  medication_review: "concernMedicationReview",
  follow_up: "concernFollowUp",
  other: "concernOther",
};

const RED_FLAG_KEY: Record<RedFlag, TKey> = {
  airway_compromise: "flagAirwayCompromise",
  unresponsive: "flagUnresponsive",
  severe_bleeding: "flagSevereBleeding",
  chest_pain: "flagChestPain",
  stroke_signs: "flagStrokeSigns",
  severe_breathing_difficulty: "flagSevereBreathingDifficulty",
  anaphylaxis_signs: "flagAnaphylaxisSigns",
  severe_pain: "flagSeverePain",
  suicidal_ideation: "flagSuicidalIdeation",
  pregnancy_complication: "flagPregnancyComplication",
};

function isKey<K extends string>(list: readonly K[], x: string): x is K {
  return (list as readonly string[]).includes(x);
}

export function visitStatusLabel(lang: Lang, status: string): string {
  return isKey(VISIT_STATUSES, status) ? t(lang, STATUS_KEY[status]) : status;
}

export function arrivalKindLabel(lang: Lang, kind: string): string {
  return isKey(ARRIVAL_KINDS, kind) ? t(lang, ARRIVAL_KEY[kind]) : kind;
}

export function serviceLabel(lang: Lang, service: string): string {
  return isKey(SERVICES, service) ? t(lang, SERVICE_KEY[service]) : service;
}

export function priorityLabel(lang: Lang, priority: string | null): string {
  if (priority === null) return t(lang, "priorityUnset");
  return isKey(PRIORITIES, priority)
    ? t(lang, PRIORITY_KEY[priority])
    : priority;
}

export function concernLabel(lang: Lang, code: string): string {
  return isKey(CONCERNS, code) ? t(lang, CONCERN_KEY[code]) : code;
}

export function redFlagLabel(lang: Lang, code: string): string {
  return isKey(RED_FLAGS, code) ? t(lang, RED_FLAG_KEY[code]) : code;
}

/** Badge class for a priority: immediate/urgent stand out, the rest stay calm. */
export function priorityBadge(priority: string | null): string {
  switch (priority) {
    case "immediate":
      return "critical";
    case "urgent":
      return "warn";
    case "standard":
      return "neutral";
    case "non_urgent":
      return "ok";
    default:
      return "neutral";
  }
}

export function priorityIndex(priority: string): number {
  return (PRIORITIES as readonly string[]).indexOf(priority);
}

/** True when `priority` is at or above the deterministic floor. */
export function meetsFloor(priority: string, floor: string): boolean {
  return priorityIndex(priority) >= priorityIndex(floor);
}

/** Human wording for a deterministic safety rule identifier such as
 *  `red_flag:chest_pain` or `vitals:spo2_below_90`. */
export function safetyRuleLabel(lang: Lang, rule: string): string {
  if (rule === "arrival:urgent") return t(lang, "ruleUrgentArrival");
  if (rule.startsWith("red_flag:")) {
    return `${t(lang, "redFlags")}: ${redFlagLabel(lang, rule.slice("red_flag:".length))}`;
  }
  switch (rule) {
    case "vitals:spo2_below_90":
      return t(lang, "ruleSpo2Below90");
    case "vitals:spo2_below_94":
      return t(lang, "ruleSpo2Below94");
    case "vitals:systolic_below_90":
      return t(lang, "ruleSystolicBelow90");
    case "vitals:systolic_above_180":
      return t(lang, "ruleSystolicAbove180");
    case "vitals:heart_rate_extreme":
      return t(lang, "ruleHeartRateExtreme");
    case "vitals:respiratory_rate_extreme":
      return t(lang, "ruleRespiratoryRateExtreme");
    case "vitals:temperature_extreme":
      return t(lang, "ruleTemperatureExtreme");
    default:
      return rule;
  }
}

export function alertKindLabel(lang: Lang, kind: string): string {
  switch (kind) {
    case "urgent_arrival":
      return t(lang, "alertUrgentArrival");
    case "patient_ready":
      return t(lang, "alertPatientReady");
    default:
      return kind;
  }
}

/** Roles that may see the access/triage board (mirrors `VISIT_READ`). */
const VISIT_READ_ROLES = [
  "registration_staff",
  "physician",
  "nurse",
  "clinical_administrator",
];

export function canReadVisits(roles: string[]): boolean {
  return roles.some((r) => VISIT_READ_ROLES.includes(r));
}

/** The board a role most often works from. */
export function defaultAccessView(
  roles: string[],
): "access" | "triage" | "ready" {
  if (roles.includes("physician")) return "ready";
  if (roles.includes("nurse")) return "triage";
  return "access";
}

/** Whole minutes as "12 min" / "1 h 05 min". */
export function formatWait(lang: Lang, minutes: number): string {
  if (minutes < 60) return `${minutes} ${t(lang, "minutesShort")}`;
  const h = Math.floor(minutes / 60);
  const m = minutes % 60;
  return `${h} h ${String(m).padStart(2, "0")} ${t(lang, "minutesShort")}`;
}

/** Local `datetime-local` value (YYYY-MM-DDTHH:MM) → ISO 8601 UTC string. */
export function localDateTimeToIso(value: string): string | null {
  if (!/^\d{4}-\d{2}-\d{2}T\d{2}:\d{2}/.test(value)) return null;
  const d = new Date(value);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}

const SCHEDULE_PAST_GRACE_MS = 60 * 60 * 1000;
const SCHEDULE_HORIZON_MS = 365 * 24 * 60 * 60 * 1000;

/** Mirrors the server's appointment window (one hour of grace for a slot
 *  that just passed, at most one year ahead) so the form can explain the
 *  rule before the request is refused. The server remains authoritative. */
export function isSchedulableAt(iso: string, now: Date = new Date()): boolean {
  const at = new Date(iso).getTime();
  return (
    at >= now.getTime() - SCHEDULE_PAST_GRACE_MS &&
    at <= now.getTime() + SCHEDULE_HORIZON_MS
  );
}

export type VisitCapabilities = {
  can_arrive: boolean;
  can_cancel: boolean;
  can_no_show: boolean;
  can_triage: boolean;
  can_assign: boolean;
  can_start_consultation: boolean;
  can_resume_consultation: boolean;
  assigned_to_other: boolean;
};

export type VisitAssignment =
  | { kind: "professional"; user_id: string; display_name: string | null }
  | {
      kind: "queue";
      queue_id: string;
      name: string | null;
      code: string | null;
    }
  | null;

export type VisitItem = {
  id: string;
  status: VisitStatus;
  arrival_kind: ArrivalKind;
  service: string;
  reason: string | null;
  scheduled_at: string | null;
  arrived_at: string | null;
  ready_at: string | null;
  consultation_started_at: string | null;
  wait_minutes: number | null;
  priority: string | null;
  handoff_summary: string | null;
  encounter_id: string | null;
  version: number;
  updated_at: string;
  facility: { id: string; name: string };
  patient: {
    id: string;
    family_name: string;
    given_name: string;
    identifier: string;
    age_years: number;
    alert_count: number;
    allergy_count: number;
  };
  assignment: VisitAssignment;
  open_alerts: number;
  capabilities: VisitCapabilities;
};

export type InternalAlert = {
  id: string;
  visit_id: string;
  kind: string;
  priority: string;
  status: "open" | "acknowledged" | "resolved";
  created_at: string;
  acknowledged_at: string | null;
  acknowledged_by_me: boolean;
  target:
    | { kind: "professional" }
    | { kind: "queue"; code: string | null; name: string | null };
  visit: {
    status: string;
    reason: string | null;
    wait_minutes: number | null;
    handoff_summary: string | null;
    encounter_id: string | null;
    version: number;
  };
  patient: {
    id: string;
    family_name: string;
    given_name: string;
    identifier: string;
  };
};

/** Where the primary action for a visit leads, given server capabilities. */
export function visitPrimaryAction(
  v: VisitItem,
): "arrive" | "triage" | "start" | "resume" | null {
  const c = v.capabilities;
  if (c.can_resume_consultation) return "resume";
  if (c.can_start_consultation) return "start";
  if (c.can_triage && v.status !== "ready_for_consultation") return "triage";
  if (c.can_arrive) return "arrive";
  return null;
}
