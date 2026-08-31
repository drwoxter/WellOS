"use client";

import Link from "next/link";
import { useCallback, useEffect, useState } from "react";
import { useParams } from "next/navigation";
import { AppShell } from "../../chrome";
import { t } from "@/lib/i18n";
import type { Lang, TKey } from "@/lib/i18n";
import {
  LOOP_STATES,
  formatDateTime,
  loopStateIndex,
  patientName,
} from "@/lib/clinical";
import { apiFetch, useSession } from "@/lib/session";

type Detail = {
  service_request: {
    id: string;
    display: string;
    code_loinc: string;
    loop_state: string;
    version: number;
    created_at: string;
    patient: {
      id: string;
      family_name: string;
      given_name: string;
      identifier: string;
    };
  };
  observations: {
    id: string;
    value: string;
    unit: string;
    reference_range: string | null;
    status: string;
    amends: string | null;
    effective_at: string;
    received_at: string;
  }[];
  rule_evaluations: {
    rule_id: string;
    rule_version: string;
    outcome: { outcome?: string } & Record<string, unknown>;
    evaluated_at: string;
  }[];
  ai_artifacts: {
    id: string;
    status: string;
    autonomy_level: string;
    model: string | null;
    output: {
      summary?: string;
      cited_sources?: string[];
      limitations?: string[];
      suggested_next_step_categories?: string[];
    } | null;
  }[];
  follow_up_tasks: {
    id: string;
    description: string;
    priority: string;
    status: string;
    due_at: string | null;
  }[];
  alerts: { id: string; severity: string; message: string; status: string }[];
  data_quality_issues: { issue: string; created_at: string }[];
  notes: { kind: string; note: string; author: string; created_at: string }[];
  capabilities: { review: boolean; notify: boolean; close: boolean };
};

type TransitionKind = "review" | "notify" | "close";

const STEP_KEYS: TKey[] = [
  "stepOrdered",
  "stepReceived",
  "stepReviewed",
  "stepNotified",
  "stepClosed",
];

function Stepper({ lang, state }: { lang: Lang; state: string }) {
  const idx = loopStateIndex(state);
  return (
    <ol className="stepper" aria-label={t(lang, "workflowProgress")}>
      {LOOP_STATES.map((s, i) => (
        <li
          key={s}
          className={i < idx ? "done" : i === idx ? "current" : ""}
          aria-current={i === idx ? "step" : undefined}
        >
          {i < idx ? "✓ " : ""}
          {t(lang, STEP_KEYS[i])}
        </li>
      ))}
    </ol>
  );
}

// Follow-up task statuses set by the backend: open, overdue (escalated),
// completed, superseded (retired by an amendment). Each renders distinctly
// so an escalated task is never mistaken for ordinary open work.
function taskStatusBadge(status: string): string {
  switch (status) {
    case "completed":
      return "ok";
    case "overdue":
      return "critical";
    case "superseded":
      return "neutral";
    default:
      return "warn";
  }
}

function taskStatusLabel(lang: Lang, status: string): string {
  switch (status) {
    case "completed":
      return t(lang, "completed");
    case "overdue":
      return t(lang, "overdue");
    case "superseded":
      return t(lang, "superseded");
    default:
      return t(lang, "open");
  }
}

function outcomeLabel(lang: Lang, outcome: string | undefined): string {
  switch (outcome) {
    case "critical":
      return t(lang, "critical");
    case "not_critical":
      return t(lang, "notCritical");
    case "unit_mismatch":
      return t(lang, "unitMismatch");
    default:
      return outcome ?? "—";
  }
}

