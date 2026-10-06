"use client";

import { useMemo, useState } from "react";
import { apiFetch } from "@/lib/session";
import { t, type Lang } from "@/lib/i18n";
import { formatDateTime } from "@/lib/clinical";
import {
  query,
  recoveryStatusLabel,
  transportNextStatuses,
  transportStatusLabel,
  urgencyLabel,
  waitlistStatusLabel,
  type CancellationEvent,
  type CancellationEventSummary,
  type CapacityExplanation,
  type CapacityForecast,
  type CursorPage,
  type ForecastOutput,
  type TransportRequest,
  type WaitlistEntry,
} from "@/lib/access";
import { ChartLegend, Heatmap, type HeatCell } from "@/components/ui/charts";
import {
  CatalogCombobox,
  ConfirmBox,
  MessageLine,
  PanelState,
  ReasonField,
  StatusBadge,
  nameFor,
  opsFetch,
  postJson,
  useAction,
  useCatalog,
  useLoader,
} from "./shared";
import { patientLabel } from "./worklists";

type Facility = { id: string; name: string };

// ---------------------------------------------------------------------------
// Waitlist and cancellation recovery
// ---------------------------------------------------------------------------

type RecoveryData = {
  events: CancellationEventSummary[];
  entries: WaitlistEntry[];
};

async function loadRecovery(facilityId: string): Promise<RecoveryData> {
  const [live, entries] = await Promise.all([
    apiFetch<CursorPage<CancellationEventSummary>>(
      `/api/v1/recovery-events${query({ status: "live", facility_id: facilityId, limit: 100 })}`,
    ),
    apiFetch<CursorPage<WaitlistEntry>>(
      `/api/v1/waitlist${query({ status: "active", facility_id: facilityId, limit: 100 })}`,
    ),
  ]);
  const events = [...live.items].sort((a, b) =>
    a.starts_at.localeCompare(b.starts_at),
  );
  return { events, entries: entries.items };
}

