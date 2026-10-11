import type { Lang } from "./i18n";
import { t } from "./i18n";
import { apiFetch } from "./session";

/** Server contract of `/api/v1/admin/dmind/evaluations/*` (synthetic tenants only). */

const OPERATIONS_PURPOSE = { "x-purpose-of-use": "operations" } as const;

export type EvalOperation = {
  operation: string;
  output_schema: string;
  deterministic_baseline: boolean;
  cases: number;
};

export type EvalSuites = {
  compiled: {
    suite_key: string;
    suite_version: string;
    rubric_version: string;
    case_count: number;
    corpus_hash: string;
    languages: string[];
    operations: EvalOperation[];
  };
  recorded: {
    id: string;
    suite_key: string;
    suite_version: string;
    rubric_version: string;
    case_count: number;
    created_at: string;
  }[];
  synthetic_tenant: boolean;
  can_manage: boolean;
  live_mode: "cli_only";
};

export type EvalTotals = {
  cases: number;
  passed: number;
  needs_review: number;
  failed: number;
  aborted: number;
  fallback_verified: number;
  safety_failures?: number;
};

export type EvalRunSummary = {
  totals: EvalTotals;
  operations: Record<string, EvalTotals>;
  languages: Record<string, EvalTotals>;
  safety_failures: { case_key: string; metric: string; detail: string }[];
  calls: number;
  input_tokens: number;
  output_tokens: number;
  estimated_cost: string;
  currency: string | null;
  p50_latency_ms: number | null;
  p95_latency_ms: number | null;
};

export type EvalRun = {
  id: string;
  suite_key: string;
  suite_version: string;
  rubric_version: string;
  provider: string;
  model: string;
  external: boolean;
  synthetic_provider: boolean;
  mode: "dry_run" | "offline" | "live";
  environment: string;
  requested_via: "cli" | "api";
  requested_by: string | null;
  operations: string[];
  stop_on_high_severity: boolean;
  status: "planned" | "running" | "completed" | "aborted" | "failed";
  abort_reason: string | null;
  created_at: string;
  finished_at: string | null;
  summary: EvalRunSummary | null;
};

export type EvalMetric = {
  metric: string;
  verification:
    "deterministic" | "heuristic" | "human_review_required" | "not_validated";
  severity: "info" | "low" | "medium" | "high";
  outcome: "pass" | "fail" | "review" | "not_applicable" | "not_validated";
  score: number | null;
  detail: string | null;
};

export type EvalReview = {
  id: string;
  reviewer: string;
  decision: ReviewDecision;
  comment: string;
  version: number;
  rubric_version: string;
  created_at: string;
};

export type EvalExecution = {
  id: string;
  case_key: string;
  title: string;
  category: string;
  operation: string;
  language: string;
  prompt_version: string;
  schema_version: string;
  provider: string;
  model: string;
  input_hash: string;
  evidence_refs: string[];
  baseline_hash: string;
  baseline_output: unknown;
  provider_output: unknown;
  execution_kind: "fresh" | "retried" | "reused" | "cached" | "skipped";
  retry_count: number;
  degraded: boolean;
  fallback_verified: boolean;
  status:
    | "passed"
    | "needs_review"
    | "failed"
    | "aborted"
    | "provider_unavailable_fallback_verified";
  failure_code: string | null;
  latency_ms: number | null;
  input_tokens: number | null;
  output_tokens: number | null;
  estimated_cost: string | null;
  currency: string | null;
  metrics: EvalMetric[];
  reviews: EvalReview[];
};

export type EvalRunDetail = {
  run: EvalRun;
  executions: EvalExecution[];
  can_manage: boolean;
};

export const REVIEW_DECISIONS = [
  "accept_result",
  "flag_quality",
  "flag_safety",
  "request_revision",
] as const;
export type ReviewDecision = (typeof REVIEW_DECISIONS)[number];

export type CreateRunRequest = {
  mode: "dry-run" | "offline";
  operations: string[];
  stop_on_high_severity: boolean;
};