export default function RequestDetailPage() {
  const { authenticated, lang } = useSession();
  const params = useParams<{ id: string }>();
  const [data, setData] = useState<Detail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const [noteError, setNoteError] = useState(false);
  const [busy, setBusy] = useState(false);
  const [confirming, setConfirming] = useState<TransitionKind | null>(null);

  const load = useCallback(() => {
    if (!authenticated) return;
    apiFetch<Detail>(`/api/v1/service-requests/${params.id}`)
      .then(setData)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, [authenticated, params.id]);

  useEffect(load, [load]);

  async function runTransition(kind: TransitionKind) {
    if (!authenticated || !data) return;
    setBusy(true);
    setError(null);
    setSuccess(null);
    try {
      await apiFetch(`/api/v1/service-requests/${params.id}/${kind}`, {
        method: "POST",
        body: JSON.stringify({
          version: data.service_request.version,
          note: note || null,
        }),
      });
      setNote("");
      setSuccess(t(lang, "actionRecorded"));
      load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
      setConfirming(null);
    }
  }

  function requestTransition(kind: TransitionKind) {
    if (!note.trim()) {
      setNoteError(true);
      return;
    }
    setNoteError(false);
    setConfirming(kind);
  }

  const sr = data?.service_request;
  const artifact = data?.ai_artifacts[data.ai_artifacts.length - 1];
  const openAlerts = data?.alerts.filter((a) => a.status === "open") ?? [];

  const nextAction: TransitionKind | null =
    sr?.loop_state === "received"
      ? "review"
      : sr?.loop_state === "reviewed"
        ? "notify"
        : sr?.loop_state === "notified"
          ? "close"
          : null;
  // Server-derived, result-specific capability hints; the backend guards on
  // the transition endpoints remain the authorization boundary.
  const canRunNextAction =
    nextAction !== null && (data?.capabilities?.[nextAction] ?? false);
  const anyTransitionCapability =
    (data?.capabilities?.review ||
      data?.capabilities?.notify ||
      data?.capabilities?.close) ??
    false;

  const actionLabel: Record<TransitionKind, TKey> = {
    review: "review",
    notify: "notify",
    close: "close",
  };
  const confirmText: Record<TransitionKind, TKey> = {
    review: "confirmReview",
    notify: "confirmNotify",
    close: "confirmClose",
  };

  return (
    <AppShell>
      <p style={{ marginTop: 0 }}>
        <Link href="/results">← {t(lang, "backToWorklist")}</Link>
      </p>
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
      {!data || !sr ? (
        <p className="muted" role="status">
          {t(lang, "loading")}
        </p>
      ) : (
        <>
          {openAlerts.length > 0 ? (
            <div className="critical-banner" role="alert">
              {t(
                lang,
                sr.loop_state === "received"
                  ? "criticalBanner"
                  : "criticalBannerReviewed",
              )}
              {openAlerts.map((a) => (
                <div key={a.id} style={{ fontWeight: 400 }}>
                  {a.message}
                </div>
              ))}
            </div>
          ) : null}

          <div className="card">
            <div className="patient-header">
              <h2>{patientName(sr.patient)}</h2>
              <span className="badge neutral">{sr.patient.identifier}</span>
            </div>
            <p style={{ marginBottom: "0.3rem" }}>
              <strong>{sr.display}</strong>{" "}
              <span className="muted">· LOINC {sr.code_loinc}</span>
            </p>
            <p className="muted" style={{ marginTop: 0 }}>
              {t(lang, "ordered")}: {formatDateTime(lang, sr.created_at)}
            </p>
            <Stepper lang={lang} state={sr.loop_state} />
            <p>
              <Link href={`/patients/${sr.patient.id}`}>
                {t(lang, "openChart")}
              </Link>
            </p>
          </div>

          <div className="card">
            <h2>{t(lang, "observations")}</h2>
            {data.data_quality_issues.map((d, i) => (
              <p key={i} className="error">
                {t(lang, "dataQualityIssues")}: {d.issue}
              </p>
            ))}
            <div className="table-wrap">
              <table>
                <thead>
                  <tr>
                    <th scope="col">{t(lang, "value")}</th>
                    <th scope="col">{t(lang, "referenceRange")}</th>
                    <th scope="col">{t(lang, "state")}</th>
                    <th scope="col">{t(lang, "collected")}</th>
                    <th scope="col">{t(lang, "received")}</th>
                  </tr>
                </thead>
                <tbody>
                  {data.observations.map((o) => (
                    <tr key={o.id}>
                      <td>
                        <strong>
                          {o.value} {o.unit}
                        </strong>
                      </td>
                      <td>{o.reference_range ?? "—"}</td>
                      <td>
                        {o.status === "amended-superseded" ? (
                          <span className="badge warn">
                            {t(lang, "supersededBy")}
                          </span>
                        ) : o.amends ? (
                          <span className="badge neutral">
                            {t(lang, "amendmentOf")}
                          </span>
                        ) : (
                          <span className="badge ok">{o.status}</span>
                        )}
                      </td>
                      <td>{formatDateTime(lang, o.effective_at)}</td>
                      <td>{formatDateTime(lang, o.received_at)}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          </div>

          {nextAction && canRunNextAction ? (
            <div className="action-area">
              <h2>{t(lang, "nextAction")}</h2>
              <label htmlFor="note">{t(lang, "workflowNotes")}</label>
              <textarea
                id="note"
                value={note}
                onChange={(e) => setNote(e.target.value)}
                rows={2}
                aria-invalid={noteError}
              />
              {noteError ? (
                <p role="alert" className="error">
                  {t(lang, "noteRequired")}
                </p>
              ) : null}
              <p>
                <button
                  className="primary"
                  disabled={busy}
                  onClick={() => requestTransition(nextAction)}
                >
                  {t(lang, actionLabel[nextAction])}
                </button>
              </p>
            </div>
          ) : sr.loop_state !== "closed" && anyTransitionCapability ? (
            <p className="muted">{t(lang, "noActionAvailable")}</p>
          ) : null}

          <div className="advisory">
            <h2>{t(lang, "aiSummary")}</h2>
            <p className="muted">{t(lang, "aiDisclaimer")}</p>
            {!artifact || artifact.status === "unavailable" ? (
              <p>{t(lang, "aiUnavailable")}</p>
            ) : (
              <>
                <p>
                  <span className="badge neutral">{artifact.status}</span>{" "}
                  <span className="badge neutral">
                    {artifact.autonomy_level}
                  </span>{" "}
                  <span className="muted">{artifact.model}</span>
                </p>
                <p>{artifact.output?.summary}</p>
                <h3 style={{ fontSize: "0.9rem" }}>{t(lang, "citations")}</h3>
                <ul>
                  {(artifact.output?.cited_sources ?? []).map((c) => (
                    <li key={c}>{c}</li>
                  ))}
                </ul>
                <h3 style={{ fontSize: "0.9rem" }}>{t(lang, "limitations")}</h3>
                <ul>
                  {(artifact.output?.limitations ?? []).map((c) => (
                    <li key={c}>{c}</li>
                  ))}
                </ul>
                <h3 style={{ fontSize: "0.9rem" }}>{t(lang, "nextSteps")}</h3>
                <ul>
                  {(artifact.output?.suggested_next_step_categories ?? []).map(
                    (c) => (
                      <li key={c}>{c}</li>
                    ),
                  )}
                </ul>
              </>
            )}
          </div>

          <details className="secondary">
            <summary>{t(lang, "ruleEvaluations")}</summary>
            <ul>
              {data.rule_evaluations.map((r, i) => (
                <li key={i}>
                  {t(lang, "evaluatedWith")} {r.rule_id} ({t(lang, "version")}{" "}
                  {r.rule_version}) — {t(lang, "outcome")}:{" "}
                  {outcomeLabel(lang, r.outcome.outcome)} ·{" "}
                  {formatDateTime(lang, r.evaluated_at)}
                </li>
              ))}
            </ul>
          </details>

          {data.follow_up_tasks.length > 0 ? (
            <details className="secondary">
              <summary>{t(lang, "followUpTasks")}</summary>
              <ul className="result-list" style={{ marginTop: "0.6rem" }}>
                {data.follow_up_tasks.map((task) => (
                  <li key={task.id} className="result-card">
                    <div className="grow">{task.description}</div>
                    <span
                      className={`badge ${task.priority === "high" ? "warn" : "neutral"}`}
                    >
                      {t(lang, "priority")}:{" "}
                      {task.priority === "high"
                        ? t(lang, "high")
                        : task.priority}
                    </span>
                    <span className={`badge ${taskStatusBadge(task.status)}`}>
                      {taskStatusLabel(lang, task.status)}
                    </span>
                  </li>
                ))}
              </ul>
            </details>
          ) : null}

          {data.notes.length > 0 ? (
            <details className="secondary">
              <summary>{t(lang, "workflowNotes")}</summary>
              <ul>
                {data.notes.map((n, i) => (
                  <li key={i}>
                    <strong>{n.author}</strong> ({n.kind},{" "}
                    {formatDateTime(lang, n.created_at)}): {n.note}
                  </li>
                ))}
              </ul>
            </details>
          ) : null}

          {confirming ? (
            <div className="modal-backdrop" role="presentation">
              <div
                className="modal"
                role="dialog"
                aria-modal="true"
                aria-labelledby="confirm-title"
              >
                <h2 id="confirm-title">{t(lang, "confirmTitle")}</h2>
                <p>{t(lang, confirmText[confirming])}</p>
                <div className="actions">
                  <button
                    className="secondary"
                    onClick={() => setConfirming(null)}
                    disabled={busy}
                  >
                    {t(lang, "cancel")}
                  </button>
                  <button
                    className="primary"
                    onClick={() => void runTransition(confirming)}
                    disabled={busy}
                  >
                    {t(lang, "confirm")}
                  </button>
                </div>
              </div>
            </div>
          ) : null}
        </>
      )}
    </AppShell>
  );
}
