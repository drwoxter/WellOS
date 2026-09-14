"use client";

import Link from "next/link";
import { useState } from "react";
import { t } from "@/lib/i18n";
import type { Lang } from "@/lib/i18n";
import { formatDateTime } from "@/lib/clinical";
import { ApiRequestError, apiFetch } from "@/lib/session";
import {
  domainKey,
  evidenceHref,
  factorText,
  gapText,
  levelBadgeClass,
  levelGlyph,
  levelKey,
  reviewKey,
  sortDomains,
  trendGlyph,
  trendKey,
} from "@/lib/risk";
import type {
  DataGap,
  DomainAssessment,
  EvidenceRef,
  RiskFactor,
  RiskHistoryEntry,
  RiskLevel,
  RiskReview,
  RiskSection,
  RiskSummary,
  RiskTrend,
} from "@/lib/risk";

/** Level badge: glyph + text so the level never depends on colour alone. */
export function LevelBadge({
  lang,
  level,
  className = "",
}: {
  lang: Lang;
  level: RiskLevel | string;
  className?: string;
}) {
  return (
    <span
      className={`${levelBadgeClass(level)} risk-level ${className}`.trim()}
      data-level={level}
    >
      <span aria-hidden="true">{levelGlyph(level)}</span>{" "}
      {t(lang, levelKey(level))}
    </span>
  );
}

export function TrendBadge({
  lang,
  trend,
}: {
  lang: Lang;
  trend: RiskTrend | string;
}) {
  return (
    <span className="badge neutral" data-trend={trend}>
      <span aria-hidden="true">{trendGlyph(trend)}</span> {t(lang, "riskTrend")}
      : {t(lang, trendKey(trend))}
    </span>
  );
}

export function ReviewBadge({
  lang,
  review,
}: {
  lang: Lang;
  review: RiskReview;
}) {
  const cls =
    review.status === "reviewed"
      ? "badge ok"
      : review.status === "acknowledged"
        ? "badge neutral"
        : "badge warn";
  return (
    <span className={cls} data-review={review.status}>
      {t(lang, reviewKey(review.status))}
      {review.reviewer && review.status !== "unreviewed"
        ? ` · ${review.reviewer}`
        : ""}
    </span>
  );
}

export function evidenceLabel(lang: Lang, e: EvidenceRef): string {
  const type = recordTypeLabel(lang, e.record_type);
  return e.observed_at
    ? `${type}: ${e.label} (${formatDateTime(lang, e.observed_at)})`
    : `${type}: ${e.label}`;
}

function recordTypeLabel(lang: Lang, type: string): string {
  switch (type) {
    case "observation":
      return t(lang, "result");
    case "alert":
      return t(lang, "alerts");
    case "condition":
      return t(lang, "conditions");
    case "medication":
      return t(lang, "medications");
    case "allergy":
      return t(lang, "allergies");
    case "encounter":
      return t(lang, "consultation");
    case "visit":
      return t(lang, "riskEvidenceVisit");
    case "vital_signs":
      return t(lang, "vitalSigns");
    case "task":
      return t(lang, "followUpTasks");
    case "service_request":
      return t(lang, "riskEvidenceRequest");
    case "care_team_assignment":
      return t(lang, "careTeamTitle");
    default:
      return type.replace(/_/g, " ");
  }
}

export function EvidenceList({
  lang,
  evidence,
  patientId,
  requestIds,
}: {
  lang: Lang;
  evidence: EvidenceRef[];
  patientId: string;
  requestIds?: Record<string, string>;
}) {
  if (evidence.length === 0) return null;
  return (
    <ul className="risk-evidence">
      {evidence.map((e) => {
        const href = evidenceHref(e, patientId, requestIds);
        const label = evidenceLabel(lang, e);
        return (
          <li key={`${e.record_type}:${e.record_id}`}>
            {href ? (
              <Link className="navlink" href={href}>
                {label}
              </Link>
            ) : (
              label
            )}
            <span className="sr-only">
              {" "}
              {e.record_type} {e.record_id}
            </span>
          </li>
        );
      })}
    </ul>
  );
}

