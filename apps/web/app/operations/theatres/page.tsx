"use client";

/**
 * `/operations/theatres` — perioperative cases with deterministic readiness
 * (theatre-conflicts.v1). Humans schedule and override; dMind may only
 * explain a session's deterministic plan.
 */
import { useEffect, useState } from "react";
import { AppShell } from "../../chrome";
import { t, type Lang } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import {
  loadCase,
  loadCases,
  loadLocations,
  scheduleCase,
  type OperationalLocation,
  type TheatreCaseListItem,
  type Readiness,
  type TheatreCase,
} from "@/lib/care-ops";
import { Card, CardHead, EmptyState, Pill } from "@/components/ui/primitives";
import {
  MessageLine,
  PanelState,
  useAction,
  useLoader,
} from "../../scheduling/shared";
import {
  FacilityPicker,
  StatusPill,
  fill,
  useCareOpsCaps,
  useFacility,
} from "../shared";

export function ReadinessView({
  lang,
  readiness,
}: {
  lang: Lang;
  readiness: Readiness | null;
}) {
  if (!readiness) {
    return (
      <div data-testid="theatre-readiness">
        <p className="muted small">{t(lang, "theatreNotScheduled")}</p>
      </div>
    );
  }
  return (
    <div data-testid="theatre-readiness">
      <p>
        <Pill tone={readiness.feasible ? "ok" : "critical"} icon>
          {readiness.feasible
            ? t(lang, "theatreReady")
            : t(lang, "theatreNotReady")}
        </Pill>{" "}
        <span className="muted small">{readiness.engine_version}</span>
      </p>
      {readiness.conflicts.length === 0 ? (
        <p className="muted small">
          {fill(t(lang, "theatreNoConflicts"), {
            engine: readiness.engine_version,
          })}
        </p>
      ) : (
        <ul>
          {readiness.conflicts.map((c, i) => (
            <li key={`${c.code}-${i}`}>
              <Pill tone={c.blocking ? "critical" : "warn"} icon>
                {c.blocking
                  ? t(lang, "theatreBlocking")
                  : t(lang, "theatreAdvisory")}
              </Pill>{" "}
              <code>{c.code}</code> — {c.detail}
            </li>
          ))}
        </ul>
      )}
    </div>
  );
}

const SCHEDULABLE = new Set([
  "requested",
  "ready_for_scheduling",
  "scheduled",
  "delayed",
]);

