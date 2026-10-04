"use client";

import { useRef, useState } from "react";
import { t, type Lang } from "@/lib/i18n";
import { apiFetch, useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import {
  SELF_SERVICE_CONSENTS,
  consentHelp,
  consentLabel,
  notificationKindLabel,
  query,
  waitlistStatusLabel,
  weekdayLabel,
  type AppNotification,
  type CalendarSource,
  type Consent,
  type Offer,
  type Preferences,
  type WaitlistEntry,
  type WeeklyWindow,
} from "@/lib/access";
import {
  ConfirmBox,
  MessageLine,
  OfferCard,
  PanelState,
  StatusBadge,
  nameFor,
  postJson,
  useAction,
  useCatalog,
  useLoader,
  type OfferAction,
} from "../../scheduling/shared";
import { patientAccessApi } from "../../scheduling/find-appointment";

const ME = "/api/v1/me";

type Items<T> = { patient_id: string; items: T[] };

// ---------------------------------------------------------------------------
// Weekly windows
// ---------------------------------------------------------------------------

export function WindowsEditor({
  lang,
  idPrefix,
  label,
  help,
  windows,
  onChange,
}: {
  lang: Lang;
  idPrefix: string;
  label: string;
  help?: string;
  windows: WeeklyWindow[];
  onChange: (w: WeeklyWindow[]) => void;
}) {
  return (
    <fieldset>
      <legend>{label}</legend>
      {help ? <p className="muted">{help}</p> : null}
      {windows.length === 0 ? (
        <p className="muted">{t(lang, "noWindows")}</p>
      ) : null}
      <ul className="stack compact">
        {windows.map((w, i) => {
          const update = (patch: Partial<WeeklyWindow>) => {
            const next = [...windows];
            next[i] = { ...w, ...patch };
            onChange(next);
          };
          return (
            <li key={i} className="window-row">
              <label className="sr-only" htmlFor={`${idPrefix}-wd-${i}`}>
                {t(lang, "weekday")}
              </label>
              <select
                id={`${idPrefix}-wd-${i}`}
                value={w.weekday}
                onChange={(e) => update({ weekday: Number(e.target.value) })}
              >
                {[1, 2, 3, 4, 5, 6, 7].map((d) => (
                  <option key={d} value={d}>
                    {weekdayLabel(lang, d)}
                  </option>
                ))}
              </select>
              <label className="sr-only" htmlFor={`${idPrefix}-start-${i}`}>
                {t(lang, "startTime")}
              </label>
              <input
                id={`${idPrefix}-start-${i}`}
                type="time"
                value={w.start.slice(0, 5)}
                onChange={(e) => update({ start: `${e.target.value}:00` })}
              />
              <label className="sr-only" htmlFor={`${idPrefix}-end-${i}`}>
                {t(lang, "endTime")}
              </label>
              <input
                id={`${idPrefix}-end-${i}`}
                type="time"
                value={w.end.slice(0, 5)}
                onChange={(e) => update({ end: `${e.target.value}:00` })}
              />
              <button
                type="button"
                className="secondary"
                onClick={() => onChange(windows.filter((_, j) => j !== i))}
                aria-label={`${t(lang, "remove")} ${weekdayLabel(lang, w.weekday)} ${w.start.slice(0, 5)}`}
              >
                {t(lang, "remove")}
              </button>
            </li>
          );
        })}
      </ul>
      <button
        type="button"
        className="secondary"
        onClick={() =>
          onChange([
            ...windows,
            { weekday: 1, start: "09:00:00", end: "13:00:00" },
          ])
        }
      >
        {t(lang, "addWindow")}
      </button>
    </fieldset>
  );
}

// ---------------------------------------------------------------------------
// Preferences, consents, calendars
// ---------------------------------------------------------------------------

type PrefForm = {
  available_windows: WeeklyWindow[];
  unavailable_windows: WeeklyWindow[];
  preferred_modalities: string[];
  preferred_facility_ids: string[];
  language: string;
  accessibility_needs: string[];
  time_zone: string;
  channels: string[];
  quiet_hours_start: string;
  quiet_hours_end: string;
  contact_email: string;
};

function toPrefForm(p: Preferences): PrefForm {
  return {
    available_windows: p.available_windows,
    unavailable_windows: p.unavailable_windows,
    preferred_modalities: p.preferred_modalities,
    preferred_facility_ids: p.preferred_facility_ids,
    language: p.language ?? "",
    accessibility_needs: p.accessibility_needs,
    time_zone: p.time_zone ?? "",
    channels: p.channels,
    quiet_hours_start: p.quiet_hours_start?.slice(0, 5) ?? "",
    quiet_hours_end: p.quiet_hours_end?.slice(0, 5) ?? "",
    contact_email: "",
  };
}

function PreferencesForm({
  lang,
  patientId,
  prefs,
  onSaved,
}: {
  lang: Lang;
  patientId: string;
  prefs: Preferences;
  onSaved: () => void;
}) {
  const { meta } = useSession();
  const modalities = useCatalog("modality");
  const accessibility = useCatalog("accessibility_capability");
  const [form, setForm] = useState<PrefForm>(() => toPrefForm(prefs));
  const [clearEmail, setClearEmail] = useState(false);
  const { busy, message, run } = useAction(lang);
  const set = (patch: Partial<PrefForm>) =>
    setForm((f) => ({ ...f, ...patch }));
  const facilities = meta?.facilities ?? [];

  function toggle(list: string[], code: string): string[] {
    return list.includes(code)
      ? list.filter((c) => c !== code)
      : [...list, code];
  }

  async function save(e: React.FormEvent) {
    e.preventDefault();
    const email = form.contact_email.trim();
    const ok = await run(async () => {
      await apiFetch(`${ME}/preferences`, {
        method: "PUT",
        body: JSON.stringify({
          patient_id: patientId,
          version: prefs.version,
          available_windows: form.available_windows,
          unavailable_windows: form.unavailable_windows,
          preferred_modalities: form.preferred_modalities,
          preferred_facility_ids: form.preferred_facility_ids,
          language: form.language || null,
          accessibility_needs: form.accessibility_needs,
          time_zone: form.time_zone.trim() || null,
          channels: form.channels,
          quiet_hours_start: form.quiet_hours_start
            ? `${form.quiet_hours_start}:00`
            : null,
          quiet_hours_end: form.quiet_hours_end
            ? `${form.quiet_hours_end}:00`
            : null,
          ...(clearEmail
            ? { contact_email: null }
            : email
              ? { contact_email: email }
              : {}),
        }),
      });
    }, "preferencesSaved");
    if (ok) onSaved();
  }

  return (
    <form
      onSubmit={(e) => void save(e)}
      aria-labelledby="prefs-h"
      data-testid="preferences-form"
    >
      <h3 id="prefs-h">{t(lang, "schedulingPreferences")}</h3>
      <p className="muted">{t(lang, "schedulingPreferencesHelp")}</p>
      <WindowsEditor
        lang={lang}
        idPrefix="avail"
        label={t(lang, "availableWindows")}
        help={t(lang, "availableWindowsHelp")}
        windows={form.available_windows}
        onChange={(w) => set({ available_windows: w })}
      />
      <WindowsEditor
        lang={lang}
        idPrefix="unavail"
        label={t(lang, "unavailableWindows")}
        windows={form.unavailable_windows}
        onChange={(w) => set({ unavailable_windows: w })}
      />
      <fieldset>
        <legend>{t(lang, "preferredModalities")}</legend>
        <div className="chips">
          {modalities.entries.map((m) => (
            <label key={m.code} className="chip">
              <input
                type="checkbox"
                checked={form.preferred_modalities.includes(m.code)}
                onChange={() =>
                  set({
                    preferred_modalities: toggle(
                      form.preferred_modalities,
                      m.code,
                    ),
                  })
                }
              />{" "}
              {nameFor(lang, modalities.entries, m.code)}
            </label>
          ))}
        </div>
      </fieldset>
      {facilities.length > 1 ? (
        <fieldset>
          <legend>{t(lang, "preferredFacilities")}</legend>
          <div className="chips">
            {facilities.map((f) => (
              <label key={f.id} className="chip">
                <input
                  type="checkbox"
                  checked={form.preferred_facility_ids.includes(f.id)}
                  onChange={() =>
                    set({
                      preferred_facility_ids: toggle(
                        form.preferred_facility_ids,
                        f.id,
                      ),
                    })
                  }
                />{" "}
                {f.name}
              </label>
            ))}
          </div>
        </fieldset>
      ) : null}
      <fieldset>
        <legend>{t(lang, "accessibilityNeeds")}</legend>
        <div className="chips">
          {accessibility.entries.map((a) => (
            <label key={a.code} className="chip">
              <input
                type="checkbox"
                checked={form.accessibility_needs.includes(a.code)}
                onChange={() =>
                  set({
                    accessibility_needs: toggle(
                      form.accessibility_needs,
                      a.code,
                    ),
                  })
                }
              />{" "}
              {nameFor(lang, accessibility.entries, a.code)}
            </label>
          ))}
        </div>
      </fieldset>
      <div className="grid-2">
        <div>
          <label htmlFor="pref-lang">{t(lang, "preferredLanguage")}</label>
          <select
            id="pref-lang"
            value={form.language}
            onChange={(e) => set({ language: e.target.value })}
          >
            <option value="">—</option>
            <option value="es">Español</option>
            <option value="en">English</option>
            <option value="ca">Català</option>
            <option value="de">Deutsch</option>
            <option value="fr">Français</option>
          </select>
        </div>
        <div>
          <label htmlFor="pref-tz">{t(lang, "timeZone")}</label>
          <input
            id="pref-tz"
            value={form.time_zone}
            onChange={(e) => set({ time_zone: e.target.value })}
            placeholder="Europe/Madrid"
          />
        </div>
      </div>
      <fieldset>
        <legend>{t(lang, "notificationChannels")}</legend>
        <p className="muted">{t(lang, "notificationChannelsHelp")}</p>
        <div className="chips">
          <label className="chip">
            <input type="checkbox" checked disabled /> {t(lang, "channelInApp")}
          </label>
          <label className="chip">
            <input
              type="checkbox"
              checked={form.channels.includes("email")}
              onChange={() => set({ channels: toggle(form.channels, "email") })}
            />{" "}
            {t(lang, "channelEmail")}
          </label>
          <label className="chip">
            <input
              type="checkbox"
              checked={form.channels.includes("webhook")}
              onChange={() =>
                set({ channels: toggle(form.channels, "webhook") })
              }
            />{" "}
            {t(lang, "channelPush")}
          </label>
        </div>
        <div className="grid-2">
          <div>
            <label htmlFor="pref-email">{t(lang, "contactEmail")}</label>
            <input
              id="pref-email"
              type="email"
              value={form.contact_email}
              onChange={(e) => set({ contact_email: e.target.value })}
              placeholder={
                prefs.has_contact_email
                  ? t(lang, "contactEmailStored")
                  : t(lang, "contactEmailNone")
              }
              disabled={clearEmail}
              autoComplete="email"
            />
            {prefs.has_contact_email ? (
              <label className="chip">
                <input
                  type="checkbox"
                  checked={clearEmail}
                  onChange={(e) => setClearEmail(e.target.checked)}
                />{" "}
                {t(lang, "removeContactEmail")}
              </label>
            ) : null}
          </div>
          <div>
            <label htmlFor="pref-qs">{t(lang, "quietHoursStart")}</label>
            <input
              id="pref-qs"
              type="time"
              value={form.quiet_hours_start}
              onChange={(e) => set({ quiet_hours_start: e.target.value })}
            />
            <label htmlFor="pref-qe">{t(lang, "quietHoursEnd")}</label>
            <input
              id="pref-qe"
              type="time"
              value={form.quiet_hours_end}
              onChange={(e) => set({ quiet_hours_end: e.target.value })}
            />
          </div>
        </div>
      </fieldset>
      <MessageLine message={message} />
      <div className="actions">
        <button type="submit" disabled={busy}>
          {t(lang, "savePreferences")}
        </button>
      </div>
    </form>
  );
}

function ConsentsPanel({ lang, patientId }: { lang: Lang; patientId: string }) {
  const state = useLoader(
    () =>
      apiFetch<Items<Consent>>(
        `${ME}/consents${query({ patient_id: patientId })}`,
      ),
    patientId,
  );
  const { busy, message, run } = useAction(lang);

  async function set(purpose: string, active: boolean) {
    const ok = await run(async () => {
      await postJson(`${ME}/consents`, {
        patient_id: patientId,
        purpose,
        status: active ? "active" : "revoked",
      });
    }, "consentUpdated");
    if (ok) state.reload();
  }

  return (
    <section aria-labelledby="consents-h" data-testid="consents">
      <h3 id="consents-h">{t(lang, "schedulingConsents")}</h3>
      <p className="muted">{t(lang, "schedulingConsentsHelp")}</p>
      <PanelState
        lang={lang}
        state={state}
        isEmpty={() => false}
        emptyKey="noConsents"
      >
        {(d) => (
          <ul className="stack compact">
            {SELF_SERVICE_CONSENTS.map((purpose) => {
              const active =
                d.items.find((c) => c.purpose === purpose)?.status === "active";
              return (
                <li key={purpose} className="row-card">
                  <div className="row-main">
                    <strong>{consentLabel(lang, purpose)}</strong>
                    <p className="muted">{consentHelp(lang, purpose)}</p>
                  </div>
                  <StatusBadge
                    label={
                      active
                        ? t(lang, "consentActive")
                        : t(lang, "consentRevoked")
                    }
                    tone={active ? "ok" : "neutral"}
                  />
                  <button
                    type="button"
                    className="secondary"
                    disabled={busy}
                    onClick={() => void set(purpose, !active)}
                    aria-pressed={active}
                  >
                    {active
                      ? t(lang, "revokeConsent")
                      : t(lang, "grantConsent")}
                  </button>
                </li>
              );
            })}
          </ul>
        )}
      </PanelState>
      <MessageLine message={message} />
    </section>
  );
}

type CalendarsData = {
  patient_id: string;
  calendar_consent: boolean;
  limits: {
    max_bytes: number;
    max_intervals: number;
    max_horizon_days: number;
  };
  items: CalendarSource[];
};

function CalendarsPanel({
  lang,
  patientId,
}: {
  lang: Lang;
  patientId: string;
}) {
  const state = useLoader(
    () =>
      apiFetch<CalendarsData>(
        `${ME}/calendars${query({ patient_id: patientId })}`,
      ),
    patientId,
  );
  const { busy, message, setMessage, run } = useAction(lang);
  const fileRef = useRef<HTMLInputElement | null>(null);
  const [tz, setTz] = useState("");
  const [disconnecting, setDisconnecting] = useState<string | null>(null);

  async function importFile(e: React.FormEvent) {
    e.preventDefault();
    const file = fileRef.current?.files?.[0];
    if (!file) {
      setMessage({ kind: "error", text: t(lang, "chooseIcsFile") });
      return;
    }
    const maxBytes = state.data?.limits.max_bytes ?? 1_000_000;
    if (file.size > maxBytes) {
      setMessage({ kind: "error", text: t(lang, "icsTooLarge") });
      return;
    }
    const text = await file.text();
    const ok = await run(async () => {
      await apiFetch(
        `${ME}/calendars/ics${query({
          patient_id: patientId,
          time_zone: tz.trim() || undefined,
        })}`,
        {
          method: "POST",
          headers: { "Content-Type": "text/calendar" },
          body: text,
        },
      );
      if (fileRef.current) fileRef.current.value = "";
    }, "calendarImported");
    if (ok) state.reload();
  }

  async function disconnect(id: string) {
    const ok = await run(async () => {
      await postJson(`${ME}/calendars/${id}/disconnect`, {
        patient_id: patientId,
      });
      setDisconnecting(null);
    }, "calendarDisconnected");
    if (ok) state.reload();
  }

  return (
    <section aria-labelledby="cal-h" data-testid="calendars">
      <h3 id="cal-h">{t(lang, "personalCalendar")}</h3>
      <p className="muted">{t(lang, "personalCalendarPrivacy")}</p>
      <PanelState
        lang={lang}
        state={state}
        isEmpty={() => false}
        emptyKey="noCalendars"
      >
        {(d) => (
          <>
            {!d.calendar_consent ? (
              <p className="advisory" role="status">
                {t(lang, "calendarConsentRequired")}
              </p>
            ) : null}
            {d.items.length === 0 ? (
              <p className="muted">{t(lang, "noCalendars")}</p>
            ) : (
              <ul className="stack compact" data-testid="calendar-sources">
                {d.items.map((s) => (
                  <li key={s.id} className="row-card">
                    <div className="row-main">
                      <strong>
                        {s.source_type === "ics_import"
                          ? t(lang, "sourceIcsImport")
                          : t(lang, "sourceDeviceSync")}
                      </strong>{" "}
                      <StatusBadge
                        label={
                          s.status === "connected"
                            ? t(lang, "calendarConnected")
                            : t(lang, "calendarDisconnected")
                        }
                        tone={s.status === "connected" ? "ok" : "neutral"}
                      />
                      <p className="muted">
                        {t(lang, "busyIntervals")}: {s.interval_count} ·{" "}
                        {s.time_zone}
                        {s.last_synced_at
                          ? ` · ${t(lang, "lastSynced")} ${formatDateTime(lang, s.last_synced_at)}`
                          : ""}
                      </p>
                    </div>
                    {s.status === "connected" ? (
                      <button
                        type="button"
                        className="secondary"
                        disabled={busy}
                        onClick={() => setDisconnecting(s.id)}
                      >
                        {t(lang, "disconnectCalendar")}
                      </button>
                    ) : null}
                    {disconnecting === s.id ? (
                      <ConfirmBox
                        lang={lang}
                        title={t(lang, "disconnectCalendar")}
                        busy={busy}
                        onConfirm={() => void disconnect(s.id)}
                        onCancel={() => setDisconnecting(null)}
                      >
                        <p className="muted">
                          {t(lang, "disconnectCalendarHelp")}
                        </p>
                      </ConfirmBox>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
            <form
              onSubmit={(e) => void importFile(e)}
              className="stack compact"
            >
              <div>
                <label htmlFor="ics-file">{t(lang, "importIcs")}</label>
                <input
                  id="ics-file"
                  ref={fileRef}
                  type="file"
                  accept=".ics,text/calendar"
                  disabled={!d.calendar_consent || busy}
                  aria-describedby="ics-help"
                />
                <p id="ics-help" className="muted">
                  {t(lang, "importIcsHelp")}
                </p>
              </div>
              <div>
                <label htmlFor="ics-tz">{t(lang, "timeZone")}</label>
                <input
                  id="ics-tz"
                  value={tz}
                  onChange={(e) => setTz(e.target.value)}
                  placeholder="Europe/Madrid"
                  disabled={!d.calendar_consent}
                />
              </div>
              <div className="actions">
                <button type="submit" disabled={!d.calendar_consent || busy}>
                  {t(lang, "importIcs")}
                </button>
              </div>
            </form>
          </>
        )}
      </PanelState>
      <MessageLine message={message} />
    </section>
  );
}

export function PreferencesSection({
  lang,
  patientId,
}: {
  lang: Lang;
  patientId: string;
}) {
  const state = useLoader(
    () =>
      apiFetch<Preferences>(
        `${ME}/preferences${query({ patient_id: patientId })}`,
      ),
    patientId,
  );
  return (
    <div className="stack">
      <div className="card">
        <PanelState
          lang={lang}
          state={state}
          isEmpty={() => false}
          emptyKey="noPreferences"
        >
          {(p) => (
            <PreferencesForm
              key={`${p.patient_id}:${p.version}`}
              lang={lang}
              patientId={patientId}
              prefs={p}
              onSaved={state.reload}
            />
          )}
        </PanelState>
      </div>
      <div className="card">
        <ConsentsPanel lang={lang} patientId={patientId} />
      </div>
      <div className="card">
        <CalendarsPanel lang={lang} patientId={patientId} />
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Waitlist
// ---------------------------------------------------------------------------

type JoinForm = {
  service_code: string;
  facility_ids: string[];
  modality_codes: string[];
  acceptable_windows: WeeklyWindow[];
  min_notice_hours: string;
  earliest: string;
  latest: string;
};

const EMPTY_JOIN: JoinForm = {
  service_code: "",
  facility_ids: [],
  modality_codes: [],
  acceptable_windows: [],
  min_notice_hours: "24",
  earliest: "",
  latest: "",
};

export function WaitlistSection({
  lang,
  patientId,
  onBooked,
}: {
  lang: Lang;
  patientId: string;
  onBooked: () => void;
}) {
  const { meta } = useSession();
  const services = useCatalog("clinical_service");
  const modalities = useCatalog("modality");
  const state = useLoader(
    () =>
      apiFetch<Items<WaitlistEntry>>(
        `${ME}/waitlist${query({ patient_id: patientId, status: "live" })}`,
      ),
    patientId,
  );
  const [joining, setJoining] = useState(false);
  const [form, setForm] = useState<JoinForm>(EMPTY_JOIN);
  const [leaving, setLeaving] = useState<string | null>(null);
  const { busy, message, run } = useAction(lang);
  const facilities = meta?.facilities ?? [];
  const set = (patch: Partial<JoinForm>) =>
    setForm((f) => ({ ...f, ...patch }));

  async function join(e: React.FormEvent) {
    e.preventDefault();
    const ok = await run(async () => {
      await postJson(`${ME}/waitlist`, {
        patient_id: patientId,
        service_code: form.service_code,
        facility_ids: form.facility_ids,
        modality_codes: form.modality_codes,
        acceptable_windows: form.acceptable_windows,
        min_notice_hours: Number(form.min_notice_hours) || 0,
        earliest: form.earliest ? new Date(form.earliest).toISOString() : null,
        latest: form.latest ? new Date(form.latest).toISOString() : null,
      });
      setJoining(false);
      setForm(EMPTY_JOIN);
    }, "waitlistJoined");
    if (ok) state.reload();
  }

  async function transition(
    entry: WaitlistEntry,
    action: "pause" | "resume" | "leave",
  ) {
    const ok = await run(
      async () => {
        await postJson(`${ME}/waitlist/${entry.id}/${action}`, {
          version: entry.version,
        });
        setLeaving(null);
      },
      action === "leave" ? "waitlistLeft" : "waitlistUpdated",
    );
    if (ok) state.reload();
  }

  async function onOffer(offer: Offer, action: OfferAction) {
    const ok = await run(async () => {
      if (action === "hold")
        await patientAccessApi.holdOffer(offer.id, offer.version);
      else if (action === "release")
        await patientAccessApi.releaseOffer(offer.id, offer.version);
      else if (action === "decline")
        await patientAccessApi.declineOffer(offer.id, offer.version, null);
      else if (action === "accept") {
        await patientAccessApi.acceptOffer(offer.id, {
          version: offer.version,
          idempotency_key: `wl:${offer.id}:${offer.version}`,
        });
        onBooked();
      }
    });
    if (ok) state.reload();
  }

  return (
    <div className="card" data-testid="waitlist-section">
      <div className="row-head">
        <div>
          <h3 style={{ marginTop: 0 }}>{t(lang, "waitlistTitle")}</h3>
          <p className="muted">{t(lang, "waitlistPatientHelp")}</p>
        </div>
        <button
          type="button"
          onClick={() => setJoining((v) => !v)}
          aria-expanded={joining}
        >
          {t(lang, "joinWaitlist")}
        </button>
      </div>
      {joining ? (
        <form
          className="card nested"
          onSubmit={(e) => void join(e)}
          aria-label={t(lang, "joinWaitlist")}
          data-testid="join-waitlist-form"
        >
          <div className="grid-2">
            <div>
              <label htmlFor="wl-service">{t(lang, "service")}</label>
              <select
                id="wl-service"
                value={form.service_code}
                onChange={(e) => set({ service_code: e.target.value })}
                required
              >
                <option value="">—</option>
                {services.entries.map((s) => (
                  <option key={s.code} value={s.code}>
                    {nameFor(lang, services.entries, s.code)}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label htmlFor="wl-notice">{t(lang, "minNoticeHours")}</label>
              <input
                id="wl-notice"
                type="number"
                min={0}
                max={720}
                value={form.min_notice_hours}
                onChange={(e) => set({ min_notice_hours: e.target.value })}
              />
            </div>
            <div>
              <label htmlFor="wl-earliest">{t(lang, "earliest")}</label>
              <input
                id="wl-earliest"
                type="datetime-local"
                value={form.earliest}
                onChange={(e) => set({ earliest: e.target.value })}
              />
            </div>
            <div>
              <label htmlFor="wl-latest">{t(lang, "latest")}</label>
              <input
                id="wl-latest"
                type="datetime-local"
                value={form.latest}
                onChange={(e) => set({ latest: e.target.value })}
              />
            </div>
          </div>
          {facilities.length > 1 ? (
            <fieldset>
              <legend>{t(lang, "facilities")}</legend>
              <div className="chips">
                {facilities.map((f) => (
                  <label key={f.id} className="chip">
                    <input
                      type="checkbox"
                      checked={form.facility_ids.includes(f.id)}
                      onChange={(e) =>
                        set({
                          facility_ids: e.target.checked
                            ? [...form.facility_ids, f.id]
                            : form.facility_ids.filter((x) => x !== f.id),
                        })
                      }
                    />{" "}
                    {f.name}
                  </label>
                ))}
              </div>
            </fieldset>
          ) : null}
          <fieldset>
            <legend>{t(lang, "modality")}</legend>
            <div className="chips">
              {modalities.entries.map((m) => (
                <label key={m.code} className="chip">
                  <input
                    type="checkbox"
                    checked={form.modality_codes.includes(m.code)}
                    onChange={(e) =>
                      set({
                        modality_codes: e.target.checked
                          ? [...form.modality_codes, m.code]
                          : form.modality_codes.filter((x) => x !== m.code),
                      })
                    }
                  />{" "}
                  {nameFor(lang, modalities.entries, m.code)}
                </label>
              ))}
            </div>
          </fieldset>
          <WindowsEditor
            lang={lang}
            idPrefix="wl"
            label={t(lang, "acceptableWindows")}
            help={t(lang, "acceptableWindowsHelp")}
            windows={form.acceptable_windows}
            onChange={(w) => set({ acceptable_windows: w })}
          />
          <p className="muted">{t(lang, "waitlistConsentNote")}</p>
          <div className="actions">
            <button type="submit" disabled={busy || !form.service_code}>
              {t(lang, "joinWaitlist")}
            </button>
            <button
              type="button"
              className="secondary"
              onClick={() => setJoining(false)}
            >
              {t(lang, "cancel")}
            </button>
          </div>
        </form>
      ) : null}
      <MessageLine message={message} />
      <PanelState
        lang={lang}
        state={state}
        isEmpty={(d) => d.items.length === 0}
        emptyKey="noWaitlistEntries"
      >
        {(d) => (
          <ul className="stack" data-testid="waitlist-entries">
            {d.items.map((w) => (
              <li key={w.id} className="row-card">
                <div className="row-main">
                  <strong>
                    {nameFor(lang, services.entries, w.service_code)}
                  </strong>{" "}
                  <StatusBadge
                    label={waitlistStatusLabel(lang, w.status)}
                    tone={
                      w.status === "offered"
                        ? "warn"
                        : w.status === "active"
                          ? "ok"
                          : "neutral"
                    }
                  />
                  <p className="muted">
                    {t(lang, "joinedAt")} {formatDateTime(lang, w.joined_at)} ·{" "}
                    {t(lang, "minNoticeHours")}: {w.min_notice_hours}
                  </p>
                  {w.current_offer ? (
                    <div className="nested">
                      <p className="advisory" role="status">
                        {t(lang, "waitlistOfferAvailable")}
                      </p>
                      <OfferCard
                        lang={lang}
                        offer={w.current_offer}
                        busy={busy}
                        onAction={(o, a) => void onOffer(o, a)}
                        compact
                      />
                    </div>
                  ) : null}
                </div>
                <div className="actions">
                  {w.status === "active" ? (
                    <button
                      type="button"
                      className="secondary"
                      disabled={busy}
                      onClick={() => void transition(w, "pause")}
                    >
                      {t(lang, "pauseWaitlist")}
                    </button>
                  ) : null}
                  {w.status === "paused" ? (
                    <button
                      type="button"
                      className="secondary"
                      disabled={busy}
                      onClick={() => void transition(w, "resume")}
                    >
                      {t(lang, "resumeWaitlist")}
                    </button>
                  ) : null}
                  <button
                    type="button"
                    className="secondary"
                    disabled={busy}
                    onClick={() => setLeaving(w.id)}
                  >
                    {t(lang, "leaveWaitlist")}
                  </button>
                </div>
                {leaving === w.id ? (
                  <ConfirmBox
                    lang={lang}
                    title={t(lang, "leaveWaitlist")}
                    busy={busy}
                    onConfirm={() => void transition(w, "leave")}
                    onCancel={() => setLeaving(null)}
                  >
                    <p className="muted">{t(lang, "leaveWaitlistHelp")}</p>
                  </ConfirmBox>
                ) : null}
              </li>
            ))}
          </ul>
        )}
      </PanelState>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Notifications
// ---------------------------------------------------------------------------

export function NotificationsSection({
  lang,
  patientId,
}: {
  lang: Lang;
  patientId: string;
}) {
  const state = useLoader(
    () =>
      apiFetch<{ items: AppNotification[] }>(
        `${ME}/notifications${query({ patient_id: patientId, limit: 50 })}`,
      ),
    patientId,
  );
  const { busy, message, run } = useAction(lang);

  async function markRead(n: AppNotification) {
    const ok = await run(async () => {
      await postJson(`${ME}/notifications/${n.id}/read`, {
        patient_id: patientId,
      });
    });
    if (ok) state.reload();
  }

  return (
    <div className="card" data-testid="notifications-section">
      <h3 style={{ marginTop: 0 }}>{t(lang, "notifications")}</h3>
      <MessageLine message={message} />
      <PanelState
        lang={lang}
        state={state}
        isEmpty={(d) => d.items.length === 0}
        emptyKey="noNotifications"
      >
        {(d) => (
          <ul className="stack compact">
            {d.items.map((n) => (
              <li
                key={n.id}
                className={`row-card${n.read_at ? "" : " unread"}`}
                data-testid="notification"
              >
                <div className="row-main">
                  <strong>{n.subject}</strong>{" "}
                  <StatusBadge
                    label={notificationKindLabel(lang, n.kind)}
                    tone={n.read_at ? "neutral" : "warn"}
                  />
                  <p>{n.body}</p>
                  {n.delivered_at ? (
                    <p className="muted">
                      {formatDateTime(lang, n.delivered_at)}
                    </p>
                  ) : null}
                </div>
                {!n.read_at ? (
                  <button
                    type="button"
                    className="secondary"
                    disabled={busy}
                    onClick={() => void markRead(n)}
                  >
                    {t(lang, "markRead")}
                  </button>
                ) : null}
              </li>
            ))}
          </ul>
        )}
      </PanelState>
    </div>
  );
}
