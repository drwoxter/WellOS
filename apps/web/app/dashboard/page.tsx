"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useCallback, useEffect, useMemo, useState } from "react";
import { AppShell } from "../chrome";
import { t } from "@/lib/i18n";
import type { Lang, TKey } from "@/lib/i18n";
import { apiFetch, useSession } from "@/lib/session";
import {
  canActClinically,
  canReadWorklist,
  canRegisterPatients,
  canSearchPatients,
  formatDateTime,
  loopStateShortLabel,
  patientName,
} from "@/lib/clinical";
import {
  availableWidgets,
  defaultConfig,
  loadConfig,
  move,
  saveConfig,
  setDensity,
  toggleHidden,
  visibleWidgets,
} from "@/lib/cockpit";
import type { CockpitConfig, CockpitWidget } from "@/lib/cockpit";
import { canReadVisits } from "@/lib/visits";
import type { InternalAlert, VisitItem } from "@/lib/visits";
import { AlertsPanel } from "../access/alerts-panel";
import { VisitCard, useVisitActions } from "../access/visit-card";

type Summary = {
  critical_open: number;
  awaiting_review: number;
  awaiting_notification: number;
  awaiting_closure: number;
  recently_closed: number;
};

type WorklistItem = {
  id: string;
  display: string;
  code_loinc: string;
  loop_state: string;
  has_open_alert: boolean;
  created_at: string;
  can_open_detail: boolean;
  patient: { family_name: string; given_name: string; identifier: string };
};

type PatientRef = {
  id?: string;
  family_name: string;
  given_name: string;
  identifier: string;
};

type Cockpit = {
  draft_consultations: {
    id: string;
    started_at: string;
    updated_at: string;
    reason: string | null;
    patient: PatientRef & { id: string };
  }[];
  attention: {
    patient: PatientRef & { id: string };
    open_alerts: number;
    open_tasks: number;
    latest_at: string | null;
    open_consultation_id: string | null;
    can_open_chart: boolean;
    can_start_encounter: boolean;
  }[];
  pending_tasks: {
    id: string;
    description: string;
    priority: string;
    status: string;
    due_at: string | null;
    created_at: string;
    service_request_id: string;
    patient: PatientRef;
    can_open_detail: boolean;
  }[];
  ai_activity: {
    id: string;
    artifact_type: string;
    status: string;
    model: string | null;
    model_version: string | null;
    generated_at: string | null;
    reviewed_at: string | null;
    review_decision: string | null;
    encounter_id: string | null;
    service_request_id: string | null;
    patient: PatientRef;
    can_open: boolean;
  }[];
  generated_at: string;
};

type PatientHit = {
  id: string;
  family_name: string;
  given_name: string;
  identifier: string;
  birth_date: string;
  can_open_chart: boolean;
  can_start_encounter: boolean;
  open_consultation_id?: string | null;
};

const WIDGET_TITLE: Record<CockpitWidget, TKey> = {
  ready: "widgetReady",
  alerts: "internalAlerts",
  triage: "widgetTriage",
  access: "widgetAccess",
  drafts: "widgetDrafts",
  attention: "widgetAttention",
  results: "widgetResults",
  tasks: "widgetTasks",
  ai: "widgetAi",
};

function artifactTypeLabel(lang: Lang, type: string): string {
  switch (type) {
    case "scribe_draft":
      return t(lang, "aiScribeDraft");
    case "encounter_summary":
      return t(lang, "aiDocSummary");
    default:
      return t(lang, "aiResultSummary");
  }
}

function artifactStatusLabel(
  lang: Lang,
  status: string,
): { label: string; cls: string } {
  switch (status) {
    case "awaiting_review":
      return { label: t(lang, "awaitingReview"), cls: "warn" };
    case "approved":
      return { label: t(lang, "artifactApproved"), cls: "ok" };
    case "rejected":
      return { label: t(lang, "artifactRejected"), cls: "neutral" };
    case "superseded":
      return { label: t(lang, "artifactSuperseded"), cls: "neutral" };
    default:
      return { label: t(lang, "artifactUnavailable"), cls: "neutral" };
  }
}

