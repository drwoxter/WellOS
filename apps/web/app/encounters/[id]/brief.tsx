"use client";

import Link from "next/link";
import { useState } from "react";
import { t } from "@/lib/i18n";
import type { Lang, TKey } from "@/lib/i18n";
import {
  formatBloodPressure,
  formatDate,
  formatDateTime,
  loopStateShortLabel,
} from "@/lib/clinical";

export type BriefNote = {
  encounter_id: string;
  started_at: string;
  signed_at: string | null;
  signed_by: string | null;
  reason_for_encounter: string | null;
  assessment: string | null;
  plan: string | null;
};

export type BriefTask = {
  id: string;
  description: string;
  priority: string;
  status: string;
  due_at: string | null;
  service_request_id: string;
};

export type BriefRequest = {
  id: string;
  display: string;
  loop_state: string;
  created_at: string;
  this_encounter: boolean;
};

export type BriefAbnormal = {
  id: string;
  service_request_id: string;
  code: string;
  display: string;
  value: string;
  unit: string;
  reference_range: string | null;
  abnormal: string;
  critical: boolean;
  effective_at: string;
};

export type Brief = {
  recent_notes: BriefNote[];
  open_tasks: BriefTask[];
  open_requests: BriefRequest[];
  recent_abnormal: BriefAbnormal[];
  generated_at: string;
};

export type BriefVitals = {
  id: string;
  systolic_mmhg: string | null;
  diastolic_mmhg: string | null;
  heart_rate_bpm: string | null;
  temperature_c: string | null;
  spo2_percent: string | null;
  weight_kg: string | null;
  recorded_at: string;
};

export type PatientBriefProps = {
  lang: Lang;
  brief: Brief;
  problems: { id: string; display: string; status: string }[];
  allergies: { substance: string; criticality: string }[];
  alerts: { severity: string; message: string }[];
  medications: { name: string; status: string }[];
  vitals: BriefVitals[];
  defaultOpen?: boolean;
};

const PREVIEW = 3;

function Disclosure<T>({
  items,
  lang,
  emptyKey,
  render,
}: {
  items: T[];
  lang: Lang;
  emptyKey: TKey;
  render: (item: T) => React.ReactNode;
}) {
  const [all, setAll] = useState(false);
  if (items.length === 0) return <p className="muted">{t(lang, emptyKey)}</p>;
  const shown = all ? items : items.slice(0, PREVIEW);
  return (
    <>
      <ul className="brief-list">{shown.map(render)}</ul>
      {items.length > PREVIEW ? (
        <button
          type="button"
          className="linklike"
          aria-expanded={all}
          onClick={() => setAll((a) => !a)}
        >
          {all
            ? t(lang, "showLess")
            : `${t(lang, "showMore")} (${items.length - PREVIEW})`}
        </button>
      ) : null}
    </>
  );
}

/** Compact clinical brief for the start of a consultation: facts already in
 *  the record, each with a reference back to its source, no interpretation. */
