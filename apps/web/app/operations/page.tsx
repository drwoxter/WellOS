"use client";

/**
 * `/operations` — Command Center. Renders the deterministic facility snapshot
 * (ED pressure, placement queue, capacity, discharge barriers, transport,
 * alerts, escalations, theatre readiness, staffing gaps and persisted
 * forecasts) with freshness and source links, plus the advisory dMind
 * care-flow brief which a reviewer accepts or rejects.
 */
import Link from "next/link";
import { useCallback, useState } from "react";
import { AppShell } from "../chrome";
import { t, type Lang } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import {
  loadSnapshot,
  requestFlowBrief,
  type AiOutcome,
  type FlowBrief,
  type Forecast,
  type Snapshot,
} from "@/lib/care-ops";
import {
  Card,
  CardHead,
  EmptyState,
  Pill,
  StatTile,
} from "@/components/ui/primitives";
import {
  MessageLine,
  PanelState,
  useAction,
  useLoader,
} from "../scheduling/shared";
import {
  AiOutcomePanel,
  FacilityPicker,
  Freshness,
  StatusPill,
  TextList,
  fill,
  useCareOpsCaps,
  useFacility,
} from "./shared";

function pct(ratio: number): string {
  return `${Math.round(ratio * 100)} %`;
}

export function ForecastSummary({
  lang,
  forecast,
}: {
  lang: Lang;
  forecast: Forecast;
}) {
  const out = forecast.output;
  const uncertainty =
    typeof out.uncertainty === "string" || typeof out.uncertainty === "number"
      ? String(out.uncertainty)
      : typeof out.confidence === "string"
        ? out.confidence
        : null;
  return (
    <li data-testid="ops-forecast">
      <p>
        <strong>{forecast.horizon}</strong>{" "}
        <StatusPill status={forecast.status} />
      </p>
      <p className="muted small">
        {fill(t(lang, "opsForecastSummary"), {
          version: forecast.forecast_version,
          when: formatDateTime(lang, forecast.generated_at),
          status: forecast.status,
        })}
      </p>
      {forecast.status === "insufficient_history" ? (
        <p role="note" className="warning">
          {t(lang, "opsForecastInsufficient")}
        </p>
      ) : (
        <>
          {uncertainty ? (
            <p className="small">
              {t(lang, "opsForecastUncertainty")}: {uncertainty}
            </p>
          ) : null}
          <dl className="kv small">
            {Object.entries(out)
              .filter(([, v]) => typeof v === "number")
              .map(([k, v]) => (
                <div key={k}>
                  <dt>{k.replaceAll("_", " ")}</dt>
                  <dd>{String(v)}</dd>
                </div>
              ))}
          </dl>
        </>
      )}
    </li>
  );
}

function BriefView({ lang, out }: { lang: Lang; out: FlowBrief }) {
  return (
    <div>
      <p>{out.summary}</p>
      {out.pressures?.length ? (
        <>
          <h4>{t(lang, "opsBriefPressures")}</h4>
          <ul>
            {out.pressures.map((p, i) => (
              <li key={`${p.area}-${i}`}>
                <strong>{p.area}</strong>: {p.explanation}
                {p.cited_sources?.length ? (
                  <span className="muted small">
                    {" "}
                    ({p.cited_sources.join(", ")})
                  </span>
                ) : null}
              </li>
            ))}
          </ul>
        </>
      ) : null}
      {out.human_decisions?.length ? (
        <>
          <h4>{t(lang, "opsBriefDecisions")}</h4>
          <ul>
            {out.human_decisions.map((d, i) => (
              <li key={i}>
                {d.text}
                {d.cited_sources?.length ? (
                  <span className="muted small">
                    {" "}
                    ({d.cited_sources.join(", ")})
                  </span>
                ) : null}
              </li>
            ))}
          </ul>
        </>
      ) : null}
      <p className="small">
        {t(lang, "opsBriefConfidence")}: {out.confidence}
      </p>
      <TextList
        lang={lang}
        titleKey="opsBriefMissing"
        items={out.missing_information}
      />
      <TextList
        lang={lang}
        titleKey="opsBriefLimitations"
        items={out.limitations}
      />
      <TextList
        lang={lang}
        titleKey="opsBriefSources"
        items={out.cited_sources}
      />
    </div>
  );
}