function taskStatusBadge(status: string): string {
  return status === "overdue" ? "critical" : "warn";
}

/** Patient picker for the prominent Start-consultation action. Opening a
 *  patient with a consultation already in progress resumes it instead of
 *  creating a second one (`resume: true`, decided server-side). */
function StartConsultation({ lang }: { lang: Lang }) {
  const router = useRouter();
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<PatientHit[] | null>(null);
  const [searched, setSearched] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [opening, setOpening] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function search(e: React.FormEvent) {
    e.preventDefault();
    const q = query.trim();
    if (q.length < 2) return;
    setBusy(true);
    setError(null);
    try {
      const res = await apiFetch<{ patients: PatientHit[] }>(
        `/api/v1/patients?query=${encodeURIComponent(q)}`,
      );
      setHits(res.patients);
      setSearched(q);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  async function open(patientId: string) {
    setOpening(patientId);
    setError(null);
    try {
      const res = await apiFetch<{ id: string }>("/api/v1/encounters", {
        method: "POST",
        body: JSON.stringify({
          patient_id: patientId,
          encounter_type: "consultation",
          resume: true,
        }),
      });
      router.push(`/encounters/${res.id}`);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setOpening(null);
    }
  }

  return (
    <section className="card start-consultation" aria-labelledby="start-h">
      <h2 id="start-h">{t(lang, "startConsultation")}</h2>
      <p className="muted">{t(lang, "startConsultationHelp")}</p>
      <form onSubmit={search} className="picker-form" role="search">
        <label htmlFor="picker-q">{t(lang, "choosePatient")}</label>
        <div className="recording-row">
          <input
            id="picker-q"
            className="grow"
            type="search"
            value={query}
            placeholder={t(lang, "searchPlaceholder")}
            autoComplete="off"
            onChange={(e) => setQuery(e.target.value)}
          />
          <button
            type="submit"
            className="primary"
            disabled={busy || query.trim().length < 2}
          >
            {busy ? t(lang, "loading") : t(lang, "search")}
          </button>
        </div>
      </form>
      {error ? (
        <p role="alert" className="error">
          {error}
        </p>
      ) : null}
      {hits ? (
        hits.length === 0 ? (
          <p className="muted" role="status">
            {t(lang, "noResultsFor")} “{searched}”.
          </p>
        ) : (
          <ul className="result-list picker-results">
            {hits.map((p) => (
              <li key={p.id} className="result-card routine">
                <div className="grow">
                  <div className="title">{patientName(p)}</div>
                  <div className="muted">
                    {p.identifier}
                    {p.open_consultation_id ? (
                      <>
                        {" · "}
                        <span className="badge warn">
                          {t(lang, "resumeHint")}
                        </span>
                      </>
                    ) : null}
                  </div>
                </div>
                {p.can_start_encounter ? (
                  <button
                    type="button"
                    className="primary"
                    disabled={opening !== null}
                    onClick={() => open(p.id)}
                  >
                    {opening === p.id
                      ? t(lang, "opening")
                      : p.open_consultation_id
                        ? t(lang, "resumeConsultation")
                        : t(lang, "startConsultation")}
                  </button>
                ) : p.can_open_chart ? (
                  <Link className="navlink" href={`/patients/${p.id}`}>
                    {t(lang, "openChart")}
                  </Link>
                ) : null}
              </li>
            ))}
          </ul>
        )
      ) : null}
    </section>
  );
}

/** One-line count of where today's patients are in the access flow. */
function FlowStrip({ lang, visits }: { lang: Lang; visits: VisitItem[] }) {
  const count = (...statuses: string[]) =>
    visits.filter((v) => statuses.includes(v.status)).length;
  const steps: { key: TKey; n: number; tone: string }[] = [
    { key: "flowScheduled", n: count("scheduled"), tone: "" },
    { key: "flowWaiting", n: count("arrived"), tone: " warn" },
    { key: "flowInTriage", n: count("triage_in_progress"), tone: "" },
    { key: "flowReady", n: count("ready_for_consultation"), tone: " ok" },
    { key: "flowInConsultation", n: count("in_consultation"), tone: "" },
  ];
  return (
    <section className="card" aria-labelledby="flow-h">
      <h2 id="flow-h">{t(lang, "flowTitle")}</h2>
      <div className="cards-grid">
        {steps.map((s) => (
          <div key={s.key} className={`stat-card${s.n > 0 ? s.tone : ""}`}>
            <span className="num">{s.n}</span>
            <span className="label">{t(lang, s.key)}</span>
          </div>
        ))}
      </div>
    </section>
  );
}

function Customizer({
  lang,
  config,
  available,
  onChange,
  onRestore,
}: {
  lang: Lang;
  config: CockpitConfig;
  available: CockpitWidget[];
  onChange: (c: CockpitConfig) => void;
  onRestore: () => void;
}) {
  const rows = config.order.filter((w) => available.includes(w));
  return (
    <div className="card customizer" aria-labelledby="customize-h">
      <h2 id="customize-h">{t(lang, "customizeDashboard")}</h2>
      <ul className="brief-list">
        {rows.map((w, i) => {
          const hidden = config.hidden.includes(w);
          const title = t(lang, WIDGET_TITLE[w]);
          return (
            <li key={w} className="recording-row">
              <span className="grow">
                {title}
                {hidden ? (
                  <>
                    {" "}
                    <span className="badge neutral">
                      {t(lang, "hideWidget")}
                    </span>
                  </>
                ) : null}
              </span>
              <button
                type="button"
                className="tertiary"
                aria-label={`${t(lang, "moveUp")}: ${title}`}
                disabled={i === 0}
                onClick={() => onChange(move(config, w, -1))}
              >
                ↑
              </button>
              <button
                type="button"
                className="tertiary"
                aria-label={`${t(lang, "moveDown")}: ${title}`}
                disabled={i === rows.length - 1}
                onClick={() => onChange(move(config, w, 1))}
              >
                ↓
              </button>
              <button
                type="button"
                className="secondary"
                aria-pressed={!hidden}
                aria-label={`${hidden ? t(lang, "showWidget") : t(lang, "hideWidget")}: ${title}`}
                onClick={() => onChange(toggleHidden(config, w))}
              >
                {hidden ? t(lang, "showWidget") : t(lang, "hideWidget")}
              </button>
            </li>
          );
        })}
      </ul>
      <fieldset className="density-choice">
        <legend>{t(lang, "density")}</legend>
        <label>
          <input
            type="radio"
            name="density"
            checked={config.density === "compact"}
            onChange={() => onChange(setDensity(config, "compact"))}
          />{" "}
          {t(lang, "densityCompact")}
        </label>
        <label>
          <input
            type="radio"
            name="density"
            checked={config.density === "expanded"}
            onChange={() => onChange(setDensity(config, "expanded"))}
          />{" "}
          {t(lang, "densityExpanded")}
        </label>
      </fieldset>
      <p className="recording-row">
        <button type="button" className="secondary" onClick={onRestore}>
          {t(lang, "restoreDefaults")}
        </button>
        <span className="muted">{t(lang, "layoutStoredLocally")}</span>
      </p>
    </div>
  );
}

function DashboardContent() {
  const { lang, authenticated, meta, metaError, reloadMeta } = useSession();
  const [summary, setSummary] = useState<Summary | null>(null);
  const [items, setItems] = useState<WorklistItem[] | null>(null);
  const [cockpit, setCockpit] = useState<Cockpit | null>(null);
  const [visits, setVisits] = useState<VisitItem[] | null>(null);
  const [alerts, setAlerts] = useState<InternalAlert[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [config, setConfig] = useState<CockpitConfig | null>(null);
  const [customizing, setCustomizing] = useState(false);

  const roles = meta?.user.roles ?? null;
  const worklistUser = roles ? canReadWorklist(roles) : false;
  const visitUser = roles ? canReadVisits(roles) : false;
  const clinician = meta ? canActClinically(meta.facilities) : false;
  const rolesKey = roles?.join(",") ?? "";

  useEffect(() => {
    if (!roles) return;
    setConfig(
      loadConfig(
        typeof window === "undefined" ? null : window.localStorage,
        roles,
      ),
    );
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rolesKey]);

  const updateConfig = useCallback((c: CockpitConfig) => {
    setConfig(c);
    saveConfig(typeof window === "undefined" ? null : window.localStorage, c);
  }, []);

  const load = useCallback(async () => {
    setError(null);
    try {
      const [work, board] = await Promise.all([
        worklistUser
          ? Promise.all([
              apiFetch<Summary>("/api/v1/worklist/summary"),
              apiFetch<{ items: WorklistItem[] }>("/api/v1/worklist"),
              apiFetch<Cockpit>("/api/v1/dashboard/cockpit"),
            ])
          : Promise.resolve(null),
        visitUser
          ? Promise.all([
              apiFetch<{ items: VisitItem[] }>("/api/v1/visits?view=access"),
              apiFetch<{ items: InternalAlert[] }>("/api/v1/alerts").catch(
                () => ({ items: [] as InternalAlert[] }),
              ),
            ])
          : Promise.resolve(null),
      ]);
      if (work) {
        setSummary(work[0]);
        setItems(work[1].items);
        setCockpit(work[2]);
      }
      if (board) {
        setVisits(board[0].items);
        setAlerts(board[1].items);
      }
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    }
  }, [worklistUser, visitUser]);

  useEffect(() => {
    if (authenticated && (worklistUser || visitUser)) void load();
  }, [authenticated, worklistUser, visitUser, load]);

  const visitActions = useVisitActions(lang, load);

  const available = useMemo(
    () => availableWidgets(visitUser, worklistUser),
    [visitUser, worklistUser],
  );
  const visible = useMemo(
    () => (config ? visibleWidgets(config, available) : []),
    [config, available],
  );

  if (error) {
    return (
      <div className="card">
        <p role="alert" className="error">
          {error}
        </p>
        <button className="secondary" onClick={load}>
          {t(lang, "retry")}
        </button>
      </div>
    );
  }
  if (!roles && metaError) {
    return (
      <div className="card">
        <p role="alert" className="error">
          {t(lang, "contextLoadFailed")}
        </p>
        <button className="secondary" onClick={reloadMeta}>
          {t(lang, "retry")}
        </button>
      </div>
    );
  }
  if (
    !roles ||
    !config ||
    (worklistUser && (!summary || !items || !cockpit)) ||
    (visitUser && (!visits || !alerts))
  ) {
    return (
      <p className="muted" role="status">
        {t(lang, "loading")}
      </p>
    );
  }

  const quickActions: { href: string; key: TKey }[] = [];
  if (canSearchPatients(roles)) {
    quickActions.push({ href: "/patients", key: "actionFindPatient" });
  }
  if (meta && canRegisterPatients(meta.facilities)) {
    quickActions.push({
      href: "/patients#register",
      key: "actionRegisterPatient",
    });
  }
  if (clinician) {
    quickActions.push({ href: "/patients", key: "actionOrderLab" });
  }
  if (visitUser) {
    quickActions.push({ href: "/access", key: "navAccess" });
  }

  const compact = config.density === "compact";
  const limit = compact ? 3 : 6;
  const priority = items?.slice(0, limit) ?? [];

  function visitWidget(w: "ready" | "triage" | "access") {
    if (!visits) return null;
    const title = t(lang, WIDGET_TITLE[w]);
    const statuses: string[] =
      w === "ready"
        ? ["ready_for_consultation", "in_consultation"]
        : w === "triage"
          ? ["arrived", "triage_in_progress"]
          : ["scheduled", "arrived"];
    const rows = visits.filter((v) => statuses.includes(v.status));
    const empty: TKey =
      w === "ready"
        ? "noReadyPatients"
        : w === "triage"
          ? "noTriageVisits"
          : "noArrivals";
    return (
      <section className="card widget" aria-labelledby={`w-${w}`}>
        <h2 id={`w-${w}`}>
          {title}{" "}
          {rows.length > 0 ? (
            <span className={`badge ${w === "access" ? "neutral" : "warn"}`}>
              {rows.length}
            </span>
          ) : null}
        </h2>
        {visitActions.message ? (
          <p
            role={visitActions.message.kind === "error" ? "alert" : "status"}
            className={
              visitActions.message.kind === "error" ? "error" : "success"
            }
          >
            {visitActions.message.text}
          </p>
        ) : null}
        {rows.length === 0 ? (
          <p className="muted">{t(lang, empty)}</p>
        ) : (
          <ul className="result-list">
            {rows.slice(0, limit).map((v) => (
              <VisitCard
                key={v.id}
                lang={lang}
                visit={v}
                busy={visitActions.busy}
                onAction={(visit, action) =>
                  void visitActions.run(visit, action)
                }
                showFacility={(meta?.facilities.length ?? 0) > 1}
                compact={compact}
              />
            ))}
          </ul>
        )}
        <p style={{ marginBottom: 0 }}>
          <Link href="/access">{t(lang, "openAccessBoard")}</Link>
        </p>
      </section>
    );
  }

  function renderWidget(w: CockpitWidget) {
    const title = t(lang, WIDGET_TITLE[w]);
    switch (w) {
      case "ready":
      case "triage":
      case "access":
        return visitWidget(w);
      case "alerts":
        return alerts ? (
          <AlertsPanel
            lang={lang}
            alerts={alerts}
            onAcknowledged={load}
            limit={limit}
            headingId={`w-${w}`}
            className="widget"
          />
        ) : null;
    }
    if (!cockpit) return null;
    switch (w) {
      case "drafts": {
        const rows = cockpit.draft_consultations.slice(0, limit);
        return (
          <section className="card widget" aria-labelledby={`w-${w}`}>
            <h2 id={`w-${w}`}>{title}</h2>
            {rows.length === 0 ? (
              <p className="muted">{t(lang, "noDraftConsultations")}</p>
            ) : (
              <ul className="result-list">
                {rows.map((d) => (
                  <li key={d.id} className="result-card routine">
                    <div className="grow">
                      <div className="title">{patientName(d.patient)}</div>
                      <div className="muted">
                        {d.reason ?? t(lang, "draftBadge")} ·{" "}
                        {t(lang, "lastUpdated")}{" "}
                        {formatDateTime(lang, d.updated_at)}
                      </div>
                    </div>
                    <Link className="navlink" href={`/encounters/${d.id}`}>
                      {t(lang, "resumeConsultation")}
                    </Link>
                  </li>
                ))}
              </ul>
            )}
          </section>
        );
      }
      case "attention": {
        const rows = cockpit.attention.slice(0, limit);
        return (
          <section className="card widget" aria-labelledby={`w-${w}`}>
            <h2 id={`w-${w}`}>{title}</h2>
            {rows.length === 0 ? (
              <p className="muted">{t(lang, "noAttention")}</p>
            ) : (
              <ul className="result-list">
                {rows.map((a) => (
                  <li
                    key={a.patient.id}
                    className={`result-card ${a.open_alerts > 0 ? "critical" : "routine"}`}
                  >
                    <div className="grow">
                      <div className="title">{patientName(a.patient)}</div>
                      <div className="muted">
                        {a.patient.identifier} · {a.open_alerts}{" "}
                        {t(lang, "openAlertsCount")} · {a.open_tasks}{" "}
                        {t(lang, "openTasksCount")}
                      </div>
                    </div>
                    {a.open_consultation_id ? (
                      <Link
                        className="navlink"
                        href={`/encounters/${a.open_consultation_id}`}
                      >
                        {t(lang, "resumeConsultation")}
                      </Link>
                    ) : a.can_open_chart ? (
                      <Link
                        className="navlink"
                        href={`/patients/${a.patient.id}`}
                      >
                        {t(lang, "openChart")}
                      </Link>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
          </section>
        );
      }
      case "results":
        return (
          <section className="card widget" aria-labelledby={`w-${w}`}>
            <h2 id={`w-${w}`}>{title}</h2>
            {summary ? (
              <div className="cards-grid">
                <div
                  className={`stat-card${summary.critical_open > 0 ? " critical" : " ok"}`}
                >
                  <span className="num">{summary.critical_open}</span>
                  <span className="label">{t(lang, "criticalOpen")}</span>
                </div>
                <div
                  className={`stat-card${summary.awaiting_review > 0 ? " warn" : ""}`}
                >
                  <span className="num">{summary.awaiting_review}</span>
                  <span className="label">{t(lang, "awaitingReview")}</span>
                </div>
                {!compact ? (
                  <>
                    <div className="stat-card">
                      <span className="num">
                        {summary.awaiting_notification}
                      </span>
                      <span className="label">
                        {t(lang, "awaitingNotification")}
                      </span>
                    </div>
                    <div className="stat-card">
                      <span className="num">{summary.awaiting_closure}</span>
                      <span className="label">
                        {t(lang, "awaitingClosure")}
                      </span>
                    </div>
                    <div className="stat-card ok">
                      <span className="num">{summary.recently_closed}</span>
                      <span className="label">{t(lang, "recentlyClosed")}</span>
                    </div>
                  </>
                ) : null}
              </div>
            ) : null}
            <h3>{t(lang, "priorityResults")}</h3>
            {priority.length === 0 ? (
              <p className="muted">{t(lang, "noPendingResults")}</p>
            ) : (
              <ul className="result-list">
                {priority.map((item) => (
                  <li
                    key={item.id}
                    className={`result-card ${item.has_open_alert ? "critical" : "routine"}`}
                  >
                    <div className="grow">
                      <div className="title">{patientName(item.patient)}</div>
                      <div className="muted">
                        {item.display} · {item.patient.identifier} ·{" "}
                        {formatDateTime(lang, item.created_at)}
                      </div>
                    </div>
                    {item.has_open_alert ? (
                      <span className="badge critical">
                        {t(lang, "critical")}
                      </span>
                    ) : null}
                    <span className="badge neutral">
                      {loopStateShortLabel(lang, item.loop_state)}
                    </span>
                    {item.can_open_detail ? (
                      <Link className="navlink" href={`/requests/${item.id}`}>
                        {t(lang, "openResult")}
                      </Link>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
            <p style={{ marginBottom: 0 }}>
              <Link href="/results">{t(lang, "viewAllResults")}</Link>
            </p>
          </section>
        );
      case "tasks": {
        const rows = cockpit.pending_tasks.slice(0, limit);
        return (
          <section className="card widget" aria-labelledby={`w-${w}`}>
            <h2 id={`w-${w}`}>{title}</h2>
            {rows.length === 0 ? (
              <p className="muted">{t(lang, "noPendingTasks")}</p>
            ) : (
              <ul className="result-list">
                {rows.map((task) => (
                  <li
                    key={task.id}
                    className={`result-card ${task.status === "overdue" ? "critical" : "routine"}`}
                  >
                    <div className="grow">
                      <div className="title">{task.description}</div>
                      <div className="muted">
                        {patientName(task.patient)} · {task.patient.identifier}
                        {task.due_at
                          ? ` · ${formatDateTime(lang, task.due_at)}`
                          : ""}
                      </div>
                    </div>
                    <span className={`badge ${taskStatusBadge(task.status)}`}>
                      {task.status === "overdue"
                        ? t(lang, "overdue")
                        : t(lang, "open")}
                    </span>
                    {task.can_open_detail ? (
                      <Link
                        className="navlink"
                        href={`/requests/${task.service_request_id}`}
                      >
                        {t(lang, "openResult")}
                      </Link>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
          </section>
        );
      }
      case "ai": {
        const rows = cockpit.ai_activity.slice(0, limit);
        return (
          <section className="card widget" aria-labelledby={`w-${w}`}>
            <h2 id={`w-${w}`}>{title}</h2>
            {rows.length === 0 ? (
              <p className="muted">{t(lang, "noAiActivity")}</p>
            ) : (
              <ul className="result-list">
                {rows.map((a) => {
                  const st = artifactStatusLabel(lang, a.status);
                  const href = a.encounter_id
                    ? `/encounters/${a.encounter_id}`
                    : a.service_request_id
                      ? `/requests/${a.service_request_id}`
                      : null;
                  return (
                    <li key={a.id} className="result-card routine">
                      <div className="grow">
                        <div className="title">
                          {artifactTypeLabel(lang, a.artifact_type)}
                        </div>
                        <div className="muted">
                          {patientName(a.patient)}
                          {a.model
                            ? ` · ${a.model}${a.model_version ? ` ${a.model_version}` : ""}`
                            : ""}
                          {a.generated_at
                            ? ` · ${formatDateTime(lang, a.generated_at)}`
                            : ""}
                        </div>
                      </div>
                      <span className={`badge ${st.cls}`}>{st.label}</span>
                      {href && a.can_open ? (
                        <Link className="navlink" href={href}>
                          {t(lang, "open")}
                        </Link>
                      ) : null}
                    </li>
                  );
                })}
              </ul>
            )}
          </section>
        );
      }
    }
  }

  return (
    <>
      <h2 style={{ marginTop: 0 }}>
        {t(lang, "welcome")}
        {meta ? `, ${meta.user.display_name}` : ""}
      </h2>
      <p className="muted">{t(lang, "dashboardIntro")}</p>

      {clinician ? <StartConsultation lang={lang} /> : null}

      {!worklistUser && !visitUser ? (
        <div className="card">
          <p className="muted" style={{ margin: 0 }}>
            {t(lang, "noWorklistAccess")}
          </p>
        </div>
      ) : null}

      <div className="card">
        <div className="recording-dock-head">
          <h2 style={{ margin: 0 }}>{t(lang, "quickActions")}</h2>
          {worklistUser || visitUser ? (
            <button
              type="button"
              className="secondary"
              aria-expanded={customizing}
              aria-controls="customizer"
              onClick={() => setCustomizing((c) => !c)}
            >
              {customizing
                ? t(lang, "doneCustomizing")
                : t(lang, "customizeDashboard")}
            </button>
          ) : null}
        </div>
        <div className="quick-actions">
          {quickActions.map((a) => (
            <Link key={a.key} href={a.href}>
              {t(lang, a.key)}
            </Link>
          ))}
        </div>
      </div>

      {visits ? <FlowStrip lang={lang} visits={visits} /> : null}

      {customizing && (worklistUser || visitUser) ? (
        <div id="customizer">
          <Customizer
            lang={lang}
            config={config}
            available={available}
            onChange={updateConfig}
            onRestore={() => updateConfig(defaultConfig(roles ?? []))}
          />
        </div>
      ) : null}

      {worklistUser || visitUser ? (
        visible.length === 0 ? (
          <p className="muted" role="status">
            {t(lang, "allWidgetsHidden")}
          </p>
        ) : (
          <div className={`cockpit ${config.density}`}>
            {visible.map((w) => (
              <div key={w}>{renderWidget(w)}</div>
            ))}
          </div>
        )
      ) : null}
    </>
  );
}

export default function DashboardPage() {
  return (
    <AppShell>
      <DashboardContent />
    </AppShell>
  );
}
