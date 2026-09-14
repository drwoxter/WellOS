import { t } from "./i18n";
import type { Lang, TKey } from "./i18n";

export type RiskLevel =
  "insufficient_data" | "low" | "moderate" | "high" | "critical";

export type RiskTrend = "improving" | "stable" | "worsening" | "unknown";

export type RiskReviewStatus = "unreviewed" | "acknowledged" | "reviewed";

export const RISK_DOMAINS = [
  "acute_safety",
  "chronic_complexity",
  "medication_allergy_safety",
  "diagnostic_result",
  "preventive_care",
  "access_utilization",
  "care_coordination",
] as const;

export type RiskDomain = (typeof RISK_DOMAINS)[number];

export type EvidenceRef = {
  record_type: string;
  record_id: string;
  label: string;
  observed_at: string | null;
};

export type RiskFactor = {
  code: string;
  level: RiskLevel;
  detail: string | null;
  evidence: EvidenceRef[];
  detected_at: string | null;
};

export type DataGap = {
  code: string;
  record_type: string;
  record_id: string | null;
  observed_at: string | null;
  max_age_days: number | null;
};

export type RiskReview = {
  status: RiskReviewStatus;
  recorded_status?: string | null;
  level_at_review?: string | null;
  reviewed_at?: string | null;
  reviewer?: string | null;
  note?: string | null;
};

export type DomainAssessment = {
  domain: RiskDomain;
  level: RiskLevel;
  factors: RiskFactor[];
  missing_data: DataGap[];
  stale_data: DataGap[];
  trend: RiskTrend;
  detected_at: string | null;
  calculated_at: string;
  rules_version: string;
  review: RiskReview;
};

export type RiskAssessment = {
  id: string;
  rules_version: string;
  calculated_at: string;
  calculated_by: string | null;
  trigger: string;
  overall_level: RiskLevel;
  safety_floor: RiskLevel;
  trend: RiskTrend;
  domains: DomainAssessment[];
  review: RiskReview;
};

export type RiskHistoryEntry = {
  id: string;
  calculated_at: string;
  overall_level: RiskLevel;
  trend: RiskTrend;
  rules_version: string;
  trigger: string;
  is_current: boolean;
  domain_levels: Record<string, RiskLevel>;
};

export type FollowUpSuggestion = {
  category: string;
  domain: RiskDomain;
  text: string;
  requires_confirmation: boolean;
};

export type RiskSummaryOutput = {
  schema_version: string;
  ai_generated: boolean;
  rules_version: string;
  overall_level: RiskLevel;
  safety_floor: RiskLevel;
  raised_to_floor: boolean;
  domains: {
    domain: RiskDomain;
    level: RiskLevel;
    summary: string;
    reasons: string[];
    cited_sources: string[];
  }[];
  missing_information: string[];
  contradictions: string[];
  follow_up_suggestions: FollowUpSuggestion[];
  limitations: string[];
  cited_sources: string[];
  confidence: "high" | "medium" | "low";
};

export type RiskSummary = {
  id: string;
  status: "awaiting_review" | "approved" | "rejected" | "superseded" | string;
  template: string;
  model: string;
  model_version: string;
  prompt_version: string;
  route: string;
  generated_at: string;
  reviewer: string | null;
  reviewed_at: string | null;
  review_decision: string | null;
  review_note: string | null;
  confirmed_tasks: number;
  output: RiskSummaryOutput;
};

export type RiskSection = {
  rules_version: string;
  current: RiskAssessment | null;
  history: RiskHistoryEntry[];
  summary: RiskSummary | null;
  follow_up_owner: { user_id: string; display_name: string } | null;
  professionals: { id: string; display_name: string }[];
  capabilities: {
    can_recalculate: boolean;
    can_acknowledge: boolean;
    can_review: boolean;
    can_assign: boolean;
  };
};

