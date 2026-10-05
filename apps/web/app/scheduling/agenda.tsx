"use client";

import { useMemo, useState } from "react";
import { apiFetch, useSession } from "@/lib/session";
import { t, type Lang } from "@/lib/i18n";
import {
  buildLanes,
  catalogName,
  dayRange,
  formatDay,
  formatTime,
  localDateKey,
  query,
  shiftDay,
  weekDays,
  weekRange,
  type Appointment,
  type CursorPage,
  type Lane,
  type LaneItem,
  type Offer,
  type SchedulableResource,
} from "@/lib/access";
import { CalendarGrid, type CalendarItem } from "@/components/ui/charts";
import { CatalogCombobox, PanelState, useCatalog, useLoader } from "./shared";

type View = "day" | "week" | "month";

/** Monday-start grid covering the whole month that contains `day`. */
export function monthGrid(day: string): {
  from: string;
  to: string;
  days: string[];
} {
  const anchor = new Date(`${day}T00:00:00`);
  const first = new Date(anchor.getFullYear(), anchor.getMonth(), 1);
  const start = new Date(first);
  start.setDate(start.getDate() - ((first.getDay() + 6) % 7));
  const lastOfMonth = new Date(anchor.getFullYear(), anchor.getMonth() + 1, 0);
  const end = new Date(lastOfMonth);
  end.setDate(end.getDate() + ((7 - lastOfMonth.getDay()) % 7) + 1);
  const days: string[] = [];
  const cursor = new Date(start);
  while (cursor < end) {
    days.push(localDateKey(cursor));
    cursor.setDate(cursor.getDate() + 1);
  }
  return { from: start.toISOString(), to: end.toISOString(), days };
}

export type AgendaFilters = {
  facility_id: string;
  service_code: string;
  specialty_code: string;
  profession_code: string;
  modality_code: string;
};

const EMPTY_FILTERS: AgendaFilters = {
  facility_id: "",
  service_code: "",
  specialty_code: "",
  profession_code: "",
  modality_code: "",
};

type AgendaData = {
  lanes: Lane[];
  unfilled: number;
  cancelled: Appointment[];
};

async function loadAgenda(
  f: AgendaFilters,
  from: string,
  to: string,
): Promise<AgendaData> {
  const [resources, appointments, live, cancelled] = await Promise.all([
    apiFetch<CursorPage<SchedulableResource>>(
      `/api/v1/scheduling/resources${query({
        facility_id: f.facility_id,
        service_code: f.service_code,
        specialty_code: f.specialty_code,
        profession_code: f.profession_code,
        limit: 100,
      })}`,
    ),
    apiFetch<CursorPage<Appointment>>(
      `/api/v1/appointments${query({
        facility_id: f.facility_id,
        service_code: f.service_code,
        from,
        to,
        limit: 200,
      })}`,
    ),
    apiFetch<CursorPage<Offer>>(
      `/api/v1/offers${query({ facility_id: f.facility_id, status: "active", limit: 200 })}`,
    ),
    apiFetch<CursorPage<Appointment>>(
      `/api/v1/appointments${query({
        facility_id: f.facility_id,
        service_code: f.service_code,
        status: "cancelled",
        from,
        to,
        limit: 50,
      })}`,
    ),
  ]);
  const inRange = (o: Offer) => o.starts_at < to && o.ends_at > from;
  const offers = live.items.filter(
    (o) =>
      inRange(o) && (!f.modality_code || o.modality_code === f.modality_code),
  );
  const appts = appointments.items.filter(
    (a) => !f.modality_code || a.modality_code === f.modality_code,
  );
  const lanes = buildLanes(resources.items, appts, offers);
  return {
    lanes,
    unfilled: lanes.filter((l) => l.items.length === 0).length,
    cancelled: cancelled.items,
  };
}

function itemTone(item: LaneItem): string {
  if (item.kind === "hold") return "warn";
  if (item.kind === "offer") return "neutral";
  return item.status === "fulfilled" ? "neutral" : "ok";
}

function ItemChip({
  lang,
  item,
  onSelect,
}: {
  lang: Lang;
  item: LaneItem;
  onSelect?: (item: LaneItem) => void;
}) {
  const serviceName = item.service
    ? lang === "es"
      ? item.service.name_es
      : item.service.name_en
    : item.service_code;
  const kindLabel =
    item.kind === "hold"
      ? t(lang, "laneHold")
      : item.kind === "offer"
        ? t(lang, "laneOffer")
        : t(lang, "laneAppointment");
  const label = `${formatTime(lang, item.starts_at)}–${formatTime(lang, item.ends_at)} ${serviceName}${
    item.patient
      ? ` · ${item.patient.family_name}, ${item.patient.given_name}`
      : ""
  }`;
  return (
    <button
      type="button"
      className={`lane-item ${itemTone(item)}`}
      aria-label={`${kindLabel}: ${label}`}
      data-kind={item.kind}
      onClick={onSelect ? () => onSelect(item) : undefined}
    >
      <span className="lane-time">{formatTime(lang, item.starts_at)}</span>
      <span className="lane-text">
        {serviceName}
        {item.patient ? (
          <span className="muted">
            {" "}
            · {item.patient.family_name}, {item.patient.given_name}
          </span>
        ) : null}
      </span>
      {item.kind !== "appointment" ? (
        <span className="badge warn">{kindLabel}</span>
      ) : null}
    </button>
  );
}

