"use client";

import Link from "next/link";
import { useState } from "react";
import { AppShell } from "../chrome";
import { t, type Lang, type TKey } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import {
  NO_SCHEDULING_CAPABILITIES,
  type AccessRequest,
  type Appointment,
} from "@/lib/access";
import { Agenda } from "./agenda";
import {
  FindAppointment,
  staffAccessApi,
  type PatientOption,
} from "./find-appointment";
import { CapacityPanel, TransportPanel, WaitlistPanel } from "./panels";
import {
  AppointmentsPanel,
  HoldsPanel,
  PatientPicker,
  RequestsPanel,
} from "./worklists";

type Tab =
  | "find"
  | "agenda"
  | "requests"
  | "holds"
  | "appointments"
  | "waitlist"
  | "capacity"
  | "transport";

const TABS: { id: Tab; label: TKey }[] = [
  { id: "find", label: "findBestAppointment" },
  { id: "agenda", label: "agendaTitle" },
  { id: "requests", label: "pendingRequests" },
  { id: "holds", label: "activeHolds" },
  { id: "appointments", label: "appointments" },
  { id: "waitlist", label: "waitlistRecovery" },
  { id: "capacity", label: "capacityPressure" },
  { id: "transport", label: "transportCoordination" },
];

function FindStage({
  lang,
  request,
  reschedule,
  patient,
  onPickPatient,
  onDone,
}: {
  lang: Lang;
  request: AccessRequest | null;
  reschedule: Appointment | null;
  patient: PatientOption | null;
  onPickPatient: (p: PatientOption | null) => void;
  onDone: () => void;
}) {
  if (!request && !reschedule && !patient) {
    return (
      <section className="card" aria-labelledby="find-h">
        <h3 id="find-h">{t(lang, "findBestAppointment")}</h3>
        <p className="muted">{t(lang, "findIntroStaff")}</p>
        <PatientPicker lang={lang} onPick={onPickPatient} />
      </section>
    );
  }
  const patients: PatientOption[] = patient
    ? [patient]
    : request?.patient
      ? [
          {
            ...request.patient,
            id: request.patient_id,
            label: `${request.patient.family_name}, ${request.patient.given_name}`,
          },
        ]
      : reschedule?.patient
        ? [
            {
              ...reschedule.patient,
              id: reschedule.patient_id,
              label: `${reschedule.patient.family_name}, ${reschedule.patient.given_name}`,
            },
          ]
        : [];
  return (
    <section className="card" aria-labelledby="find-h">
      <div className="row-head">
        <h3 id="find-h">{t(lang, "findBestAppointment")}</h3>
        <button type="button" className="secondary" onClick={onDone}>
          {t(lang, "startOver")}
        </button>
      </div>
      <FindAppointment
        key={`${request?.id ?? ""}:${reschedule?.id ?? ""}:${patient?.id ?? ""}`}
        lang={lang}
        api={staffAccessApi}
        mode="staff"
        patients={patients}
        initialPatientId={patient?.id ?? reschedule?.patient_id ?? null}
        initialRequest={request}
        rescheduleOf={reschedule}
        onBooked={onDone}
        headingId="find-h"
      />
    </section>
  );
}