function CommandCenter() {
  const { lang, meta } = useSession();
  const caps = useCareOpsCaps();
  const { facilityId, setFacilityId, facilities } = useFacility();
  const snap = useLoader(
    () => loadSnapshot(facilityId ?? ""),
    facilityId ?? "",
    Boolean(facilityId),
  );
  const brief = useAction(lang);
  const [briefOutcome, setBriefOutcome] = useState<AiOutcome<FlowBrief> | null>(
    null,
  );
  const askBrief = useCallback(
    () =>
      brief.run(async () => {
        if (!facilityId) return;
        const r = await requestFlowBrief(facilityId, lang);
        setBriefOutcome(r.brief);
      }),
    [brief, facilityId, lang],
  );
  const flowHref = `/operations/flow`;
  return (
    <section className="page" aria-labelledby="ops-title">
      <header className="page-head">
        <h1 id="ops-title">{t(lang, "opsTitle")}</h1>
        <p className="muted">{t(lang, "opsIntro")}</p>
        <div className="row wrap gap">
          <FacilityPicker
            lang={lang}
            facilityId={facilityId}
            facilities={facilities}
            onChange={setFacilityId}
          />
          <button
            type="button"
            className="secondary"
            onClick={snap.reload}
            disabled={!facilityId || snap.loading}
          >
            {t(lang, "opsRefresh")}
          </button>
        </div>
      </header>
      {!facilityId ? (
        <p className="muted">{t(lang, "opsSelectFacility")}</p>
      ) : (
        <PanelState
          lang={lang}
          state={snap}
          emptyKey="opsSelectFacility"
          isEmpty={() => false}
        >
          {(s: Snapshot) => {
            const alertCount = s.alerts.reduce((n, a) => n + a.count, 0);
            return (
              <div className="stack">
                <Freshness lang={lang} generatedAt={s.generated_at} />
                <div className="grid tiles" data-testid="ops-tiles">
                  <StatTile
                    label={t(lang, "opsAwaitingTriage")}
                    value={s.emergency.awaiting_triage}
                    tone={s.emergency.awaiting_triage > 0 ? "warn" : "neutral"}
                    href="/access"
                  />
                  <StatTile
                    label={t(lang, "opsAwaitingClinician")}
                    value={s.emergency.awaiting_clinician}
                    href="/access"
                  />
                  <StatTile
                    label={t(lang, "opsBoarding")}
                    value={s.placement_queue.pending}
                    tone={s.placement_queue.pending > 0 ? "warn" : "neutral"}
                    href={flowHref}
                  />
                  <StatTile
                    label={t(lang, "opsMaxWait")}
                    value={`${s.placement_queue.max_wait_minutes} min`}
                    href={flowHref}
                  />
                  <StatTile
                    label={t(lang, "opsAvailable")}
                    value={`${s.capacity.available} / ${s.capacity.total}`}
                    tone={
                      s.capacity.occupancy_ratio >= 0.9 ? "critical" : "neutral"
                    }
                    href={flowHref}
                  />
                  <StatTile
                    label={t(lang, "opsOccupancy")}
                    value={pct(s.capacity.occupancy_ratio)}
                    href={flowHref}
                  />
                  <StatTile
                    label={t(lang, "opsAlerts")}
                    value={alertCount}
                    tone={alertCount > 0 ? "critical" : "neutral"}
                    href="/dashboard"
                  />
                  <StatTile
                    label={t(lang, "opsTransportDelayed")}
                    value={s.transport.delayed}
                    tone={s.transport.delayed > 0 ? "warn" : "neutral"}
                    href="/scheduling"
                  />
                </div>

                <div className="grid two">
                  <Card>
                    <CardHead title={t(lang, "opsEmergency")} />
                    <h4>{t(lang, "opsByPriority")}</h4>
                    {s.emergency.by_priority.length === 0 ? (
                      <p className="muted">—</p>
                    ) : (
                      <ul className="inline-list" data-testid="ops-priority">
                        {s.emergency.by_priority.map((row) => (
                          <li key={`${row.priority}:${row.status}`}>
                            <Pill
                              tone={
                                row.priority === "immediate" ||
                                row.priority === "very_urgent"
                                  ? "critical"
                                  : "neutral"
                              }
                              icon
                            >
                              {row.priority}
                            </Pill>{" "}
                            {row.status.replace(/_/g, " ")} · {row.count}
                            {row.max_wait_minutes != null
                              ? ` · ${Math.round(row.max_wait_minutes)} min`
                              : ""}
                          </li>
                        ))}
                      </ul>
                    )}
                    <p className="small">
                      {t(lang, "opsInConsultation")}:{" "}
                      {s.emergency.in_consultation}
                    </p>
                  </Card>

                  <Card>
                    <CardHead title={t(lang, "opsUnits")} />
                    <p className="muted small">
                      {fill(t(lang, "opsUnitsSummary"), {
                        n: s.capacity.units.length,
                        available: s.capacity.available,
                        total: s.capacity.total,
                      })}
                    </p>
                    <ul className="bars" aria-label={t(lang, "opsUnits")}>
                      {s.capacity.units.map((u) => {
                        const name =
                          lang === "es"
                            ? (u.name_es ?? u.name_en)
                            : (u.name_en ?? u.name_es);
                        const ratio = u.total > 0 ? u.occupied / u.total : 0;
                        return (
                          <li key={u.unit_id}>
                            <span>{name ?? u.unit_id}</span>{" "}
                            <span>
                              {u.occupied}/{u.total} ({pct(ratio)})
                            </span>
                            <div
                              className="bar"
                              role="img"
                              aria-label={`${name ?? u.unit_id}: ${u.occupied}/${u.total}`}
                            >
                              <div style={{ width: pct(ratio) }} />
                            </div>
                          </li>
                        );
                      })}
                    </ul>
                  </Card>

                  <Card>
                    <CardHead title={t(lang, "opsDischarge")} />
                    {s.discharge.barriers.length === 0 ? (
                      <p className="muted">{t(lang, "opsNoBarriers")}</p>
                    ) : (
                      <ul>
                        {s.discharge.barriers.map((b, i) => (
                          <li key={`${b.category}-${i}`}>
                            {b.category ?? "—"}: {b.count}
                          </li>
                        ))}
                      </ul>
                    )}
                    <p className="small">
                      {t(lang, "opsOverdueTasks")}: {s.discharge.overdue_tasks}
                    </p>
                    <p className="small">
                      {t(lang, "opsTransportOpen")}: {s.transport.open}
                    </p>
                  </Card>

                  <Card>
                    <CardHead title={t(lang, "opsEscalations")} />
                    {s.escalations.length === 0 ? (
                      <p className="muted">{t(lang, "opsNoEscalations")}</p>
                    ) : (
                      <ul>
                        {s.escalations.map((e, i) => (
                          <li key={`${e.kind}-${e.status}-${i}`}>
                            <StatusPill status={e.status} /> {e.kind}: {e.count}
                          </li>
                        ))}
                      </ul>
                    )}
                    <Link href={flowHref} className="small">
                      {t(lang, "navCareFlow")}
                    </Link>
                  </Card>

                  <Card>
                    <CardHead title={t(lang, "opsTheatre")} />
                    <dl className="kv">
                      <div>
                        <dt>{t(lang, "opsCasesNext24h")}</dt>
                        <dd>{s.theatre.cases_next_24h}</dd>
                      </div>
                      <div>
                        <dt>{t(lang, "opsReady")}</dt>
                        <dd>{s.theatre.ready}</dd>
                      </div>
                      <div>
                        <dt>{t(lang, "opsBlockingConflicts")}</dt>
                        <dd>{s.theatre.blocking_conflicts}</dd>
                      </div>
                      <div>
                        <dt>{t(lang, "opsMissingPrereqs")}</dt>
                        <dd>{s.theatre.missing_prerequisites}</dd>
                      </div>
                    </dl>
                    <Link href="/operations/theatres" className="small">
                      {t(lang, "navTheatres")}
                    </Link>
                  </Card>

                  <Card>
                    <CardHead title={t(lang, "opsWorkforce")} />
                    <dl className="kv">
                      <div>
                        <dt>{t(lang, "opsShiftsNext24h")}</dt>
                        <dd>{s.workforce.shifts_next_24h}</dd>
                      </div>
                      <div>
                        <dt>{t(lang, "opsGaps")}</dt>
                        <dd>{s.workforce.gaps.length}</dd>
                      </div>
                    </dl>
                    <Link href="/operations/workforce" className="small">
                      {t(lang, "navWorkforce")}
                    </Link>
                  </Card>
                </div>

                <Card>
                  <CardHead title={t(lang, "opsForecasts")} />
                  {s.forecasts.length === 0 ? (
                    <EmptyState inline title={t(lang, "opsForecastNone")} />
                  ) : (
                    <ul className="plain">
                      {s.forecasts.map((f) => (
                        <ForecastSummary key={f.id} lang={lang} forecast={f} />
                      ))}
                    </ul>
                  )}
                </Card>

                {meta?.ai_capabilities ? (
                  <Card tone="dmind" data-testid="ops-brief">
                    <CardHead
                      title={t(lang, "opsBrief")}
                      actions={
                        <button
                          type="button"
                          className="primary"
                          disabled={brief.busy}
                          onClick={() => void askBrief()}
                        >
                          {t(lang, "opsBriefRequest")}
                        </button>
                      }
                    />
                    <p className="muted small">{t(lang, "opsBriefIntro")}</p>
                    <MessageLine message={brief.message} />
                    {briefOutcome ? (
                      <AiOutcomePanel
                        lang={lang}
                        outcome={briefOutcome}
                        canReview={caps.can_review_ai}
                        render={(out) => <BriefView lang={lang} out={out} />}
                        testId="ops-brief-outcome"
                      />
                    ) : null}
                  </Card>
                ) : null}
              </div>
            );
          }}
        </PanelState>
      )}
    </section>
  );
}

export default function OperationsPage() {
  return (
    <AppShell>
      <CommandCenter />
    </AppShell>
  );
}
