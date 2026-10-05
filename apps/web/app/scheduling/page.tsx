"use client";

import Link from "next/link";
import { useState, type ReactNode } from "react";
import { AppShell } from "../chrome";
import { t, type Lang, type TKey } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import {
  NO_SCHEDULING_CAPABILITIES,
  hasSchedulingConsoleAccess,
  type AccessRequest,
  type Appointment,
} from "@/lib/access";
import { Icon } from "@/components/ui/icons";
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

type TabDef = {
  id: Tab;
  label: TKey;
  icon: (p: { className?: string }) => ReactNode;
};

/**
 * Grouped navigation: the same eight workspaces as before, but arranged by
 * intent (plan and book / work queues / operations) instead of one flat row
 * of equal tabs. Order is preserved so keyboard traversal is unchanged.
 */
const GROUPS: { label: TKey; items: TabDef[] }[] = [
  {
    label: "schedGroupPlan",
    items: [
      { id: "find", label: "findBestAppointment", icon: Icon.Search },
      { id: "agenda", label: "agendaTitle", icon: Icon.Calendar },
    ],
  },
  {
    label: "schedGroupQueues",
    items: [
      { id: "requests", label: "pendingRequests", icon: Icon.Tasks },
      { id: "holds", label: "activeHolds", icon: Icon.Clock },
      { id: "appointments", label: "appointments", icon: Icon.Check },
      { id: "waitlist", label: "waitlistRecovery", icon: Icon.Bell },
    ],
  },
  {
    label: "schedGroupOps",
    items: [
      { id: "capacity", label: "capacityPressure", icon: Icon.Grid },
      { id: "transport", label: "transportCoordination", icon: Icon.Truck },
    ],
  },
];

function FindStage({
  lang,
  request,
  reschedule,
  patient,
  onPickPatient,
  onBooked,
  onDone,
}: {
  lang: Lang;
  request: AccessRequest | null;
  reschedule: Appointment | null;
  patient: PatientOption | null;
  onPickPatient: (p: PatientOption | null) => void;
  onBooked: () => void;
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
        onBooked={onBooked}
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

  if (meta && !hasSchedulingConsoleAccess(caps)) {
    return (
      <div className="card">
        <p role="alert" className="error">
          {t(lang, "noSchedulingAccess")}
        </p>
      </div>
    );
  }

  const allowed = (id: Tab) => {
    if (id === "find") return caps.can_manage;
    if (id === "waitlist") return caps.can_manage_waitlist || caps.can_read;
    if (id === "capacity") return caps.can_review_capacity;
    if (id === "transport")
      return caps.can_coordinate_transport || caps.can_read;
    return caps.can_read;
  };
  const groups = GROUPS.map((g) => ({
    ...g,
    items: g.items.filter((x) => allowed(x.id)),
  })).filter((g) => g.items.length > 0);
  const visible = groups.flatMap((g) => g.items);
  const active = visible.some((x) => x.id === tab) ? tab : visible[0]?.id;
  const activeDef = visible.find((x) => x.id === active);

  const move = (from: Tab, delta: number) => {
    const idx = visible.findIndex((v) => v.id === from);
    const n = (idx + delta + visible.length) % visible.length;
    setTab(visible[n].id);
    document.getElementById(`stab-${visible[n].id}`)?.focus();
  };

  const showFacilityFilter =
    facilities.length > 1 && active !== "find" && active !== "agenda";

  return (
    <>
      <div className="row-head">
        <div>
          <h2 style={{ marginTop: 0 }}>{t(lang, "schedulingTitle")}</h2>
          <p className="muted">{t(lang, "schedulingIntro")}</p>
        </div>
        {caps.can_manage && active !== "find" ? (
          <div className="actions">
            <button
              type="button"
              className="primary-cta"
              data-testid="find-cta"
              onClick={() => setTab("find")}
            >
              <Icon.Search />
              {t(lang, "findBestAppointment")}
            </button>
          </div>
        ) : null}
      </div>
      {meta?.environment?.synthetic_data ? (
        <p className="advisory synthetic-notice" role="status">
          {t(lang, "syntheticDataNotice")}
        </p>
      ) : null}
      <div className="sched-layout">
        <nav className="sched-rail" aria-label={t(lang, "schedNavLabel")}>
          <div
            role="tablist"
            aria-label={t(lang, "schedulingTitle")}
            aria-orientation="vertical"
            className="sched-tablist"
          >
            {groups.map((g) => (
              <div key={g.label} className="sched-group">
                {groups.length > 1 ? (
                  <div className="nav-group-label" aria-hidden="true">
                    {t(lang, g.label)}
                  </div>
                ) : null}
                {g.items.map((x) => {
                  const I = x.icon;
                  return (
                    <button
                      key={x.id}
                      type="button"
                      role="tab"
                      id={`stab-${x.id}`}
                      className="sched-tab"
                      aria-selected={active === x.id}
                      aria-controls="scheduling-panel"
                      tabIndex={active === x.id ? 0 : -1}
                      onClick={() => setTab(x.id)}
                      onKeyDown={(e) => {
                        if (e.key === "ArrowRight" || e.key === "ArrowDown") {
                          e.preventDefault();
                          move(x.id, 1);
                        } else if (
                          e.key === "ArrowLeft" ||
                          e.key === "ArrowUp"
                        ) {
                          e.preventDefault();
                          move(x.id, -1);
                        } else if (e.key === "Home") {
                          e.preventDefault();
                          setTab(visible[0].id);
                          document
                            .getElementById(`stab-${visible[0].id}`)
                            ?.focus();
                        } else if (e.key === "End") {
                          e.preventDefault();
                          const last = visible[visible.length - 1].id;
                          setTab(last);
                          document.getElementById(`stab-${last}`)?.focus();
                        }
                      }}
                    >
                      <I />
                      <span>{t(lang, x.label)}</span>
                    </button>
                  );
                })}
              </div>
            ))}
          </div>
          {caps.can_manage_catalog || caps.can_manage_resources ? (
            <div className="sched-group sched-admin">
              <div className="nav-group-label">
                {t(lang, "schedGroupAdmin")}
              </div>
              {caps.can_manage_catalog ? (
                <Link href="/scheduling/catalog" className="sched-tab link">
                  <Icon.Layers />
                  <span>{t(lang, "catalogAdmin")}</span>
                </Link>
              ) : null}
              {caps.can_manage_resources ? (
                <Link href="/scheduling/resources" className="sched-tab link">
                  <Icon.Settings />
                  <span>{t(lang, "resourceAdmin")}</span>
                </Link>
              ) : null}
            </div>
          ) : null}
        </nav>
        <div
          id="scheduling-panel"
          role="tabpanel"
          aria-labelledby={`stab-${active}`}
          className="sched-detail"
        >
          {activeDef || showFacilityFilter ? (
            <div className="sched-detail-head">
              {activeDef ? (
                <p className="sched-crumb muted">
                  {t(
                    lang,
                    groups.find((g) => g.items.includes(activeDef))?.label ??
                      "schedulingTitle",
                  )}
                  <Icon.ChevronRight />
                  <strong>{t(lang, activeDef.label)}</strong>
                </p>
              ) : null}
              {showFacilityFilter ? (
                <div className="filters compact">
                  <div>
                    <label htmlFor="sched-facility">
                      {t(lang, "facility")}
                    </label>
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
            </div>
          ) : null}
          {active === "find" ? (
            <FindStage
              lang={lang}
              request={openRequest}
              reschedule={reschedule}
              patient={patient}
              onPickPatient={setPatient}
              onBooked={bump}
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