export function WaitlistPanel({
  lang,
  facilityId,
  canManage,
}: {
  lang: Lang;
  facilityId: string;
  canManage: boolean;
}) {
  const state = useLoader(() => loadRecovery(facilityId), facilityId);
  const services = useCatalog("clinical_service");
  const { busy, message, run } = useAction(lang);
  const [selected, setSelected] = useState<CancellationEvent | null>(null);
  const [override, setOverride] = useState<{
    event: CancellationEvent;
    entryId: string;
    reason: string;
  } | null>(null);
  const [closing, setClosing] = useState<{
    event: CancellationEvent;
    reason: string;
  } | null>(null);

  async function openEvent(e: CancellationEventSummary) {
    await run(async () => {
      const d = await apiFetch<CancellationEvent>(
        `/api/v1/recovery-events/${e.id}`,
      );
      setSelected(d);
    });
  }

  async function rank(e: CancellationEvent) {
    const ok = await run(async () => {
      const res = await apiFetch<{ event: CancellationEvent }>(
        `/api/v1/recovery-events/${e.id}/rank`,
        { method: "POST", body: JSON.stringify({ language: lang }) },
      );
      setSelected(res.event);
    }, "recoveryRanked");
    if (ok) state.reload();
  }

  async function applyOverride() {
    if (!override) return;
    const ok = await run(async () => {
      const res = await apiFetch<{ event: CancellationEvent }>(
        `/api/v1/recovery-events/${override.event.id}/override`,
        {
          method: "POST",
          body: JSON.stringify({
            waitlist_entry_id: override.entryId,
            reason: override.reason.trim(),
            version: override.event.version,
          }),
        },
      );
      setSelected(res.event);
      setOverride(null);
    }, "overrideApplied");
    if (ok) state.reload();
  }

  async function revoke(e: CancellationEvent) {
    const ok = await run(async () => {
      const res = await apiFetch<{ event: CancellationEvent }>(
        `/api/v1/recovery-events/${e.id}/revoke-offer`,
        {
          method: "POST",
          body: JSON.stringify({ reason: t(lang, "revokedByStaff") }),
        },
      );
      setSelected(res.event);
    }, "offerRevoked");
    if (ok) state.reload();
  }

  async function closeEvent() {
    if (!closing) return;
    const ok = await run(async () => {
      await postJson(`/api/v1/recovery-events/${closing.event.id}/close`, {
        reason: closing.reason.trim(),
      });
      setClosing(null);
      setSelected(null);
    }, "recoveryClosedMsg");
    if (ok) state.reload();
  }

  return (
    <section className="card" aria-labelledby="waitlist-h">
      <h3 id="waitlist-h">{t(lang, "waitlistRecovery")}</h3>
      <p className="muted">{t(lang, "waitlistRecoveryHelp")}</p>
      <MessageLine message={message} />
      <PanelState
        lang={lang}
        state={state}
        isEmpty={(d) => d.events.length === 0 && d.entries.length === 0}
        emptyKey="noRecoveryEvents"
      >
        {(d) => (
          <>
            <h4>{t(lang, "cancellationEvents")}</h4>
            {d.events.length === 0 ? (
              <p className="muted">{t(lang, "noRecoveryEvents")}</p>
            ) : (
              <ul className="stack" data-testid="recovery-events">
                {d.events.map((e) => (
                  <li key={e.id} className="row-card">
                    <div className="row-main">
                      <strong>
                        {nameFor(lang, services.entries, e.service_code)}
                      </strong>{" "}
                      · {formatDateTime(lang, e.starts_at)}
                      <div className="muted">
                        {t(lang, "eligibleCandidates")}: {e.eligible_count} ·{" "}
                        {t(lang, "offersMade")}: {e.offers_made} ·{" "}
                        {e.ranking_mode === "dmind"
                          ? t(lang, "rankingDmind")
                          : t(lang, "rankingDeterministic")}
                      </div>
                    </div>
                    <StatusBadge
                      label={recoveryStatusLabel(lang, e.status)}
                      tone={e.status === "offered" ? "warn" : "neutral"}
                    />
                    <button
                      type="button"
                      className="secondary"
                      onClick={() => void openEvent(e)}
                      aria-expanded={selected?.id === e.id}
                    >
                      {t(lang, "inspectRanking")}
                    </button>
                  </li>
                ))}
              </ul>
            )}
            {selected ? (
              <div
                className="card nested"
                role="region"
                aria-label={t(lang, "rankingExplanation")}
                data-testid="recovery-detail"
              >
                <h4>
                  {t(lang, "rankingExplanation")} ·{" "}
                  {recoveryStatusLabel(lang, selected.status)}
                </h4>
                <p className="advisory" role="status">
                  {selected.ranking_mode === "dmind"
                    ? t(lang, "recoveryDmindNotice")
                    : t(lang, "recoveryDeterministicNotice")}
                </p>
                {selected.override_reason ? (
                  <p className="muted">
                    {t(lang, "overrideReason")}: {selected.override_reason}
                  </p>
                ) : null}
                <ol className="stack">
                  {[...selected.eligible]
                    .sort((a, b) => a.position - b.position)
                    .map((c) => (
                      <li key={c.entry_id} className="row-card">
                        <div className="row-main">
                          <strong>
                            {c.patient ? patientLabel(c.patient) : c.patient_id}
                          </strong>{" "}
                          · {urgencyLabel(lang, c.urgency)} ·{" "}
                          {t(lang, "waitedHours")}: {c.waited_hours}
                          {c.rank !== null && c.rank !== c.position ? (
                            <span className="muted">
                              {" "}
                              ({t(lang, "fairnessRank")} {c.rank})
                            </span>
                          ) : null}
                          {c.explanation ? (
                            <div className="muted">{c.explanation}</div>
                          ) : null}
                          {c.reasons.length > 0 ? (
                            <div className="muted">{c.reasons.join(" · ")}</div>
                          ) : null}
                        </div>
                        {c.outcome ? (
                          <StatusBadge label={c.outcome} />
                        ) : canManage &&
                          selected.status !== "closed" &&
                          selected.status !== "filled" ? (
                          <button
                            type="button"
                            className="secondary"
                            disabled={busy}
                            onClick={() =>
                              setOverride({
                                event: selected,
                                entryId: c.entry_id,
                                reason: "",
                              })
                            }
                          >
                            {t(lang, "offerToThisPatient")}
                          </button>
                        ) : null}
                      </li>
                    ))}
                </ol>
                {canManage ? (
                  <div className="actions">
                    {selected.status === "open" ? (
                      <button
                        type="button"
                        disabled={busy}
                        onClick={() => void rank(selected)}
                      >
                        {t(lang, "rankAndOffer")}
                      </button>
                    ) : null}
                    {selected.status === "offered" ? (
                      <button
                        type="button"
                        className="secondary"
                        disabled={busy}
                        onClick={() => void revoke(selected)}
                      >
                        {t(lang, "revokeOffer")}
                      </button>
                    ) : null}
                    {selected.status !== "closed" &&
                    selected.status !== "filled" ? (
                      <button
                        type="button"
                        className="secondary"
                        disabled={busy}
                        onClick={() =>
                          setClosing({ event: selected, reason: "" })
                        }
                      >
                        {t(lang, "closeEvent")}
                      </button>
                    ) : null}
                    <button
                      type="button"
                      className="secondary"
                      onClick={() => setSelected(null)}
                    >
                      {t(lang, "closePanel")}
                    </button>
                  </div>
                ) : null}
                {override ? (
                  <ConfirmBox
                    lang={lang}
                    title={t(lang, "overrideRanking")}
                    busy={busy}
                    disabled={override.reason.trim().length < 3}
                    onConfirm={() => void applyOverride()}
                    onCancel={() => setOverride(null)}
                  >
                    <p className="muted">{t(lang, "overrideRankingHelp")}</p>
                    <ReasonField
                      id="override-reason"
                      lang={lang}
                      value={override.reason}
                      onChange={(v) => setOverride({ ...override, reason: v })}
                      required
                      label={t(lang, "overrideReason")}
                    />
                  </ConfirmBox>
                ) : null}
                {closing ? (
                  <ConfirmBox
                    lang={lang}
                    title={t(lang, "closeEvent")}
                    busy={busy}
                    disabled={closing.reason.trim().length < 3}
                    onConfirm={() => void closeEvent()}
                    onCancel={() => setClosing(null)}
                  >
                    <ReasonField
                      id="close-event-reason"
                      lang={lang}
                      value={closing.reason}
                      onChange={(v) => setClosing({ ...closing, reason: v })}
                      required
                    />
                  </ConfirmBox>
                ) : null}
              </div>
            ) : null}
            <h4>{t(lang, "activeWaitlist")}</h4>
            {d.entries.length === 0 ? (
              <p className="muted">{t(lang, "noWaitlistEntries")}</p>
            ) : (
              <div className="table-wrap">
                <table data-testid="waitlist-entries">
                  <thead>
                    <tr>
                      <th scope="col">{t(lang, "patient")}</th>
                      <th scope="col">{t(lang, "service")}</th>
                      <th scope="col">{t(lang, "urgency")}</th>
                      <th scope="col">{t(lang, "joinedAt")}</th>
                      <th scope="col">{t(lang, "status")}</th>
                    </tr>
                  </thead>
                  <tbody>
                    {d.entries.map((w) => (
                      <tr key={w.id}>
                        <td>
                          {w.patient ? patientLabel(w.patient) : w.patient_id}
                        </td>
                        <td>
                          {nameFor(lang, services.entries, w.service_code)}
                        </td>
                        <td>{urgencyLabel(lang, w.urgency)}</td>
                        <td>{formatDateTime(lang, w.joined_at)}</td>
                        <td>
                          <StatusBadge
                            label={waitlistStatusLabel(lang, w.status)}
                            tone={w.status === "active" ? "ok" : "neutral"}
                          />
                        </td>
                      </tr>
                    ))}
                  </tbody>
                </table>
              </div>
            )}
          </>
        )}
      </PanelState>
    </section>
  );
}