/**
 * Day/week agenda with one lane per schedulable resource. Items are read
 * from the authoritative appointment and offer records; nothing is derived
 * client-side beyond grouping.
 */
export function Agenda({
  lang,
  onSelectItem,
}: {
  lang: Lang;
  onSelectItem?: (item: LaneItem) => void;
}) {
  const { meta } = useSession();
  const facilities = meta?.facilities.filter((f) => f.accessible) ?? [];
  const [view, setView] = useState<View>("day");
  const [day, setDay] = useState(() => localDateKey(new Date()));
  const [filters, setFilters] = useState<AgendaFilters>(EMPTY_FILTERS);
  const services = useCatalog("clinical_service");
  const specialties = useCatalog("specialty");
  const professions = useCatalog("profession");
  const modalities = useCatalog("modality");

  const month = useMemo(() => monthGrid(day), [day]);
  const range =
    view === "day"
      ? dayRange(day)
      : view === "week"
        ? weekRange(day)
        : { from: month.from, to: month.to };
  const key = JSON.stringify({ view, day, filters });
  const state = useLoader(
    () => loadAgenda(filters, range.from, range.to),
    key,
    Boolean(meta),
  );

  const days = useMemo(
    () =>
      view === "week" ? weekDays(day) : view === "month" ? month.days : [day],
    [view, day, month],
  );
  const weekdayHeaders = useMemo(() => {
    const fmt = new Intl.DateTimeFormat(lang === "es" ? "es-ES" : "en-GB", {
      weekday: "short",
    });
    return weekDays(day).map((d) => fmt.format(new Date(`${d}T12:00:00`)));
  }, [lang, day]);
  const monthLabel = useMemo(
    () =>
      new Intl.DateTimeFormat(lang === "es" ? "es-ES" : "en-GB", {
        month: "long",
        year: "numeric",
      }).format(new Date(`${day}T12:00:00`)),
    [lang, day],
  );
  const shiftBy = view === "day" ? 1 : view === "week" ? 7 : 0;
  const shift = (dir: 1 | -1) => {
    if (view === "month") {
      const d = new Date(`${day}T00:00:00`);
      d.setDate(1);
      d.setMonth(d.getMonth() + dir);
      setDay(localDateKey(d));
    } else {
      setDay(shiftDay(day, dir * shiftBy));
    }
  };

  const set = (k: keyof AgendaFilters) => (v: string) =>
    setFilters((f) => ({ ...f, [k]: v }));

  const renderSelect = (
    id: string,
    label: string,
    value: string,
    onChange: (v: string) => void,
    options: { value: string; label: string }[],
    allLabel: string,
  ) => (
    <div>
      <label htmlFor={id}>{label}</label>
      <select id={id} value={value} onChange={(e) => onChange(e.target.value)}>
        <option value="">{allLabel}</option>
        {options.map((o) => (
          <option key={o.value} value={o.value}>
            {o.label}
          </option>
        ))}
      </select>
    </div>
  );

  const catalogOptions = (
    entries: { code: string; name_en: string; name_es: string }[],
  ) => entries.map((e) => ({ value: e.code, label: catalogName(lang, e) }));

  return (
    <section className="card agenda" aria-labelledby="agenda-h">
      <div className="agenda-head">
        <h3 id="agenda-h">{t(lang, "agendaTitle")}</h3>
        <div className="tabs" role="tablist" aria-label={t(lang, "agendaView")}>
          {(["day", "week", "month"] as View[]).map((v) => (
            <button
              key={v}
              type="button"
              role="tab"
              id={`agenda-tab-${v}`}
              aria-selected={view === v}
              aria-controls="agenda-panel"
              onClick={() => setView(v)}
            >
              {t(
                lang,
                v === "day"
                  ? "viewDay"
                  : v === "week"
                    ? "viewWeek"
                    : "viewMonth",
              )}
            </button>
          ))}
        </div>
      </div>
      <div className="agenda-nav">
        <button
          type="button"
          className="secondary"
          onClick={() => shift(-1)}
          aria-label={t(lang, "previousPeriod")}
        >
          ‹
        </button>
        <label className="sr-only" htmlFor="agenda-day">
          {t(lang, "date")}
        </label>
        <input
          id="agenda-day"
          type="date"
          value={day}
          onChange={(e) => e.target.value && setDay(e.target.value)}
        />
        <button
          type="button"
          className="secondary"
          onClick={() => shift(1)}
          aria-label={t(lang, "nextPeriod")}
        >
          ›
        </button>
        <button
          type="button"
          className="tertiary"
          onClick={() => setDay(localDateKey(new Date()))}
        >
          {t(lang, "today")}
        </button>
      </div>
      <div className="filters">
        {facilities.length > 1
          ? renderSelect(
              "agenda-facility",
              t(lang, "facility"),
              filters.facility_id,
              set("facility_id"),
              facilities.map((f) => ({ value: f.id, label: f.name })),
              t(lang, "allFacilities"),
            )
          : null}
        <CatalogCombobox
          id="agenda-service"
          lang={lang}
          label={t(lang, "service")}
          entries={services.entries}
          value={filters.service_code}
          onChange={set("service_code")}
          placeholder={t(lang, "allServices")}
        />
        <CatalogCombobox
          id="agenda-specialty"
          lang={lang}
          label={t(lang, "specialty")}
          entries={specialties.entries}
          value={filters.specialty_code}
          onChange={set("specialty_code")}
          placeholder={t(lang, "allSpecialties")}
        />
        <CatalogCombobox
          id="agenda-profession"
          lang={lang}
          label={t(lang, "profession")}
          entries={professions.entries}
          value={filters.profession_code}
          onChange={set("profession_code")}
          placeholder={t(lang, "allProfessions")}
        />
        {renderSelect(
          "agenda-modality",
          t(lang, "modality"),
          filters.modality_code,
          set("modality_code"),
          catalogOptions(modalities.entries),
          t(lang, "allModalities"),
        )}
      </div>
      <div
        id="agenda-panel"
        role="tabpanel"
        aria-labelledby={`agenda-tab-${view}`}
      >
        <PanelState
          lang={lang}
          state={state}
          emptyKey="noResourcesForFilters"
          isEmpty={(d) => d.lanes.length === 0}
        >
          {(d) => (
            <>
              <p className="muted" role="status">
                {t(lang, "unfilledCapacity").replace("{n}", String(d.unfilled))}
                {d.cancelled.length > 0
                  ? ` · ${t(lang, "cancelledInRange").replace("{n}", String(d.cancelled.length))}`
                  : ""}
              </p>
              {view === "month" ? (
                <CalendarGrid
                  label={`${t(lang, "agendaTitle")} · ${monthLabel}`}
                  headers={weekdayHeaders}
                  todayKey={localDateKey(new Date())}
                  moreLabel={(n) =>
                    t(lang, "moreItems").replace("{n}", String(n))
                  }
                  days={days.map((dk) => {
                    const inMonth = dk.slice(0, 7) === day.slice(0, 7);
                    const items: CalendarItem[] = d.lanes
                      .flatMap((lane) => lane.items)
                      .filter((i) => localDateKey(new Date(i.starts_at)) === dk)
                      .sort((a, b) => a.starts_at.localeCompare(b.starts_at))
                      .map((i) => ({
                        id: `${i.kind}-${i.id}`,
                        label: `${formatTime(lang, i.starts_at)} ${
                          i.service
                            ? catalogName(lang, i.service, i.service_code)
                            : i.service_code
                        }`,
                        title: i.patient
                          ? `${i.patient.family_name}, ${i.patient.given_name}`
                          : undefined,
                        tone: itemTone(i) as CalendarItem["tone"],
                        onClick: onSelectItem
                          ? () => onSelectItem(i)
                          : undefined,
                      }));
                    return {
                      key: dk,
                      day: String(Number(dk.slice(8, 10))),
                      items,
                      muted: !inMonth,
                    };
                  })}
                />
              ) : (
                <div
                  className="lanes"
                  role="table"
                  aria-label={t(lang, "agendaTitle")}
                >
                  <div role="row" className="lane-row lane-header">
                    <div role="columnheader" className="lane-name">
                      {t(lang, "resource")}
                    </div>
                    {days.map((dk) => (
                      <div role="columnheader" key={dk} className="lane-day">
                        {formatDay(lang, `${dk}T12:00:00`)}
                      </div>
                    ))}
                  </div>
                  {d.lanes.map((lane) => (
                    <div role="row" className="lane-row" key={lane.resource.id}>
                      <div role="rowheader" className="lane-name">
                        <strong>{lane.resource.name}</strong>
                        <div className="muted small">
                          {lane.resource.resource_type_code} ·{" "}
                          {lane.resource.time_zone}
                        </div>
                      </div>
                      {days.map((dk) => {
                        const items = lane.items.filter(
                          (i) => localDateKey(new Date(i.starts_at)) === dk,
                        );
                        return (
                          <div role="cell" key={dk} className="lane-day">
                            {items.length === 0 ? (
                              <span className="muted small">
                                {t(lang, "free")}
                              </span>
                            ) : (
                              items.map((i) => (
                                <ItemChip
                                  key={`${i.kind}-${i.id}`}
                                  lang={lang}
                                  item={i}
                                  onSelect={onSelectItem}
                                />
                              ))
                            )}
                          </div>
                        );
                      })}
                    </div>
                  ))}
                </div>
              )}
            </>
          )}
        </PanelState>
      </div>
    </section>
  );
}
