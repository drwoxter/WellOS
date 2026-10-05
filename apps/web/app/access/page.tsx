"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AppShell } from "../chrome";
import { t } from "@/lib/i18n";
import type { Lang, TKey } from "@/lib/i18n";
import { ApiRequestError, apiFetch, useSession } from "@/lib/session";
import { canReadVisits, defaultAccessView } from "@/lib/visits";
import type { InternalAlert, VisitItem } from "@/lib/visits";
import { StatTile } from "@/components/ui/primitives";
import { AlertsPanel } from "./alerts-panel";
import { NewVisit } from "./new-visit";
import { VisitCard, useVisitActions } from "./visit-card";

type View = "access" | "triage" | "ready" | "closed";

const VIEWS: { id: View; label: TKey }[] = [
  { id: "access", label: "tabArrivals" },
  { id: "triage", label: "tabTriage" },
  { id: "ready", label: "tabReady" },
  { id: "closed", label: "tabClosedToday" },
];

const MANAGE_ROLES = ["registration_staff", "nurse", "clinical_administrator"];

const PRIORITY_RANK: Record<string, number> = {
  immediate: 4,
  urgent: 3,
  standard: 2,
  non_urgent: 1,
};

/**
 * Operational ordering for a queue: triage priority first, then the longest
 * wait. Nothing is inferred — both signals come from the visit record.
 */
export function sortQueue(items: VisitItem[]): VisitItem[] {
  return items
    .map((v, i) => ({ v, i }))
    .sort((a, b) => {
      const pa = PRIORITY_RANK[a.v.priority ?? ""] ?? 0;
      const pb = PRIORITY_RANK[b.v.priority ?? ""] ?? 0;
      if (pa !== pb) return pb - pa;
      const wa = a.v.wait_minutes ?? -1;
      const wb = b.v.wait_minutes ?? -1;
      if (wa !== wb) return wb - wa;
      return a.i - b.i;
    })
    .map((x) => x.v);
}

export function summarizeQueue(items: VisitItem[]) {
  let urgent = 0;
  let scheduled = 0;
  let walkIn = 0;
  let longest = 0;
  for (const v of items) {
    if (
      v.arrival_kind === "urgent" ||
      v.priority === "immediate" ||
      v.priority === "urgent"
    )
      urgent++;
    if (v.arrival_kind === "scheduled") scheduled++;
    if (v.arrival_kind === "walk_in") walkIn++;
    if (v.wait_minutes !== null && v.wait_minutes > longest)
      longest = v.wait_minutes;
  }
  return { total: items.length, urgent, scheduled, walkIn, longest };
}

