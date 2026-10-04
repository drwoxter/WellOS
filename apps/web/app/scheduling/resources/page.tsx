"use client";

import Link from "next/link";
import { useState } from "react";
import { AppShell } from "../../chrome";
import { t, type Lang } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import {
  EXCEPTION_KINDS,
  NO_SCHEDULING_CAPABILITIES,
  exceptionKindLabel,
  query,
  weekdayLabel,
  type AvailabilityRule,
  type CatalogEntry,
  type Page,
  type ResourceService,
  type SchedulableResource,
} from "@/lib/access";
import {
  ConfirmBox,
  MessageLine,
  PanelState,
  ReasonField,
  StatusBadge,
  nameFor,
  opsFetch,
  postOps,
  useAction,
  useCatalog,
  useLoader,
} from "../shared";

type Facility = { id: string; name: string };

type ResourceForm = {
  facility_id: string;
  resource_type_code: string;
  name: string;
  profession_code: string;
  specialty_codes: string[];
  languages: string;
  accessibility_codes: string[];
  capacity: string;
  time_zone: string;
  services: ServiceRow[];
  reason: string;
};

type ServiceRow = {
  service_code: string;
  duration_minutes: string;
  prep_minutes: string;
  cleanup_minutes: string;
  modality_codes: string[];
};

function emptyForm(facilityId: string): ResourceForm {
  return {
    facility_id: facilityId,
    resource_type_code: "",
    name: "",
    profession_code: "",
    specialty_codes: [],
    languages: "",
    accessibility_codes: [],
    capacity: "1",
    time_zone: "",
    services: [],
    reason: "",
  };
}

function toForm(r: SchedulableResource): ResourceForm {
  return {
    facility_id: r.facility_id,
    resource_type_code: r.resource_type_code,
    name: r.name,
    profession_code: r.profession_code ?? "",
    specialty_codes: r.specialty_codes,
    languages: r.languages.join(", "),
    accessibility_codes: r.accessibility_codes,
    capacity: String(r.capacity),
    time_zone: r.time_zone,
    services: (r.services ?? []).map((s) => ({
      service_code: s.service_code,
      duration_minutes: s.duration_minutes?.toString() ?? "",
      prep_minutes: s.prep_minutes?.toString() ?? "",
      cleanup_minutes: s.cleanup_minutes?.toString() ?? "",
      modality_codes: s.modality_codes,
    })),
    reason: "",
  };
}

function intOrNull(v: string): number | null {
  const s = v.trim();
  if (!s) return null;
  const n = Number(s);
  return Number.isInteger(n) ? n : null;
}

function serviceBodies(rows: ServiceRow[]): ResourceService[] {
  return rows
    .filter((s) => s.service_code)
    .map((s) => ({
      service_code: s.service_code,
      duration_minutes: intOrNull(s.duration_minutes),
      prep_minutes: intOrNull(s.prep_minutes),
      cleanup_minutes: intOrNull(s.cleanup_minutes),
      modality_codes: s.modality_codes,
    }));
}

function CodeChips({
  id,
  lang,
  label,
  entries,
  selected,
  onChange,
}: {
  id: string;
  lang: Lang;
  label: string;
  entries: CatalogEntry[];
  selected: string[];
  onChange: (codes: string[]) => void;
}) {
  return (
    <fieldset id={id}>
      <legend>{label}</legend>
      {entries.length === 0 ? (
        <p className="muted">{t(lang, "noCatalogEntries")}</p>
      ) : (
        <div className="chips">
          {entries.map((e) => (
            <label key={e.code} className="chip">
              <input
                type="checkbox"
                checked={selected.includes(e.code)}
                onChange={(ev) =>
                  onChange(
                    ev.target.checked
                      ? [...selected, e.code]
                      : selected.filter((c) => c !== e.code),
                  )
                }
              />{" "}
              {nameFor(lang, entries, e.code)}
            </label>
          ))}
        </div>
      )}
    </fieldset>
  );
}