function toLocalInput(iso: string): string {
  const d = new Date(iso);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(
    d.getHours(),
  )}:${pad(d.getMinutes())}`;
}

/**
 * Human booking form. The server re-runs theatre-conflicts.v1 inside the
 * scheduling transaction; this form only carries the coordinator's decision.
 */
function ScheduleForm({
  lang,
  theatreCase,
  theatres,
  onScheduled,
}: {
  lang: Lang;
  theatreCase: TheatreCase;
  theatres: OperationalLocation[];
  onScheduled: () => Promise<void>;
}) {
  const action = useAction(lang);
  const [theatreId, setTheatreId] = useState(
    theatreCase.theatre_location_id ?? "",
  );
  const [start, setStart] = useState(
    theatreCase.scheduled_start
      ? toLocalInput(theatreCase.scheduled_start)
      : "",
  );
  const [override, setOverride] = useState("");
  useEffect(() => {
    setTheatreId(theatreCase.theatre_location_id ?? "");
    setStart(
      theatreCase.scheduled_start
        ? toLocalInput(theatreCase.scheduled_start)
        : "",
    );
    setOverride("");
  }, [
    theatreCase.id,
    theatreCase.theatre_location_id,
    theatreCase.scheduled_start,
  ]);
  if (theatres.length === 0) {
    return <p className="muted small">{t(lang, "theatreNoRooms")}</p>;
  }
  const chosen = theatres.find((x) => x.id === theatreId);
  return (
    <form
      className="stack"
      data-testid="theatre-schedule"
      aria-label={t(lang, "theatreSchedule")}
      onSubmit={(e) => {
        e.preventDefault();
        if (!chosen || !start) return;
        void action.run(async () => {
          await scheduleCase(theatreCase.id, {
            expected_version: theatreCase.version,
            theatre_location_id: chosen.id,
            expected_theatre_version: chosen.version,
            scheduled_start: new Date(start).toISOString(),
            override_reason: override.trim() || undefined,
          });
          await onScheduled();
        }, "theatreScheduled_ok");
      }}
    >
      <h4>{t(lang, "theatreSchedule")}</h4>
      <p className="muted small">{t(lang, "theatreScheduleIntro")}</p>
      <div className="grid two">
        <label className="field">
          <span>{t(lang, "theatreRoom")}</span>
          <select
            required
            value={theatreId}
            onChange={(e) => setTheatreId(e.target.value)}
          >
            <option value="">{t(lang, "theatreSelectRoom")}</option>
            {theatres.map((x) => (
              <option key={x.id} value={x.id}>
                {lang === "es" ? x.name_es : x.name_en}
              </option>
            ))}
          </select>
        </label>
        <label className="field">
          <span>{t(lang, "theatreStart")}</span>
          <input
            required
            type="datetime-local"
            value={start}
            onChange={(e) => setStart(e.target.value)}
          />
        </label>
        <label className="field">
          <span>{t(lang, "theatreOverrideReason")}</span>
          <input
            value={override}
            onChange={(e) => setOverride(e.target.value)}
            maxLength={200}
          />
        </label>
      </div>
      <button type="submit" className="secondary" disabled={action.busy}>
        {t(lang, "theatreScheduleSubmit")}
      </button>
      <MessageLine message={action.message} />
    </form>
  );
}

function CaseDetail({
  lang,
  id,
  theatres,
  canSchedule,
  onChanged,
}: {
  lang: Lang;
  id: string;
  theatres: OperationalLocation[];
  canSchedule: boolean;
  onChanged: () => Promise<void>;
}) {
  const detail = useLoader(() => loadCase(id), id);
  return (
    <PanelState
      lang={lang}
      state={detail}
      emptyKey="theatreNoCases"
      isEmpty={() => false}
    >
      {(d) => (
        <div className="stack">
          <dl className="kv">
            <div>
              <dt>{t(lang, "theatreProcedure")}</dt>
              <dd>{d.case.procedure_code}</dd>
            </div>
            <div>
              <dt>{t(lang, "flowService")}</dt>
              <dd>{d.case.responsible_service_code ?? "—"}</dd>
            </div>
            <div>
              <dt>{t(lang, "theatreScheduled")}</dt>
              <dd>
                {d.case.scheduled_start
                  ? `${formatDateTime(lang, d.case.scheduled_start)} – ${
                      d.case.scheduled_end
                        ? formatDateTime(lang, d.case.scheduled_end)
                        : ""
                    }`
                  : t(lang, "theatreUnscheduled")}
              </dd>
            </div>
            <div>
              <dt>{t(lang, "theatreDuration")}</dt>
              <dd>{d.case.expected_duration_minutes} min</dd>
            </div>
            <div>
              <dt>{t(lang, "theatreEquipment")}</dt>
              <dd>{d.case.required_equipment_codes.join(", ") || "—"}</dd>
            </div>
          </dl>
          <h4>{t(lang, "theatreReadiness")}</h4>
          <ReadinessView lang={lang} readiness={d.readiness} />
          {canSchedule && SCHEDULABLE.has(d.case.status) ? (
            <ScheduleForm
              lang={lang}
              theatreCase={d.case}
              theatres={theatres}
              onScheduled={async () => {
                await Promise.all([detail.reload(), onChanged()]);
              }}
            />
          ) : null}
          <p className="muted small">{t(lang, "theatreExplainIntro")}</p>
        </div>
      )}
    </PanelState>
  );
}

function Theatres() {
  const { lang } = useSession();
  const { facilityId, setFacilityId, facilities } = useFacility();
  const caps = useCareOpsCaps();
  const cases = useLoader(
    () => loadCases(facilityId ?? ""),
    facilityId ?? "",
    Boolean(facilityId),
  );
  const rooms = useLoader(
    () => loadLocations(facilityId ?? ""),
    facilityId ?? "",
    Boolean(facilityId) && caps.can_manage_theatre,
  );
  const theatres = (rooms.data?.locations ?? []).filter(
    (l) => l.type_code === "theatre" && l.active,
  );
  const [selected, setSelected] = useState<string | null>(null);
  return (
    <section className="page" aria-labelledby="theatre-title">
      <header className="page-head">
        <h1 id="theatre-title">{t(lang, "theatreTitle")}</h1>
        <p className="muted">{t(lang, "theatreIntro")}</p>
        <FacilityPicker
          lang={lang}
          facilityId={facilityId}
          facilities={facilities}
          onChange={setFacilityId}
        />
      </header>
      {!facilityId ? (
        <p className="muted">{t(lang, "opsSelectFacility")}</p>
      ) : (
        <div className="grid two">
          <Card>
            <CardHead title={t(lang, "theatreCases")} />
            <PanelState
              lang={lang}
              state={cases}
              emptyKey="theatreNoCases"
              isEmpty={(d) => d.cases.length === 0}
            >
              {(d) => (
                <ul className="plain" role="list" data-testid="theatre-cases">
                  {d.cases.map(
                    ({ case: c, readiness }: TheatreCaseListItem) => (
                      <li key={c.id}>
                        <button
                          type="button"
                          className={`listrow${selected === c.id ? " selected" : ""}`}
                          aria-pressed={selected === c.id}
                          onClick={() => setSelected(c.id)}
                        >
                          <strong>{c.procedure_code}</strong>{" "}
                          <StatusPill status={c.status} />{" "}
                          <Pill
                            tone={
                              c.priority === "urgent" ||
                              c.priority === "emergency"
                                ? "critical"
                                : "neutral"
                            }
                            outline
                          >
                            {c.priority}
                          </Pill>
                          <span className="muted small">
                            {" "}
                            {c.scheduled_start
                              ? formatDateTime(lang, c.scheduled_start)
                              : t(lang, "theatreUnscheduled")}
                          </span>
                        </button>
                      </li>
                    ),
                  )}
                </ul>
              )}
            </PanelState>
          </Card>
          <Card>
            <CardHead title={t(lang, "theatreReadiness")} />
            {selected ? (
              <CaseDetail
                lang={lang}
                id={selected}
                theatres={theatres}
                canSchedule={caps.can_manage_theatre}
                onChanged={() => cases.reload()}
              />
            ) : (
              <EmptyState inline title={t(lang, "theatreCases")} />
            )}
          </Card>
        </div>
      )}
    </section>
  );
}

export default function TheatresPage() {
  return (
    <AppShell>
      <Theatres />
    </AppShell>
  );
}