// ---------------------------------------------------------------------------
// Capacity pressure
// ---------------------------------------------------------------------------

async function loadForecasts(
  facilityId: string,
): Promise<{ forecasts: CapacityForecast[] }> {
  return opsFetch<{ forecasts: CapacityForecast[] }>(
    `/api/v1/capacity/forecasts${query({ facility_id: facilityId, limit: 20 })}`,
  );
}

/**
 * Day-by-day pressure map for one ready forecast: weeks as rows, weekdays as
 * columns. The ratio demand/capacity is the deterministic forecast's own
 * numbers; a day with zero capacity (closure, no availability) is marked
 * closed rather than scored.
 */
function CapacityHeatmap({
  lang,
  forecast,
}: {
  lang: Lang;
  forecast: Extract<ForecastOutput, { status: "ready" }>;
}) {
  const { rows, columns, summary } = useMemo(() => {
    const byDate = new Map(forecast.days.map((d) => [d.date, d]));
    const dates = forecast.days.map((d) => d.date).sort();
    if (dates.length === 0) {
      return { rows: [], columns: [], summary: "" };
    }
    const first = new Date(`${dates[0]}T00:00:00`);
    const dow = (first.getDay() + 6) % 7;
    first.setDate(first.getDate() - dow);
    const last = new Date(`${dates[dates.length - 1]}T00:00:00`);
    const fmtDay = new Intl.DateTimeFormat(lang === "es" ? "es-ES" : "en-GB", {
      weekday: "short",
    });
    const fmtDate = new Intl.DateTimeFormat(lang === "es" ? "es-ES" : "en-GB", {
      day: "numeric",
      month: "short",
    });
    const columns = Array.from({ length: 7 }, (_, i) => {
      const d = new Date(first);
      d.setDate(d.getDate() + i);
      return fmtDay.format(d);
    });
    const rows: { label: string; cells: HeatCell[] }[] = [];
    const cursor = new Date(first);
    while (cursor <= last) {
      const cells: HeatCell[] = [];
      const weekLabel = fmtDate.format(cursor);
      for (let i = 0; i < 7; i++) {
        const key = `${cursor.getFullYear()}-${String(cursor.getMonth() + 1).padStart(2, "0")}-${String(cursor.getDate()).padStart(2, "0")}`;
        const d = byDate.get(key);
        const dateLabel = fmtDate.format(cursor);
        if (!d) {
          cells.push({ value: null, text: "", title: dateLabel });
        } else if (d.available_capacity <= 0) {
          cells.push({
            value: null,
            text: "—",
            title: t(lang, "heatmapClosed").replace("{date}", dateLabel),
            closed: true,
          });
        } else {
          const ratio = Math.min(1, d.expected_demand / d.available_capacity);
          cells.push({
            value: ratio,
            text: `${Math.round(d.expected_demand)}/${d.available_capacity}`,
            title: t(lang, "heatmapCell")
              .replace("{date}", dateLabel)
              .replace("{demand}", d.expected_demand.toFixed(1))
              .replace("{cap}", String(d.available_capacity)),
          });
        }
        cursor.setDate(cursor.getDate() + 1);
      }
      rows.push({ label: weekLabel, cells });
    }
    const summary = t(lang, "heatmapSummary")
      .replace("{n}", String(forecast.pressure_days.length))
      .replace("{total}", String(forecast.days.length));
    return { rows, columns, summary };
  }, [forecast, lang]);
  if (rows.length === 0) return null;
  return (
    <div className="capacity-heatmap" data-testid="capacity-heatmap">
      <h5>{t(lang, "heatmapTitle")}</h5>
      <p className="muted">{t(lang, "heatmapHelp")}</p>
      <Heatmap
        rows={rows.map((r) => r.label)}
        cols={columns}
        cells={rows.map((r) => r.cells)}
        title={t(lang, "heatmapTitle")}
        summary={summary}
      />
      <ChartLegend
        items={[
          { tone: "ok", label: t(lang, "heatmapLegendLow") },
          { tone: "critical", label: t(lang, "heatmapLegendHigh") },
          { tone: "neutral", label: t(lang, "heatmapLegendClosed") },
        ]}
      />
    </div>
  );
}

