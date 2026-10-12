"use client";

/**
 * `/operations/theatres` — perioperative cases with deterministic readiness
 * (theatre-conflicts.v1). Humans schedule and override; dMind may only
 * explain a session's deterministic plan.
 */
import { useState } from "react";
import { AppShell } from "../../chrome";
import { t, type Lang } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import {
  loadCase,
  loadCases,
  type Readiness,
  type TheatreCase,
} from "@/lib/care-ops";
import { Card, CardHead, EmptyState, Pill } from "@/components/ui/primitives";
import { PanelState, useLoader } from "../../scheduling/shared";
import { FacilityPicker, StatusPill, fill, useFacility } from "../shared";

export function ReadinessView({
  lang,
  readiness,
}: {
  lang: Lang;
  readiness: Readiness;
}) {
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

function CaseDetail({ lang, id }: { lang: Lang; id: string }) {
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
          <p className="muted small">{t(lang, "theatreExplainIntro")}</p>
        </div>
      )}
    </PanelState>
  );
}

function Theatres() {
  const { lang } = useSession();
  const { facilityId, setFacilityId, facilities } = useFacility();
  const cases = useLoader(
    () => loadCases(facilityId ?? ""),
    facilityId ?? "",
    Boolean(facilityId),
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
                  {d.cases.map((c: TheatreCase) => (
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
                  ))}
                </ul>
              )}
            </PanelState>
          </Card>
          <Card>
            <CardHead title={t(lang, "theatreReadiness")} />
            {selected ? (
              <CaseDetail lang={lang} id={selected} />
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
