"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useCallback, useEffect, useMemo, useState } from "react";
import { AppShell } from "../chrome";
import { t } from "@/lib/i18n";
import type { Lang, TKey } from "@/lib/i18n";
import { ApiRequestError, apiFetch, useSession } from "@/lib/session";
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
  clearLegacyLocalConfig,
  defaultConfig,
  loadPreferences,
  move,
  moveBefore,
  parseLayout,
  readLegacyLocalConfig,
  sameConfig,
  savePreferences,
  setDensity,
  setSize,
  sizeOf,
  toggleHidden,
  topPriorities,
  visibleWidgets,
} from "@/lib/cockpit";
import type { CockpitConfig, CockpitWidget, Priority } from "@/lib/cockpit";
import { greetingKey } from "@/lib/home";
import { canReadVisits, visitStatusLabel } from "@/lib/visits";
import type { InternalAlert, VisitItem } from "@/lib/visits";
import { Icon } from "@/components/ui/icons";
import { EmptyState, Pill, StatTile } from "@/components/ui/primitives";
import type { Tone } from "@/components/ui/primitives";
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
    service_request_id: string | null;
    patient_id: string;
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

const PRIORITY_LABEL: Record<Priority["key"], TKey> = {
  critical: "prioCritical",
  alerts: "prioAlerts",
  ready: "prioReady",
  triage: "prioTriage",
  overdue: "prioOverdue",
  review: "prioReview",
  attention: "prioAttention",
  drafts: "prioDrafts",
};

/** The three things that matter most right now, readable without scrolling.
 *  Counts come straight from the worklist, cockpit, visit and alert feeds the
 *  role may read; nothing is estimated. */
function PriorityStrip({
  lang,
  priorities,
}: {
  lang: Lang;
  priorities: Priority[];
}) {
  return (
    <section className="priority-strip" aria-labelledby="prio-h">
      <h2 id="prio-h" className="sr-only">
        {t(lang, "prioritiesTitle")}
      </h2>
      {priorities.length === 0 ? (
        <div className="priority-tile all-clear" role="status">
          <span className="priority-icon" aria-hidden="true">
            <Icon.Check />
          </span>
          <span className="priority-body">
            <strong>{t(lang, "prioAllClear")}</strong>
            <span className="muted">{t(lang, "prioAllClearHelp")}</span>
          </span>
        </div>
      ) : (
        <ol className="priority-list">
          {priorities.map((p, i) => (
            <li key={p.key}>
              <a
                className={`priority-tile ${p.tone}`}
                href={p.href}
                data-testid={`priority-${p.key}`}
              >
                <span className="priority-rank" aria-hidden="true">
                  {i + 1}
                </span>
                <span className="priority-count">{p.count}</span>
                <span className="priority-body">
                  <strong>{t(lang, PRIORITY_LABEL[p.key])}</strong>
                </span>
                <span className="priority-go" aria-hidden="true">
                  <Icon.ArrowRight />
                </span>
              </a>
            </li>
          ))}
        </ol>
      )}
    </section>
  );
}

const ARRIVAL_TONE: Record<string, Tone> = {
  urgent: "critical",
  walk_in: "warn",
  scheduled: "neutral",
  remote: "teal",
};
const ARRIVAL_SHORT: Record<string, TKey> = {
  urgent: "arrivalUrgentShort",
  walk_in: "arrivalWalkInShort",
  scheduled: "arrivalScheduledShort",
  remote: "arrivalRemoteShort",
};
const EXPECTED_STATUSES: readonly string[] = [
  "in_consultation",
  "ready_for_consultation",
  "triage_in_progress",
  "arrived",
  "scheduled",
];

/** Everyone expected today, urgent and ready first, with the arrival kind
 *  spelled out so scheduled, urgent and walk-in patients are never confused. */