function ForecastTable({
  lang,
  forecast,
}: {
  lang: Lang;
  forecast: ForecastOutput;
}) {
  if (forecast.status !== "ready") {
    return (
      <p className="advisory" role="status" data-testid="insufficient-history">
        {t(lang, "insufficientHistory")} ({forecast.history_weeks}/
        {forecast.required_weeks} {t(lang, "weeks")}). {forecast.detail}
      </p>
    );
  }
  return (
    <>
      <p className="muted">
        {t(lang, "confidence")}: {Math.round(forecast.confidence * 100)}% ·{" "}
        {t(lang, "historyWeeks")}: {forecast.history_weeks} ·{" "}
        {t(lang, "pressureDays")}: {forecast.pressure_days.length}
      </p>
      <CapacityHeatmap lang={lang} forecast={forecast} />
      <div className="table-wrap">
        <table>
          <thead>
            <tr>
              <th scope="col">{t(lang, "date")}</th>
              <th scope="col">{t(lang, "expectedDemand")}</th>
              <th scope="col">{t(lang, "availableCapacity")}</th>
              <th scope="col">{t(lang, "gap")}</th>
              <th scope="col">{t(lang, "factors")}</th>
            </tr>
          </thead>
          <tbody>
            {forecast.days.map((d) => (
              <tr
                key={d.date}
                className={
                  forecast.pressure_days.includes(d.date) ? "pressure" : ""
                }
              >
                <td>{d.date}</td>
                <td>
                  {d.expected_demand.toFixed(1)}{" "}
                  <span className="muted">
                    ({d.demand_low.toFixed(0)}–{d.demand_high.toFixed(0)})
                  </span>
                </td>
                <td>{d.available_capacity}</td>
                <td>{d.gap > 0 ? `+${d.gap.toFixed(1)}` : d.gap.toFixed(1)}</td>
                <td className="muted">
                  {d.factors.map((f) => f.detail).join("; ")}
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      </div>
      {forecast.recommendations.length > 0 ? (
        <>
          <h5>{t(lang, "forecastRecommendations")}</h5>
          <ul>
            {forecast.recommendations.map((r) => (
              <li key={r}>{r}</li>
            ))}
          </ul>
        </>
      ) : null}
    </>
  );
}

export function CapacityPanel({
  lang,
  facilityId,
  facilities,
}: {
  lang: Lang;
  facilityId: string;
  facilities: Facility[];
}) {
  const state = useLoader(() => loadForecasts(facilityId), facilityId);
  const services = useCatalog("clinical_service");
  const { busy, message, run } = useAction(lang);
  const [selected, setSelected] = useState<CapacityForecast | null>(null);
  const [explanation, setExplanation] = useState<CapacityExplanation | null>(
    null,
  );
  const [newFacility, setNewFacility] = useState(facilityId);
  const [newService, setNewService] = useState("");

  async function create() {
    const ok = await run(async () => {
      const f = await opsFetch<CapacityForecast>("/api/v1/capacity/forecasts", {
        method: "POST",
        body: JSON.stringify({
          facility_id: newFacility || facilityId || facilities[0]?.id,
          service_code: newService,
          horizon_days: 28,
        }),
      });
      setSelected(f);
      setExplanation(null);
    }, "forecastCreated");
    if (ok) state.reload();
  }

  async function explain(f: CapacityForecast) {
    await run(async () => {
      const res = await opsFetch<{
        explanation: CapacityExplanation;
        forecast: CapacityForecast;
      }>(`/api/v1/capacity/forecasts/${f.id}/explain`, {
        method: "POST",
        body: JSON.stringify({ language: lang }),
      });
      setSelected(res.forecast);
      setExplanation(res.explanation);
    });
  }

  return (
    <section className="card" aria-labelledby="capacity-h">
      <h3 id="capacity-h">{t(lang, "capacityPressure")}</h3>
      <p className="muted">{t(lang, "capacityHelp")}</p>
      <div className="filters">
        {facilities.length > 1 ? (
          <div>
            <label htmlFor="cap-facility">{t(lang, "facility")}</label>
            <select
              id="cap-facility"
              value={newFacility}
              onChange={(e) => setNewFacility(e.target.value)}
            >
              {facilities.map((f) => (
                <option key={f.id} value={f.id}>
                  {f.name}
                </option>
              ))}
            </select>
          </div>
        ) : null}
        <CatalogCombobox
          id="cap-service"
          lang={lang}
          label={t(lang, "service")}
          entries={services.entries}
          value={newService}
          onChange={setNewService}
          placeholder={t(lang, "selectService")}
        />
        <button
          type="button"
          disabled={busy || !newService}
          onClick={() => void create()}
        >
          {t(lang, "runForecast")}
        </button>
      </div>
      <MessageLine message={message} />
      <PanelState
        lang={lang}
        state={state}
        isEmpty={(d) => d.forecasts.length === 0}
        emptyKey="noForecasts"
      >
        {(d) => (
          <ul className="stack" data-testid="forecast-list">
            {d.forecasts.map((f) => (
              <li key={f.id} className="row-card">
                <div className="row-main">
                  <strong>
                    {nameFor(lang, services.entries, f.service_code)}
                  </strong>{" "}
                  · {f.horizon_start} → {f.horizon_end}
                  <div className="muted">
                    {f.forecast_version} · {formatDateTime(lang, f.created_at)}
                  </div>
                </div>
                <StatusBadge
                  label={
                    f.forecast.status === "ready"
                      ? t(lang, "forecastReady")
                      : t(lang, "insufficientHistory")
                  }
                  tone={f.forecast.status === "ready" ? "ok" : "warn"}
                />
                <button
                  type="button"
                  className="secondary"
                  onClick={() => {
                    setSelected(f);
                    setExplanation(null);
                  }}
                  aria-expanded={selected?.id === f.id}
                >
                  {t(lang, "viewEvidence")}
                </button>
              </li>
            ))}
          </ul>
        )}
      </PanelState>
      {selected ? (
        <div
          className="card nested"
          role="region"
          aria-label={t(lang, "forecastEvidence")}
          data-testid="forecast-detail"
        >
          <h4>
            {nameFor(lang, services.entries, selected.service_code)} ·{" "}
            {selected.horizon_start} → {selected.horizon_end}
          </h4>
          <ForecastTable lang={lang} forecast={selected.forecast} />
          <p className="muted">{t(lang, "forecastBoundary")}</p>
          {selected.forecast.status === "ready" ? (
            <div className="actions">
              <button
                type="button"
                className="secondary"
                disabled={busy}
                onClick={() => void explain(selected)}
              >
                {t(lang, "explainWithDmind")}
              </button>
            </div>
          ) : null}
          {explanation ? (
            <div
              className={
                explanation.synthetic ? "advisory synthetic-notice" : "advisory"
              }
              role="status"
              data-testid="capacity-explanation"
            >
              <p>
                <strong>{t(lang, "dmindExplanation")}</strong>
                {explanation.synthetic
                  ? ` · ${t(lang, "syntheticProviderNotice")}`
                  : ""}
                {explanation.reused ? ` · ${t(lang, "rankingReused")}` : ""}
              </p>
              <p>{explanation.explanation.summary}</p>
              {explanation.explanation.pressure_points.map((p) => (
                <p key={p.date}>
                  <strong>{p.date}</strong>: {p.explanation}
                </p>
              ))}
              {explanation.explanation.recommendations.length > 0 ? (
                <ul>
                  {explanation.explanation.recommendations.map((r, i) => (
                    <li key={i}>
                      {r.text}
                      {r.requires_confirmation
                        ? ` (${t(lang, "requiresHumanConfirmation")})`
                        : ""}
                    </li>
                  ))}
                </ul>
              ) : null}
              {explanation.explanation.limitations.length > 0 ? (
                <p className="muted">
                  {explanation.explanation.limitations.join(" ")}
                </p>
              ) : null}
            </div>
          ) : null}
        </div>
      ) : null}
    </section>
  );
}

// ---------------------------------------------------------------------------
// Transport coordination
// ---------------------------------------------------------------------------

type Vehicle = {
  id: string;
  name: string;
  resource_type_code: string;
  facility_id: string;
  capacity: number;
  accessibility_codes: string[];
  time_zone: string;
};

type TransportData = { items: TransportRequest[]; vehicles: Vehicle[] };

async function loadTransport(facilityId: string): Promise<TransportData> {
  const [list, vehicles] = await Promise.all([
    apiFetch<{ items: TransportRequest[] }>(
      `/api/v1/transport${query({ facility_id: facilityId, limit: 100 })}`,
    ),
    apiFetch<{ items: Vehicle[] }>(
      `/api/v1/transport/vehicles${query({ facility_id: facilityId })}`,
    ),
  ]);
  return { items: list.items, vehicles: vehicles.items };
}

const CLOSED_TRANSPORT = ["completed", "cancelled", "failed"];

export function TransportPanel({
  lang,
  facilityId,
  canCoordinate,
}: {
  lang: Lang;
  facilityId: string;
  canCoordinate: boolean;
}) {
  const state = useLoader(() => loadTransport(facilityId), facilityId);
  const { busy, message, run } = useAction(lang);
  const [transition, setTransition] = useState<{
    request: TransportRequest;
    status: string;
    vehicleId: string;
    reason: string;
  } | null>(null);

  async function apply() {
    if (!transition) return;
    const needsReason =
      transition.status === "cancelled" || transition.status === "failed";
    const ok = await run(async () => {
      await postJson(`/api/v1/transport/${transition.request.id}/transition`, {
        status: transition.status,
        version: transition.request.version,
        vehicle_resource_id: transition.vehicleId || null,
        reason: needsReason ? transition.reason.trim() : null,
      });
      setTransition(null);
    }, "transportUpdated");
    if (ok) state.reload();
  }

  return (
    <section className="card" aria-labelledby="transport-h">
      <h3 id="transport-h">{t(lang, "transportCoordination")}</h3>
      <p className="muted">{t(lang, "transportHelp")}</p>
      <MessageLine message={message} />
      <PanelState
        lang={lang}
        state={state}
        isEmpty={(d) => d.items.length === 0}
        emptyKey="noTransportRequests"
      >
        {(d) => (
          <ul className="stack" data-testid="transport-list">
            {d.items.map((r) => {
              const next = transportNextStatuses(r.status);
              return (
                <li key={r.id} className="row-card">
                  <div className="row-main">
                    <strong>
                      {r.patient ? patientLabel(r.patient) : r.patient_id}
                    </strong>{" "}
                    · {formatDateTime(lang, r.appointment_starts_at)}
                    <div className="muted">
                      {r.requirements.length > 0
                        ? r.requirements.join(", ")
                        : t(lang, "noSpecialRequirements")}
                      {r.vehicle_name ? ` · ${r.vehicle_name}` : ""}
                      {r.pickup_window_start
                        ? ` · ${t(lang, "pickupWindow")}: ${formatDateTime(lang, r.pickup_window_start)}`
                        : ""}
                      {r.location_sharing_open
                        ? ` · ${t(lang, "liveLocationActive")}`
                        : ""}
                    </div>
                    {r.emergency ? (
                      <p className="error" role="note">
                        {t(lang, "emergencyTransportNotice")}
                        {r.authorized_by
                          ? ` · ${t(lang, "authorizedBy")} ${r.authorized_by}`
                          : ""}
                      </p>
                    ) : null}
                    {r.failure_reason ? (
                      <div className="muted">
                        {t(lang, "reason")}: {r.failure_reason}
                      </div>
                    ) : null}
                  </div>
                  <StatusBadge
                    label={transportStatusLabel(lang, r.status)}
                    tone={
                      r.status === "failed" || r.status === "cancelled"
                        ? "critical"
                        : CLOSED_TRANSPORT.includes(r.status)
                          ? "neutral"
                          : r.status === "requested"
                            ? "warn"
                            : "ok"
                    }
                  />
                  {canCoordinate && next.length > 0 ? (
                    <button
                      type="button"
                      className="secondary"
                      disabled={busy}
                      onClick={() =>
                        setTransition({
                          request: r,
                          status: next[0],
                          vehicleId: r.vehicle_resource_id ?? "",
                          reason: "",
                        })
                      }
                    >
                      {t(lang, "updateTransport")}
                    </button>
                  ) : null}
                  {transition?.request.id === r.id ? (
                    <ConfirmBox
                      lang={lang}
                      title={t(lang, "updateTransport")}
                      busy={busy}
                      disabled={
                        (transition.status === "cancelled" ||
                          transition.status === "failed") &&
                        transition.reason.trim().length < 3
                      }
                      onConfirm={() => void apply()}
                      onCancel={() => setTransition(null)}
                    >
                      <div>
                        <label htmlFor={`tr-status-${r.id}`}>
                          {t(lang, "newStatus")}
                        </label>
                        <select
                          id={`tr-status-${r.id}`}
                          value={transition.status}
                          onChange={(e) =>
                            setTransition({
                              ...transition,
                              status: e.target.value,
                            })
                          }
                        >
                          {next.map((s) => (
                            <option key={s} value={s}>
                              {transportStatusLabel(lang, s)}
                            </option>
                          ))}
                        </select>
                      </div>
                      {transition.status === "scheduled" ? (
                        <div>
                          <label htmlFor={`tr-vehicle-${r.id}`}>
                            {t(lang, "vehicle")}
                          </label>
                          <select
                            id={`tr-vehicle-${r.id}`}
                            value={transition.vehicleId}
                            onChange={(e) =>
                              setTransition({
                                ...transition,
                                vehicleId: e.target.value,
                              })
                            }
                          >
                            <option value="">
                              {t(lang, "noVehicleAssigned")}
                            </option>
                            {d.vehicles.map((v) => (
                              <option key={v.id} value={v.id}>
                                {v.name}
                                {v.accessibility_codes.length > 0
                                  ? ` (${v.accessibility_codes.join(", ")})`
                                  : ""}
                              </option>
                            ))}
                          </select>
                        </div>
                      ) : null}
                      {transition.status === "cancelled" ||
                      transition.status === "failed" ? (
                        <ReasonField
                          id={`tr-reason-${r.id}`}
                          lang={lang}
                          value={transition.reason}
                          onChange={(v) =>
                            setTransition({ ...transition, reason: v })
                          }
                          required
                        />
                      ) : null}
                    </ConfirmBox>
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
