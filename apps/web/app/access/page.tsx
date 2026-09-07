"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { AppShell } from "../chrome";
import { t } from "@/lib/i18n";
import type { Lang, TKey } from "@/lib/i18n";
import { ApiRequestError, apiFetch, useSession } from "@/lib/session";
import { canReadVisits, defaultAccessView } from "@/lib/visits";
import type { InternalAlert, VisitItem } from "@/lib/visits";
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
        <>
          {alerts && alerts.length > 0 ? (
            <AlertsPanel lang={lang} alerts={alerts} onAcknowledged={load} />
          ) : null}
          {canManage ? <NewVisit lang={lang} onCreated={load} /> : null}
          <section className="card" aria-labelledby="board-h">
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
                <ul className="result-list">
                  {items.map((v) => (
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
        </>
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