export type WorklistItem = {
  assessment_id: string;
  patient: {
    id: string;
    family_name: string;
    given_name: string;
    identifier: string;
    birth_date: string;
    facility_id: string;
    facility: string;
  };
  overall_level: RiskLevel;
  focus_level: RiskLevel;
  focus_domain: string;
  trend: RiskTrend;
  domain_levels: Record<string, RiskLevel>;
  explained: {
    domain: RiskDomain;
    level: RiskLevel;
    trend: RiskTrend;
    detected_at: string | null;
    factors: RiskFactor[];
    missing_data: DataGap[];
    stale_data: DataGap[];
  }[];
  calculated_at: string;
  rules_version: string;
  service: string | null;
  owner: { user_id: string; display_name: string } | null;
  treating_professional: string | null;
  review: RiskReview;
  capabilities: {
    can_acknowledge: boolean;
    can_assign: boolean;
    can_review: boolean;
    can_open_360: boolean;
  };
};

export type Worklist = {
  items: WorklistItem[];
  services: string[];
  professionals: { id: string; display_name: string }[];
  domains: string[];
  rules_version: string;
  limit: number;
};

const RISK_ROLES = [
  "physician",
  "nurse",
  "pharmacist",
  "clinical_administrator",
];

export function canReadRisk(roles: string[]): boolean {
  return roles.some((r) => RISK_ROLES.includes(r));
}

export const LEVEL_ORDER: RiskLevel[] = [
  "critical",
  "high",
  "moderate",
  "low",
  "insufficient_data",
];

export function levelKey(level: string): TKey {
  switch (level) {
    case "critical":
      return "riskLevelCritical";
    case "high":
      return "riskLevelHigh";
    case "moderate":
      return "riskLevelModerate";
    case "low":
      return "riskLevelLow";
    default:
      return "riskLevelInsufficient";
  }
}

/** Text marker so the level is never conveyed by colour alone. */
export function levelGlyph(level: string): string {
  switch (level) {
    case "critical":
      return "!!!";
    case "high":
      return "!!";
    case "moderate":
      return "!";
    case "low":
      return "–";
    default:
      return "?";
  }
}

export function levelBadgeClass(level: string): string {
  switch (level) {
    case "critical":
      return "badge critical";
    case "high":
      return "badge warn";
    case "moderate":
      return "badge warn";
    case "low":
      return "badge ok";
    default:
      return "badge neutral";
  }
}

export function levelLabel(lang: Lang, level: string): string {
  return `${levelGlyph(level)} ${t(lang, levelKey(level))}`;
}

export function domainKey(domain: string): TKey {
  switch (domain) {
    case "acute_safety":
      return "riskDomainAcuteSafety";
    case "chronic_complexity":
      return "riskDomainChronicComplexity";
    case "medication_allergy_safety":
      return "riskDomainMedicationAllergy";
    case "diagnostic_result":
      return "riskDomainDiagnosticResult";
    case "preventive_care":
      return "riskDomainPreventiveCare";
    case "access_utilization":
      return "riskDomainAccessUtilization";
    case "care_coordination":
      return "riskDomainCareCoordination";
    default:
      return "riskDomainOverall";
  }
}

export function trendKey(trend: string): TKey {
  switch (trend) {
    case "improving":
      return "riskTrendImproving";
    case "stable":
      return "riskTrendStable";
    case "worsening":
      return "riskTrendWorsening";
    default:
      return "riskTrendUnknown";
  }
}

export function trendGlyph(trend: string): string {
  switch (trend) {
    case "improving":
      return "↓";
    case "stable":
      return "→";
    case "worsening":
      return "↑";
    default:
      return "·";
  }
}

export function reviewKey(status: string): TKey {
  switch (status) {
    case "acknowledged":
      return "riskReviewAcknowledged";
    case "reviewed":
      return "riskReviewReviewed";
    default:
      return "riskReviewUnreviewed";
  }
}

