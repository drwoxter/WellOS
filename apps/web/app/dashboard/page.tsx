"use client";

import Link from "next/link";
import { useCallback, useEffect, useState } from "react";
import { AppShell } from "../chrome";
import { t } from "@/lib/i18n";
import type { TKey } from "@/lib/i18n";
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

function DashboardContent() {
  const { lang, authenticated, meta, metaError, reloadMeta } = useSession();
  const [summary, setSummary] = useState<Summary | null>(null);
  const [items, setItems] = useState<WorklistItem[] | null>(null);
  const [error, setError] = useState<string | null>(null);

  const roles = meta?.user.roles ?? null;
  const worklistUser = roles ? canReadWorklist(roles) : false;

  const load = useCallback(() => {
    setError(null);
    Promise.all([
      apiFetch<Summary>("/api/v1/worklist/summary"),
      apiFetch<{ items: WorklistItem[] }>("/api/v1/worklist?critical=true"),
      apiFetch<{ items: WorklistItem[] }>("/api/v1/worklist"),
    ])
      .then(([s, critical, recent]) => {
        setSummary(s);
        const seen = new Set(critical.items.map((i) => i.id));
        setItems([
          ...critical.items,
          ...recent.items.filter((i) => !seen.has(i.id)),
        ]);
      })
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, []);

  useEffect(() => {
    if (authenticated && worklistUser) load();
  }, [authenticated, worklistUser, load]);

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
  if (!roles || (worklistUser && (!summary || !items))) {
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
  if (meta && canActClinically(meta.facilities)) {
    quickActions.push({ href: "/patients", key: "actionStartEncounter" });
    quickActions.push({ href: "/patients", key: "actionOrderLab" });
  }

  const priority = items?.slice(0, 5) ?? [];

  return (
    <>
      <h2 style={{ marginTop: 0 }}>
        {t(lang, "welcome")}
        {meta ? `, ${meta.user.display_name}` : ""}
      </h2>
      <p className="muted">{t(lang, "dashboardIntro")}</p>

      {!worklistUser ? (
        <div className="card">
          <p className="muted" style={{ margin: 0 }}>
            {t(lang, "noWorklistAccess")}
          </p>
        </div>
      ) : null}

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
          <div className="stat-card">
            <span className="num">{summary.awaiting_notification}</span>
            <span className="label">{t(lang, "awaitingNotification")}</span>
          </div>
          <div className="stat-card">
            <span className="num">{summary.awaiting_closure}</span>
            <span className="label">{t(lang, "awaitingClosure")}</span>
          </div>
          <div className="stat-card ok">
            <span className="num">{summary.recently_closed}</span>
            <span className="label">{t(lang, "recentlyClosed")}</span>
          </div>
        </div>
      ) : null}

      <div className="card">
        <h2>{t(lang, "quickActions")}</h2>
        <div className="quick-actions">
          {quickActions.map((a) => (
            <Link key={a.key} href={a.href}>
              {t(lang, a.key)}
            </Link>
          ))}
        </div>
      </div>

      {worklistUser ? (
        <div className="card">
          <h2>{t(lang, "priorityResults")}</h2>
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
        </div>
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
