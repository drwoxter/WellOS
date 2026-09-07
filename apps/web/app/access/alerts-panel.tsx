"use client";

import { useState } from "react";
import { t } from "@/lib/i18n";
import type { Lang } from "@/lib/i18n";
import { apiFetch } from "@/lib/session";
import { formatDateTime, patientName } from "@/lib/clinical";
import {
  alertKindLabel,
  formatWait,
  priorityBadge,
  priorityLabel,
} from "@/lib/visits";
import type { InternalAlert } from "@/lib/visits";
import { visitErrorMessage } from "./visit-card";

/** Alerts addressed to the signed-in professional or to a queue they cover.
 *  Acknowledging records receipt; it never changes the visit itself. */
export function AlertsPanel({
  lang,
  alerts,
  onAcknowledged,
  limit,
  headingId = "alerts-h",
  className = "",
}: {
  lang: Lang;
  alerts: InternalAlert[];
  onAcknowledged: () => Promise<unknown>;
  limit?: number;
  headingId?: string;
  className?: string;
}) {
  const [busy, setBusy] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  async function ack(id: string) {
    setBusy(id);
    setError(null);
    try {
      await apiFetch(`/api/v1/alerts/${id}/acknowledge`, { method: "POST" });
      await onAcknowledged();
    } catch (err) {
      setError(visitErrorMessage(lang, err));
    } finally {
      setBusy(null);
    }
  }

  return (
    <section
      className={`card alerts-panel ${className}`.trim()}
      aria-labelledby={headingId}
    >
      <h2 id={headingId}>
        {t(lang, "internalAlerts")}{" "}
        {alerts.some((a) => a.status === "open") ? (
          <span className="badge critical">
            {alerts.filter((a) => a.status === "open").length}
          </span>
        ) : null}
      </h2>
      {error ? (
        <p role="alert" className="error">
          {error}
        </p>
      ) : null}
      {alerts.length === 0 ? (
        <p className="muted">{t(lang, "noInternalAlerts")}</p>
      ) : (
        <ul className="result-list">
          {(limit ? alerts.slice(0, limit) : alerts).map((a) => (
            <li
              key={a.id}
              className={`result-card ${a.priority === "immediate" ? "critical" : a.priority === "urgent" ? "warn" : "routine"}`}
            >
              <div className="grow">
                <div className="title">
                  {alertKindLabel(lang, a.kind)} — {patientName(a.patient)}
                </div>
                <div className="visit-meta">
                  <span className={`badge ${priorityBadge(a.priority)}`}>
                    {priorityLabel(lang, a.priority)}
                  </span>
                  {a.target.kind === "queue" ? (
                    <span className="muted">
                      {t(lang, "forQueue")} {a.target.name ?? a.target.code}
                    </span>
                  ) : null}
                  {a.visit.wait_minutes !== null ? (
                    <span className="muted">
                      {t(lang, "waiting")}{" "}
                      {formatWait(lang, a.visit.wait_minutes)}
                    </span>
                  ) : null}
                  <span className="muted">
                    {formatDateTime(lang, a.created_at)}
                  </span>
                </div>
                {a.visit.handoff_summary || a.visit.reason ? (
                  <div className="muted">
                    {a.visit.handoff_summary ?? a.visit.reason}
                  </div>
                ) : null}
              </div>
              {a.status === "open" ? (
                <button
                  type="button"
                  className="secondary"
                  disabled={busy !== null}
                  onClick={() => ack(a.id)}
                >
                  {busy === a.id ? t(lang, "loading") : t(lang, "acknowledge")}
                </button>
              ) : (
                <span className="badge ok">{t(lang, "acknowledgedBadge")}</span>
              )}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