/** Plain-language explanation of a deterministic factor code. */
export function factorKey(code: string): TKey | null {
  switch (code) {
    case "open_critical_alert":
      return "riskFactorOpenCriticalAlert";
    case "visit_priority":
      return "riskFactorVisitPriority";
    case "abnormal_vitals":
      return "riskFactorAbnormalVitals";
    case "multiple_chronic_conditions":
      return "riskFactorMultipleChronic";
    case "polypharmacy":
      return "riskFactorPolypharmacy";
    case "medication_allergy_conflict":
      return "riskFactorAllergyConflict";
    case "duplicate_medication":
      return "riskFactorDuplicateMedication";
    case "allergy_status_unknown":
      return "riskFactorAllergyUnknown";
    case "critical_result_unreviewed":
      return "riskFactorCriticalResult";
    case "critical_result_open_loop":
      return "riskFactorCriticalOpenLoop";
    case "abnormal_result_unreviewed":
      return "riskFactorAbnormalResult";
    case "abnormal_result_open_loop":
      return "riskFactorAbnormalOpenLoop";
    case "hba1c_overdue":
      return "riskFactorHba1cOverdue";
    case "renal_function_overdue":
      return "riskFactorRenalOverdue";
    case "blood_pressure_overdue":
      return "riskFactorBpOverdue";
    case "lipid_screening_overdue":
      return "riskFactorLipidOverdue";
    case "frequent_unscheduled_visits":
      return "riskFactorFrequentVisits";
    case "repeated_no_show":
      return "riskFactorNoShow";
    case "no_recent_follow_up":
      return "riskFactorNoRecentFollowUp";
    case "overdue_follow_up":
      return "riskFactorOverdueTasks";
    case "pending_result_overdue":
      return "riskFactorPendingResult";
    case "unhandled_alert":
      return "riskFactorUnhandledAlert";
    case "no_responsible_professional":
      return "riskFactorNoResponsible";
    default:
      return null;
  }
}

/** Plain-language name of the data a gap refers to. */
export function gapText(lang: Lang, gap: DataGap): string {
  const c = gap.code;
  const key: TKey = c.startsWith("vitals")
    ? "riskGapVitals"
    : c.startsWith("hba1c")
      ? "riskGapHba1c"
      : c.startsWith("creatinine")
        ? "riskGapRenal"
        : c.startsWith("blood_pressure")
          ? "riskGapBloodPressure"
          : c.startsWith("lipid")
            ? "riskGapLipid"
            : c.startsWith("laboratory_results")
              ? "riskGapLaboratory"
              : c.startsWith("allergy")
                ? "riskGapAllergy"
                : c.startsWith("consultation") || c.startsWith("follow_up")
                  ? "riskGapConsultation"
                  : c.startsWith("problem_list")
                    ? "riskGapProblemList"
                    : c.startsWith("responsible_professional")
                      ? "riskGapResponsible"
                      : "riskGapClinicalRecord";
  const base = t(lang, key);
  return gap.max_age_days
    ? `${base} (${t(lang, "riskGapOlderThan")} ${gap.max_age_days} ${t(lang, "riskDays")})`
    : base;
}

export function factorText(lang: Lang, f: RiskFactor): string {
  const key = factorKey(f.code);
  const base = key ? t(lang, key) : f.code.replace(/_/g, " ");
  return f.detail ? `${base} (${f.detail})` : base;
}

/** Where a piece of evidence can be opened in WellOS, when it has a screen. */
export function evidenceHref(
  e: EvidenceRef,
  patientId: string,
  requestIds?: Record<string, string>,
): string | null {
  switch (e.record_type) {
    case "encounter":
      return `/encounters/${e.record_id}`;
    case "service_request":
      return `/requests/${e.record_id}`;
    case "observation":
      return requestIds?.[e.record_id]
        ? `/requests/${requestIds[e.record_id]}`
        : `/patients/${patientId}/360#results`;
    case "visit":
      return "/access";
    case "alert":
      return `/patients/${patientId}/360#alerts`;
    case "task":
      return `/patients/${patientId}/360#pending`;
    case "condition":
      return `/patients/${patientId}/360#conditions`;
    case "medication":
    case "allergy":
      return `/patients/${patientId}/360#medications`;
    case "vital_signs":
      return `/patients/${patientId}/360#encounters`;
    case "care_team_assignment":
      return `/patients/${patientId}/360#care-team`;
    default:
      return null;
  }
}

/** Domains ordered so the most severe appears first, ties by domain order. */
export function sortDomains<T extends { domain: string; level: RiskLevel }>(
  domains: T[],
): T[] {
  return [...domains].sort((a, b) => {
    const d = LEVEL_ORDER.indexOf(a.level) - LEVEL_ORDER.indexOf(b.level);
    if (d !== 0) return d;
    return (
      RISK_DOMAINS.indexOf(a.domain as RiskDomain) -
      RISK_DOMAINS.indexOf(b.domain as RiskDomain)
    );
  });
}
