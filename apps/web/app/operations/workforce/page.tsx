"use client";

/**
 * `/operations/workforce` — shifts with configured minimum coverage and
 * deterministic gaps by profession/capability (workforce-coverage.v1).
 * Reassignments are human-approved; nothing here mutates rosters.
 */
import { useState } from "react";
import { AppShell } from "../../chrome";
import { t, type Lang } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import {
  loadShift,
  loadShifts,
  type ShiftListItem,
  type CoverageGap,
  type Shift,
} from "@/lib/care-ops";
import { Card, CardHead, EmptyState, Pill } from "@/components/ui/primitives";
import { PanelState, useLoader } from "../../scheduling/shared";
import { FacilityPicker, StatusPill, useFacility } from "../shared";

export function GapsTable({ lang, gaps }: { lang: Lang; gaps: CoverageGap[] }) {
  if (gaps.length === 0) {
    return (
      <p className="muted" role="status">
        {t(lang, "wfNoGaps")}
      </p>
    );
  }
  return (
    <table className="data" data-testid="wf-gaps">
      <thead>
        <tr>
          <th scope="col">{t(lang, "coordProfession")}</th>
          <th scope="col">{t(lang, "wfRequired")}</th>
          <th scope="col">{t(lang, "wfPresent")}</th>
          <th scope="col">{t(lang, "wfOnCall")}</th>
          <th scope="col">{t(lang, "wfShortfall")}</th>
        </tr>
      </thead>
      <tbody>
        {gaps.map((g, i) => (
          <tr key={`${g.profession_code}-${i}`}>
            <th scope="row">
              {g.profession_code}
              {g.capability_codes.length ? (
                <span className="muted small">
                  {" "}
                  · {g.capability_codes.join(", ")}
                </span>
              ) : null}
              {g.specialty_codes.length ? (
                <span className="muted small">
                  {" "}
                  · {g.specialty_codes.join(", ")}
                </span>
              ) : null}
            </th>
            <td>{g.required}</td>
            <td>{g.present}</td>
            <td>{g.on_call}</td>
            <td>
              <Pill tone={g.shortfall > 0 ? "critical" : "ok"} icon>
                {g.shortfall > 0
                  ? `${t(lang, "wfGap")} ${g.shortfall}`
                  : t(lang, "wfCovered")}
              </Pill>
            </td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

function ShiftDetail({ lang, id }: { lang: Lang; id: string }) {
  const d = useLoader(() => loadShift(id), id);
  return (
    <PanelState
      lang={lang}
      state={d}
      emptyKey="wfNoShifts"
      isEmpty={() => false}
    >
      {(s) => (
        <div className="stack">
          <p>
            <strong>{s.shift.label}</strong>{" "}
            <StatusPill status={s.shift.status} />{" "}
            <Pill tone={s.coverage.covered ? "ok" : "critical"} icon>
              {s.coverage.covered ? t(lang, "wfCovered") : t(lang, "wfGap")}
            </Pill>
          </p>
          <p className="muted small">
            {t(lang, "wfLocalDate")}: {s.shift.local_date} ·{" "}
            {formatDateTime(lang, s.shift.starts_at)} –{" "}
            {formatDateTime(lang, s.shift.ends_at)}
          </p>
          <h4>{t(lang, "wfGaps")}</h4>
          <GapsTable lang={lang} gaps={s.coverage.gaps} />
          <h4>{t(lang, "wfAssignments")}</h4>
          <p className="small">{s.assignments.length}</p>
          <h4>{t(lang, "wfReallocation")}</h4>
          {s.reallocation_candidates.length === 0 ? (
            <p className="muted small">{t(lang, "wfNoReallocation")}</p>
          ) : (
            <p className="small">{s.reallocation_candidates.length}</p>
          )}
        </div>
      )}
    </PanelState>
  );
}

function Workforce() {
  const { lang } = useSession();
  const { facilityId, setFacilityId, facilities } = useFacility();
  const shifts = useLoader(
    () => loadShifts(facilityId ?? ""),
    facilityId ?? "",
    Boolean(facilityId),
  );
  const [selected, setSelected] = useState<string | null>(null);
  return (
    <section className="page" aria-labelledby="wf-title">
      <header className="page-head">
        <h1 id="wf-title">{t(lang, "wfTitle")}</h1>
        <p className="muted">{t(lang, "wfIntro")}</p>
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
            <CardHead title={t(lang, "wfShifts")} />
            <PanelState
              lang={lang}
              state={shifts}
              emptyKey="wfNoShifts"
              isEmpty={(d) => d.shifts.length === 0}
            >
              {(d) => (
                <ul className="plain" role="list" data-testid="wf-shifts">
                  {d.shifts.map(({ shift: s, coverage }: ShiftListItem) => (
                    <li key={s.id}>
                      <button
                        type="button"
                        className={`listrow${selected === s.id ? " selected" : ""}`}
                        aria-pressed={selected === s.id}
                        onClick={() => setSelected(s.id)}
                      >
                        <strong>{s.label}</strong>{" "}
                        <StatusPill status={s.status} />
                        <span className="muted small">
                          {" "}
                          {s.local_date} · {formatDateTime(lang, s.starts_at)}
                        </span>
                      </button>
                    </li>
                  ))}
                </ul>
              )}
            </PanelState>
          </Card>
          <Card>
            <CardHead title={t(lang, "wfCoverage")} />
            {selected ? (
              <ShiftDetail lang={lang} id={selected} />
            ) : (
              <EmptyState inline title={t(lang, "wfShifts")} />
            )}
          </Card>
        </div>
      )}
    </section>
  );
}

export default function WorkforcePage() {
  return (
    <AppShell>
      <Workforce />
    </AppShell>
  );
}