function ExpectedToday({
  lang,
  visits,
  limit,
}: {
  lang: Lang;
  visits: VisitItem[];
  limit: number;
}) {
  const rows = visits
    .filter((v) => EXPECTED_STATUSES.includes(v.status))
    .sort((a, b) => {
      const urgency = (v: VisitItem) =>
        (v.arrival_kind === "urgent" ? 0 : 2) +
        (v.status === "ready_for_consultation" ? 0 : 1);
      const ua = urgency(a);
      const ub = urgency(b);
      if (ua !== ub) return ua - ub;
      return (a.scheduled_at ?? a.arrived_at ?? "").localeCompare(
        b.scheduled_at ?? b.arrived_at ?? "",
      );
    });
  return (
    <section className="card expected-today" aria-labelledby="expected-h">
      <div className="card-head">
        <h2 id="expected-h">
          <Icon.Calendar /> {t(lang, "expectedToday")}{" "}
          {rows.length > 0 ? (
            <span className="badge neutral">{rows.length}</span>
          ) : null}
        </h2>
        <Link href="/access" className="navlink">
          {t(lang, "openAccessBoard")}
        </Link>
      </div>
      {rows.length === 0 ? (
        <EmptyState inline title={t(lang, "noExpectedToday")} />
      ) : (
        <ul className="expected-list">
          {rows.slice(0, limit).map((v) => (
            <li key={v.id} className={`expected-row kind-${v.arrival_kind}`}>
              <span className="expected-when">
                {v.scheduled_at
                  ? formatDateTime(lang, v.scheduled_at).split(" ").pop()
                  : v.wait_minutes !== null
                    ? `${v.wait_minutes} min`
                    : "—"}
              </span>
              <span className="expected-who">
                <strong>{patientName(v.patient)}</strong>
                <span className="muted">
                  {v.reason ?? v.service} · {visitStatusLabel(lang, v.status)}
                </span>
              </span>
              <Pill
                tone={ARRIVAL_TONE[v.arrival_kind] ?? "neutral"}
                icon={v.arrival_kind === "urgent"}
              >
                {t(
                  lang,
                  ARRIVAL_SHORT[v.arrival_kind] ?? "arrivalScheduledShort",
                )}
              </Pill>
              {v.encounter_id && v.capabilities.can_resume_consultation ? (
                <Link
                  className="navlink"
                  href={`/encounters/${v.encounter_id}`}
                >
                  {t(lang, "resumeConsultation")}
                </Link>
              ) : null}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}

/** One-line count of where today's patients are in the access flow. */
function FlowStrip({ lang, visits }: { lang: Lang; visits: VisitItem[] }) {
  const count = (...statuses: string[]) =>
    visits.filter((v) => statuses.includes(v.status)).length;
  const steps: { key: TKey; n: number; tone: Tone }[] = [
    { key: "flowScheduled", n: count("scheduled"), tone: "neutral" },
    { key: "flowWaiting", n: count("arrived"), tone: "warn" },
    { key: "flowInTriage", n: count("triage_in_progress"), tone: "neutral" },
    { key: "flowReady", n: count("ready_for_consultation"), tone: "ok" },
    { key: "flowInConsultation", n: count("in_consultation"), tone: "teal" },
  ];
  return (
    <section className="card flow-strip" aria-labelledby="flow-h">
      <h2 id="flow-h">{t(lang, "flowTitle")}</h2>
      <div className="flow-tiles">
        {steps.map((s) => (
          <StatTile
            key={s.key}
            value={s.n}
            label={t(lang, s.key)}
            tone={s.n > 0 ? s.tone : "neutral"}
          />
        ))}
      </div>
    </section>
  );
}

type SaveStatus =
  | { kind: "idle" }
  | { kind: "saving" }
  | { kind: "saved" }
  | { kind: "conflict" }
  | { kind: "failed" };

/** Edit-mode panel: hidden widgets, density, restore, save and cancel. The
 *  layout on screen is the live preview; nothing is stored until Save. */
function EditPanel({
  lang,
  draft,
  available,
  dirty,
  status,
  onChange,
  onRestore,
  onSave,
  onCancel,
}: {
  lang: Lang;
  draft: CockpitConfig;
  available: CockpitWidget[];
  dirty: boolean;
  status: SaveStatus;
  onChange: (c: CockpitConfig) => void;
  onRestore: () => void;
  onSave: () => void;
  onCancel: () => void;
}) {
  const hidden = draft.order.filter(
    (w) => available.includes(w) && draft.hidden.includes(w),
  );
  return (
    <div
      className="card edit-panel tone-teal"
      id="customizer"
      aria-labelledby="customize-h"
      role="region"
    >
      <div className="card-head">
        <h2 id="customize-h">
          <Icon.Settings /> {t(lang, "customizeDashboard")}
        </h2>
        <p className="muted edit-hint" role="status">
          {status.kind === "saving"
            ? t(lang, "loading")
            : status.kind === "failed"
              ? t(lang, "cockpitSaveFailed")
              : dirty
                ? t(lang, "cockpitPreview")
                : t(lang, "layoutStoredLocally")}
        </p>
      </div>
      <div className="edit-panel-body">
        <div>
          <h3>{t(lang, "cockpitHidden")}</h3>
          {hidden.length === 0 ? (
            <p className="muted">{t(lang, "cockpitNoHidden")}</p>
          ) : (
            <div className="chip-group">
              {hidden.map((w) => {
                const title = t(lang, WIDGET_TITLE[w]);
                return (
                  <button
                    key={w}
                    type="button"
                    className="chip"
                    aria-label={`${t(lang, "showWidget")}: ${title}`}
                    onClick={() => onChange(toggleHidden(draft, w))}
                  >
                    <Icon.Eye /> {title}
                  </button>
                );
              })}
            </div>
          )}
        </div>
        <fieldset className="density-choice">
          <legend>{t(lang, "density")}</legend>
          <label>
            <input
              type="radio"
              name="density"
              checked={draft.density === "compact"}
              onChange={() => onChange(setDensity(draft, "compact"))}
            />{" "}
            {t(lang, "densityCompact")}
          </label>
          <label>
            <input
              type="radio"
              name="density"
              checked={draft.density === "expanded"}
              onChange={() => onChange(setDensity(draft, "expanded"))}
            />{" "}
            {t(lang, "densityExpanded")}
          </label>
        </fieldset>
      </div>
      <div className="edit-actions">
        <button type="button" className="tertiary" onClick={onRestore}>
          {t(lang, "restoreDefaults")}
        </button>
        <span className="grow" />
        <button type="button" className="secondary" onClick={onCancel}>
          {t(lang, "cockpitCancel")}
        </button>
        <button
          type="button"
          className="primary"
          disabled={status.kind === "saving"}
          onClick={onSave}
        >
          {t(lang, "cockpitSave")}
        </button>
      </div>
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
  const [version, setVersion] = useState(0);
  const [draft, setDraft] = useState<CockpitConfig | null>(null);
  const [saveStatus, setSaveStatus] = useState<SaveStatus>({ kind: "idle" });
  const [dragging, setDragging] = useState<CockpitWidget | null>(null);
  const [dropTarget, setDropTarget] = useState<CockpitWidget | null>(null);

  const roles = meta?.user.roles ?? null;
  const worklistUser = roles ? canReadWorklist(roles) : false;
  const visitUser = roles ? canReadVisits(roles) : false;
  const clinician = meta ? canActClinically(meta.facilities) : false;
  const hasCockpit = worklistUser || visitUser;
  const rolesKey = roles?.join(",") ?? "";

  // Server is the source of truth for the layout. A valid legacy
  // browser-only layout is migrated once (first load with nothing stored)
  // and the browser copy is dropped afterwards.
  useEffect(() => {
    if (!roles || !authenticated) return;
    const fallback = defaultConfig(roles);
    if (!hasCockpit) {
      setConfig(fallback);
      return;
    }
    let cancelled = false;
    const storage = typeof window === "undefined" ? null : window.localStorage;
    (async () => {
      try {
        const prefs = await loadPreferences();
        if (cancelled) return;
        if (prefs.layout) {
          setConfig(parseLayout(prefs.layout, fallback));
          setVersion(prefs.version ?? 0);
        } else {
          const legacy = readLegacyLocalConfig(storage);
          if (legacy) {
            try {
              const saved = await savePreferences(legacy, 0);
              if (cancelled) return;
              setConfig(parseLayout(saved.layout, fallback));
              setVersion(saved.version ?? 1);
            } catch {
              if (cancelled) return;
              setConfig(legacy);
              setVersion(0);
            }
          } else {
            setConfig(fallback);
            setVersion(prefs.version ?? 0);
          }
        }
        clearLegacyLocalConfig(storage);
      } catch {
        if (!cancelled) {
          setConfig(fallback);
          setVersion(0);
        }
      }
    })();
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [rolesKey, authenticated, hasCockpit]);

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
    if (authenticated && hasCockpit) void load();
  }, [authenticated, hasCockpit, load]);

  const visitActions = useVisitActions(lang, load);

  const available = useMemo(
    () => availableWidgets(visitUser, worklistUser),
    [visitUser, worklistUser],
  );
  const shown = draft ?? config;
  const visible = useMemo(
    () => (shown ? visibleWidgets(shown, available) : []),
    [shown, available],
  );
  const editing = draft !== null;

  const reloadLayout = useCallback(async () => {
    if (!roles) return;
    const fallback = defaultConfig(roles);
    const prefs = await loadPreferences();
    setConfig(prefs.layout ? parseLayout(prefs.layout, fallback) : fallback);
    setVersion(prefs.version ?? 0);
  }, [roles]);

  const save = useCallback(async () => {
    if (!draft || !config) return;
    if (sameConfig(draft, config)) {
      setDraft(null);
      setSaveStatus({ kind: "idle" });
      return;
    }
    setSaveStatus({ kind: "saving" });
    try {
      const saved = await savePreferences(draft, version);
      setConfig(parseLayout(saved.layout, draft));
      setVersion(saved.version);
      setDraft(null);
      setSaveStatus({ kind: "saved" });
    } catch (e) {
      if (e instanceof ApiRequestError && e.status === 409) {
        try {
          await reloadLayout();
        } catch {
          // Keep whatever we have; the user can retry.
        }
        setDraft(null);
        setSaveStatus({ kind: "conflict" });
      } else {
        setSaveStatus({ kind: "failed" });
      }
    }
  }, [draft, config, version, reloadLayout]);

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
    !shown ||
    (worklistUser && (!summary || !items || !cockpit)) ||
    (visitUser && (!visits || !alerts))
  ) {
    return (
      <div className="cockpit-skeleton" role="status" aria-live="polite">
        <span className="sr-only">{t(lang, "loading")}</span>
        <div className="skeleton" style={{ height: "4.5rem" }} />
        <div className="skeleton" style={{ height: "10rem" }} />
        <div className="skeleton" style={{ height: "14rem" }} />
      </div>
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

  const compact = shown.density === "compact";
  const limit = compact ? 3 : 6;
  const priority = items?.slice(0, limit) ?? [];

  const priorities = topPriorities({
    criticalOpen: summary?.critical_open,
    awaitingReview: summary?.awaiting_review,
    ready: visits?.filter(
      (v) =>
        v.status === "ready_for_consultation" &&
        (v.capabilities.can_start_consultation ||
          v.capabilities.can_resume_consultation),
    ).length,
    waitingTriage: visits?.filter(
      (v) => v.status === "arrived" && v.capabilities.can_triage,
    ).length,
    highAlerts: alerts?.filter(
      (a) =>
        a.status === "open" &&
        (a.priority === "high" || a.priority === "critical"),
    ).length,
    overdueTasks: cockpit?.pending_tasks.filter((x) => x.status === "overdue")
      .length,
    attention: cockpit?.attention.length,
    drafts: cockpit?.draft_consultations.length,
  });

  const hour = new Date().getHours();
  const facilityName =
    meta && meta.facilities.length === 1 ? meta.facilities[0].name : null;

  function updateDraft(c: CockpitConfig) {
    setDraft(c);
    if (saveStatus.kind !== "saving") setSaveStatus({ kind: "idle" });
  }
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
                        href={
                          task.service_request_id
                            ? `/requests/${task.service_request_id}`
                            : `/patients/${task.patient_id}/360`
                        }
                      >
                        {task.service_request_id
                          ? t(lang, "openResult")
                          : t(lang, "openPatient360")}
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

  const editable = shown.order.filter(
    (w) => available.includes(w) && !shown.hidden.includes(w),
  );

  function cell(w: CockpitWidget) {
    const title = t(lang, WIDGET_TITLE[w]);
    const size = sizeOf(shown!, w);
    const idx = editable.indexOf(w);
    return (
      <div
        key={w}
        className={[
          "cockpit-cell",
          `size-${size}`,
          editing ? "editing" : "",
          dragging === w ? "dragging" : "",
          dropTarget === w && dragging !== w ? "drop-target" : "",
        ]
          .filter(Boolean)
          .join(" ")}
        draggable={editing || undefined}
        onDragStart={
          editing
            ? (e) => {
                e.dataTransfer.setData("text/plain", w);
                e.dataTransfer.effectAllowed = "move";
                setDragging(w);
              }
            : undefined
        }
        onDragOver={
          editing
            ? (e) => {
                e.preventDefault();
                e.dataTransfer.dropEffect = "move";
                if (dropTarget !== w) setDropTarget(w);
              }
            : undefined
        }
        onDragLeave={
          editing
            ? () => {
                if (dropTarget === w) setDropTarget(null);
              }
            : undefined
        }
        onDrop={
          editing
            ? (e) => {
                e.preventDefault();
                const from =
                  (e.dataTransfer.getData("text/plain") as CockpitWidget) ||
                  dragging;
                if (from && from !== w && draft) {
                  updateDraft(moveBefore(draft, from, w));
                }
                setDragging(null);
                setDropTarget(null);
              }
            : undefined
        }
        onDragEnd={
          editing
            ? () => {
                setDragging(null);
                setDropTarget(null);
              }
            : undefined
        }
      >
        {editing && draft ? (
          <div className="cell-tools" role="group" aria-label={title}>
            <span className="drag-handle" title={t(lang, "dragToReorder")}>
              <Icon.Drag />
              <span className="sr-only">{t(lang, "dragToReorder")}</span>
            </span>
            <span className="cell-title">{title}</span>
            <button
              type="button"
              className="tertiary icon-button"
              aria-label={`${t(lang, "moveUp")}: ${title}`}
              disabled={idx <= 0}
              onClick={() => updateDraft(move(draft, w, -1))}
            >
              ↑
            </button>
            <button
              type="button"
              className="tertiary icon-button"
              aria-label={`${t(lang, "moveDown")}: ${title}`}
              disabled={idx < 0 || idx === editable.length - 1}
              onClick={() => updateDraft(move(draft, w, 1))}
            >
              ↓
            </button>
            <button
              type="button"
              className="tertiary icon-button"
              aria-pressed={size === "full"}
              aria-label={`${t(lang, "widgetSize")}: ${title} — ${
                size === "full" ? t(lang, "sizeFull") : t(lang, "sizeHalf")
              }`}
              onClick={() =>
                updateDraft(
                  setSize(draft, w, size === "full" ? "half" : "full"),
                )
              }
            >
              {size === "full" ? <Icon.Collapse /> : <Icon.Expand />}
            </button>
            <button
              type="button"
              className="tertiary icon-button"
              aria-label={`${t(lang, "hideWidget")}: ${title}`}
              onClick={() => updateDraft(toggleHidden(draft, w))}
            >
              <Icon.EyeOff />
            </button>
          </div>
        ) : null}
        {renderWidget(w)}
      </div>
    );
  }

  return (
    <div className={`cockpit-page${editing ? " editing" : ""}`}>
      <header className="cockpit-hero">
        <div className="cockpit-hero-text">
          <p className="eyebrow">
            {new Date().toLocaleDateString(lang, {
              weekday: "long",
              day: "numeric",
              month: "long",
            })}
            {facilityName ? ` · ${facilityName}` : ""}
          </p>
          <h1>
            {t(lang, greetingKey(hour))}
            {meta ? `, ${meta.user.display_name}` : ""}
          </h1>
        </div>
        <div className="cockpit-hero-actions">
          {quickActions.map((a) => (
            <Link key={a.key} href={a.href} className="chip">
              {t(lang, a.key)}
            </Link>
          ))}
          {hasCockpit ? (
            editing ? null : (
              <button
                type="button"
                className="secondary"
                aria-expanded={editing}
                aria-controls="customizer"
                onClick={() => {
                  setDraft(config);
                  setSaveStatus({ kind: "idle" });
                }}
              >
                <Icon.Settings /> {t(lang, "cockpitEdit")}
              </button>
            )
          ) : null}
        </div>
      </header>

      {saveStatus.kind === "saved" || saveStatus.kind === "conflict" ? (
        <p
          className={`notice ${saveStatus.kind === "saved" ? "ok" : "warn"}`}
          role="status"
        >
          {saveStatus.kind === "saved"
            ? t(lang, "cockpitSaved")
            : t(lang, "cockpitConflict")}
        </p>
      ) : null}

      {hasCockpit ? (
        <PriorityStrip lang={lang} priorities={priorities} />
      ) : null}

      {clinician || visits ? (
        <div className="cockpit-top">
          {clinician ? <StartConsultation lang={lang} /> : null}
          {visits ? (
            <ExpectedToday lang={lang} visits={visits} limit={limit + 2} />
          ) : null}
        </div>
      ) : null}

      {!hasCockpit ? (
        <div className="card">
          <p className="muted" style={{ margin: 0 }}>
            {t(lang, "noWorklistAccess")}
          </p>
        </div>
      ) : null}

      {visits ? <FlowStrip lang={lang} visits={visits} /> : null}

      {editing && draft && hasCockpit ? (
        <EditPanel
          lang={lang}
          draft={draft}
          available={available}
          dirty={!sameConfig(draft, config)}
          status={saveStatus}
          onChange={updateDraft}
          onRestore={() => updateDraft(defaultConfig(roles ?? []))}
          onSave={() => void save()}
          onCancel={() => {
            setDraft(null);
            setSaveStatus({ kind: "idle" });
          }}
        />
      ) : null}

      {hasCockpit ? (
        visible.length === 0 ? (
          <p className="muted" role="status">
            {t(lang, "allWidgetsHidden")}
          </p>
        ) : (
          <div className={`cockpit-grid ${shown.density}`}>
            {visible.map((w) => cell(w))}
          </div>
        )
      ) : null}
    </div>
  );
}

export default function DashboardPage() {
  return (
    <AppShell>
      <DashboardContent />
    </AppShell>
  );
}
