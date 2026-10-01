"use client";

import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AppShell } from "../../chrome";
import { t, type Lang } from "@/lib/i18n";
import { apiFetch, useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import {
  appointmentStatusLabel,
  query,
  requestStatusLabel,
  transportStatusLabel,
  type AccessRequest,
  type Appointment,
  type AppointmentHistoryEntry,
  type MatchResult,
  type Me,
  type TransportRequest,
} from "@/lib/access";
import {
  ConfirmBox,
  MessageLine,
  PanelState,
  ReasonField,
  StatusBadge,
  nameFor,
  postJson,
  useAction,
  useCatalog,
  useLoader,
} from "../../scheduling/shared";
import {
  FindAppointment,
  patientAccessApi,
  type PatientOption,
} from "../../scheduling/find-appointment";
import {
  NotificationsSection,
  PreferencesSection,
  WaitlistSection,
} from "./sections";

const ME = "/api/v1/me";

type Tab =
  | "appointments"
  | "find"
  | "requests"
  | "waitlist"
  | "preferences"
  | "notifications";

const TABS: Tab[] = [
  "appointments",
  "find",
  "requests",
  "waitlist",
  "preferences",
  "notifications",
];

const TAB_KEY: Record<Tab, Parameters<typeof t>[1]> = {
  appointments: "myAppointmentsTab",
  find: "findBestAppointment",
  requests: "myRequestsTab",
  waitlist: "waitlistTitle",
  preferences: "preferencesTab",
  notifications: "notifications",
};

type Items<T> = { patient_id: string; items: T[] };

const CANCEL_REASONS = [
  "patient_request",
  "feeling_better",
  "scheduling_conflict",
  "transport_issue",
  "other",
] as const;

function cancelReasonLabel(lang: Lang, code: (typeof CANCEL_REASONS)[number]) {
  const key: Record<(typeof CANCEL_REASONS)[number], Parameters<typeof t>[1]> =
    {
      patient_request: "cancelReasonPatientRequest",
      feeling_better: "cancelReasonFeelingBetter",
      scheduling_conflict: "cancelReasonConflict",
      transport_issue: "cancelReasonTransport",
      other: "cancelReasonOther",
    };
  return t(lang, key[code]);
}

// ---------------------------------------------------------------------------
// Appointment card
// ---------------------------------------------------------------------------

function AppointmentCard({
  lang,
  appt,
  transport,
  busy,
  onConfirm,
  onReschedule,
  onCancel,
  onRequestTransport,
  onCancelTransport,
}: {
  lang: Lang;
  appt: Appointment;
  transport: TransportRequest | undefined;
  busy: boolean;
  onConfirm: (a: Appointment) => void;
  onReschedule: (a: Appointment) => void;
  onCancel: (a: Appointment, reason: string, note: string) => void;
  onRequestTransport: (
    a: Appointment,
    body: {
      requirements: string[];
      origin_area_code: string | null;
      pickup_address: string | null;
      note: string | null;
    },
  ) => void;
  onCancelTransport: (tr: TransportRequest, reason: string) => void;
}) {
  const services = useCatalog("clinical_service");
  const modalities = useCatalog("modality");
  const accessibility = useCatalog("accessibility_capability");
  const [panel, setPanel] = useState<
    "none" | "cancel" | "transport" | "history" | "cancelTransport"
  >("none");
  const [reason, setReason] =
    useState<(typeof CANCEL_REASONS)[number]>("patient_request");
  const [note, setNote] = useState("");
  const [trReqs, setTrReqs] = useState<string[]>([]);
  const [trArea, setTrArea] = useState("");
  const [trAddress, setTrAddress] = useState("");
  const [trNote, setTrNote] = useState("");
  const [trCancelReason, setTrCancelReason] = useState("");
  const history = useLoader(
    () =>
      panel === "history"
        ? apiFetch<{ items: AppointmentHistoryEntry[] }>(
            `${ME}/appointments/${appt.id}/history`,
          )
        : Promise.resolve({ items: [] as AppointmentHistoryEntry[] }),
    `${appt.id}:${panel === "history"}`,
  );

  const active = appt.status === "confirmed" || appt.status === "rescheduled";
  const upcoming = new Date(appt.starts_at).getTime() > Date.now();
  const needsConfirmation =
    active && appt.confirmation_required && !appt.patient_confirmed_at;
  const resource = appt.primary_resource?.name;
  const transportActive =
    transport &&
    !["completed", "cancelled", "failed"].includes(transport.status);

  return (
    <li
      className={`row-card appointment-card status-${appt.status}`}
      data-testid="appointment-card"
      data-status={appt.status}
    >
      <div className="row-main">
        <div className="row-head">
          <strong>{nameFor(lang, services.entries, appt.service_code)}</strong>
          <StatusBadge
            label={appointmentStatusLabel(lang, appt.status)}
            tone={
              active ? "ok" : appt.status === "cancelled" ? "warn" : "neutral"
            }
          />
        </div>
        <p className="appointment-when">
          <time dateTime={appt.starts_at}>
            {formatDateTime(lang, appt.starts_at)}
          </time>
          {" – "}
          <time dateTime={appt.ends_at}>
            {new Date(appt.ends_at).toLocaleTimeString(
              lang === "es" ? "es" : "en",
              { hour: "2-digit", minute: "2-digit" },
            )}
          </time>
        </p>
        <p className="muted">
          {appt.facility_name ?? t(lang, "facility")} ·{" "}
          {nameFor(lang, modalities.entries, appt.modality_code)}
          {resource ? ` · ${resource}` : ""}
        </p>
        {appt.cancellation_reason ? (
          <p className="muted">
            {t(lang, "cancellationReason")}: {appt.cancellation_reason}
          </p>
        ) : null}
        {needsConfirmation ? (
          <p className="advisory" role="status">
            {t(lang, "pleaseConfirmAttendance")}
            {appt.confirmation_due_at
              ? ` (${t(lang, "by")} ${formatDateTime(lang, appt.confirmation_due_at)})`
              : ""}
          </p>
        ) : null}
        {active && appt.patient_confirmed_at ? (
          <p className="muted">
            {t(lang, "attendanceConfirmedAt")}{" "}
            {formatDateTime(lang, appt.patient_confirmed_at)}
          </p>
        ) : null}
        {transport ? (
          <p className="muted" data-testid="transport-status">
            {t(lang, "transport")}:{" "}
            <StatusBadge
              label={transportStatusLabel(lang, transport.status)}
              tone={transportActive ? "ok" : "neutral"}
            />
            {transport.vehicle_name ? ` · ${transport.vehicle_name}` : ""}
            {transport.pickup_window_start
              ? ` · ${t(lang, "pickupWindow")} ${formatDateTime(lang, transport.pickup_window_start)}`
              : ""}
          </p>
        ) : null}
      </div>
      <div className="actions wrap">
        {needsConfirmation ? (
          <button
            type="button"
            disabled={busy}
            onClick={() => onConfirm(appt)}
            data-testid="confirm-attendance"
          >
            {t(lang, "confirmAttendance")}
          </button>
        ) : null}
        {active ? (
          <a
            className="button secondary"
            href={patientAccessApi.icsPath(appt.id, lang)}
            download
          >
            {t(lang, "addToCalendar")}
          </a>
        ) : null}
        {active && upcoming ? (
          <>
            <button
              type="button"
              className="secondary"
              disabled={busy}
              onClick={() => onReschedule(appt)}
            >
              {t(lang, "reschedule")}
            </button>
            <button
              type="button"
              className="secondary"
              disabled={busy}
              aria-expanded={panel === "cancel"}
              onClick={() => setPanel(panel === "cancel" ? "none" : "cancel")}
            >
              {t(lang, "cancelAppointment")}
            </button>
            {!transport || !transportActive ? (
              <button
                type="button"
                className="secondary"
                disabled={busy}
                aria-expanded={panel === "transport"}
                onClick={() =>
                  setPanel(panel === "transport" ? "none" : "transport")
                }
              >
                {t(lang, "requestTransport")}
              </button>
            ) : (
              <button
                type="button"
                className="secondary"
                disabled={busy}
                aria-expanded={panel === "cancelTransport"}
                onClick={() =>
                  setPanel(
                    panel === "cancelTransport" ? "none" : "cancelTransport",
                  )
                }
              >
                {t(lang, "cancelTransport")}
              </button>
            )}
          </>
        ) : null}
        <button
          type="button"
          className="secondary"
          aria-expanded={panel === "history"}
          onClick={() => setPanel(panel === "history" ? "none" : "history")}
        >
          {t(lang, "history")}
        </button>
      </div>
      {panel === "cancel" ? (
        <ConfirmBox
          lang={lang}
          title={t(lang, "cancelAppointment")}
          busy={busy}
          confirmLabel={t(lang, "cancelAppointment")}
          onConfirm={() => onCancel(appt, reason, note.trim())}
          onCancel={() => setPanel("none")}
        >
          <p className="muted">{t(lang, "cancelPolicyHelp")}</p>
          <label htmlFor={`cr-${appt.id}`}>
            {t(lang, "cancellationReason")}
          </label>
          <select
            id={`cr-${appt.id}`}
            value={reason}
            onChange={(e) =>
              setReason(e.target.value as (typeof CANCEL_REASONS)[number])
            }
          >
            {CANCEL_REASONS.map((c) => (
              <option key={c} value={c}>
                {cancelReasonLabel(lang, c)}
              </option>
            ))}
          </select>
          <ReasonField
            id={`cn-${appt.id}`}
            lang={lang}
            value={note}
            onChange={setNote}
            label={t(lang, "noteOptional")}
          />
        </ConfirmBox>
      ) : null}
      {panel === "transport" ? (
        <ConfirmBox
          lang={lang}
          title={t(lang, "requestTransport")}
          busy={busy}
          confirmLabel={t(lang, "requestTransport")}
          onConfirm={() =>
            onRequestTransport(appt, {
              requirements: trReqs,
              origin_area_code: trArea.trim() || null,
              pickup_address: trAddress.trim() || null,
              note: trNote.trim() || null,
            })
          }
          onCancel={() => setPanel("none")}
        >
          <p className="muted">{t(lang, "requestTransportHelp")}</p>
          <fieldset>
            <legend>{t(lang, "accessibilityNeeds")}</legend>
            <div className="chips">
              {accessibility.entries.map((a) => (
                <label key={a.code} className="chip">
                  <input
                    type="checkbox"
                    checked={trReqs.includes(a.code)}
                    onChange={(e) =>
                      setTrReqs(
                        e.target.checked
                          ? [...trReqs, a.code]
                          : trReqs.filter((x) => x !== a.code),
                      )
                    }
                  />{" "}
                  {nameFor(lang, accessibility.entries, a.code)}
                </label>
              ))}
            </div>
          </fieldset>
          <label htmlFor={`ta-${appt.id}`}>{t(lang, "originArea")}</label>
          <input
            id={`ta-${appt.id}`}
            value={trArea}
            onChange={(e) => setTrArea(e.target.value)}
            maxLength={32}
          />
          <label htmlFor={`tad-${appt.id}`}>{t(lang, "pickupAddress")}</label>
          <input
            id={`tad-${appt.id}`}
            value={trAddress}
            onChange={(e) => setTrAddress(e.target.value)}
            maxLength={200}
            autoComplete="street-address"
            aria-describedby={`tadh-${appt.id}`}
          />
          <p id={`tadh-${appt.id}`} className="muted">
            {t(lang, "pickupAddressHelp")}
          </p>
          <ReasonField
            id={`tn-${appt.id}`}
            lang={lang}
            value={trNote}
            onChange={setTrNote}
            label={t(lang, "noteOptional")}
          />
        </ConfirmBox>
      ) : null}
      {panel === "cancelTransport" && transport ? (
        <ConfirmBox
          lang={lang}
          title={t(lang, "cancelTransport")}
          busy={busy}
          onConfirm={() => onCancelTransport(transport, trCancelReason.trim())}
          onCancel={() => setPanel("none")}
        >
          <ReasonField
            id={`tcr-${appt.id}`}
            lang={lang}
            value={trCancelReason}
            onChange={setTrCancelReason}
            label={t(lang, "noteOptional")}
          />
        </ConfirmBox>
      ) : null}
      {panel === "history" ? (
        <div className="nested" data-testid="appointment-history">
          <PanelState
            lang={lang}
            state={history}
            isEmpty={(d) => d.items.length === 0}
            emptyKey="noHistory"
          >
            {(d) => (
              <ol className="timeline compact">
                {d.items.map((h, i) => (
                  <li key={`${h.version}-${i}`}>
                    <time dateTime={h.recorded_at}>
                      {formatDateTime(lang, h.recorded_at)}
                    </time>{" "}
                    — {appointmentStatusLabel(lang, h.to_status)}
                    {h.starts_at_after && h.starts_at_before
                      ? ` · ${formatDateTime(lang, h.starts_at_before)} → ${formatDateTime(lang, h.starts_at_after)}`
                      : ""}
                    {h.reason_code ? ` · ${h.reason_code}` : ""}
                  </li>
                ))}
              </ol>
            )}
          </PanelState>
        </div>
      ) : null}
    </li>
  );
}

// ---------------------------------------------------------------------------
// Appointments section
// ---------------------------------------------------------------------------

function AppointmentsSection({
  lang,
  patientId,
  onReschedule,
  refreshKey,
}: {
  lang: Lang;
  patientId: string;
  onReschedule: (a: Appointment, m: MatchResult) => void;
  refreshKey: number;
}) {
  const [range, setRange] = useState<"upcoming" | "past">("upcoming");
  const appts = useLoader(
    () =>
      apiFetch<Items<Appointment>>(
        `${ME}/appointments${query({ patient_id: patientId, range, limit: 50 })}`,
      ),
    `${patientId}:${range}:${refreshKey}`,
  );
  const transports = useLoader(
    () =>
      apiFetch<Items<TransportRequest>>(
        `${ME}/transport${query({ patient_id: patientId })}`,
      ),
    `${patientId}:${refreshKey}`,
  );
  const { busy, message, run } = useAction(lang);
  const byAppt = useMemo(() => {
    const m = new Map<string, TransportRequest>();
    for (const tr of transports.data?.items ?? []) {
      const prev = m.get(tr.appointment_id);
      if (!prev || new Date(tr.created_at) > new Date(prev.created_at)) {
        m.set(tr.appointment_id, tr);
      }
    }
    return m;
  }, [transports.data]);

  const reloadAll = () => {
    appts.reload();
    transports.reload();
  };

  return (
    <div className="card" data-testid="appointments-section">
      <div className="row-head">
        <h3 style={{ marginTop: 0 }}>{t(lang, "myAppointmentsTab")}</h3>
        <div className="segmented" role="group" aria-label={t(lang, "range")}>
          <button
            type="button"
            className={range === "upcoming" ? "" : "secondary"}
            aria-pressed={range === "upcoming"}
            onClick={() => setRange("upcoming")}
          >
            {t(lang, "upcoming")}
          </button>
          <button
            type="button"
            className={range === "past" ? "" : "secondary"}
            aria-pressed={range === "past"}
            onClick={() => setRange("past")}
          >
            {t(lang, "past")}
          </button>
        </div>
      </div>
      <MessageLine message={message} />
      <PanelState
        lang={lang}
        state={appts}
        isEmpty={(d) => d.items.length === 0}
        emptyKey={
          range === "upcoming" ? "noUpcomingAppointments" : "noPastAppointments"
        }
      >
        {(d) => (
          <ul className="stack">
            {d.items.map((a) => (
              <AppointmentCard
                key={`${a.id}:${a.version}`}
                lang={lang}
                appt={a}
                transport={byAppt.get(a.id)}
                busy={busy}
                onConfirm={(ap) =>
                  void run(async () => {
                    await postJson(`${ME}/appointments/${ap.id}/confirm`, {
                      version: ap.version,
                    });
                    reloadAll();
                  }, "attendanceConfirmed")
                }
                onReschedule={(ap) =>
                  void run(async () => {
                    const m = await postJson<MatchResult>(
                      `${ME}/appointments/${ap.id}/reschedule-options`,
                      { version: ap.version, ranking: true },
                    );
                    onReschedule(ap, m);
                  })
                }
                onCancel={(ap, reason_code, note) =>
                  void run(async () => {
                    await postJson(`${ME}/appointments/${ap.id}/cancel`, {
                      version: ap.version,
                      reason_code,
                      note: note || null,
                    });
                    reloadAll();
                  }, "appointmentCancelled")
                }
                onRequestTransport={(ap, body) =>
                  void run(async () => {
                    await postJson(`${ME}/transport`, {
                      patient_id: patientId,
                      appointment_id: ap.id,
                      ...body,
                    });
                    reloadAll();
                  }, "transportRequested")
                }
                onCancelTransport={(tr, reason) =>
                  void run(async () => {
                    await postJson(`${ME}/transport/${tr.id}/cancel`, {
                      version: tr.version,
                      reason: reason || null,
                    });
                    reloadAll();
                  }, "transportCancelled")
                }
              />
            ))}
          </ul>
        )}
      </PanelState>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Requests section
// ---------------------------------------------------------------------------

function RequestsSection({
  lang,
  patientId,
  onContinue,
  refreshKey,
}: {
  lang: Lang;
  patientId: string;
  onContinue: (r: AccessRequest) => void;
  refreshKey: number;
}) {
  const services = useCatalog("clinical_service");
  const state = useLoader(
    () =>
      apiFetch<Items<AccessRequest>>(
        `${ME}/access-requests${query({ patient_id: patientId, status: "live" })}`,
      ),
    `${patientId}:${refreshKey}`,
  );
  const { busy, message, run } = useAction(lang);
  const [withdrawing, setWithdrawing] = useState<string | null>(null);

  return (
    <div className="card" data-testid="requests-section">
      <h3 style={{ marginTop: 0 }}>{t(lang, "myRequestsTab")}</h3>
      <p className="muted">{t(lang, "myRequestsHelp")}</p>
      <MessageLine message={message} />
      <PanelState
        lang={lang}
        state={state}
        isEmpty={(d) => d.items.length === 0}
        emptyKey="noOpenRequests"
      >
        {(d) => (
          <ul className="stack">
            {d.items.map((r) => (
              <li key={r.id} className="row-card" data-testid="request-card">
                <div className="row-main">
                  <strong>
                    {r.constraints.service_code
                      ? nameFor(
                          lang,
                          services.entries,
                          r.constraints.service_code,
                        )
                      : t(lang, "serviceNotYetIdentified")}
                  </strong>{" "}
                  <StatusBadge
                    label={requestStatusLabel(lang, r.status)}
                    tone={
                      r.status === "needs_clinical_triage"
                        ? "warn"
                        : r.status === "options_ready"
                          ? "ok"
                          : "neutral"
                    }
                  />
                  {r.free_text ? <p>{r.free_text}</p> : null}
                  {r.status === "needs_clinical_triage" ? (
                    <p className="advisory" role="status">
                      {t(lang, "needsTriagePatient")}
                    </p>
                  ) : null}
                  {r.missing_info.length > 0 ? (
                    <p className="muted">
                      {t(lang, "missingInfo")}: {r.missing_info.join(", ")}
                    </p>
                  ) : null}
                  <p className="muted">{formatDateTime(lang, r.updated_at)}</p>
                </div>
                <div className="actions">
                  {r.status !== "needs_clinical_triage" ? (
                    <button
                      type="button"
                      disabled={busy}
                      onClick={() => onContinue(r)}
                    >
                      {t(lang, "continueRequest")}
                    </button>
                  ) : null}
                  <button
                    type="button"
                    className="secondary"
                    disabled={busy}
                    onClick={() => setWithdrawing(r.id)}
                  >
                    {t(lang, "withdrawRequest")}
                  </button>
                </div>
                {withdrawing === r.id ? (
                  <ConfirmBox
                    lang={lang}
                    title={t(lang, "withdrawRequest")}
                    busy={busy}
                    onConfirm={() =>
                      void run(async () => {
                        await postJson(
                          `${ME}/access-requests/${r.id}/withdraw`,
                          {
                            version: r.version,
                          },
                        );
                        setWithdrawing(null);
                        state.reload();
                      }, "requestWithdrawn")
                    }
                    onCancel={() => setWithdrawing(null)}
                  />
                ) : null}
              </li>
            ))}
          </ul>
        )}
      </PanelState>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Page
// ---------------------------------------------------------------------------

export default function MyAppointmentsPage() {
  const { lang, meta, authenticated } = useSession();
  const me = useLoader(() => apiFetch<Me>(ME), "me", authenticated === true);
  const [patientId, setPatientId] = useState<string | null>(null);
  const [tab, setTab] = useState<Tab>("appointments");
  const [refreshKey, setRefreshKey] = useState(0);
  const [findSeed, setFindSeed] = useState<{
    key: number;
    request?: AccessRequest | null;
    result?: MatchResult | null;
    rescheduleOf?: Appointment | null;
  }>({ key: 0 });
  const tabRefs = useRef<Array<HTMLButtonElement | null>>([]);

  const patients: PatientOption[] = useMemo(
    () =>
      (me.data?.patients ?? []).map((p) => ({
        id: p.patient_id,
        given_name: p.patient.given_name,
        family_name: p.patient.family_name,
        label: `${p.patient.given_name} ${p.patient.family_name}`,
      })),
    [me.data],
  );

  useEffect(() => {
    if (!patientId && patients.length > 0) setPatientId(patients[0].id);
    if (patientId && !patients.some((p) => p.id === patientId)) {
      setPatientId(patients[0]?.id ?? null);
    }
  }, [patients, patientId]);

  const selfService = meta?.scheduling_capabilities?.self_service ?? false;
  const current = me.data?.patients.find((p) => p.patient_id === patientId);

  const bump = useCallback(() => setRefreshKey((k) => k + 1), []);

  function openFind(seed: Omit<typeof findSeed, "key">) {
    setFindSeed((s) => ({ key: s.key + 1, ...seed }));
    setTab("find");
  }

  function onTabKey(e: React.KeyboardEvent, i: number) {
    const n = TABS.length;
    let next: number | null = null;
    if (e.key === "ArrowRight") next = (i + 1) % n;
    else if (e.key === "ArrowLeft") next = (i - 1 + n) % n;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = n - 1;
    if (next === null) return;
    e.preventDefault();
    setTab(TABS[next]);
    tabRefs.current[next]?.focus();
  }

  return (
    <AppShell>
      <div className="my-appointments" data-testid="my-appointments">
        {authenticated === null || (authenticated && me.loading) ? (
          <p className="muted" role="status">
            {t(lang, "loading")}
          </p>
        ) : !authenticated ? (
          <p className="advisory" role="status">
            {t(lang, "signInRequired")}
          </p>
        ) : !selfService || me.denied ? (
          <div className="card" data-testid="self-service-unauthorized">
            <h2>{t(lang, "navMyAppointments")}</h2>
            <p role="status">{t(lang, "selfServiceUnauthorized")}</p>
          </div>
        ) : me.error ? (
          <p className="error" role="alert">
            {me.error}
          </p>
        ) : patients.length === 0 ? (
          <div className="card" data-testid="no-grants">
            <h2>{t(lang, "navMyAppointments")}</h2>
            <p role="status">{t(lang, "noPatientGrants")}</p>
          </div>
        ) : (
          <>
            <header className="page-head">
              <div>
                <h2 style={{ marginBottom: 4 }}>
                  {t(lang, "navMyAppointments")}
                </h2>
                <p className="muted">{t(lang, "myAppointmentsIntro")}</p>
              </div>
              {patients.length > 1 ? (
                <div>
                  <label htmlFor="me-patient">{t(lang, "managingFor")}</label>
                  <select
                    id="me-patient"
                    value={patientId ?? ""}
                    onChange={(e) => setPatientId(e.target.value)}
                    data-testid="patient-switcher"
                  >
                    {(me.data?.patients ?? []).map((p) => (
                      <option key={p.patient_id} value={p.patient_id}>
                        {p.patient.given_name} {p.patient.family_name}
                        {p.relationship !== "self"
                          ? ` (${t(
                              lang,
                              p.relationship === "parent_guardian"
                                ? "relationshipParentGuardian"
                                : "relationshipAuthorizedProxy",
                            )})`
                          : ""}
                      </option>
                    ))}
                  </select>
                </div>
              ) : current && current.relationship !== "self" ? (
                <p className="muted" data-testid="managing-for">
                  {t(lang, "managingFor")}: {current.patient.given_name}{" "}
                  {current.patient.family_name}
                </p>
              ) : null}
            </header>
            {meta?.environment?.synthetic_data ? (
              <p className="advisory synthetic-notice" role="status">
                {t(lang, "syntheticDataNotice")}
              </p>
            ) : null}
            <div className="hero-action">
              <button
                type="button"
                className="primary-cta"
                onClick={() => openFind({})}
                data-testid="find-best-appointment"
              >
                {t(lang, "findBestAppointment")}
              </button>
            </div>
            <div
              className="tabs scrollable"
              role="tablist"
              aria-label={t(lang, "navMyAppointments")}
            >
              {TABS.map((tb, i) => (
                <button
                  key={tb}
                  ref={(el) => {
                    tabRefs.current[i] = el;
                  }}
                  type="button"
                  role="tab"
                  id={`me-tab-${tb}`}
                  aria-selected={tab === tb}
                  aria-controls={`me-panel-${tb}`}
                  tabIndex={tab === tb ? 0 : -1}
                  className={tab === tb ? "tab active" : "tab"}
                  onClick={() => setTab(tb)}
                  onKeyDown={(e) => onTabKey(e, i)}
                >
                  {t(lang, TAB_KEY[tb])}
                </button>
              ))}
            </div>
            {patientId ? (
              <div
                role="tabpanel"
                id={`me-panel-${tab}`}
                aria-labelledby={`me-tab-${tab}`}
                className="tab-panel"
              >
                {tab === "appointments" ? (
                  <AppointmentsSection
                    lang={lang}
                    patientId={patientId}
                    refreshKey={refreshKey}
                    onReschedule={(a, m) =>
                      openFind({ result: m, rescheduleOf: a })
                    }
                  />
                ) : null}
                {tab === "find" ? (
                  <div className="card">
                    <FindAppointment
                      key={`${patientId}:${findSeed.key}`}
                      lang={lang}
                      api={patientAccessApi}
                      mode="patient"
                      patients={patients.filter((p) => p.id === patientId)}
                      initialPatientId={patientId}
                      initialRequest={findSeed.request ?? null}
                      initialResult={findSeed.result ?? null}
                      rescheduleOf={findSeed.rescheduleOf ?? null}
                      headingId="me-find-h"
                      onBooked={() => {
                        bump();
                        setTab("appointments");
                      }}
                      onRequestChanged={bump}
                    />
                  </div>
                ) : null}
                {tab === "requests" ? (
                  <RequestsSection
                    lang={lang}
                    patientId={patientId}
                    refreshKey={refreshKey}
                    onContinue={(r) => openFind({ request: r })}
                  />
                ) : null}
                {tab === "waitlist" ? (
                  <WaitlistSection
                    lang={lang}
                    patientId={patientId}
                    onBooked={bump}
                  />
                ) : null}
                {tab === "preferences" ? (
                  <PreferencesSection lang={lang} patientId={patientId} />
                ) : null}
                {tab === "notifications" ? (
                  <NotificationsSection lang={lang} patientId={patientId} />
                ) : null}
              </div>
            ) : null}
          </>
        )}
      </div>
    </AppShell>
  );
}