function AccessBoard() {
  const { lang, authenticated, meta } = useSession();
  const roles = meta?.user.roles ?? [];
  const facilities = meta?.facilities.filter((f) => f.accessible) ?? [];
  const [view, setView] = useState<View | null>(null);
  const [facilityId, setFacilityId] = useState<string>("");
  const [items, setItems] = useState<VisitItem[] | null>(null);
  const [alerts, setAlerts] = useState<InternalAlert[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [denied, setDenied] = useState(false);
  // Responses from a superseded load (tab or facility changed meanwhile) are
  // dropped so a slow request cannot overwrite the current list.
  const generation = useRef(0);

  const activeView: View = view ?? defaultAccessView(roles);

  const load = useCallback(async () => {
    const gen = ++generation.current;
    setError(null);
    const params = new URLSearchParams({ view: activeView });
    if (facilityId) params.set("facility_id", facilityId);
    try {
      const [v, a] = await Promise.all([
        apiFetch<{ items: VisitItem[] }>(`/api/v1/visits?${params}`),
        apiFetch<{ items: InternalAlert[] }>("/api/v1/alerts").catch(() => ({
          items: [] as InternalAlert[],
        })),
      ]);
      if (gen !== generation.current) return;
      setItems(v.items);
      setAlerts(a.items);
      setDenied(false);
    } catch (err) {
      if (gen !== generation.current) return;
      if (err instanceof ApiRequestError && err.status === 403) {
        setDenied(true);
      } else {
        setError(err instanceof Error ? err.message : String(err));
      }
    }
  }, [activeView, facilityId]);

  const boardUser = canReadVisits(roles);

  useEffect(() => {
    if (!authenticated || !meta || !boardUser) return;
    setItems(null);
    void load();
  }, [authenticated, meta, boardUser, load]);

  const { busy, message, run } = useVisitActions(lang, load);

  const canManage = roles.some((r) => MANAGE_ROLES.includes(r));
  const ordered = useMemo(() => (items ? sortQueue(items) : null), [items]);
  const summary = useMemo(
    () => (items && activeView !== "closed" ? summarizeQueue(items) : null),
    [items, activeView],
  );

  if (meta && !boardUser) {
    return (
      <div className="card">
        <p role="alert" className="error">
          {t(lang, "noAccessBoard")}
        </p>
      </div>
    );
  }

  return (
    <>
      <h2 style={{ marginTop: 0 }}>{t(lang, "accessTitle")}</h2>
      <p className="muted">{t(lang, "accessIntro")}</p>
      {denied ? (
        <div className="card">
          <p role="alert" className="error">
            {t(lang, "notAuthorized")}
          </p>
        </div>
      ) : (
        <div className="access-layout">
          <section className="card access-board" aria-labelledby="board-h">
            <h2 id="board-h" className="sr-only">
              {t(lang, "accessTitle")}
            </h2>
            <div className="tabs" role="tablist" aria-label={t(lang, "state")}>
              {VIEWS.map((v) => (
                <button
                  key={v.id}
                  type="button"
                  role="tab"
                  id={`tab-${v.id}`}
                  aria-selected={activeView === v.id}
                  aria-controls="visit-panel"
                  onClick={() => setView(v.id)}
                >
                  {t(lang, v.label)}
                </button>
              ))}
            </div>
            {facilities.length > 1 ? (
              <div className="filters">
                <div>
                  <label htmlFor="visit-facility">{t(lang, "facility")}</label>
                  <select
                    id="visit-facility"
                    value={facilityId}
                    onChange={(e) => setFacilityId(e.target.value)}
                  >
                    <option value="">{t(lang, "allFacilities")}</option>
                    {facilities.map((f) => (
                      <option key={f.id} value={f.id}>
                        {f.name}
                      </option>
                    ))}
                  </select>
                </div>
              </div>
            ) : null}
            {message ? (
              <p
                role={message.kind === "error" ? "alert" : "status"}
                className={message.kind === "error" ? "error" : "success"}
              >
                {message.text}
              </p>
            ) : null}
            {summary && summary.total > 0 ? (
              <section
                className="queue-summary"
                aria-label={t(lang, "queueSummary")}
              >
                <StatTile
                  value={summary.total}
                  label={t(lang, "queueInQueue")}
                  tone="teal"
                />
                <StatTile
                  value={summary.urgent}
                  label={t(lang, "queueUrgent")}
                  tone={summary.urgent > 0 ? "critical" : "neutral"}
                />
                <StatTile
                  value={summary.scheduled}
                  label={t(lang, "queueScheduled")}
                />
                <StatTile
                  value={summary.walkIn}
                  label={t(lang, "queueWalkIn")}
                />
                <StatTile
                  value={t(lang, "queueMinutes").replace(
                    "{n}",
                    String(summary.longest),
                  )}
                  label={t(lang, "queueLongestWait")}
                  tone={summary.longest >= 60 ? "warn" : "neutral"}
                />
                <p className="muted small queue-sort-help">
                  {t(lang, "queueSortHelp")}
                </p>
              </section>
            ) : null}
            <div
              id="visit-panel"
              role="tabpanel"
              aria-labelledby={`tab-${activeView}`}
            >
              {error ? (
                <div>
                  <p role="alert" className="error">
                    {error}
                  </p>
                  <button className="secondary" onClick={() => void load()}>
                    {t(lang, "retry")}
                  </button>
                </div>
              ) : items === null ? (
                <p className="muted" role="status">
                  {t(lang, "loading")}
                </p>
              ) : items.length === 0 ? (
                <p className="muted" role="status">
                  {t(lang, "noVisitsInList")}
                </p>
              ) : (
                <ul className="result-list queue-list">
                  {(ordered ?? items).map((v) => (
                    <VisitCard
                      key={v.id}
                      lang={lang}
                      visit={v}
                      busy={busy}
                      onAction={(visit, action) => void run(visit, action)}
                      showFacility={facilities.length > 1 && !facilityId}
                    />
                  ))}
                </ul>
              )}
            </div>
          </section>
          <aside
            className="access-side"
            aria-label={t(lang, "boardOperations")}
          >
            {alerts && alerts.length > 0 ? (
              <AlertsPanel lang={lang} alerts={alerts} onAcknowledged={load} />
            ) : null}
            {canManage ? <NewVisit lang={lang} onCreated={load} /> : null}
          </aside>
        </div>
      )}
    </>
  );
}

export default function AccessPage() {
  return (
    <AppShell>
      <AccessBoard />
    </AppShell>
  );
}
