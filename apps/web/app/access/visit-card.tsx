"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useCallback, useState } from "react";
import { t } from "@/lib/i18n";
import type { Lang } from "@/lib/i18n";
import { ApiRequestError, apiFetch } from "@/lib/session";
import { formatDateTime, patientName } from "@/lib/clinical";
import {
  arrivalKindLabel,
  formatWait,
  priorityBadge,
  priorityLabel,
  serviceLabel,
  visitStatusLabel,
} from "@/lib/visits";
import type { VisitItem } from "@/lib/visits";

export type VisitAction = "arrive" | "cancel" | "no_show" | "start";

export type ActionMessage = { kind: "error" | "success"; text: string };

/** Translate a failed visit mutation into user wording. Conflicts mean the
 *  visit moved on under us (stale version, transition no longer valid, a
 *  consultation already open); the caller reloads so the card reflects the
 *  current state. */
export function visitErrorMessage(lang: Lang, err: unknown): string {
  if (err instanceof ApiRequestError) {
    if (err.status === 403) return t(lang, "actionNotPermitted");
    if (err.status === 409) {
      if (err.code === "patient_already_present")
        return t(lang, "patientAlreadyPresent");
      return t(lang, "visitConflict");
    }
  }
  return err instanceof Error ? err.message : String(err);
}

export function isConflict(err: unknown): boolean {
  return err instanceof ApiRequestError && err.status === 409;
}

/** Runs a visit action against the server and reports the outcome. Every
 *  action is re-authorized server-side; the capability hints only decide
 *  which buttons are drawn. */
export function useVisitActions(lang: Lang, reload: () => Promise<unknown>) {
  const router = useRouter();
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<ActionMessage | null>(null);

  const run = useCallback(
    async (visit: VisitItem, action: VisitAction) => {
      setBusy(`${visit.id}:${action}`);
      setMessage(null);
      try {
        if (action === "start") {
          const res = await apiFetch<{ encounter_id: string }>(
            `/api/v1/visits/${visit.id}/start-consultation`,
            {
              method: "POST",
              body: JSON.stringify({ version: visit.version }),
            },
          );
          router.push(`/encounters/${res.encounter_id}`);
          return;
        }
        const path =
          action === "arrive"
            ? "arrive"
            : action === "cancel"
              ? "cancel"
              : "no-show";
        await apiFetch(`/api/v1/visits/${visit.id}/${path}`, {
          method: "POST",
          body: JSON.stringify({ version: visit.version }),
        });
        await reload();
      } catch (err) {
        setMessage({ kind: "error", text: visitErrorMessage(lang, err) });
        if (isConflict(err)) await reload().catch(() => undefined);
      } finally {
        setBusy(null);
      }
    },
    [lang, reload, router],
  );

  return { busy, message, setMessage, run };
}

function AssignmentLine({ lang, v }: { lang: Lang; v: VisitItem }) {
  if (!v.assignment) {
    if (
      v.status === "ready_for_consultation" ||
      v.status === "in_consultation"
    ) {
      return <span className="muted">{t(lang, "unassigned")}</span>;
    }
    return null;
  }
  if (v.assignment.kind === "professional") {
    return (
      <span className="muted">
        {t(lang, "assignedTo")} {v.assignment.display_name ?? "—"}
      </span>
    );
  }
  return (
    <span className="muted">
      {t(lang, "queue")}: {v.assignment.name ?? v.assignment.code ?? "—"}
    </span>
  );
}