function GapList({
  lang,
  gaps,
  labelKey,
}: {
  lang: Lang;
  gaps: DataGap[];
  labelKey: "riskMissingData" | "riskStaleData";
}) {
  if (gaps.length === 0) return null;
  return (
    <p className="muted risk-gaps">
      <strong>{t(lang, labelKey)}:</strong>{" "}
      {gaps.map((g) => gapText(lang, g)).join("; ")}
    </p>
  );
}

export type DomainLike = {
  domain: string;
  level: RiskLevel;
  trend: RiskTrend;
  factors: RiskFactor[];
  missing_data: DataGap[];
  stale_data: DataGap[];
  detected_at: string | null;
  calculated_at?: string;
  rules_version?: string;
  review?: RiskReview;
};

/** One risk domain: plain-language reasons, then expandable technical
 *  evidence linked to the WellOS source records. */
export function DomainCard({
  lang,
  domain,
  patientId,
  requestIds,
  rulesVersion,
  headingLevel = 3,
}: {
  lang: Lang;
  domain: DomainLike;
  patientId: string;
  requestIds?: Record<string, string>;
  rulesVersion?: string;
  headingLevel?: 3 | 4;
}) {
  const Heading = headingLevel === 3 ? "h3" : "h4";
  const id = `risk-domain-${domain.domain}`;
  const noFactors = domain.factors.length === 0;
  return (
    <li
      className={`result-card risk-domain ${domain.level === "critical" ? "critical" : ""}`}
      aria-labelledby={id}
      data-domain={domain.domain}
      data-level={domain.level}
    >
      <div className="grow">
        <div className="risk-domain-head">
          <Heading id={id} className="title">
            {t(lang, domainKey(domain.domain))}
          </Heading>
          <LevelBadge lang={lang} level={domain.level} />
          <TrendBadge lang={lang} trend={domain.trend} />
          {domain.review ? (
            <ReviewBadge lang={lang} review={domain.review} />
          ) : null}
        </div>
        {noFactors ? (
          <p className="muted">
            {domain.level === "insufficient_data"
              ? t(lang, "riskInsufficientExplain")
              : t(lang, "riskNoFactors")}
          </p>
        ) : (
          <ul className="risk-factors">
            {domain.factors.map((f) => (
              <li key={f.code}>{factorText(lang, f)}</li>
            ))}
          </ul>
        )}
        <GapList
          lang={lang}
          gaps={domain.missing_data}
          labelKey="riskMissingData"
        />
        <GapList
          lang={lang}
          gaps={domain.stale_data}
          labelKey="riskStaleData"
        />
        <details className="risk-technical">
          <summary>{t(lang, "riskTechnicalEvidence")}</summary>
          <dl className="risk-meta">
            <dt>{t(lang, "riskRulesVersion")}</dt>
            <dd>{domain.rules_version ?? rulesVersion ?? "—"}</dd>
            <dt>{t(lang, "riskDetectedAt")}</dt>
            <dd>
              {domain.detected_at
                ? formatDateTime(lang, domain.detected_at)
                : "—"}
            </dd>
            {domain.calculated_at ? (
              <>
                <dt>{t(lang, "riskCalculatedAt")}</dt>
                <dd>{formatDateTime(lang, domain.calculated_at)}</dd>
              </>
            ) : null}
          </dl>
          {domain.factors.map((f) => (
            <div key={f.code} className="risk-factor-detail">
              <p>
                <code>{f.code}</code> <LevelBadge lang={lang} level={f.level} />
                {f.detected_at ? (
                  <span className="muted">
                    {" "}
                    · {formatDateTime(lang, f.detected_at)}
                  </span>
                ) : null}
              </p>
              <EvidenceList
                lang={lang}
                evidence={f.evidence}
                patientId={patientId}
                requestIds={requestIds}
              />
            </div>
          ))}
          {noFactors ? (
            <p className="muted">{t(lang, "riskNoEvidence")}</p>
          ) : null}
        </details>
      </div>
    </li>
  );
}