function ResourceEditor({
  lang,
  resource,
  facilities,
  onSaved,
  onCancel,
}: {
  lang: Lang;
  resource: SchedulableResource | null;
  facilities: Facility[];
  onSaved: () => void;
  onCancel: () => void;
}) {
  const types = useCatalog("resource_type");
  const professions = useCatalog("profession");
  const specialties = useCatalog("specialty");
  const accessibility = useCatalog("accessibility_capability");
  const services = useCatalog("clinical_service");
  const modalities = useCatalog("modality");
  const [form, setForm] = useState<ResourceForm>(() =>
    resource ? toForm(resource) : emptyForm(facilities[0]?.id ?? ""),
  );
  const { busy, message, run } = useAction(lang);
  const set = (patch: Partial<ResourceForm>) =>
    setForm((f) => ({ ...f, ...patch }));
  const valid =
    form.name.trim().length > 0 &&
    form.resource_type_code.length > 0 &&
    form.facility_id.length > 0 &&
    (intOrNull(form.capacity) ?? 0) >= 1;

  async function save(e: React.FormEvent) {
    e.preventDefault();
    const languages = form.languages
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
    const ok = await run(
      async () => {
        if (resource) {
          await postOps(`/api/v1/scheduling/resources/${resource.id}`, {
            version: resource.version,
            name: form.name.trim(),
            profession_code: form.profession_code || null,
            specialty_codes: form.specialty_codes,
            languages,
            accessibility_codes: form.accessibility_codes,
            capacity: intOrNull(form.capacity),
            time_zone: form.time_zone.trim() || null,
            services: serviceBodies(form.services),
            reason: form.reason.trim() || null,
          });
        } else {
          await postOps("/api/v1/scheduling/resources", {
            facility_id: form.facility_id,
            resource_type_code: form.resource_type_code,
            name: form.name.trim(),
            profession_code: form.profession_code || null,
            specialty_codes: form.specialty_codes,
            languages,
            accessibility_codes: form.accessibility_codes,
            capacity: intOrNull(form.capacity),
            time_zone: form.time_zone.trim() || null,
            services: serviceBodies(form.services),
          });
        }
      },
      resource ? "resourceUpdated" : "resourceCreated",
    );
    if (ok) onSaved();
  }

  return (
    <form
      className="card nested"
      onSubmit={(e) => void save(e)}
      aria-labelledby="res-editor-h"
      data-testid="resource-editor"
    >
      <h4 id="res-editor-h">
        {resource ? t(lang, "editResource") : t(lang, "addResource")}
      </h4>
      <div className="grid-2">
        <div>
          <label htmlFor="re-name">{t(lang, "name")}</label>
          <input
            id="re-name"
            value={form.name}
            onChange={(e) => set({ name: e.target.value })}
            required
            maxLength={200}
          />
        </div>
        <div>
          <label htmlFor="re-facility">{t(lang, "facility")}</label>
          <select
            id="re-facility"
            value={form.facility_id}
            onChange={(e) => set({ facility_id: e.target.value })}
            disabled={!!resource}
            required
          >
            {facilities.map((f) => (
              <option key={f.id} value={f.id}>
                {f.name}
              </option>
            ))}
          </select>
        </div>
        <div>
          <label htmlFor="re-type">{t(lang, "resourceType")}</label>
          <select
            id="re-type"
            value={form.resource_type_code}
            onChange={(e) => set({ resource_type_code: e.target.value })}
            disabled={!!resource}
            required
          >
            <option value="">—</option>
            {types.entries.map((x) => (
              <option key={x.code} value={x.code}>
                {nameFor(lang, types.entries, x.code)}
              </option>
            ))}
          </select>
        </div>
        <div>
          <label htmlFor="re-prof">{t(lang, "profession")}</label>
          <select
            id="re-prof"
            value={form.profession_code}
            onChange={(e) => set({ profession_code: e.target.value })}
          >
            <option value="">—</option>
            {professions.entries.map((x) => (
              <option key={x.code} value={x.code}>
                {nameFor(lang, professions.entries, x.code)}
              </option>
            ))}
          </select>
        </div>
        <div>
          <label htmlFor="re-cap">{t(lang, "capacity")}</label>
          <input
            id="re-cap"
            type="number"
            min={1}
            max={500}
            value={form.capacity}
            onChange={(e) => set({ capacity: e.target.value })}
            required
          />
        </div>
        <div>
          <label htmlFor="re-tz">{t(lang, "timeZone")}</label>
          <input
            id="re-tz"
            value={form.time_zone}
            onChange={(e) => set({ time_zone: e.target.value })}
            placeholder="Europe/Madrid"
            aria-describedby="re-tz-help"
          />
          <p id="re-tz-help" className="muted">
            {t(lang, "timeZoneHelp")}
          </p>
        </div>
        <div>
          <label htmlFor="re-lang">{t(lang, "languages")}</label>
          <input
            id="re-lang"
            value={form.languages}
            onChange={(e) => set({ languages: e.target.value })}
            placeholder="es, en, ca"
            aria-describedby="re-lang-help"
          />
          <p id="re-lang-help" className="muted">
            {t(lang, "commaSeparated")}
          </p>
        </div>
      </div>
      <CodeChips
        id="re-spec"
        lang={lang}
        label={t(lang, "specialties")}
        entries={specialties.entries}
        selected={form.specialty_codes}
        onChange={(codes) => set({ specialty_codes: codes })}
      />
      <CodeChips
        id="re-acc"
        lang={lang}
        label={t(lang, "accessibilityCapabilities")}
        entries={accessibility.entries}
        selected={form.accessibility_codes}
        onChange={(codes) => set({ accessibility_codes: codes })}
      />
      <fieldset>
        <legend>{t(lang, "servicesDelivered")}</legend>
        <p className="muted">{t(lang, "servicesDeliveredHelp")}</p>
        {form.services.map((s, i) => (
          <div key={i} className="grid-2 service-row">
            <div>
              <label htmlFor={`re-svc-${i}`}>{t(lang, "service")}</label>
              <select
                id={`re-svc-${i}`}
                value={s.service_code}
                onChange={(e) => {
                  const next = [...form.services];
                  next[i] = { ...s, service_code: e.target.value };
                  set({ services: next });
                }}
              >
                <option value="">—</option>
                {services.entries.map((x) => (
                  <option key={x.code} value={x.code}>
                    {nameFor(lang, services.entries, x.code)}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label htmlFor={`re-dur-${i}`}>
                {t(lang, "durationMinutes")}
              </label>
              <input
                id={`re-dur-${i}`}
                type="number"
                min={5}
                max={600}
                value={s.duration_minutes}
                onChange={(e) => {
                  const next = [...form.services];
                  next[i] = { ...s, duration_minutes: e.target.value };
                  set({ services: next });
                }}
              />
            </div>
            <div>
              <label htmlFor={`re-prep-${i}`}>{t(lang, "prepMinutes")}</label>
              <input
                id={`re-prep-${i}`}
                type="number"
                min={0}
                max={240}
                value={s.prep_minutes}
                onChange={(e) => {
                  const next = [...form.services];
                  next[i] = { ...s, prep_minutes: e.target.value };
                  set({ services: next });
                }}
              />
            </div>
            <div>
              <label htmlFor={`re-clean-${i}`}>
                {t(lang, "cleanupMinutes")}
              </label>
              <input
                id={`re-clean-${i}`}
                type="number"
                min={0}
                max={240}
                value={s.cleanup_minutes}
                onChange={(e) => {
                  const next = [...form.services];
                  next[i] = { ...s, cleanup_minutes: e.target.value };
                  set({ services: next });
                }}
              />
            </div>
            <CodeChips
              id={`re-mod-${i}`}
              lang={lang}
              label={t(lang, "modality")}
              entries={modalities.entries}
              selected={s.modality_codes}
              onChange={(codes) => {
                const next = [...form.services];
                next[i] = { ...s, modality_codes: codes };
                set({ services: next });
              }}
            />
            <div>
              <button
                type="button"
                className="secondary"
                onClick={() =>
                  set({ services: form.services.filter((_, j) => j !== i) })
                }
              >
                {t(lang, "removeService")}
              </button>
            </div>
          </div>
        ))}
        <button
          type="button"
          className="secondary"
          onClick={() =>
            set({
              services: [
                ...form.services,
                {
                  service_code: "",
                  duration_minutes: "",
                  prep_minutes: "",
                  cleanup_minutes: "",
                  modality_codes: [],
                },
              ],
            })
          }
        >
          {t(lang, "addService")}
        </button>
      </fieldset>
      {resource ? (
        <ReasonField
          id="re-reason"
          lang={lang}
          value={form.reason}
          onChange={(v) => set({ reason: v })}
          label={t(lang, "changeReason")}
        />
      ) : null}
      <MessageLine message={message} />
      <div className="actions">
        <button type="submit" disabled={busy || !valid}>
          {resource ? t(lang, "saveChanges") : t(lang, "createResource")}
        </button>
        <button type="button" className="secondary" onClick={onCancel}>
          {t(lang, "cancel")}
        </button>
      </div>
    </form>
  );
}

type RuleRow = {
  weekday: string;
  start_local: string;
  end_local: string;
  kind: string;
  capacity: string;
};

function rulesToRows(rules: AvailabilityRule[]): RuleRow[] {
  return rules.map((r) => ({
    weekday: String(r.weekday),
    start_local: r.start_local.slice(0, 5),
    end_local: r.end_local.slice(0, 5),
    kind: r.kind,
    capacity: r.capacity?.toString() ?? "",
  }));
}

function AvailabilityEditor({
  lang,
  resource,
  onSaved,
}: {
  lang: Lang;
  resource: SchedulableResource;
  onSaved: () => void;
}) {
  const [rows, setRows] = useState<RuleRow[]>(() =>
    rulesToRows(resource.availability_rules ?? []),
  );
  const { busy, message, run } = useAction(lang);
  const valid = rows.every(
    (r) => r.start_local && r.end_local && r.start_local < r.end_local,
  );

  async function save() {
    const ok = await run(async () => {
      await postOps(
        `/api/v1/scheduling/resources/${resource.id}/availability`,
        {
          version: resource.version,
          rules: rows.map((r) => ({
            weekday: Number(r.weekday),
            start_local: `${r.start_local}:00`,
            end_local: `${r.end_local}:00`,
            kind: r.kind,
            capacity: intOrNull(r.capacity),
          })),
        },
      );
    }, "availabilitySaved");
    if (ok) onSaved();
  }

  return (
    <section className="card nested" aria-labelledby="avail-h">
      <h4 id="avail-h">{t(lang, "weeklyAvailability")}</h4>
      <p className="muted">{t(lang, "weeklyAvailabilityHelp")}</p>
      <div className="table-wrap">
        <table data-testid="availability-rules">
          <thead>
            <tr>
              <th scope="col">{t(lang, "weekday")}</th>
              <th scope="col">{t(lang, "startTime")}</th>
              <th scope="col">{t(lang, "endTime")}</th>
              <th scope="col">{t(lang, "ruleKind")}</th>
              <th scope="col">{t(lang, "capacity")}</th>
              <th scope="col">{t(lang, "actions")}</th>
            </tr>
          </thead>
          <tbody>
            {rows.map((r, i) => {
              const update = (patch: Partial<RuleRow>) => {
                const next = [...rows];
                next[i] = { ...r, ...patch };
                setRows(next);
              };
              return (
                <tr key={i}>
                  <td>
                    <label className="sr-only" htmlFor={`rule-wd-${i}`}>
                      {t(lang, "weekday")}
                    </label>
                    <select
                      id={`rule-wd-${i}`}
                      value={r.weekday}
                      onChange={(e) => update({ weekday: e.target.value })}
                    >
                      {[1, 2, 3, 4, 5, 6, 7].map((d) => (
                        <option key={d} value={d}>
                          {weekdayLabel(lang, d)}
                        </option>
                      ))}
                    </select>
                  </td>
                  <td>
                    <label className="sr-only" htmlFor={`rule-start-${i}`}>
                      {t(lang, "startTime")}
                    </label>
                    <input
                      id={`rule-start-${i}`}
                      type="time"
                      value={r.start_local}
                      onChange={(e) => update({ start_local: e.target.value })}
                      required
                    />
                  </td>
                  <td>
                    <label className="sr-only" htmlFor={`rule-end-${i}`}>
                      {t(lang, "endTime")}
                    </label>
                    <input
                      id={`rule-end-${i}`}
                      type="time"
                      value={r.end_local}
                      onChange={(e) => update({ end_local: e.target.value })}
                      required
                    />
                  </td>
                  <td>
                    <label className="sr-only" htmlFor={`rule-kind-${i}`}>
                      {t(lang, "ruleKind")}
                    </label>
                    <select
                      id={`rule-kind-${i}`}
                      value={r.kind}
                      onChange={(e) => update({ kind: e.target.value })}
                    >
                      <option value="available">
                        {t(lang, "ruleAvailable")}
                      </option>
                      <option value="break">{t(lang, "ruleBreak")}</option>
                    </select>
                  </td>
                  <td>
                    <label className="sr-only" htmlFor={`rule-cap-${i}`}>
                      {t(lang, "capacity")}
                    </label>
                    <input
                      id={`rule-cap-${i}`}
                      type="number"
                      min={1}
                      value={r.capacity}
                      onChange={(e) => update({ capacity: e.target.value })}
                      placeholder={String(resource.capacity)}
                    />
                  </td>
                  <td>
                    <button
                      type="button"
                      className="secondary"
                      onClick={() => setRows(rows.filter((_, j) => j !== i))}
                    >
                      {t(lang, "remove")}
                    </button>
                  </td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
      <MessageLine message={message} />
      <div className="actions">
        <button
          type="button"
          className="secondary"
          onClick={() =>
            setRows([
              ...rows,
              {
                weekday: "1",
                start_local: "09:00",
                end_local: "13:00",
                kind: "available",
                capacity: "",
              },
            ])
          }
        >
          {t(lang, "addRule")}
        </button>
        <button
          type="button"
          disabled={busy || !valid}
          onClick={() => void save()}
        >
          {t(lang, "saveAvailability")}
        </button>
      </div>
    </section>
  );
}

function ExceptionsEditor({
  lang,
  resource,
  onChanged,
}: {
  lang: Lang;
  resource: SchedulableResource;
  onChanged: () => void;
}) {
  const [form, setForm] = useState({
    kind: "leave",
    starts_at: "",
    ends_at: "",
    capacity_delta: "",
    reason_code: "",
  });
  const { busy, message, run } = useAction(lang);
  const [removing, setRemoving] = useState<string | null>(null);
  const valid =
    form.starts_at &&
    form.ends_at &&
    new Date(form.starts_at) < new Date(form.ends_at) &&
    (form.kind !== "extra_capacity" ||
      (intOrNull(form.capacity_delta) ?? 0) > 0);

  async function add(e: React.FormEvent) {
    e.preventDefault();
    const ok = await run(async () => {
      await postOps(`/api/v1/scheduling/resources/${resource.id}/exceptions`, {
        kind: form.kind,
        starts_at: new Date(form.starts_at).toISOString(),
        ends_at: new Date(form.ends_at).toISOString(),
        capacity_delta:
          form.kind === "extra_capacity"
            ? intOrNull(form.capacity_delta)
            : null,
        reason_code: form.reason_code.trim() || null,
      });
      setForm({ ...form, starts_at: "", ends_at: "", capacity_delta: "" });
    }, "exceptionAdded");
    if (ok) onChanged();
  }

  async function remove(id: string) {
    const ok = await run(async () => {
      await opsFetch(
        `/api/v1/scheduling/resources/${resource.id}/exceptions/${id}`,
        { method: "DELETE" },
      );
      setRemoving(null);
    }, "exceptionRemoved");
    if (ok) onChanged();
  }

  const exceptions = resource.exceptions ?? [];
  return (
    <section className="card nested" aria-labelledby="exc-h">
      <h4 id="exc-h">{t(lang, "exceptions")}</h4>
      <p className="muted">{t(lang, "exceptionsHelp")}</p>
      {exceptions.length === 0 ? (
        <p className="muted">{t(lang, "noExceptions")}</p>
      ) : (
        <ul className="stack" data-testid="exception-list">
          {exceptions.map((x) => (
            <li key={x.id} className="row-card">
              <div className="row-main">
                <strong>{exceptionKindLabel(lang, x.kind)}</strong> ·{" "}
                {formatDateTime(lang, x.starts_at)} →{" "}
                {formatDateTime(lang, x.ends_at)}
                {x.capacity_delta ? ` · +${x.capacity_delta}` : ""}
                {x.reason_code ? ` · ${x.reason_code}` : ""}
              </div>
              <button
                type="button"
                className="secondary"
                disabled={busy}
                onClick={() => setRemoving(x.id)}
              >
                {t(lang, "remove")}
              </button>
              {removing === x.id ? (
                <ConfirmBox
                  lang={lang}
                  title={t(lang, "removeException")}
                  busy={busy}
                  onConfirm={() => void remove(x.id)}
                  onCancel={() => setRemoving(null)}
                />
              ) : null}
            </li>
          ))}
        </ul>
      )}
      <form onSubmit={(e) => void add(e)} className="grid-2">
        <div>
          <label htmlFor="exc-kind">{t(lang, "exceptionKind")}</label>
          <select
            id="exc-kind"
            value={form.kind}
            onChange={(e) => setForm({ ...form, kind: e.target.value })}
          >
            {EXCEPTION_KINDS.map((k) => (
              <option key={k} value={k}>
                {exceptionKindLabel(lang, k)}
              </option>
            ))}
          </select>
        </div>
        <div>
          <label htmlFor="exc-start">{t(lang, "startsAt")}</label>
          <input
            id="exc-start"
            type="datetime-local"
            value={form.starts_at}
            onChange={(e) => setForm({ ...form, starts_at: e.target.value })}
            required
          />
        </div>
        <div>
          <label htmlFor="exc-end">{t(lang, "endsAt")}</label>
          <input
            id="exc-end"
            type="datetime-local"
            value={form.ends_at}
            onChange={(e) => setForm({ ...form, ends_at: e.target.value })}
            required
          />
        </div>
        {form.kind === "extra_capacity" ? (
          <div>
            <label htmlFor="exc-delta">{t(lang, "capacityDelta")}</label>
            <input
              id="exc-delta"
              type="number"
              min={1}
              value={form.capacity_delta}
              onChange={(e) =>
                setForm({ ...form, capacity_delta: e.target.value })
              }
              required
            />
          </div>
        ) : null}
        <div>
          <label htmlFor="exc-reason">{t(lang, "reasonCode")}</label>
          <input
            id="exc-reason"
            value={form.reason_code}
            onChange={(e) => setForm({ ...form, reason_code: e.target.value })}
            maxLength={64}
          />
        </div>
        <div className="actions">
          <button type="submit" disabled={busy || !valid}>
            {t(lang, "addException")}
          </button>
        </div>
      </form>
      <MessageLine message={message} />
    </section>
  );
}

function ResourceDetail({
  lang,
  id,
  facilities,
  onChanged,
  onClose,
}: {
  lang: Lang;
  id: string;
  facilities: Facility[];
  onChanged: () => void;
  onClose: () => void;
}) {
  const state = useLoader(
    () => opsFetch<SchedulableResource>(`/api/v1/scheduling/resources/${id}`),
    id,
  );
  const [editing, setEditing] = useState(false);
  const [deactivating, setDeactivating] = useState("");
  const [showDeactivate, setShowDeactivate] = useState(false);
  const { busy, message, run } = useAction(lang);
  const types = useCatalog("resource_type");
  const services = useCatalog("clinical_service");

  async function setActive(r: SchedulableResource, active: boolean) {
    const ok = await run(async () => {
      await postOps(`/api/v1/scheduling/resources/${r.id}`, {
        version: r.version,
        active,
        reason: active ? t(lang, "reactivated") : deactivating.trim(),
      });
      setShowDeactivate(false);
    }, "resourceUpdated");
    if (ok) {
      state.reload();
      onChanged();
    }
  }

  return (
    <div
      className="card"
      role="region"
      aria-label={t(lang, "resourceDetail")}
      data-testid="resource-detail"
    >
      <PanelState
        lang={lang}
        state={state}
        isEmpty={() => false}
        emptyKey="noResources"
      >
        {(r) => (
          <>
            <div className="row-head">
              <div>
                <h3 style={{ marginTop: 0 }}>{r.name}</h3>
                <p className="muted">
                  {nameFor(lang, types.entries, r.resource_type_code)} ·{" "}
                  {facilities.find((f) => f.id === r.facility_id)?.name ??
                    r.facility_id}{" "}
                  · {r.time_zone} · {t(lang, "capacity")} {r.capacity}
                </p>
              </div>
              <div className="actions">
                <StatusBadge
                  label={r.active ? t(lang, "active") : t(lang, "inactive")}
                  tone={r.active ? "ok" : "neutral"}
                />
                <button
                  type="button"
                  className="secondary"
                  onClick={() => setEditing((v) => !v)}
                  aria-expanded={editing}
                >
                  {t(lang, "editResource")}
                </button>
                {r.active ? (
                  <button
                    type="button"
                    className="secondary"
                    disabled={busy}
                    onClick={() => setShowDeactivate(true)}
                  >
                    {t(lang, "deactivate")}
                  </button>
                ) : (
                  <button
                    type="button"
                    className="secondary"
                    disabled={busy}
                    onClick={() => void setActive(r, true)}
                  >
                    {t(lang, "reactivate")}
                  </button>
                )}
                <button type="button" className="secondary" onClick={onClose}>
                  {t(lang, "closePanel")}
                </button>
              </div>
            </div>
            <MessageLine message={message} />
            {showDeactivate ? (
              <ConfirmBox
                lang={lang}
                title={t(lang, "deactivate")}
                busy={busy}
                disabled={deactivating.trim().length < 3}
                onConfirm={() => void setActive(r, false)}
                onCancel={() => setShowDeactivate(false)}
              >
                <p className="muted">{t(lang, "deactivateResourceHelp")}</p>
                <ReasonField
                  id="res-deact-reason"
                  lang={lang}
                  value={deactivating}
                  onChange={setDeactivating}
                  required
                  label={t(lang, "changeReason")}
                />
              </ConfirmBox>
            ) : null}
            {editing ? (
              <ResourceEditor
                lang={lang}
                resource={r}
                facilities={facilities}
                onSaved={() => {
                  setEditing(false);
                  state.reload();
                  onChanged();
                }}
                onCancel={() => setEditing(false)}
              />
            ) : null}
            <h4>{t(lang, "servicesDelivered")}</h4>
            {(r.services ?? []).length === 0 ? (
              <p className="muted">{t(lang, "noServicesConfigured")}</p>
            ) : (
              <ul>
                {(r.services ?? []).map((s) => (
                  <li key={s.service_code}>
                    {nameFor(lang, services.entries, s.service_code)}
                    {s.duration_minutes ? ` · ${s.duration_minutes} min` : ""}
                    {s.prep_minutes || s.cleanup_minutes
                      ? ` (+${s.prep_minutes ?? 0}/+${s.cleanup_minutes ?? 0})`
                      : ""}
                    {s.modality_codes.length > 0
                      ? ` · ${s.modality_codes.join(", ")}`
                      : ""}
                  </li>
                ))}
              </ul>
            )}
            <AvailabilityEditor
              key={`${r.id}:${r.version}`}
              lang={lang}
              resource={r}
              onSaved={() => {
                state.reload();
                onChanged();
              }}
            />
            <ExceptionsEditor
              lang={lang}
              resource={r}
              onChanged={() => {
                state.reload();
                onChanged();
              }}
            />
          </>
        )}
      </PanelState>
    </div>
  );
}

function ResourcesAdmin() {
  const { lang, meta } = useSession();
  const caps = meta?.scheduling_capabilities ?? NO_SCHEDULING_CAPABILITIES;
  const facilities = meta?.facilities.filter((f) => f.accessible) ?? [];
  const [facilityId, setFacilityId] = useState("");
  const [typeCode, setTypeCode] = useState("");
  const [includeInactive, setIncludeInactive] = useState(false);
  const [adding, setAdding] = useState(false);
  const [selected, setSelected] = useState<string | null>(null);
  const types = useCatalog("resource_type");
  const state = useLoader(
    () =>
      opsFetch<Page<SchedulableResource>>(
        `/api/v1/scheduling/resources${query({
          facility_id: facilityId,
          resource_type_code: typeCode,
          include_inactive: includeInactive || undefined,
          limit: 200,
        })}`,
      ),
    `${facilityId}:${typeCode}:${includeInactive}`,
    caps.can_manage_resources,
  );

  if (meta && !caps.can_manage_resources) {
    return (
      <div className="card">
        <p role="alert" className="error">
          {t(lang, "unauthorizedPanel")}
        </p>
        <Link href="/scheduling">{t(lang, "backToScheduling")}</Link>
      </div>
    );
  }

  return (
    <>
      <div className="row-head">
        <div>
          <h2 style={{ marginTop: 0 }}>{t(lang, "resourceAdmin")}</h2>
          <p className="muted">{t(lang, "resourceAdminIntro")}</p>
        </div>
        <Link href="/scheduling" className="button secondary">
          {t(lang, "backToScheduling")}
        </Link>
      </div>
      <div className="card">
        <div className="filters">
          {facilities.length > 1 ? (
            <div>
              <label htmlFor="res-facility">{t(lang, "facility")}</label>
              <select
                id="res-facility"
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
          ) : null}
          <div>
            <label htmlFor="res-type">{t(lang, "resourceType")}</label>
            <select
              id="res-type"
              value={typeCode}
              onChange={(e) => setTypeCode(e.target.value)}
            >
              <option value="">{t(lang, "allTypes")}</option>
              {types.entries.map((x) => (
                <option key={x.code} value={x.code}>
                  {nameFor(lang, types.entries, x.code)}
                </option>
              ))}
            </select>
          </div>
          <label className="chip">
            <input
              type="checkbox"
              checked={includeInactive}
              onChange={(e) => setIncludeInactive(e.target.checked)}
            />{" "}
            {t(lang, "includeInactive")}
          </label>
          <button
            type="button"
            data-testid="add-resource"
            onClick={() => setAdding(true)}
          >
            {t(lang, "addResource")}
          </button>
        </div>
        {adding ? (
          <ResourceEditor
            lang={lang}
            resource={null}
            facilities={facilities}
            onSaved={() => {
              setAdding(false);
              state.reload();
            }}
            onCancel={() => setAdding(false)}
          />
        ) : null}
        <PanelState
          lang={lang}
          state={state}
          isEmpty={(d) => d.items.length === 0}
          emptyKey="noResources"
        >
          {(d) => (
            <div className="table-wrap">
              <table data-testid="resource-table">
                <thead>
                  <tr>
                    <th scope="col">{t(lang, "name")}</th>
                    <th scope="col">{t(lang, "resourceType")}</th>
                    <th scope="col">{t(lang, "facility")}</th>
                    <th scope="col">{t(lang, "capacity")}</th>
                    <th scope="col">{t(lang, "status")}</th>
                    <th scope="col">{t(lang, "actions")}</th>
                  </tr>
                </thead>
                <tbody>
                  {d.items.map((r) => (
                    <tr key={r.id}>
                      <td>{r.name}</td>
                      <td>
                        {nameFor(lang, types.entries, r.resource_type_code)}
                      </td>
                      <td>
                        {facilities.find((f) => f.id === r.facility_id)?.name ??
                          r.facility_id}
                      </td>
                      <td>{r.capacity}</td>
                      <td>
                        <StatusBadge
                          label={
                            r.active ? t(lang, "active") : t(lang, "inactive")
                          }
                          tone={r.active ? "ok" : "neutral"}
                        />
                      </td>
                      <td>
                        <button
                          type="button"
                          className="secondary"
                          onClick={() => setSelected(r.id)}
                          aria-expanded={selected === r.id}
                        >
                          {t(lang, "manage")}
                        </button>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </PanelState>
      </div>
      {selected ? (
        <ResourceDetail
          key={selected}
          lang={lang}
          id={selected}
          facilities={facilities}
          onChanged={state.reload}
          onClose={() => setSelected(null)}
        />
      ) : null}
    </>
  );
}

export default function ResourcesPage() {
  return (
    <AppShell>
      <ResourcesAdmin />
    </AppShell>
  );
}
