"use client";

/**
 * `/my/care` — the patient/representative view of active care. Shows only
 * released, patient-appropriate operational information (unit, expected
 * next step, patient-visible tasks, appointments, procedures, transport);
 * staff-only coordination is never sent by the API.
 */
import { useEffect, useMemo, useState } from "react";
import { AppShell } from "../../chrome";
import { t, type Lang } from "@/lib/i18n";
import { apiFetch, useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import { loadMyCare, type MyCare, type MyCareEpisode } from "@/lib/care-ops";
import { notificationKindLabel } from "@/lib/access";
import type { Me } from "@/lib/access";
import { Card, CardHead, EmptyState, Pill } from "@/components/ui/primitives";
import { PanelState, useLoader } from "../../scheduling/shared";
import { Freshness, StatusPill } from "../../operations/shared";

function pick(lang: Lang, en: string, es: string): string {
  return lang === "es" ? es || en : en || es;
}

function nextStepText(lang: Lang, e: MyCareEpisode): string {
  const n = e.next_step;
  if (!n) return t(lang, "myCareNextStepUnknown");
  if (n.procedure_code) {
    return `${n.procedure_code.replaceAll("_", " ")}${
      n.scheduled_start ? ` · ${formatDateTime(lang, n.scheduled_start)}` : ""
    }`;
  }
  const text = pick(lang, n.text_en ?? "", n.text_es ?? "");
  return text || t(lang, "myCareNextStepUnknown");
}

export function MyCareEpisodes({ lang, data }: { lang: Lang; data: MyCare }) {
  const facility = data.contact?.facility_name ?? "";
  return (
    <div className="stack" data-testid="mycare-episodes">
      <Freshness lang={lang} generatedAt={data.generated_at} />
      {data.episodes.map((e) => {
        const dischargePrep =
          e.status === "discharge_preparation" ||
          e.status === "discharge_ready" ||
          e.pending_tasks.some((task) => task.discharge_preparation);
        return (
          <Card key={e.episode_id} data-testid="mycare-episode">
            <CardHead
              title={e.episode_type_code.replaceAll("_", " ")}
              eyebrow={
                <>
                  <StatusPill status={e.status} /> {facility}
                </>
              }
            />
            <dl className="kv">
              <div>
                <dt>{t(lang, "myCareLocation")}</dt>
                <dd data-testid="mycare-location">
                  {e.location
                    ? [
                        pick(
                          lang,
                          e.location.unit_en ?? "",
                          e.location.unit_es ?? "",
                        ),
                        pick(
                          lang,
                          e.location.position_en,
                          e.location.position_es,
                        ),
                      ]
                        .filter(Boolean)
                        .join(" · ")
                    : t(lang, "myCareNoPlacement")}
                </dd>
              </div>
              <div>
                <dt>{t(lang, "myCareSince")}</dt>
                <dd>
                  {e.started_at ? formatDateTime(lang, e.started_at) : "—"}
                </dd>
              </div>
              <div>
                <dt>{t(lang, "myCareNextStep")}</dt>
                <dd>{nextStepText(lang, e)}</dd>
              </div>
              {dischargePrep ? (
                <div>
                  <dt>{t(lang, "myCareDischargePrep")}</dt>
                  <dd>{t(lang, "myCareDischargePrepText")}</dd>
                </div>
              ) : null}
            </dl>
            <h3>{t(lang, "myCarePending")}</h3>
            {e.pending_tasks.length === 0 ? (
              <p className="muted">{t(lang, "myCareNoPending")}</p>
            ) : (
              <ul>
                {e.pending_tasks.map((task) => (
                  <li key={task.id}>
                    {pick(lang, task.text_en ?? "", task.text_es ?? "") ||
                      t(lang, "myCareNextStepUnknown")}{" "}
                    <StatusPill status={task.status} />
                    {task.due_at ? (
                      <span className="muted small">
                        {" "}
                        · {formatDateTime(lang, task.due_at)}
                      </span>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
            {e.procedures.length > 0 ? (
              <>
                <h3>{t(lang, "myCareProcedures")}</h3>
                <ul>
                  {e.procedures.map((p) => (
                    <li key={p.id}>
                      {p.procedure_code.replaceAll("_", " ")}{" "}
                      {p.scheduled_start
                        ? formatDateTime(lang, p.scheduled_start)
                        : ""}{" "}
                      <StatusPill status={p.status} />
                    </li>
                  ))}
                </ul>
              </>
            ) : null}
          </Card>
        );
      })}
      {data.appointments.length > 0 ? (
        <Card>
          <CardHead title={t(lang, "myCareAppointments")} />
          <ul>
            {data.appointments.map((a) => (
              <li key={a.id}>
                {formatDateTime(lang, a.starts_at)} ·{" "}
                {a.service_code.replaceAll("_", " ")}
              </li>
            ))}
          </ul>
        </Card>
      ) : null}
      {data.transport.length > 0 ? (
        <Card>
          <CardHead title={t(lang, "myCareTransport")} />
          <ul>
            {data.transport.map((tr) => (
              <li key={tr.id}>
                <StatusPill status={tr.status} />{" "}
                {tr.pickup_window_start
                  ? formatDateTime(lang, tr.pickup_window_start)
                  : ""}
              </li>
            ))}
          </ul>
        </Card>
      ) : null}
      <Card>
        <CardHead title={t(lang, "myCareNotifications")} />
        {data.notifications.length === 0 ? (
          <p className="muted">{t(lang, "myCareNoNotifications")}</p>
        ) : (
          <ul data-testid="mycare-notifications">
            {data.notifications.map((n) => (
              <li key={n.id}>
                {notificationKindLabel(lang, n.kind)}{" "}
                <span className="muted small">
                  {formatDateTime(lang, n.delivered_at ?? n.created_at)}
                </span>
              </li>
            ))}
          </ul>
        )}
      </Card>
      <p className="muted small">
        <Pill tone="neutral" outline>
          {t(lang, "myCareNotCleared")}
        </Pill>
      </p>
    </div>
  );
}

function MyCareView() {
  const { lang } = useSession();
  const me = useLoader(() => apiFetch<Me>("/api/v1/me"), "me");
  const [patientId, setPatientId] = useState<string | null>(null);
  const patients = useMemo(() => me.data?.patients ?? [], [me.data]);
  useEffect(() => {
    if (!patientId && patients.length > 0) setPatientId(patients[0].patient_id);
    if (patientId && !patients.some((p) => p.patient_id === patientId)) {
      setPatientId(patients[0]?.patient_id ?? null);
    }
  }, [patients, patientId]);
  const care = useLoader(
    () => loadMyCare(patientId ?? undefined),
    patientId ?? "",
    me.data !== null,
  );
  return (
    <section className="page" aria-labelledby="mycare-title">
      <header className="page-head">
        <h1 id="mycare-title">{t(lang, "myCareTitle")}</h1>
        <p className="muted">{t(lang, "myCareIntro")}</p>
        {patients.length > 1 ? (
          <label className="field inline">
            <span>{t(lang, "patient")}</span>
            <select
              value={patientId ?? ""}
              onChange={(e) => setPatientId(e.target.value)}
              data-testid="mycare-patient"
            >
              {patients.map((p) => (
                <option key={p.patient_id} value={p.patient_id}>
                  {p.patient.given_name} {p.patient.family_name}
                </option>
              ))}
            </select>
          </label>
        ) : null}
      </header>
      <PanelState
        lang={lang}
        state={care}
        emptyKey="myCareNone"
        isEmpty={(d: MyCare) =>
          d.episodes.length === 0 &&
          d.appointments.length === 0 &&
          d.notifications.length === 0
        }
      >
        {(d: MyCare) => <MyCareEpisodes lang={lang} data={d} />}
      </PanelState>
      {me.data && patients.length === 0 ? (
        <EmptyState inline title={t(lang, "myCareNone")} />
      ) : null}
    </section>
  );
}

export default function MyCarePage() {
  return (
    <AppShell>
      <MyCareView />
    </AppShell>
  );
}