export function DomainList({
  lang,
  domains,
  patientId,
  requestIds,
  rulesVersion,
  limit,
}: {
  lang: Lang;
  domains: DomainLike[];
  patientId: string;
  requestIds?: Record<string, string>;
  rulesVersion?: string;
  limit?: number;
}) {
  const sorted = sortDomains(domains);
  const shown = limit ? sorted.slice(0, limit) : sorted;
  return (
    <ul className="result-list risk-domains">
      {shown.map((d) => (
        <DomainCard
          key={d.domain}
          lang={lang}
          domain={d}
          patientId={patientId}
          requestIds={requestIds}
          rulesVersion={rulesVersion}
        />
      ))}
    </ul>
  );
}

/** Risk evolution: one row per snapshot, most recent first. */
export function RiskHistory({
  lang,
  history,
}: {
  lang: Lang;
  history: RiskHistoryEntry[];
}) {
  if (history.length === 0) {
    return <p className="muted">{t(lang, "riskNoHistory")}</p>;
  }
  return (
    <div className="table-wrap">
      <table className="risk-history">
        <caption className="sr-only">{t(lang, "riskEvolution")}</caption>
        <thead>
          <tr>
            <th scope="col">{t(lang, "riskCalculatedAt")}</th>
            <th scope="col">{t(lang, "riskOverall")}</th>
            <th scope="col">{t(lang, "riskTrend")}</th>
            <th scope="col">{t(lang, "riskTrigger")}</th>
            <th scope="col">{t(lang, "riskRulesVersion")}</th>
          </tr>
        </thead>
        <tbody>
          {history.map((h) => (
            <tr key={h.id} data-current={h.is_current}>
              <td>
                {formatDateTime(lang, h.calculated_at)}
                {h.is_current ? (
                  <>
                    {" "}
                    <span className="badge neutral">
                      {t(lang, "riskCurrent")}
                    </span>
                  </>
                ) : null}
              </td>
              <td>
                <LevelBadge lang={lang} level={h.overall_level} />
              </td>
              <td>
                <span aria-hidden="true">{trendGlyph(h.trend)}</span>{" "}
                {t(lang, trendKey(h.trend))}
              </td>
              <td>{triggerLabel(lang, h.trigger)}</td>
              <td>
                <code>{h.rules_version}</code>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
    </div>
  );
}

function triggerLabel(lang: Lang, trigger: string): string {
  switch (trigger) {
    case "seed":
      return t(lang, "riskTriggerSeed");
    case "manual":
      return t(lang, "riskTriggerManual");
    case "review":
      return t(lang, "riskTriggerReview");
    default:
      return trigger.replace(/\./g, " › ").replace(/_/g, " ");
  }
}

// ---------------------------------------------------------------------------
// Review actions: acknowledge · mark reviewed · assign
// ---------------------------------------------------------------------------

export type ActionKind = "acknowledge" | "review" | "assign";

export function errorText(lang: Lang, err: unknown): string {
  if (err instanceof ApiRequestError) {
    if (err.status === 409) return t(lang, "riskConflict");
    if (err.status === 403 || err.status === 404)
      return t(lang, "notAuthorized");
  }
  return err instanceof Error ? err.message : String(err);
}

/** Acknowledge / mark reviewed / assign for one patient's current
 *  assessment. Every action posts the displayed assessment id so a review
 *  can never land on a newer snapshot the professional has not seen. */
export function RiskActions({
  lang,
  patientId,
  assessmentId,
  domain,
  review,
  capabilities,
  professionals,
  owner,
  onChanged,
  idPrefix,
}: {
  lang: Lang;
  patientId: string;
  assessmentId: string;
  domain: string;
  review: RiskReview;
  capabilities: {
    can_acknowledge: boolean;
    can_review: boolean;
    can_assign: boolean;
  };
  professionals: { id: string; display_name: string }[];
  owner: { user_id: string; display_name: string } | null;
  onChanged: () => Promise<unknown>;
  idPrefix: string;
}) {
  const [busy, setBusy] = useState<ActionKind | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const [assignee, setAssignee] = useState(owner?.user_id ?? "");
  const [showAssign, setShowAssign] = useState(false);

  async function run(kind: ActionKind) {
    setBusy(kind);
    setError(null);
    setSuccess(null);
    try {
      if (kind === "assign") {
        if (!assignee) {
          setError(t(lang, "riskChooseProfessional"));
          return;
        }
        await apiFetch(`/api/v1/patients/${patientId}/risk/assign`, {
          method: "POST",
          body: JSON.stringify({ assignee_user_id: assignee }),
        });
        setSuccess(t(lang, "riskAssigned"));
        setShowAssign(false);
      } else {
        await apiFetch(`/api/v1/patients/${patientId}/risk/${kind}`, {
          method: "POST",
          body: JSON.stringify({
            assessment_id: assessmentId,
            domain,
            note: note.trim() || null,
          }),
        });
        setSuccess(
          t(lang, kind === "acknowledge" ? "riskAcknowledged" : "riskReviewed"),
        );
        setNote("");
      }
      await onChanged();
    } catch (err) {
      setError(errorText(lang, err));
    } finally {
      setBusy(null);
    }
  }

  const canAct =
    capabilities.can_acknowledge ||
    capabilities.can_review ||
    capabilities.can_assign;
  if (!canAct) return null;
  const noteId = `${idPrefix}-note`;
  const assignId = `${idPrefix}-assignee`;
  return (
    <div className="risk-actions">
      {error ? (
        <p role="alert" className="error">
          {error}
        </p>
      ) : null}
      {success ? (
        <p role="status" className="success">
          {success}
        </p>
      ) : null}
      {capabilities.can_acknowledge || capabilities.can_review ? (
        <>
          <label htmlFor={noteId}>{t(lang, "riskReviewNote")}</label>
          <input
            id={noteId}
            type="text"
            value={note}
            maxLength={500}
            onChange={(e) => setNote(e.target.value)}
            disabled={busy !== null}
          />
        </>
      ) : null}
      <div className="risk-action-row">
        {capabilities.can_acknowledge && review.status === "unreviewed" ? (
          <button
            type="button"
            className="secondary"
            disabled={busy !== null}
            aria-busy={busy === "acknowledge"}
            onClick={() => void run("acknowledge")}
          >
            {t(lang, "riskAcknowledge")}
          </button>
        ) : null}
        {capabilities.can_review && review.status !== "reviewed" ? (
          <button
            type="button"
            className="primary"
            disabled={busy !== null}
            aria-busy={busy === "review"}
            onClick={() => void run("review")}
          >
            {t(lang, "riskMarkReviewed")}
          </button>
        ) : null}
        {capabilities.can_assign ? (
          <button
            type="button"
            className="secondary"
            disabled={busy !== null}
            aria-expanded={showAssign}
            aria-controls={`${idPrefix}-assign-form`}
            onClick={() => setShowAssign((s) => !s)}
          >
            {owner ? t(lang, "riskReassign") : t(lang, "riskAssign")}
          </button>
        ) : null}
      </div>
      {showAssign && capabilities.can_assign ? (
        <form
          id={`${idPrefix}-assign-form`}
          className="risk-assign"
          onSubmit={(e) => {
            e.preventDefault();
            void run("assign");
          }}
        >
          <label htmlFor={assignId}>{t(lang, "riskAssignTo")}</label>
          <select
            id={assignId}
            value={assignee}
            onChange={(e) => setAssignee(e.target.value)}
            disabled={busy !== null}
          >
            <option value="">{t(lang, "riskChooseProfessional")}</option>
            {professionals.map((p) => (
              <option key={p.id} value={p.id}>
                {p.display_name}
              </option>
            ))}
          </select>
          <button
            type="submit"
            className="primary"
            disabled={busy !== null || !assignee}
            aria-busy={busy === "assign"}
          >
            {t(lang, "riskConfirmAssign")}
          </button>
        </form>
      ) : null}
    </div>
  );
}

// ---------------------------------------------------------------------------
// dMind Risk Agent panel: generate · review · confirm suggestions
// ---------------------------------------------------------------------------

/** Governed dMind risk summary. Output is always labelled AI-generated,
 *  levels can only be raised to the deterministic floor, and a suggestion
 *  becomes a task only through an explicit professional confirmation. */
export function RiskSummaryPanel({
  lang,
  patientId,
  risk,
  onChanged,
  compact = false,
}: {
  lang: Lang;
  patientId: string;
  risk: RiskSection;
  onChanged: () => Promise<unknown>;
  compact?: boolean;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [confirming, setConfirming] = useState<number | null>(null);
  const [priority, setPriority] = useState<"routine" | "urgent">("routine");
  const [dueDays, setDueDays] = useState(14);
  const [confirmed, setConfirmed] = useState<number[]>([]);
  const summary = risk.summary;
  const current = risk.current;
  const canGenerate = risk.capabilities.can_recalculate && current !== null;
  const canReview = risk.capabilities.can_review;

  async function call(label: string, fn: () => Promise<unknown>, ok: string) {
    setBusy(label);
    setError(null);
    setSuccess(null);
    try {
      await fn();
      setSuccess(ok);
      await onChanged();
    } catch (err) {
      setError(errorText(lang, err));
    } finally {
      setBusy(null);
    }
  }

  function generate() {
    if (!current) return;
    void call(
      "generate",
      () =>
        apiFetch(`/api/v1/patients/${patientId}/risk/summary`, {
          method: "POST",
          body: JSON.stringify({ language: lang, assessment_id: current.id }),
        }),
      t(lang, "riskSummaryGenerated"),
    );
  }

  function review(decision: "approve" | "reject") {
    if (!summary) return;
    void call(
      decision,
      () =>
        apiFetch(
          `/api/v1/patients/${patientId}/risk/summary/${summary.id}/review`,
          {
            method: "POST",
            body: JSON.stringify({ decision }),
          },
        ),
      t(
        lang,
        decision === "approve" ? "riskSummaryApproved" : "riskSummaryRejected",
      ),
    );
  }

  function confirmSuggestion(index: number) {
    if (!summary) return;
    void call(
      `confirm-${index}`,
      async () => {
        await apiFetch(
          `/api/v1/patients/${patientId}/risk/summary/${summary.id}/confirm`,
          {
            method: "POST",
            body: JSON.stringify({
              suggestion_index: index,
              priority,
              due_in_days: dueDays,
            }),
          },
        );
        setConfirmed((c) => [...c, index]);
        setConfirming(null);
      },
      t(lang, "riskTaskCreated"),
    );
  }

  if (!current) return null;
  const stale = summary ? summary.status === "superseded" : false;
  const out = summary?.output;

  return (
    <section
      className="card dmind-panel risk-summary"
      aria-labelledby="risk-summary-h"
    >
      <div className="risk-domain-head">
        <h2 id="risk-summary-h">{t(lang, "riskDmindTitle")}</h2>
        <span className="badge neutral">{t(lang, "riskAiGenerated")}</span>
      </div>
      <p className="muted">{t(lang, "riskDmindHelp")}</p>
      {error ? (
        <p role="alert" className="error">
          {error}
        </p>
      ) : null}
      {success ? (
        <p role="status" className="success">
          {success}
        </p>
      ) : null}
      {!summary || stale ? (
        <>
          {stale ? (
            <p className="muted">{t(lang, "riskSummaryStale")}</p>
          ) : (
            <p className="muted">{t(lang, "riskNoSummary")}</p>
          )}
          {canGenerate ? (
            <button
              type="button"
              className="primary"
              disabled={busy !== null}
              aria-busy={busy === "generate"}
              onClick={generate}
            >
              {t(lang, "riskGenerateSummary")}
            </button>
          ) : null}
        </>
      ) : out ? (
        <>
          <div className="risk-domain-head">
            <LevelBadge lang={lang} level={out.overall_level} />
            <span className="badge neutral">
              {t(lang, "riskSafetyFloor")}:{" "}
              {t(lang, levelKey(out.safety_floor))}
            </span>
            <span className={`badge confidence-${out.confidence}`}>
              {t(lang, "confidence")}: {t(lang, confidenceKey(out.confidence))}
            </span>
            <span
              className={`badge ${summary.status === "approved" ? "ok" : summary.status === "rejected" ? "critical" : "warn"}`}
              data-status={summary.status}
            >
              {t(lang, summaryStatusKey(summary.status))}
            </span>
          </div>
          {out.raised_to_floor ? (
            <p className="muted">{t(lang, "riskRaisedToFloor")}</p>
          ) : null}
          <ul className="risk-explanations">
            {sortDomains(out.domains)
              .filter((d) => !compact || d.reasons.length > 0)
              .map((d) => (
                <li key={d.domain}>
                  <strong>{t(lang, domainKey(d.domain))}</strong>{" "}
                  <LevelBadge lang={lang} level={d.level} />
                  <p>{d.summary}</p>
                  {d.cited_sources.length > 0 ? (
                    <p className="muted risk-citations">
                      {t(lang, "riskCitedSources")}: {d.cited_sources.length}
                    </p>
                  ) : null}
                </li>
              ))}
          </ul>
          {out.missing_information.length > 0 ? (
            <p>
              <strong>{t(lang, "riskMissingData")}:</strong>{" "}
              {out.missing_information.join("; ")}
            </p>
          ) : null}
          {out.contradictions.length > 0 ? (
            <p>
              <strong>{t(lang, "riskContradictions")}:</strong>{" "}
              {out.contradictions.join("; ")}
            </p>
          ) : null}

          <h3>{t(lang, "riskSuggestions")}</h3>
          {out.follow_up_suggestions.length === 0 ? (
            <p className="muted">{t(lang, "riskNoSuggestions")}</p>
          ) : (
            <ul className="risk-suggestions">
              {out.follow_up_suggestions.map((s, i) => (
                <li key={i}>
                  <div className="risk-domain-head">
                    <span>{s.text}</span>
                    <span className="badge neutral">
                      {t(lang, domainKey(s.domain))}
                    </span>
                    <span className="badge warn">
                      {t(lang, "riskRequiresConfirmation")}
                    </span>
                  </div>
                  {summary.status === "approved" && canReview ? (
                    confirmed.includes(i) ? (
                      <p className="success" role="status">
                        {t(lang, "riskTaskCreated")}
                      </p>
                    ) : confirming === i ? (
                      <form
                        className="risk-confirm"
                        onSubmit={(e) => {
                          e.preventDefault();
                          confirmSuggestion(i);
                        }}
                      >
                        <label htmlFor={`risk-priority-${i}`}>
                          {t(lang, "priority")}
                        </label>
                        <select
                          id={`risk-priority-${i}`}
                          value={priority}
                          onChange={(e) =>
                            setPriority(
                              e.target.value === "urgent"
                                ? "urgent"
                                : "routine",
                            )
                          }
                        >
                          <option value="routine">{t(lang, "routine")}</option>
                          <option value="urgent">
                            {t(lang, "priorityUrgent")}
                          </option>
                        </select>
                        <label htmlFor={`risk-due-${i}`}>
                          {t(lang, "riskDueInDays")}
                        </label>
                        <input
                          id={`risk-due-${i}`}
                          type="number"
                          min={1}
                          max={365}
                          value={dueDays}
                          onChange={(e) =>
                            setDueDays(Number(e.target.value) || 14)
                          }
                        />
                        <div className="risk-action-row">
                          <button
                            type="submit"
                            className="primary"
                            disabled={busy !== null}
                            aria-busy={busy === `confirm-${i}`}
                          >
                            {t(lang, "riskConfirmCreateTask")}
                          </button>
                          <button
                            type="button"
                            className="secondary"
                            disabled={busy !== null}
                            onClick={() => setConfirming(null)}
                          >
                            {t(lang, "cancel")}
                          </button>
                        </div>
                      </form>
                    ) : (
                      <button
                        type="button"
                        className="secondary"
                        disabled={busy !== null}
                        onClick={() => setConfirming(i)}
                      >
                        {t(lang, "riskConvertToTask")}
                      </button>
                    )
                  ) : null}
                </li>
              ))}
            </ul>
          )}

          {summary.status === "awaiting_review" && canReview ? (
            <div className="risk-action-row">
              <button
                type="button"
                className="primary"
                disabled={busy !== null}
                aria-busy={busy === "approve"}
                onClick={() => review("approve")}
              >
                {t(lang, "riskApproveSummary")}
              </button>
              <button
                type="button"
                className="secondary"
                disabled={busy !== null}
                aria-busy={busy === "reject"}
                onClick={() => review("reject")}
              >
                {t(lang, "riskRejectSummary")}
              </button>
            </div>
          ) : null}
          {summary.status === "awaiting_review" ? (
            <p className="muted">{t(lang, "riskSummaryAwaiting")}</p>
          ) : null}

          <details className="risk-technical">
            <summary>{t(lang, "riskTechnicalEvidence")}</summary>
            <ul className="muted">
              {out.limitations.map((l, i) => (
                <li key={i}>{l}</li>
              ))}
            </ul>
            <dl className="risk-meta">
              <dt>{t(lang, "riskProvider")}</dt>
              <dd>
                {summary.model} {summary.model_version} · {summary.route}
              </dd>
              <dt>{t(lang, "riskPromptVersion")}</dt>
              <dd>
                <code>{summary.prompt_version}</code> ·{" "}
                <code>{out.schema_version}</code>
              </dd>
              <dt>{t(lang, "riskRulesVersion")}</dt>
              <dd>
                <code>{out.rules_version}</code>
              </dd>
              <dt>{t(lang, "generatedAt")}</dt>
              <dd>{formatDateTime(lang, summary.generated_at)}</dd>
              {summary.reviewed_at ? (
                <>
                  <dt>{t(lang, "riskReviewedBy")}</dt>
                  <dd>
                    {summary.reviewer} ·{" "}
                    {formatDateTime(lang, summary.reviewed_at)}
                  </dd>
                </>
              ) : null}
              <dt>{t(lang, "riskCitedSources")}</dt>
              <dd>{out.cited_sources.length}</dd>
            </dl>
          </details>
          {canGenerate && summary.status !== "awaiting_review" ? (
            <button
              type="button"
              className="secondary"
              disabled={busy !== null}
              aria-busy={busy === "generate"}
              onClick={generate}
            >
              {t(lang, "riskRegenerateSummary")}
            </button>
          ) : null}
        </>
      ) : null}
    </section>
  );
}

function confidenceKey(c: string) {
  return c === "high"
    ? ("confidenceHigh" as const)
    : c === "medium"
      ? ("confidenceMedium" as const)
      : ("confidenceLow" as const);
}

function summaryStatusKey(s: string) {
  switch (s) {
    case "approved":
      return "riskSummaryStatusApproved" as const;
    case "rejected":
      return "riskSummaryStatusRejected" as const;
    case "superseded":
      return "riskSummaryStatusSuperseded" as const;
    default:
      return "riskSummaryStatusAwaiting" as const;
  }
}

/** Header strip for the current assessment: overall level, floor, trend,
 *  review state, timestamps and the follow-up owner. */
export function RiskOverviewHeader({
  lang,
  risk,
}: {
  lang: Lang;
  risk: RiskSection;
}) {
  const c = risk.current;
  if (!c) return null;
  return (
    <div className="risk-overview">
      <div className="risk-domain-head">
        <LevelBadge
          lang={lang}
          level={c.overall_level}
          className="risk-overall"
        />
        <TrendBadge lang={lang} trend={c.trend} />
        <ReviewBadge lang={lang} review={c.review} />
        {c.safety_floor !== "low" && c.safety_floor !== "insufficient_data" ? (
          <span className="badge neutral">
            {t(lang, "riskSafetyFloor")}: {t(lang, levelKey(c.safety_floor))}
          </span>
        ) : null}
      </div>
      <p className="muted">
        {t(lang, "riskCalculatedAt")} {formatDateTime(lang, c.calculated_at)} ·{" "}
        {t(lang, "riskRulesVersion")} <code>{c.rules_version}</code>
        {risk.follow_up_owner
          ? ` · ${t(lang, "riskFollowUpOwner")}: ${risk.follow_up_owner.display_name}`
          : ` · ${t(lang, "riskUnassigned")}`}
      </p>
      {c.overall_level === "insufficient_data" ? (
        <p className="muted">{t(lang, "riskInsufficientExplain")}</p>
      ) : null}
    </div>
  );
}