export function VisitCard({
  lang,
  visit: v,
  busy,
  onAction,
  showFacility,
  compact,
}: {
  lang: Lang;
  visit: VisitItem;
  busy: string | null;
  onAction: (visit: VisitItem, action: VisitAction) => void;
  showFacility?: boolean;
  compact?: boolean;
}) {
  const [confirming, setConfirming] = useState<"cancel" | "no_show" | null>(
    null,
  );
  const c = v.capabilities;
  const anyBusy = busy !== null;
  const isBusy = (a: VisitAction) => busy === `${v.id}:${a}`;
  const closed =
    v.status === "completed" ||
    v.status === "cancelled" ||
    v.status === "no_show";
  const tone =
    v.priority === "immediate" || v.arrival_kind === "urgent"
      ? "critical"
      : v.priority === "urgent"
        ? "warn"
        : closed
          ? "closed"
          : "routine";
  const label = `${patientName(v.patient)} — ${visitStatusLabel(lang, v.status)}`;

  return (
    <li className={`result-card visit-card ${tone}`} aria-label={label}>
      <div className="grow">
        <div className="title">
          {patientName(v.patient)}{" "}
          <span className="muted">
            · {v.patient.identifier} · {v.patient.age_years}{" "}
            {t(lang, "yearsShort")}
          </span>
        </div>
        <div className="visit-meta">
          <span className={`badge ${closed ? "neutral" : "ok"}`}>
            {visitStatusLabel(lang, v.status)}
          </span>
          {v.status !== "scheduled" ? (
            <span className={`badge ${priorityBadge(v.priority)}`}>
              {priorityLabel(lang, v.priority)}
            </span>
          ) : null}
          <span>
            {arrivalKindLabel(lang, v.arrival_kind)} ·{" "}
            {serviceLabel(lang, v.service)}
          </span>
          {v.status === "scheduled" && v.scheduled_at ? (
            <span>
              {t(lang, "scheduledFor")} {formatDateTime(lang, v.scheduled_at)}
            </span>
          ) : null}
          {!closed && v.wait_minutes !== null ? (
            <span>
              {t(lang, "waiting")} {formatWait(lang, v.wait_minutes)}
            </span>
          ) : null}
          {showFacility ? <span>{v.facility.name}</span> : null}
          {v.patient.alert_count > 0 ? (
            <span className="badge critical">
              {v.patient.alert_count} {t(lang, "openAlertsCount")}
            </span>
          ) : null}
          {v.patient.allergy_count > 0 ? (
            <span className="badge warn">
              {t(lang, "allergies")}: {v.patient.allergy_count}
            </span>
          ) : null}
        </div>
        {v.reason ? <div className="visit-reason">{v.reason}</div> : null}
        {!compact && v.handoff_summary ? (
          <div className="muted">{v.handoff_summary}</div>
        ) : null}
        <div className="visit-meta">
          <AssignmentLine lang={lang} v={v} />
        </div>
      </div>
      <div className="visit-actions">
        {c.can_resume_consultation && v.encounter_id ? (
          <Link
            className="navlink button-link primary"
            href={`/encounters/${v.encounter_id}`}
          >
            {t(lang, "resumeConsultation")}
          </Link>
        ) : c.can_resume_consultation || c.can_start_consultation ? (
          <button
            type="button"
            className="primary"
            disabled={anyBusy}
            onClick={() => onAction(v, "start")}
          >
            {isBusy("start")
              ? t(lang, "opening")
              : c.can_resume_consultation
                ? t(lang, "resumeConsultation")
                : t(lang, "startConsultation")}
          </button>
        ) : null}
        {c.can_triage && v.status !== "ready_for_consultation" ? (
          <Link className="navlink button-link" href={`/visits/${v.id}/triage`}>
            {v.status === "triage_in_progress"
              ? t(lang, "continueTriage")
              : t(lang, "openTriage")}
          </Link>
        ) : null}
        {c.can_arrive ? (
          <button
            type="button"
            className="primary"
            disabled={anyBusy}
            onClick={() => onAction(v, "arrive")}
          >
            {isBusy("arrive") ? t(lang, "loading") : t(lang, "markArrived")}
          </button>
        ) : null}
        {confirming ? (
          <div className="confirm-box visit-confirm" role="group">
            <p>
              {confirming === "cancel"
                ? t(lang, "confirmCancelVisit")
                : t(lang, "confirmNoShow")}
            </p>
            <div className="visit-actions">
              <button
                type="button"
                className="danger"
                disabled={anyBusy}
                onClick={() => {
                  onAction(v, confirming);
                  setConfirming(null);
                }}
              >
                {t(lang, "confirm")}
              </button>
              <button
                type="button"
                className="secondary"
                onClick={() => setConfirming(null)}
              >
                {t(lang, "cancel")}
              </button>
            </div>
          </div>
        ) : (
          <>
            {c.can_no_show ? (
              <button
                type="button"
                className="tertiary"
                disabled={anyBusy}
                onClick={() => setConfirming("no_show")}
              >
                {isBusy("no_show") ? t(lang, "loading") : t(lang, "markNoShow")}
              </button>
            ) : null}
            {c.can_cancel ? (
              <button
                type="button"
                className="tertiary"
                disabled={anyBusy}
                onClick={() => setConfirming("cancel")}
              >
                {isBusy("cancel") ? t(lang, "loading") : t(lang, "cancelVisit")}
              </button>
            ) : null}
          </>
        )}
      </div>
    </li>
  );
}
