"use client";

import { useState } from "react";
import { apiFetch } from "@/lib/session";
import { t, type Lang } from "@/lib/i18n";
import { formatDateTime } from "@/lib/clinical";
import {
  appointmentStatusLabel,
  catalogName,
  formatDay,
  formatRange,
  query,
  requestStatusLabel,
  urgencyLabel,
  type AccessRequest,
  type Appointment,
  type AppointmentHistoryEntry,
  type CursorPage,
  type Offer,
  type PatientSummary,
} from "@/lib/access";
import type { PatientOption } from "./find-appointment";
import {
  ConfirmBox,
  MessageLine,
  OfferCard,
  PanelState,
  ReasonField,
  StatusBadge,
  appointmentTone,
  errorText,
  postJson,
  useAction,
  useCatalog,
  useLoader,
  type OfferAction,
} from "./shared";

type PatientHit = PatientSummary & { identifier: string; birth_date: string };

export function patientLabel(p: PatientSummary): string {
  return `${p.family_name}, ${p.given_name}${p.identifier ? ` (${p.identifier})` : ""}`;
}

/** Staff pick a patient by searching the directory; the server scopes hits. */
export function PatientPicker({
  lang,
  onPick,
}: {
  lang: Lang;
  onPick: (p: PatientOption) => void;
}) {
  const [q, setQ] = useState("");
  const [hits, setHits] = useState<PatientHit[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

  async function search(e: React.FormEvent) {
    e.preventDefault();
    const term = q.trim();
    if (term.length < 2) return;
    setBusy(true);
    setError(null);
    try {
      const res = await apiFetch<{ patients: PatientHit[] }>(
        `/api/v1/patients?query=${encodeURIComponent(term)}`,
      );
      setHits(res.patients);
    } catch (err) {
      setError(errorText(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="patient-picker">
      <form
        className="search-row"
        onSubmit={(e) => void search(e)}
        role="search"
      >
        <label htmlFor="sched-patient-q">{t(lang, "searchPatient")}</label>
        <input
          id="sched-patient-q"
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder={t(lang, "searchPatientHint")}
          minLength={2}
        />
        <button
          type="submit"
          className="secondary"
          disabled={busy || q.trim().length < 2}
        >
          {t(lang, "search")}
        </button>
      </form>
      {error ? (
        <p role="alert" className="error">
          {error}
        </p>
      ) : null}
      {hits ? (
        hits.length === 0 ? (
          <p className="muted" role="status">
            {t(lang, "noPatientsFound")}
          </p>
        ) : (
          <ul className="result-list compact" aria-label={t(lang, "patients")}>
            {hits.map((p) => (
              <li key={p.id}>
                <button
                  type="button"
                  className="tertiary"
                  onClick={() => {
                    onPick({ ...p, label: patientLabel(p) });
                    setHits(null);
                    setQ("");
                  }}
                >
                  {patientLabel(p)} · {formatDay(lang, p.birth_date)}
                </button>
              </li>
            ))}
          </ul>
        )
      ) : null}
    </div>
  );
}

const PENDING: string[] = [
  "submitted",
  "needs_clinical_triage",
  "options_ready",
  "draft",
];

async function loadPending(facilityId: string): Promise<AccessRequest[]> {
  const pages = await Promise.all(
    PENDING.map((status) =>
      apiFetch<CursorPage<AccessRequest>>(
        `/api/v1/access-requests${query({ status, facility_id: facilityId, limit: 50 })}`,
      ),
    ),
  );
  return pages
    .flatMap((p) => p.items)
    .sort((a, b) => a.created_at.localeCompare(b.created_at));
}

function urgencyTone(u: string): "ok" | "warn" | "critical" | "neutral" {
  return u === "urgent" ? "critical" : u === "priority" ? "warn" : "neutral";
}

/** Access requests waiting for options, triage or booking. */
export function RequestsPanel({
  lang,
  facilityId,
  onOpen,
  refreshKey,
}: {
  lang: Lang;
  facilityId: string;
  onOpen: (r: AccessRequest) => void;
  refreshKey: number;
}) {
  const state = useLoader(
    () => loadPending(facilityId),
    `${facilityId}:${refreshKey}`,
  );
  const services = useCatalog("clinical_service");
  const { busy, message, run } = useAction(lang);
  const [closing, setClosing] = useState<{
    r: AccessRequest;
    reason: string;
  } | null>(null);

  const serviceName = (code: string | null) => {
    if (!code) return t(lang, "noServiceYet");
    const e = services.entries.find((s) => s.code === code);
    return e ? catalogName(lang, e) : code;
  };

  async function close() {
    if (!closing) return;
    const { r, reason } = closing;
    const ok = await run(async () => {
      await postJson(`/api/v1/access-requests/${r.id}/close`, {
        version: r.version,
        reason: reason.trim(),
      });
    }, "requestClosed");
    if (ok) {
      setClosing(null);
      state.reload();
    }
  }

  return (
    <section className="card" aria-labelledby="requests-h">
      <h3 id="requests-h">{t(lang, "pendingRequests")}</h3>
      <MessageLine message={message} />
      <PanelState
        lang={lang}
        state={state}
        emptyKey="noPendingRequests"
        isEmpty={(d) => d.length === 0}
      >
        {(items) => (
          <ul className="result-list">
            {items.map((r) => (
              <li key={r.id} className="result-card">
                <div className="offer-head">
                  <div>
                    <strong>
                      {r.patient ? patientLabel(r.patient) : r.patient_id}
                    </strong>
                    <div className="muted">
                      {serviceName(r.constraints.service_code)}
                      {r.constraints.specialty_code
                        ? ` · ${r.constraints.specialty_code}`
                        : ""}{" "}
                      · {formatDay(lang, r.created_at)}
                    </div>
                    {r.free_text ? (
                      <p className="small">{r.free_text}</p>
                    ) : null}
                    {r.missing_info.length > 0 ? (
                      <p className="small warn-text">
                        {t(lang, "missingInformation")}:{" "}
                        {r.missing_info.join(", ")}
                      </p>
                    ) : null}
                    {r.triage_reason ? (
                      <p className="small warn-text">{r.triage_reason}</p>
                    ) : null}
                  </div>
                  <div className="badges">
                    <StatusBadge
                      label={requestStatusLabel(lang, r.status)}
                      tone={
                        r.status === "needs_clinical_triage"
                          ? "warn"
                          : "neutral"
                      }
                    />
                    <StatusBadge
                      label={urgencyLabel(lang, r.urgency)}
                      tone={urgencyTone(r.urgency)}
                    />
                  </div>
                </div>
                {closing?.r.id === r.id ? (
                  <ConfirmBox
                    lang={lang}
                    title={t(lang, "closeRequest")}
                    busy={busy}
                    disabled={closing.reason.trim().length < 3}
                    onConfirm={() => void close()}
                    onCancel={() => setClosing(null)}
                  >
                    <ReasonField
                      id={`close-${r.id}`}
                      lang={lang}
                      value={closing.reason}
                      onChange={(v) => setClosing({ r, reason: v })}
                      required
                    />
                  </ConfirmBox>
                ) : (
                  <div className="visit-actions">
                    <button
                      type="button"
                      className="primary"
                      disabled={busy}
                      onClick={() => onOpen(r)}
                    >
                      {t(lang, "findOptions")}
                    </button>
                    <button
                      type="button"
                      className="tertiary"
                      disabled={busy}
                      onClick={() => setClosing({ r, reason: "" })}
                    >
                      {t(lang, "closeRequest")}
                    </button>
                  </div>
                )}
              </li>
            ))}
          </ul>
        )}
      </PanelState>
    </section>
  );
}

async function loadLiveOffers(facilityId: string): Promise<Offer[]> {
  const page = await apiFetch<CursorPage<Offer>>(
    `/api/v1/offers${query({ status: "active", facility_id: facilityId, limit: 200 })}`,
  );
  return [...page.items].sort((a, b) => a.starts_at.localeCompare(b.starts_at));
}

/** Active holds and open offers across the facility, with staff actions. */
export function HoldsPanel({
  lang,
  facilityId,
  refreshKey,
  onChanged,
}: {
  lang: Lang;
  facilityId: string;
  refreshKey: number;
  onChanged: () => void;
}) {
  const state = useLoader(
    () => loadLiveOffers(facilityId),
    `${facilityId}:${refreshKey}`,
  );
  const { busy, message, run } = useAction(lang);
  const [pending, setPending] = useState<{
    offer: Offer;
    action: "accept" | "decline";
    reason: string;
  } | null>(null);

  async function act(offer: Offer, action: OfferAction) {
    if (action === "accept" || action === "decline") {
      setPending({ offer, action, reason: "" });
      return;
    }
    const ok = await run(
      async () => {
        await postJson(
          `/api/v1/offers/${offer.id}/${action === "hold" ? "hold" : "release-hold"}`,
          { version: offer.version },
        );
      },
      action === "hold" ? "holdPlaced" : "holdReleased",
    );
    if (ok) {
      state.reload();
      onChanged();
    }
  }

  async function confirmPending() {
    if (!pending) return;
    const { offer, action, reason } = pending;
    const ok = await run(
      async () => {
        if (action === "accept") {
          await postJson(`/api/v1/offers/${offer.id}/accept`, {
            version: offer.version,
            idempotency_key: `staff:${offer.id}:${offer.version}`,
            override_reason: reason.trim() || null,
          });
        } else {
          await postJson(`/api/v1/offers/${offer.id}/decline`, {
            version: offer.version,
            reason: reason.trim() || null,
          });
        }
      },
      action === "accept" ? "appointmentConfirmed" : "offerDeclined",
    );
    if (ok) {
      setPending(null);
      state.reload();
      onChanged();
    }
  }

  return (
    <section className="card" aria-labelledby="holds-h">
      <h3 id="holds-h">{t(lang, "activeHolds")}</h3>
      <MessageLine message={message} />
      {pending ? (
        <ConfirmBox
          lang={lang}
          title={
            pending.action === "accept"
              ? t(lang, "confirmAppointment")
              : t(lang, "declineOption")
          }
          busy={busy}
          onConfirm={() => void confirmPending()}
          onCancel={() => setPending(null)}
        >
          <p className="muted">
            {formatRange(lang, pending.offer.starts_at, pending.offer.ends_at)}
            {pending.offer.patient
              ? ` · ${patientLabel(pending.offer.patient)}`
              : ""}
          </p>
          <ReasonField
            id="hold-action-reason"
            lang={lang}
            value={pending.reason}
            onChange={(v) => setPending({ ...pending, reason: v })}
            label={
              pending.action === "accept"
                ? t(lang, "overrideReasonOptional")
                : t(lang, "reason")
            }
          />
        </ConfirmBox>
      ) : null}
      <PanelState
        lang={lang}
        state={state}
        emptyKey="noActiveHolds"
        isEmpty={(d) => d.length === 0}
      >
        {(items) => (
          <ul className="result-list">
            {items.map((o) => (
              <OfferCard
                key={o.id}
                lang={lang}
                offer={o}
                busy={busy}
                showPatient
                compact
                onAction={(offer, action) => void act(offer, action)}
              />
            ))}
          </ul>
        )}
      </PanelState>
    </section>
  );
}

type ApptAction = "cancel" | "no_show" | "fulfil";

/** Confirmed: today → +60 days. Closed states: the past 30 days → +30 days. */
function appointmentWindow(status: string): { from: string; to: string } {
  const start = new Date();
  start.setHours(0, 0, 0, 0);
  const from = new Date(start);
  const to = new Date(start);
  if (status === "confirmed") to.setDate(to.getDate() + 60);
  else {
    from.setDate(from.getDate() - 30);
    to.setDate(to.getDate() + 30);
  }
  return { from: from.toISOString(), to: to.toISOString() };
}

async function loadAppointments(
  facilityId: string,
  status: string,
): Promise<Appointment[]> {
  const w = appointmentWindow(status);
  const page = await apiFetch<CursorPage<Appointment>>(
    `/api/v1/appointments${query({
      facility_id: facilityId,
      status,
      from: w.from,
      to: w.to,
      limit: 100,
    })}`,
  );
  return page.items;
}

/** Confirmed and recently closed appointments with human transitions. */
export function AppointmentsPanel({
  lang,
  facilityId,
  refreshKey,
  onChanged,
  onReschedule,
}: {
  lang: Lang;
  facilityId: string;
  refreshKey: number;
  onChanged: () => void;
  onReschedule: (a: Appointment) => void;
}) {
  const [status, setStatus] = useState("confirmed");
  const state = useLoader(
    () => loadAppointments(facilityId, status),
    `${facilityId}:${status}:${refreshKey}`,
  );
  const { busy, message, run } = useAction(lang);
  const [pending, setPending] = useState<{
    a: Appointment;
    action: ApptAction;
    reason: string;
    override: string;
  } | null>(null);
  const [history, setHistory] = useState<{
    id: string;
    items: AppointmentHistoryEntry[];
  } | null>(null);

  async function confirmPending() {
    if (!pending) return;
    const { a, action, reason, override } = pending;
    const ok = await run(async () => {
      if (action === "cancel") {
        await postJson(`/api/v1/appointments/${a.id}/cancel`, {
          version: a.version,
          reason_code: "cancelled_by_staff",
          note: reason.trim() || null,
          override_reason: override.trim() || null,
        });
      } else {
        await postJson(
          `/api/v1/appointments/${a.id}/${action === "no_show" ? "no-show" : "fulfil"}`,
          {
            version: a.version,
            reason_code: action === "no_show" ? "no_show" : "attended",
            note: reason.trim() || null,
            override_reason: override.trim() || null,
          },
        );
      }
    }, "appointmentUpdated");
    if (ok) {
      setPending(null);
      state.reload();
      onChanged();
    }
  }

  async function showHistory(a: Appointment) {
    if (history?.id === a.id) {
      setHistory(null);
      return;
    }
    await run(async () => {
      const h = await apiFetch<{ items: AppointmentHistoryEntry[] }>(
        `/api/v1/appointments/${a.id}/history`,
      );
      setHistory({ id: a.id, items: h.items });
    });
  }

  return (
    <section className="card" aria-labelledby="appts-h">
      <h3 id="appts-h">{t(lang, "appointments")}</h3>
      <div className="filters">
        <div>
          <label htmlFor="appt-status">{t(lang, "state")}</label>
          <select
            id="appt-status"
            value={status}
            onChange={(e) => setStatus(e.target.value)}
          >
            {[
              "confirmed",
              "cancelled",
              "no_show",
              "fulfilled",
              "rescheduled",
            ].map((s) => (
              <option key={s} value={s}>
                {appointmentStatusLabel(lang, s)}
              </option>
            ))}
          </select>
        </div>
      </div>
      <MessageLine message={message} />
      <PanelState
        lang={lang}
        state={state}
        emptyKey="noAppointmentsInList"
        isEmpty={(d) => d.length === 0}
      >
        {(items) => (
          <ul className="result-list">
            {items.map((a) => {
              const serviceName = a.service
                ? lang === "es"
                  ? a.service.name_es
                  : a.service.name_en
                : a.service_code;
              return (
                <li key={a.id} className="result-card">
                  <div className="offer-head">
                    <div>
                      <strong>
                        {formatRange(lang, a.starts_at, a.ends_at, a.time_zone)}
                      </strong>
                      <div className="muted">
                        {a.patient ? patientLabel(a.patient) : a.patient_id}
                      </div>
                      <div className="muted">
                        {serviceName} · {a.modality_code}
                        {a.primary_resource
                          ? ` · ${a.primary_resource.name}`
                          : ""}
                        {a.facility_name ? ` · ${a.facility_name}` : ""}
                      </div>
                      {a.confirmation_required && !a.patient_confirmed_at ? (
                        <p className="small warn-text">
                          {t(lang, "awaitingPatientConfirmation")}
                        </p>
                      ) : null}
                      {a.override_reason ? (
                        <p className="small muted">
                          {t(lang, "overrideReason")}: {a.override_reason}
                        </p>
                      ) : null}
                      {a.cancellation_reason ? (
                        <p className="small muted">
                          {t(lang, "reason")}: {a.cancellation_reason}
                          {a.cancellation_note
                            ? ` — ${a.cancellation_note}`
                            : ""}
                        </p>
                      ) : null}
                    </div>
                    <div className="badges">
                      <StatusBadge
                        label={appointmentStatusLabel(lang, a.status)}
                        tone={appointmentTone(a.status)}
                      />
                      {a.visit_id ? (
                        <a className="badge neutral" href={`/access`}>
                          {t(lang, "linkedVisit")}
                        </a>
                      ) : null}
                    </div>
                  </div>
                  {pending?.a.id === a.id ? (
                    <ConfirmBox
                      lang={lang}
                      title={
                        pending.action === "cancel"
                          ? t(lang, "cancelAppointment")
                          : pending.action === "no_show"
                            ? t(lang, "markNoShow")
                            : t(lang, "markFulfilled")
                      }
                      busy={busy}
                      disabled={
                        pending.action === "no_show" &&
                        new Date(a.starts_at) > new Date() &&
                        pending.override.trim().length < 3
                      }
                      onConfirm={() => void confirmPending()}
                      onCancel={() => setPending(null)}
                    >
                      {pending.action === "cancel" ? (
                        <>
                          <ReasonField
                            id={`appt-reason-${a.id}`}
                            lang={lang}
                            value={pending.reason}
                            onChange={(v) =>
                              setPending({ ...pending, reason: v })
                            }
                          />
                          <ReasonField
                            id={`appt-override-${a.id}`}
                            lang={lang}
                            value={pending.override}
                            onChange={(v) =>
                              setPending({ ...pending, override: v })
                            }
                            label={t(lang, "overrideReasonIfOutsidePolicy")}
                          />
                        </>
                      ) : pending.action === "no_show" ? (
                        <ReasonField
                          id={`appt-override-${a.id}`}
                          lang={lang}
                          value={pending.override}
                          onChange={(v) =>
                            setPending({ ...pending, override: v })
                          }
                          label={
                            new Date(a.starts_at) > new Date()
                              ? t(lang, "overrideReasonRequired")
                              : t(lang, "overrideReasonOptional")
                          }
                          required={new Date(a.starts_at) > new Date()}
                        />
                      ) : null}
                    </ConfirmBox>
                  ) : (
                    <div className="visit-actions">
                      {a.status === "confirmed" ? (
                        <>
                          <button
                            type="button"
                            className="secondary"
                            disabled={busy}
                            onClick={() => onReschedule(a)}
                          >
                            {t(lang, "reschedule")}
                          </button>
                          <button
                            type="button"
                            className="tertiary"
                            disabled={busy}
                            onClick={() =>
                              setPending({
                                a,
                                action: "cancel",
                                reason: "",
                                override: "",
                              })
                            }
                          >
                            {t(lang, "cancelAppointment")}
                          </button>
                          <button
                            type="button"
                            className="tertiary"
                            disabled={busy}
                            onClick={() =>
                              setPending({
                                a,
                                action: "no_show",
                                reason: "",
                                override: "",
                              })
                            }
                          >
                            {t(lang, "markNoShow")}
                          </button>
                          <button
                            type="button"
                            className="tertiary"
                            disabled={busy}
                            onClick={() =>
                              setPending({
                                a,
                                action: "fulfil",
                                reason: "",
                                override: "",
                              })
                            }
                          >
                            {t(lang, "markFulfilled")}
                          </button>
                        </>
                      ) : null}
                      <button
                        type="button"
                        className="tertiary"
                        disabled={busy}
                        aria-expanded={history?.id === a.id}
                        onClick={() => void showHistory(a)}
                      >
                        {t(lang, "history")}
                      </button>
                      <a
                        className="tertiary button-link"
                        href={`/api/v1/appointments/${a.id}/ics?lang=${lang}`}
                      >
                        {t(lang, "downloadIcs")}
                      </a>
                    </div>
                  )}
                  {history?.id === a.id ? (
                    <ol
                      className="history-list"
                      aria-label={t(lang, "history")}
                    >
                      {history.items.map((h) => (
                        <li key={h.version}>
                          {formatDateTime(lang, h.recorded_at)} ·{" "}
                          {h.from_status
                            ? `${appointmentStatusLabel(lang, h.from_status)} → `
                            : ""}
                          {appointmentStatusLabel(lang, h.to_status)}
                          {h.starts_at_after &&
                          h.starts_at_after !== h.starts_at_before
                            ? ` · ${formatDateTime(lang, h.starts_at_after)}`
                            : ""}
                          {h.reason_code ? ` — ${h.reason_code}` : ""}
                          {h.note ? ` (${h.note})` : ""}
                          {h.override ? ` · ${t(lang, "overrideReason")}` : ""}
                        </li>
                      ))}
                    </ol>
                  ) : null}
                </li>
              );
            })}
          </ul>
        )}
      </PanelState>
    </section>
  );
}