export function PatientBrief({
  lang,
  brief,
  problems,
  allergies,
  alerts,
  medications,
  vitals,
  defaultOpen = true,
}: PatientBriefProps) {
  const active = problems.filter((p) => p.status !== "resolved");
  return (
    <details className="card patient-brief" open={defaultOpen}>
      <summary>
        <h2>{t(lang, "patientBrief")}</h2>
        <span className="muted">
          {t(lang, "briefHelp")} {formatDateTime(lang, brief.generated_at)}
        </span>
      </summary>

      <div className="brief-grid">
        <section aria-labelledby="brief-alerts">
          <h3 id="brief-alerts">{t(lang, "safetyInformation")}</h3>
          {allergies.length === 0 && alerts.length === 0 ? (
            <p className="muted">{t(lang, "noKnownAllergies")}</p>
          ) : (
            <ul className="brief-list">
              {allergies.map((a) => (
                <li key={a.substance}>
                  <span
                    className={`badge ${a.criticality === "high" ? "critical" : "warn"}`}
                  >
                    {t(lang, "allergies")}
                  </span>{" "}
                  {a.substance}
                </li>
              ))}
              {alerts.map((a, i) => (
                <li key={i}>
                  <span
                    className={`badge ${a.severity === "critical" ? "critical" : "warn"}`}
                  >
                    {t(lang, "alerts")}
                  </span>{" "}
                  {a.message}
                </li>
              ))}
            </ul>
          )}
        </section>

        <section aria-labelledby="brief-problems">
          <h3 id="brief-problems">{t(lang, "activeProblems")}</h3>
          <Disclosure
            items={active}
            lang={lang}
            emptyKey="noActiveProblems"
            render={(p) => (
              <li key={p.id}>
                {p.display}
                {p.status === "provisional" ? (
                  <span
                    className="badge neutral"
                    style={{ marginLeft: "0.4rem" }}
                  >
                    {t(lang, "dxProvisional")}
                  </span>
                ) : null}
              </li>
            )}
          />
        </section>

        <section aria-labelledby="brief-meds">
          <h3 id="brief-meds">{t(lang, "medications")}</h3>
          <Disclosure
            items={medications}
            lang={lang}
            emptyKey="noMedications"
            render={(m) => <li key={m.name}>{m.name}</li>}
          />
        </section>

        <section aria-labelledby="brief-notes">
          <h3 id="brief-notes">{t(lang, "recentNotes")}</h3>
          <Disclosure
            items={brief.recent_notes}
            lang={lang}
            emptyKey="noRecentNotes"
            render={(n) => (
              <li key={n.encounter_id}>
                <Link
                  className="navlink"
                  href={`/encounters/${n.encounter_id}`}
                >
                  {formatDate(lang, n.signed_at ?? n.started_at)}
                </Link>
                {n.reason_for_encounter ? ` — ${n.reason_for_encounter}` : ""}
                {n.assessment ? (
                  <div className="muted brief-clamp">{n.assessment}</div>
                ) : null}
              </li>
            )}
          />
        </section>

        <section aria-labelledby="brief-outstanding">
          <h3 id="brief-outstanding">{t(lang, "outstandingItems")}</h3>
          {brief.open_tasks.length === 0 && brief.open_requests.length === 0 ? (
            <p className="muted">{t(lang, "noOutstanding")}</p>
          ) : (
            <ul className="brief-list">
              {brief.open_tasks.map((task) => (
                <li key={task.id}>
                  <span
                    className={`badge ${task.status === "overdue" ? "critical" : "warn"}`}
                  >
                    {task.status === "overdue"
                      ? t(lang, "overdue")
                      : t(lang, "followUpTasks")}
                  </span>{" "}
                  <Link
                    className="navlink"
                    href={`/requests/${task.service_request_id}`}
                  >
                    {task.description}
                  </Link>
                  {task.due_at ? (
                    <span className="muted">
                      {" "}
                      · {t(lang, "dueAt")} {formatDate(lang, task.due_at)}
                    </span>
                  ) : null}
                </li>
              ))}
              {brief.open_requests.map((r) => (
                <li key={r.id}>
                  <span className="badge neutral">
                    {loopStateShortLabel(lang, r.loop_state)}
                  </span>{" "}
                  <Link className="navlink" href={`/requests/${r.id}`}>
                    {r.display}
                  </Link>
                  <span className="muted">
                    {" "}
                    · {formatDate(lang, r.created_at)}
                  </span>
                </li>
              ))}
            </ul>
          )}
        </section>

        <section aria-labelledby="brief-abnormal">
          <h3 id="brief-abnormal">{t(lang, "recentAbnormal")}</h3>
          <Disclosure
            items={brief.recent_abnormal}
            lang={lang}
            emptyKey="noRecentAbnormal"
            render={(r) => (
              <li key={r.id}>
                <span className={`badge ${r.critical ? "critical" : "warn"}`}>
                  {r.critical ? t(lang, "critical") : t(lang, "abnormalFlag")}{" "}
                  {r.abnormal === "high" ? "↑" : "↓"}
                </span>{" "}
                <Link
                  className="navlink"
                  href={`/requests/${r.service_request_id}`}
                >
                  {r.display}
                </Link>{" "}
                <strong>
                  {r.value} {r.unit}
                </strong>
                {r.reference_range ? (
                  <span className="muted"> ({r.reference_range})</span>
                ) : null}
                <span className="muted">
                  {" "}
                  · {formatDate(lang, r.effective_at)}
                </span>
              </li>
            )}
          />
        </section>

        <section aria-labelledby="brief-vitals">
          <h3 id="brief-vitals">{t(lang, "vitalTrends")}</h3>
          {vitals.length === 0 ? (
            <p className="muted">{t(lang, "noVitals")}</p>
          ) : (
            <div className="table-scroll">
              <table className="vitals-trend">
                <thead>
                  <tr>
                    <th scope="col">{t(lang, "recordedAt")}</th>
                    <th scope="col">{t(lang, "bloodPressure")}</th>
                    <th scope="col">{t(lang, "heartRate")}</th>
                    <th scope="col">{t(lang, "temperature")}</th>
                    <th scope="col">{t(lang, "oxygenSaturation")}</th>
                    <th scope="col">{t(lang, "weight")}</th>
                  </tr>
                </thead>
                <tbody>
                  {vitals.slice(0, 5).map((v) => (
                    <tr key={v.id}>
                      <td>{formatDate(lang, v.recorded_at)}</td>
                      <td>
                        {formatBloodPressure(
                          v.systolic_mmhg,
                          v.diastolic_mmhg,
                        ) ?? "—"}
                      </td>
                      <td>{v.heart_rate_bpm ?? "—"}</td>
                      <td>{v.temperature_c ?? "—"}</td>
                      <td>{v.spo2_percent ?? "—"}</td>
                      <td>{v.weight_kg ?? "—"}</td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </section>
      </div>
      <p className="muted" style={{ fontSize: "0.85rem" }}>
        {t(lang, "supportingReferences")}: {t(lang, "conditions")},{" "}
        {t(lang, "medications")}, {t(lang, "encounters")},{" "}
        {t(lang, "serviceRequests")}, {t(lang, "vitalSigns")}
      </p>
    </details>
  );
}

// ---------------------------------------------------------------------------
// Diagnostic history and trends
// ---------------------------------------------------------------------------

export type DiagnosticResult = {
  id: string;
  service_request_id: string;
  value: string;
  unit: string;
  reference_range: string | null;
  abnormal: "high" | "low" | null;
  critical: boolean;
  status: string;
  superseded: boolean;
  loop_state: string;
  effective_at: string;
  received_at: string;
};

export type DiagnosticTest = {
  code: string;
  display: string;
  unit: string;
  reference_range: string | null;
  results: DiagnosticResult[];
  pending: {
    id: string;
    display: string;
    loop_state: string;
    created_at: string;
  }[];
  pending_count: number;
  latest_value: string | null;
  latest_abnormal: "high" | "low" | null;
  direction: "rising" | "falling" | "stable" | "insufficient";
  result_count: number;
};

export type TrendAnalysis = {
  schema_version: string;
  provider: { provider: string; model: string; model_version: string };
  language: string;
  statements: { code: string; text: string; facts: string[] }[];
  limitations: string[];
};

export type Diagnostics = {
  tests: DiagnosticTest[];
  analysis: TrendAnalysis | null;
  generated_at: string;
};

function directionKey(d: DiagnosticTest["direction"]): TKey {
  switch (d) {
    case "rising":
      return "trendRising";
    case "falling":
      return "trendFalling";
    case "stable":
      return "trendStable";
    default:
      return "trendInsufficient";
  }
}

function directionGlyph(d: DiagnosticTest["direction"]): string {
  return d === "rising"
    ? "↗"
    : d === "falling"
      ? "↘"
      : d === "stable"
        ? "→"
        : "·";
}

export function DiagnosticHistory({
  lang,
  diagnostics,
  defaultOpen = false,
}: {
  lang: Lang;
  diagnostics: Diagnostics;
  defaultOpen?: boolean;
}) {
  const [expanded, setExpanded] = useState<Record<string, boolean>>({});
  const { tests, analysis } = diagnostics;
  return (
    <details className="card diagnostic-history" open={defaultOpen}>
      <summary>
        <h2>{t(lang, "diagnosticHistory")}</h2>
        <span className="muted">
          {tests.length} · {t(lang, "measurements").toLowerCase()}
        </span>
      </summary>
      <p className="muted">{t(lang, "diagnosticHelp")}</p>
      {tests.length === 0 ? (
        <p className="muted">{t(lang, "noDiagnostics")}</p>
      ) : (
        <ul className="diag-groups" aria-label={t(lang, "measurements")}>
          {tests.map((test) => {
            const open = expanded[test.code] ?? false;
            const visible = open ? test.results : test.results.slice(0, 3);
            return (
              <li key={test.code} className="diag-group objective">
                <div className="diag-head">
                  <div className="grow">
                    <strong>{test.display}</strong>{" "}
                    <span className="muted">
                      {test.unit}
                      {test.reference_range
                        ? ` · ${t(lang, "referenceRange")} ${test.reference_range}`
                        : ""}
                    </span>
                  </div>
                  {test.latest_value !== null ? (
                    <span
                      className={`badge ${test.latest_abnormal ? "warn" : "ok"}`}
                    >
                      {t(lang, "latest")} {test.latest_value}
                      {test.latest_abnormal === "high"
                        ? " ↑"
                        : test.latest_abnormal === "low"
                          ? " ↓"
                          : ""}
                    </span>
                  ) : null}
                  <span
                    className="badge neutral trend"
                    title={t(lang, directionKey(test.direction))}
                  >
                    <span aria-hidden="true">
                      {directionGlyph(test.direction)}
                    </span>{" "}
                    {t(lang, directionKey(test.direction))}
                  </span>
                  {test.pending_count > 0 ? (
                    <span className="badge neutral">
                      {test.pending_count} {t(lang, "pendingResults")}
                    </span>
                  ) : null}
                </div>
                <div className="table-scroll">
                  <table className="vitals-trend">
                    <thead>
                      <tr>
                        <th scope="col">{t(lang, "collected")}</th>
                        <th scope="col">{t(lang, "value")}</th>
                        <th scope="col">{t(lang, "referenceRange")}</th>
                        <th scope="col">{t(lang, "status")}</th>
                      </tr>
                    </thead>
                    <tbody>
                      {visible.map((r) => (
                        <tr
                          key={r.id}
                          className={
                            r.superseded ? "superseded-row" : undefined
                          }
                        >
                          <td>{formatDate(lang, r.effective_at)}</td>
                          <td>
                            <Link
                              className="navlink"
                              href={`/requests/${r.service_request_id}`}
                            >
                              {r.value} {r.unit}
                            </Link>
                            {r.abnormal ? (
                              <span
                                className={`badge ${r.critical ? "critical" : "warn"}`}
                                style={{ marginLeft: "0.4rem" }}
                              >
                                {r.critical
                                  ? t(lang, "critical")
                                  : t(lang, "abnormalFlag")}{" "}
                                {r.abnormal === "high" ? "↑" : "↓"}
                              </span>
                            ) : null}
                          </td>
                          <td>{r.reference_range ?? "—"}</td>
                          <td>
                            {r.superseded
                              ? t(lang, "superseded")
                              : loopStateShortLabel(lang, r.loop_state)}
                          </td>
                        </tr>
                      ))}
                      {test.pending.map((p) => (
                        <tr key={p.id} className="pending-row">
                          <td>{formatDate(lang, p.created_at)}</td>
                          <td className="muted">
                            <Link
                              className="navlink"
                              href={`/requests/${p.id}`}
                            >
                              {t(lang, "pendingResults")}
                            </Link>
                          </td>
                          <td>—</td>
                          <td>{loopStateShortLabel(lang, p.loop_state)}</td>
                        </tr>
                      ))}
                    </tbody>
                  </table>
                </div>
                {test.results.length > 3 ? (
                  <button
                    type="button"
                    className="linklike"
                    aria-expanded={open}
                    onClick={() =>
                      setExpanded((e) => ({ ...e, [test.code]: !open }))
                    }
                  >
                    {open
                      ? t(lang, "showLess")
                      : `${t(lang, "showMore")} (${test.results.length - 3})`}
                  </button>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}

      {analysis && analysis.statements.length > 0 ? (
        <section className="ai-section diag-ai" aria-labelledby="diag-ai-title">
          <h3 id="diag-ai-title">
            <span className="badge neutral">{t(lang, "aiAssistiveDraft")}</span>{" "}
            {t(lang, "aiTrendCommentary")}
          </h3>
          <ul>
            {analysis.statements.map((s, i) => (
              <li key={i}>
                {s.text}
                <div className="muted" style={{ fontSize: "0.8rem" }}>
                  {t(lang, "aiFactsUsed")}: {s.facts.join(", ")}
                </div>
              </li>
            ))}
          </ul>
          <p className="muted" style={{ fontSize: "0.85rem" }}>
            {t(lang, "aiModel")}: {analysis.provider.provider}/
            {analysis.provider.model} {analysis.provider.model_version}
          </p>
          {analysis.limitations.length > 0 ? (
            <details>
              <summary>{t(lang, "limitations")}</summary>
              <ul className="muted">
                {analysis.limitations.map((l, i) => (
                  <li key={i}>{l}</li>
                ))}
              </ul>
            </details>
          ) : null}
        </section>
      ) : null}
    </details>
  );
}
