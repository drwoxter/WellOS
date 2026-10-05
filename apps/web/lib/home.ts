import type { AccessRequest, Appointment, AppNotification } from "./access";
import type { MyDiagnostics, MyReleasedResult } from "./diagnostics";
import { apiFetch } from "./session";
import { formatDate } from "./clinical";
import type { Lang, TKey } from "./i18n";
import type { TrendPoint } from "@/components/ui/charts";

/**
 * Deterministic "what can I actually do now" ranking computed by the server
 * from the patient's own appointments, requests, releases and notifications.
 * Nothing is scored or predicted; `none` is an honest answer.
 */
export type HomeNextAction =
  | { kind: "confirm_attendance"; appointment_id: string }
  | { kind: "review_options"; access_request_id: string | null }
  | { kind: "new_results"; count: number }
  | { kind: "prepare_appointment"; appointment_id: string | null }
  | { kind: "read_notifications"; count: number }
  | { kind: "none" };

export type HomeTrendPoint = {
  value: string | number;
  unit: string;
  reference_range: string | null;
  interpretation: string;
  effective_at: string;
  report_id: string;
};

export type HomeTrend = {
  code: string;
  unit: string;
  display: string | null;
  points: HomeTrendPoint[];
};

export type PatientHome = {
  generated_at: string;
  patient_id: string;
  relationship: string;
  patient: { given_name: string; family_name: string };
  next_action: HomeNextAction;
  counts: {
    upcoming_appointments: number;
    to_confirm: number;
    open_requests: number;
    options_ready: number;
    unread_notifications: number;
    new_results: number;
    under_review: number;
    pending_orders: number;
  };
  appointments: { upcoming: Appointment[]; recent: Appointment[] };
  requests: AccessRequest[];
  notifications: AppNotification[];
  diagnostics: Omit<MyDiagnostics, "relationship">;
  trends: HomeTrend[];
};

export function loadPatientHome(patientId?: string): Promise<PatientHome> {
  const q = patientId ? `?patient_id=${encodeURIComponent(patientId)}` : "";
  return apiFetch<PatientHome>(`/api/v1/me/home${q}`);
}

export function greetingKey(hour: number): TKey {
  if (hour < 12) return "greetingMorning";
  if (hour < 19) return "greetingAfternoon";
  return "greetingEvening";
}

const RANGE = /^\s*(-?\d+(?:\.\d+)?)\s*[-–]\s*(-?\d+(?:\.\d+)?)\s*/;

/** Numeric band from a textual reference range such as "3.5-5.1 mmol/L". */
export function referenceBand(
  range: string | null | undefined,
): { low: number | null; high: number | null } | null {
  if (!range) return null;
  const m = RANGE.exec(range);
  if (!m) return null;
  return { low: Number(m[1]), high: Number(m[2]) };
}

export function trendPoints(lang: Lang, trend: HomeTrend): TrendPoint[] {
  return trend.points
    .map((p) => {
      const y = Number(p.value);
      return {
        x: new Date(p.effective_at).getTime(),
        y,
        label: formatDate(lang, p.effective_at),
        flag:
          p.interpretation === "critical"
            ? ("critical" as const)
            : p.interpretation === "abnormal"
              ? ("abnormal" as const)
              : ("normal" as const),
      };
    })
    .filter((p) => Number.isFinite(p.y) && Number.isFinite(p.x));
}

/** The result whose approved explanation the home surfaces: newest first. */
export function latestExplained(
  released: MyReleasedResult[],
): MyReleasedResult | null {
  return released.find((r) => r.explanation_en || r.explanation_es) ?? null;
}

/** Released within the "new" window the server uses for `counts.new_results`. */
export const NEW_RESULT_DAYS = 14;

export function isNewRelease(r: MyReleasedResult, now = new Date()): boolean {
  return (
    now.getTime() - new Date(r.released_at).getTime() <=
    NEW_RESULT_DAYS * 24 * 3600 * 1000
  );
}