export type CreateRunResponse = {
  run_id: string;
  status: EvalRun["status"];
  totals: EvalTotals;
  safety_failures: { case_key: string; metric: string; detail: string }[];
};

const BASE = "/api/v1/admin/dmind/evaluations";

function ops<T>(path: string, init?: RequestInit): Promise<T> {
  return apiFetch<T>(path, {
    ...init,
    headers: { ...OPERATIONS_PURPOSE, ...(init?.headers ?? {}) },
  });
}

export function loadSuites(): Promise<EvalSuites> {
  return ops(`${BASE}/suites`);
}

export function loadRuns(): Promise<{ runs: EvalRun[] }> {
  return ops(`${BASE}/runs`);
}

export function loadRun(id: string): Promise<EvalRunDetail> {
  return ops(`${BASE}/runs/${encodeURIComponent(id)}`);
}

export function createRun(body: CreateRunRequest): Promise<CreateRunResponse> {
  return ops(`${BASE}/runs`, { method: "POST", body: JSON.stringify(body) });
}

export function createReview(
  executionId: string,
  decision: ReviewDecision,
  comment: string,
): Promise<EvalReview> {
  return ops(`${BASE}/executions/${encodeURIComponent(executionId)}/reviews`, {
    method: "POST",
    body: JSON.stringify({ decision, comment }),
  });
}

/** Roles that may open the evaluation cockpit (mirrors `dmind_eval.read`). */
export function canReadEvaluations(roles: string[]): boolean {
  return roles.some(
    (r) => r === "clinical_administrator" || r === "security_auditor",
  );
}

export type EvalTone = "neutral" | "ok" | "warn" | "critical" | "dmind";

export function statusTone(
  status: EvalExecution["status"] | EvalRun["status"],
): EvalTone {
  switch (status) {
    case "passed":
    case "completed":
      return "ok";
    case "needs_review":
    case "provider_unavailable_fallback_verified":
    case "running":
    case "planned":
      return "warn";
    case "failed":
      return "critical";
    default:
      return "neutral";
  }
}

export function outcomeTone(outcome: EvalMetric["outcome"]): EvalTone {
  switch (outcome) {
    case "pass":
      return "ok";
    case "fail":
      return "critical";
    case "review":
      return "warn";
    default:
      return "neutral";
  }
}

export function statusLabel(lang: Lang, status: string): string {
  switch (status) {
    case "passed":
    case "completed":
      return t(lang, "evalStatusPassed");
    case "needs_review":
      return t(lang, "evalStatusNeedsReview");
    case "failed":
      return t(lang, "evalStatusFailed");
    case "aborted":
      return t(lang, "evalStatusAborted");
    case "provider_unavailable_fallback_verified":
      return t(lang, "evalStatusFallbackVerified");
    case "running":
      return t(lang, "evalStatusRunning");
    case "planned":
      return t(lang, "evalStatusPlanned");
    default:
      return status;
  }
}

export function verificationLabel(
  lang: Lang,
  v: EvalMetric["verification"],
): string {
  switch (v) {
    case "deterministic":
      return t(lang, "evalVerifDeterministic");
    case "heuristic":
      return t(lang, "evalVerifHeuristic");
    case "human_review_required":
      return t(lang, "evalVerifHuman");
    default:
      return t(lang, "evalVerifNotValidated");
  }
}

export function decisionLabel(lang: Lang, d: ReviewDecision): string {
  switch (d) {
    case "accept_result":
      return t(lang, "evalDecisionAccept");
    case "flag_quality":
      return t(lang, "evalDecisionFlagQuality");
    case "flag_safety":
      return t(lang, "evalDecisionFlagSafety");
    case "request_revision":
      return t(lang, "evalDecisionRevision");
  }
}

/** Pass share of a totals block, `null` when nothing ran. */
export function passRate(t: EvalTotals): number | null {
  const ran = t.cases - t.aborted;
  if (ran <= 0) return null;
  return Math.round((t.passed / ran) * 100);
}