function SchedulingConsole() {
  const { lang, meta } = useSession();
  const caps = meta?.scheduling_capabilities ?? NO_SCHEDULING_CAPABILITIES;
  const facilities = meta?.facilities.filter((f) => f.accessible) ?? [];
  const [tab, setTab] = useState<Tab>("find");
  const [facilityId, setFacilityId] = useState("");
  const [refresh, setRefresh] = useState(0);
  const [openRequest, setOpenRequest] = useState<AccessRequest | null>(null);
  const [reschedule, setReschedule] = useState<Appointment | null>(null);
  const [patient, setPatient] = useState<PatientOption | null>(null);
  const bump = () => setRefresh((n) => n + 1);

  if (meta && !caps.can_read) {
    return (
      <div className="card">
        <p role="alert" className="error">
          {t(lang, "noSchedulingAccess")}
        </p>
      </div>
    );
  }

  const visible = TABS.filter((x) => {
    if (x.id === "find") return caps.can_manage;
    if (x.id === "waitlist") return caps.can_manage_waitlist || caps.can_read;
    if (x.id === "capacity") return caps.can_review_capacity;
    if (x.id === "transport")
      return caps.can_coordinate_transport || caps.can_read;
    return true;
  });
  const active = visible.some((x) => x.id === tab) ? tab : visible[0]?.id;

  return (
    <>
      <div className="row-head">
        <div>
          <h2 style={{ marginTop: 0 }}>{t(lang, "schedulingTitle")}</h2>
          <p className="muted">{t(lang, "schedulingIntro")}</p>
        </div>
        <div className="actions">
          {caps.can_manage_catalog ? (
            <Link href="/scheduling/catalog" className="button secondary">
              {t(lang, "catalogAdmin")}
            </Link>
          ) : null}
          {caps.can_manage_resources ? (
            <Link href="/scheduling/resources" className="button secondary">
              {t(lang, "resourceAdmin")}
            </Link>
          ) : null}
        </div>
      </div>
      {meta?.environment?.synthetic_data ? (
        <p className="advisory synthetic-notice" role="status">
          {t(lang, "syntheticDataNotice")}
        </p>
      ) : null}
      {caps.can_manage && active !== "find" ? (
        <p>
          <button
            type="button"
            className="primary-cta"
            data-testid="find-cta"
            onClick={() => setTab("find")}
          >
            {t(lang, "findBestAppointment")}
          </button>
        </p>
      ) : null}
      <div
        className="tabs"
        role="tablist"
        aria-label={t(lang, "schedulingTitle")}
      >
        {visible.map((x) => (
          <button
            key={x.id}
            type="button"
            role="tab"
            id={`stab-${x.id}`}
            aria-selected={active === x.id}
            aria-controls="scheduling-panel"
            tabIndex={active === x.id ? 0 : -1}
            onClick={() => setTab(x.id)}
            onKeyDown={(e) => {
              const idx = visible.findIndex((v) => v.id === x.id);
              if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
                e.preventDefault();
                const n =
                  (idx + (e.key === "ArrowRight" ? 1 : -1) + visible.length) %
                  visible.length;
                setTab(visible[n].id);
                document.getElementById(`stab-${visible[n].id}`)?.focus();
              }
            }}
          >
            {t(lang, x.label)}
          </button>
        ))}
      </div>
      {facilities.length > 1 && active !== "find" && active !== "agenda" ? (
        <div className="filters">
          <div>
            <label htmlFor="sched-facility">{t(lang, "facility")}</label>
            <select
              id="sched-facility"
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
      <div
        id="scheduling-panel"
        role="tabpanel"
        aria-labelledby={`stab-${active}`}
      >
        {active === "find" ? (
          <FindStage
            lang={lang}
            request={openRequest}
            reschedule={reschedule}
            patient={patient}
            onPickPatient={setPatient}
            onDone={() => {
              setOpenRequest(null);
              setReschedule(null);
              setPatient(null);
              bump();
            }}
          />
        ) : null}
        {active === "agenda" ? <Agenda lang={lang} /> : null}
        {active === "requests" ? (
          <RequestsPanel
            lang={lang}
            facilityId={facilityId}
            refreshKey={refresh}
            onOpen={(r) => {
              setOpenRequest(r);
              setReschedule(null);
              setPatient(null);
              setTab("find");
            }}
          />
        ) : null}
        {active === "holds" ? (
          <HoldsPanel
            lang={lang}
            facilityId={facilityId}
            refreshKey={refresh}
            onChanged={bump}
          />
        ) : null}
        {active === "appointments" ? (
          <AppointmentsPanel
            lang={lang}
            facilityId={facilityId}
            refreshKey={refresh}
            onChanged={bump}
            onReschedule={(a) => {
              setReschedule(a);
              setOpenRequest(null);
              setPatient(null);
              setTab("find");
            }}
          />
        ) : null}
        {active === "waitlist" ? (
          <WaitlistPanel
            lang={lang}
            facilityId={facilityId}
            canManage={caps.can_manage_waitlist}
          />
        ) : null}
        {active === "capacity" ? (
          <CapacityPanel
            lang={lang}
            facilityId={facilityId}
            facilities={facilities}
          />
        ) : null}
        {active === "transport" ? (
          <TransportPanel
            lang={lang}
            facilityId={facilityId}
            canCoordinate={caps.can_coordinate_transport}
          />
        ) : null}
      </div>
    </>
  );
}

export default function SchedulingPage() {
  return (
    <AppShell>
      <SchedulingConsole />
    </AppShell>
  );
}
