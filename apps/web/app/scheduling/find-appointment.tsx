"use client";

import { useEffect, useId, useMemo, useState } from "react";
import { apiFetch, useSession } from "@/lib/session";
import { t, type Lang } from "@/lib/i18n";
import {
  URGENCIES,
  formatRange,
  preparationText,
  rejectionLabel,
  requestStatusLabel,
  urgencyLabel,
  weekdayLabel,
  type AccessRequest,
  type Appointment,
  type ConstraintsInput,
  type MatchResult,
  type Offer,
  type PatientSummary,
  type WeeklyWindow,
} from "@/lib/access";
import {
  MessageLine,
  OfferCard,
  RankingNotice,
  ReasonField,
  nameFor,
  postJson,
  useAction,
  useCatalog,
  type OfferAction,
} from "./shared";

/**
 * Transport adapter: the same three-stage flow runs for staff
 * (`/api/v1/access-requests`, `/api/v1/offers`) and for patients
 * (`/api/v1/me/...`, where the patient is derived from the grant server-side).
 */
export type AccessApi = {
  createRequest(body: {
    patient_id: string;
    facility_id: string | null;
    free_text: string | null;
    constraints: ConstraintsInput;
    urgency?: string | null;
    submit: boolean;
    idempotency_key: string;
  }): Promise<AccessRequest>;
  amendRequest(
    id: string,
    body: {
      version: number;
      facility_id: string | null;
      free_text: string | null;
      constraints: ConstraintsInput;
      urgency?: string | null;
    },
  ): Promise<AccessRequest>;
  submitRequest(id: string, version: number): Promise<AccessRequest>;
  interpret(
    id: string,
    version: number,
    language: string,
  ): Promise<{ request: AccessRequest; intent: IntentResult }>;
  matchRequest(
    id: string,
    version: number,
    language: string,
  ): Promise<MatchResult>;
  clearTriage?(
    id: string,
    version: number,
    reason: string,
  ): Promise<AccessRequest>;
  holdOffer(id: string, version: number): Promise<Offer>;
  releaseOffer(id: string, version: number): Promise<Offer>;
  declineOffer(
    id: string,
    version: number,
    reason: string | null,
  ): Promise<Offer>;
  acceptOffer(
    id: string,
    body: {
      version: number;
      idempotency_key: string;
      override_reason?: string | null;
      reschedule_of?: string | null;
      reschedule_reason?: string | null;
    },
  ): Promise<Appointment>;
  icsPath(appointmentId: string, lang: Lang): string;
};

export type IntentResult = {
  artifact_id: string;
  output: {
    missing_information: string[];
    clinical_triage_suggested: boolean;
    triage_reasons: string[];
    limitations: string[];
  };
  provider: { provider: string; model: string | null };
  synthetic: boolean;
  reused: boolean;
  triage_floor: string | null;
  triage_suggested: boolean;
};

const STAFF_BASE = "/api/v1";
const ME_BASE = "/api/v1/me";

function v(version: number) {
  return { version };
}

export const staffAccessApi: AccessApi = {
  createRequest: (body) => postJson(`${STAFF_BASE}/access-requests`, body),
  amendRequest: (id, body) =>
    apiFetch(`${STAFF_BASE}/access-requests/${id}`, {
      method: "PATCH",
      body: JSON.stringify(body),
    }),
  submitRequest: (id, version) =>
    postJson(`${STAFF_BASE}/access-requests/${id}/submit`, v(version)),
  interpret: (id, version, language) =>
    postJson(`${STAFF_BASE}/access-requests/${id}/interpret`, {
      version,
      language,
    }),
  matchRequest: (id, version, language) =>
    postJson(`${STAFF_BASE}/access-requests/${id}/match`, {
      version,
      language,
      ranking: true,
    }),
  clearTriage: (id, version, reason) =>
    postJson(`${STAFF_BASE}/access-requests/${id}/clear-triage`, {
      version,
      reason,
    }),
  holdOffer: (id, version) =>
    postJson(`${STAFF_BASE}/offers/${id}/hold`, v(version)),
  releaseOffer: (id, version) =>
    postJson(`${STAFF_BASE}/offers/${id}/release-hold`, v(version)),
  declineOffer: (id, version, reason) =>
    postJson(`${STAFF_BASE}/offers/${id}/decline`, { version, reason }),
  acceptOffer: (id, body) =>
    postJson(`${STAFF_BASE}/offers/${id}/accept`, body),
  icsPath: (id, lang) => `${STAFF_BASE}/appointments/${id}/ics?lang=${lang}`,
};

export const patientAccessApi: AccessApi = {
  createRequest: (body) => postJson(`${ME_BASE}/access-requests`, body),
  amendRequest: (id, body) =>
    postJson(`${ME_BASE}/access-requests/${id}/amend`, body),
  submitRequest: (id, version) =>
    postJson(`${ME_BASE}/access-requests/${id}/submit`, v(version)),
  interpret: (id, version, language) =>
    postJson(`${ME_BASE}/access-requests/${id}/interpret`, {
      version,
      language,
    }),
  matchRequest: (id, version, language) =>
    postJson(`${ME_BASE}/access-requests/${id}/match`, {
      version,
      language,
      ranking: true,
    }),
  holdOffer: (id, version) =>
    postJson(`${ME_BASE}/offers/${id}/hold`, v(version)),
  releaseOffer: (id, version) =>
    postJson(`${ME_BASE}/offers/${id}/release`, v(version)),
  declineOffer: (id, version, reason) =>
    postJson(`${ME_BASE}/offers/${id}/decline`, { version, reason }),
  acceptOffer: (id, body) =>
    postJson(`${ME_BASE}/offers/${id}/accept`, {
      version: body.version,
      idempotency_key: body.idempotency_key,
      reschedule_of: body.reschedule_of ?? null,
      reschedule_reason: body.reschedule_reason ?? null,
    }),
  icsPath: (id, lang) => `${ME_BASE}/appointments/${id}/ics?lang=${lang}`,
};

export type PatientOption = PatientSummary & { label: string };

type Stage = "need" | "options" | "booked";

type Form = {
  service_code: string;
  specialty_code: string;
  modality_codes: string[];
  facility_ids: string[];
  earliest: string;
  latest: string;
  preferred_windows: WeeklyWindow[];
  max_travel_minutes: string;
  continuity_required: boolean;
  accessibility_codes: string[];
  language: string;
  transport_requested: boolean;
  has_referral: boolean;
  free_text: string;
  urgency: string;
};

const EMPTY_FORM: Form = {
  service_code: "",
  specialty_code: "",
  modality_codes: [],
  facility_ids: [],
  earliest: "",
  latest: "",
  preferred_windows: [],
  max_travel_minutes: "",
  continuity_required: false,
  accessibility_codes: [],
  language: "",
  transport_requested: false,
  has_referral: false,
  free_text: "",
  urgency: "routine",
};

function toDateInput(iso: string | null): string {
  return iso ? iso.slice(0, 10) : "";
}

function dateToIso(day: string, endOfDay: boolean): string | null {
  if (!day) return null;
  const d = new Date(`${day}T${endOfDay ? "23:59:59" : "00:00:00"}`);
  return Number.isNaN(d.getTime()) ? null : d.toISOString();
}

function fromRequest(r: AccessRequest, prev: Form): Form {
  const c = r.constraints;
  return {
    ...prev,
    service_code: c.service_code ?? "",
    specialty_code: c.specialty_code ?? "",
    modality_codes: c.modality_codes,
    facility_ids: c.facility_ids,
    earliest: toDateInput(c.earliest),
    latest: toDateInput(c.latest),
    preferred_windows: c.preferred_windows,
    max_travel_minutes:
      c.max_travel_minutes === null ? "" : String(c.max_travel_minutes),
    continuity_required: c.continuity_required,
    accessibility_codes: c.accessibility_codes,
    language: c.language ?? "",
    transport_requested: c.transport_requested,
    has_referral: c.has_referral,
    free_text: r.free_text ?? prev.free_text,
    urgency: r.urgency,
  };
}

function toConstraints(f: Form): ConstraintsInput {
  const travel = f.max_travel_minutes.trim()
    ? Number.parseInt(f.max_travel_minutes, 10)
    : null;
  return {
    service_code: f.service_code || null,
    specialty_code: f.specialty_code || null,
    modality_codes: f.modality_codes,
    facility_ids: f.facility_ids,
    earliest: dateToIso(f.earliest, false),
    latest: dateToIso(f.latest, true),
    preferred_windows: f.preferred_windows,
    max_travel_minutes:
      travel !== null && Number.isFinite(travel) ? travel : null,
    continuity_required: f.continuity_required,
    accessibility_codes: f.accessibility_codes,
    language: f.language.trim() || null,
    transport_requested: f.transport_requested,
    has_referral: f.has_referral,
  };
}

function newKey(): string {
  return typeof crypto !== "undefined" && "randomUUID" in crypto
    ? crypto.randomUUID()
    : `${Date.now()}-${Math.random().toString(16).slice(2)}`;
}

function toggle(list: string[], code: string): string[] {
  return list.includes(code) ? list.filter((c) => c !== code) : [...list, code];
}

export function FindAppointment({
  lang,
  api,
  mode,
  patients,
  initialPatientId,
  initialRequest,
  initialResult,
  rescheduleOf,
  onBooked,
  onRequestChanged,
  headingId,
}: {
  lang: Lang;
  api: AccessApi;
  mode: "staff" | "patient";
  /** Patients this user may book for (grants for patients; search hits for staff). */
  patients: PatientOption[];
  initialPatientId?: string | null;
  /** Continue an existing request (e.g. one picked from the pending list). */
  initialRequest?: AccessRequest | null;
  /** Start at stage 2 with an existing match (e.g. reschedule options). */
  initialResult?: MatchResult | null;
  rescheduleOf?: Appointment | null;
  onBooked?: (a: Appointment) => void;
  onRequestChanged?: (r: AccessRequest) => void;
  headingId?: string;
}) {
  const { meta } = useSession();
  const services = useCatalog("clinical_service");
  const specialties = useCatalog("specialty");
  const modalities = useCatalog("modality");
  const accessibility = useCatalog("accessibility_capability");
  const { busy, message, setMessage, run } = useAction(lang);
  const ids = useId();

  const [stage, setStage] = useState<Stage>(initialResult ? "options" : "need");
  const startRequest = initialResult?.request ?? initialRequest ?? null;
  const [patientId, setPatientId] = useState<string>(
    startRequest?.patient_id ??
      initialPatientId ??
      (mode === "patient" ? patients[0]?.id : null) ??
      "",
  );
  const [form, setForm] = useState<Form>(() =>
    startRequest ? fromRequest(startRequest, EMPTY_FORM) : EMPTY_FORM,
  );
  const [request, setRequest] = useState<AccessRequest | null>(startRequest);
  const [intent, setIntent] = useState<IntentResult | null>(null);
  const [result, setResult] = useState<MatchResult | null>(
    initialResult ?? null,
  );
  const [appointment, setAppointment] = useState<Appointment | null>(null);
  const [decline, setDecline] = useState<{
    offer: Offer;
    reason: string;
  } | null>(null);
  const [overrideReason, setOverrideReason] = useState("");
  const [triageReason, setTriageReason] = useState("");
  const [idemKey, setIdemKey] = useState(newKey);

  useEffect(() => {
    if (mode === "patient" && !patientId && patients.length > 0) {
      setPatientId(patients[0].id);
    }
  }, [mode, patients, patientId]);

  const facilities = meta?.facilities ?? [];
  const patient = patients.find((p) => p.id === patientId) ?? null;
  const update = <K extends keyof Form>(key: K, value: Form[K]) =>
    setForm((f) => ({ ...f, [key]: value }));

  function applyRequest(r: AccessRequest) {
    setRequest(r);
    onRequestChanged?.(r);
  }

  async function ensureRequest(submit: boolean): Promise<AccessRequest> {
    const constraints = toConstraints(form);
    const facility_id =
      form.facility_ids.length === 1 ? form.facility_ids[0] : null;
    const free_text = form.free_text.trim() || null;
    const urgency = mode === "staff" ? form.urgency : undefined;
    if (
      !request ||
      request.status === "booked" ||
      request.status === "closed" ||
      request.status === "withdrawn"
    ) {
      if (!patientId) throw new Error(t(lang, "selectPatientFirst"));
      const r = await api.createRequest({
        patient_id: patientId,
        facility_id,
        free_text,
        constraints,
        urgency,
        submit,
        idempotency_key: idemKey,
      });
      applyRequest(r);
      return r;
    }
    let r = request;
    if (
      r.status === "draft" ||
      r.status === "submitted" ||
      r.status === "options_ready" ||
      r.status === "needs_clinical_triage"
    ) {
      r = await api.amendRequest(r.id, {
        version: r.version,
        facility_id,
        free_text,
        constraints,
        urgency,
      });
      applyRequest(r);
    }
    if (submit && r.status === "draft") {
      r = await api.submitRequest(r.id, r.version);
      applyRequest(r);
    }
    return r;
  }

  async function interpret() {
    await run(async () => {
      const r = await ensureRequest(false);
      const out = await api.interpret(r.id, r.version, lang);
      applyRequest(out.request);
      setIntent(out.intent);
      setForm((f) => fromRequest(out.request, f));
    });
  }

  async function find() {
    if (!form.service_code && !form.specialty_code && !form.free_text.trim()) {
      setMessage({ kind: "error", text: t(lang, "needServiceOrText") });
      return;
    }
    await run(async () => {
      const r = await ensureRequest(true);
      if (r.status === "needs_clinical_triage") {
        setStage("options");
        setResult(null);
        return;
      }
      const m = await api.matchRequest(r.id, r.version, lang);
      applyRequest(m.request);
      setResult(m);
      setStage("options");
    });
  }

  async function rematch() {
    if (!request) return;
    await run(async () => {
      const m = await api.matchRequest(request.id, request.version, lang);
      applyRequest(m.request);
      setResult(m);
    });
  }

  async function clearTriage() {
    const clear = api.clearTriage;
    if (!request || !clear || triageReason.trim().length < 3) return;
    await run(async () => {
      const r = await clear(request.id, request.version, triageReason.trim());
      applyRequest(r);
      setTriageReason("");
      const m = await api.matchRequest(r.id, r.version, lang);
      applyRequest(m.request);
      setResult(m);
    });
  }

  function replaceOffer(o: Offer) {
    setResult((m) =>
      m ? { ...m, offers: m.offers.map((x) => (x.id === o.id ? o : x)) } : m,
    );
  }

  async function onOffer(offer: Offer, action: OfferAction) {
    if (action === "decline") {
      setDecline({ offer, reason: "" });
      return;
    }
    await run(
      async () => {
        if (action === "hold")
          replaceOffer(await api.holdOffer(offer.id, offer.version));
        else if (action === "release")
          replaceOffer(await api.releaseOffer(offer.id, offer.version));
        else if (action === "accept") {
          const a = await api.acceptOffer(offer.id, {
            version: offer.version,
            idempotency_key: `${idemKey}:${offer.id}`,
            override_reason: overrideReason.trim() || null,
            reschedule_of: rescheduleOf?.id ?? null,
            reschedule_reason: rescheduleOf
              ? form.free_text.trim() || null
              : null,
          });
          setAppointment(a);
          setStage("booked");
          onBooked?.(a);
        }
      },
      action === "hold"
        ? "holdPlaced"
        : action === "release"
          ? "holdReleased"
          : undefined,
    );
  }

  async function confirmDecline() {
    if (!decline) return;
    const { offer, reason } = decline;
    const ok = await run(async () => {
      replaceOffer(
        await api.declineOffer(offer.id, offer.version, reason.trim() || null),
      );
    }, "offerDeclined");
    if (ok) setDecline(null);
  }

  function reset() {
    setStage("need");
    setRequest(null);
    setIntent(null);
    setResult(null);
    setAppointment(null);
    setDecline(null);
    setOverrideReason("");
    setForm(EMPTY_FORM);
    setIdemKey(newKey());
    setMessage(null);
  }

  const stages: { key: Stage; label: string }[] = [
    { key: "need", label: t(lang, "stageNeed") },
    { key: "options", label: t(lang, "stageOptions") },
    { key: "booked", label: t(lang, "stageConfirm") },
  ];
  const stageIndex = stages.findIndex((s) => s.key === stage);

  const rejected = useMemo(
    () =>
      Object.entries(result?.rejected_summary ?? {}).sort(
        (a, b) => b[1] - a[1],
      ),
    [result],
  );

  return (
    <section
      className="card find-appointment"
      aria-labelledby={headingId ?? `${ids}-h`}
    >
      <h2 id={headingId ?? `${ids}-h`}>{t(lang, "findBestAppointment")}</h2>
      <ol className="stepper" aria-label={t(lang, "findBestAppointment")}>
        {stages.map((s, i) => (
          <li
            key={s.key}
            className={
              i === stageIndex ? "current" : i < stageIndex ? "done" : ""
            }
            aria-current={i === stageIndex ? "step" : undefined}
          >
            {i + 1}. {s.label}
          </li>
        ))}
      </ol>
      {rescheduleOf ? (
        <p className="advisory">
          {t(lang, "reschedulingOf")}{" "}
          {formatRange(
            lang,
            rescheduleOf.starts_at,
            rescheduleOf.ends_at,
            rescheduleOf.time_zone,
          )}
        </p>
      ) : null}
      <MessageLine message={message} />
      {services.error ? (
        <p className="error" role="alert">
          {services.error}
        </p>
      ) : null}

      {stage === "need" ? (
        <form
          className="stack"
          onSubmit={(e) => {
            e.preventDefault();
            void find();
          }}
        >
          {request ? (
            <p className="muted" data-testid="request-patient">
              {t(lang, "patient")}:{" "}
              {patient?.label ??
                (request.patient
                  ? `${request.patient.family_name}, ${request.patient.given_name}`
                  : request.patient_id)}{" "}
              · {requestStatusLabel(lang, request.status)}
            </p>
          ) : patients.length > 1 || mode === "staff" ? (
            <div>
              <label htmlFor={`${ids}-patient`}>{t(lang, "patient")}</label>
              <select
                id={`${ids}-patient`}
                value={patientId}
                onChange={(e) => setPatientId(e.target.value)}
                required
              >
                {mode === "staff" ? (
                  <option value="">{t(lang, "selectPatient")}</option>
                ) : null}
                {patients.map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.label}
                  </option>
                ))}
              </select>
            </div>
          ) : patient ? (
            <p className="muted">
              {t(lang, "patient")}: {patient.label}
            </p>
          ) : null}

          <div>
            <label htmlFor={`${ids}-text`}>{t(lang, "describeNeed")}</label>
            <textarea
              id={`${ids}-text`}
              rows={3}
              maxLength={2000}
              value={form.free_text}
              onChange={(e) => update("free_text", e.target.value)}
              placeholder={t(lang, "describeNeedHint")}
            />
            <div className="visit-actions">
              <button
                type="button"
                className="secondary"
                disabled={busy || !form.free_text.trim() || !patientId}
                onClick={() => void interpret()}
              >
                {t(lang, "interpretWithDmind")}
              </button>
            </div>
            {intent ? (
              <div
                className={
                  intent.synthetic ? "advisory synthetic-notice" : "advisory"
                }
                role="status"
              >
                <p>
                  {intent.synthetic
                    ? t(lang, "intentSyntheticNotice")
                    : t(lang, "intentNotice")}
                  {intent.reused ? ` ${t(lang, "rankingReused")}` : ""}
                </p>
                {intent.output.missing_information.length > 0 ? (
                  <>
                    <strong>{t(lang, "missingInformation")}</strong>
                    <ul>
                      {intent.output.missing_information.map((q, i) => (
                        <li key={i}>{q}</li>
                      ))}
                    </ul>
                  </>
                ) : null}
                {intent.triage_floor || intent.triage_suggested ? (
                  <p className="warn-text">{t(lang, "triageSuggested")}</p>
                ) : null}
              </div>
            ) : null}
          </div>

          <div className="filters">
            <div>
              <label htmlFor={`${ids}-service`}>{t(lang, "service")}</label>
              <select
                id={`${ids}-service`}
                value={form.service_code}
                onChange={(e) => update("service_code", e.target.value)}
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
              <label htmlFor={`${ids}-specialty`}>{t(lang, "specialty")}</label>
              <select
                id={`${ids}-specialty`}
                value={form.specialty_code}
                onChange={(e) => update("specialty_code", e.target.value)}
              >
                <option value="">—</option>
                {specialties.entries.map((s) => (
                  <option key={s.code} value={s.code}>
                    {nameFor(lang, specialties.entries, s.code)}
                  </option>
                ))}
              </select>
            </div>
            <div>
              <label htmlFor={`${ids}-earliest`}>
                {t(lang, "earliestDate")}
              </label>
              <input
                id={`${ids}-earliest`}
                type="date"
                value={form.earliest}
                onChange={(e) => update("earliest", e.target.value)}
              />
            </div>
            <div>
              <label htmlFor={`${ids}-latest`}>{t(lang, "latestDate")}</label>
              <input
                id={`${ids}-latest`}
                type="date"
                value={form.latest}
                onChange={(e) => update("latest", e.target.value)}
              />
            </div>
            {mode === "staff" ? (
              <div>
                <label htmlFor={`${ids}-urgency`}>{t(lang, "urgency")}</label>
                <select
                  id={`${ids}-urgency`}
                  value={form.urgency}
                  onChange={(e) => update("urgency", e.target.value)}
                >
                  {URGENCIES.map((u) => (
                    <option key={u} value={u}>
                      {urgencyLabel(lang, u)}
                    </option>
                  ))}
                </select>
              </div>
            ) : null}
          </div>

          <fieldset>
            <legend>{t(lang, "modality")}</legend>
            <div className="chip-row">
              {modalities.entries.map((m) => (
                <label key={m.code} className="chip">
                  <input
                    type="checkbox"
                    checked={form.modality_codes.includes(m.code)}
                    onChange={() =>
                      update(
                        "modality_codes",
                        toggle(form.modality_codes, m.code),
                      )
                    }
                  />
                  {nameFor(lang, modalities.entries, m.code)}
                </label>
              ))}
            </div>
          </fieldset>

          {facilities.length > 0 ? (
            <fieldset>
              <legend>{t(lang, "facilities")}</legend>
              <div className="chip-row">
                {facilities.map((f) => (
                  <label key={f.id} className="chip">
                    <input
                      type="checkbox"
                      checked={form.facility_ids.includes(f.id)}
                      onChange={() =>
                        update("facility_ids", toggle(form.facility_ids, f.id))
                      }
                    />
                    {f.name}
                  </label>
                ))}
              </div>
            </fieldset>
          ) : null}

          <details className="secondary">
            <summary>{t(lang, "morePreferences")}</summary>
            <WindowsEditor
              lang={lang}
              idPrefix={`${ids}-win`}
              windows={form.preferred_windows}
              onChange={(w) => update("preferred_windows", w)}
            />
            <div className="filters">
              <div>
                <label htmlFor={`${ids}-travel`}>
                  {t(lang, "maxTravelMinutes")}
                </label>
                <input
                  id={`${ids}-travel`}
                  type="number"
                  min={5}
                  max={600}
                  value={form.max_travel_minutes}
                  onChange={(e) => update("max_travel_minutes", e.target.value)}
                />
              </div>
              <div>
                <label htmlFor={`${ids}-lang`}>
                  {t(lang, "preferredLanguage")}
                </label>
                <input
                  id={`${ids}-lang`}
                  value={form.language}
                  maxLength={12}
                  placeholder="es, en, ca…"
                  onChange={(e) => update("language", e.target.value)}
                />
              </div>
            </div>
            {accessibility.entries.length > 0 ? (
              <fieldset>
                <legend>{t(lang, "accessibilityNeeds")}</legend>
                <div className="chip-row">
                  {accessibility.entries.map((a) => (
                    <label key={a.code} className="chip">
                      <input
                        type="checkbox"
                        checked={form.accessibility_codes.includes(a.code)}
                        onChange={() =>
                          update(
                            "accessibility_codes",
                            toggle(form.accessibility_codes, a.code),
                          )
                        }
                      />
                      {nameFor(lang, accessibility.entries, a.code)}
                    </label>
                  ))}
                </div>
              </fieldset>
            ) : null}
            <div className="chip-row">
              <label className="chip">
                <input
                  type="checkbox"
                  checked={form.continuity_required}
                  onChange={(e) =>
                    update("continuity_required", e.target.checked)
                  }
                />
                {t(lang, "continuityRequired")}
              </label>
              <label className="chip">
                <input
                  type="checkbox"
                  checked={form.transport_requested}
                  onChange={(e) =>
                    update("transport_requested", e.target.checked)
                  }
                />
                {t(lang, "transportRequested")}
              </label>
              <label className="chip">
                <input
                  type="checkbox"
                  checked={form.has_referral}
                  onChange={(e) => update("has_referral", e.target.checked)}
                />
                {t(lang, "hasReferral")}
              </label>
            </div>
          </details>

          <div className="visit-actions">
            <button
              type="submit"
              className="primary cta"
              disabled={busy || !patientId}
              data-testid="find-best-appointment"
            >
              {busy ? t(lang, "loading") : t(lang, "findBestAppointment")}
            </button>
          </div>
        </form>
      ) : null}

      {stage === "options" ? (
        <div className="stack">
          {request ? (
            <p className="muted">
              {t(lang, "requestStatus")}:{" "}
              {requestStatusLabel(lang, request.status)}
              {request.urgency !== "routine"
                ? ` · ${t(lang, "urgency")}: ${urgencyLabel(lang, request.urgency)}`
                : ""}
            </p>
          ) : null}
          {request?.status === "needs_clinical_triage" ? (
            <div className="advisory" role="status">
              <p>
                {t(
                  lang,
                  mode === "staff" ? "needsTriageStaff" : "needsTriagePatient",
                )}
              </p>
              {request.triage_reason ? (
                <p className="muted">{request.triage_reason}</p>
              ) : null}
              {api.clearTriage ? (
                <div className="stack">
                  <ReasonField
                    id={`${ids}-triage-reason`}
                    lang={lang}
                    value={triageReason}
                    onChange={setTriageReason}
                    required
                    label={t(lang, "clearTriageReason")}
                  />
                  <div className="visit-actions">
                    <button
                      type="button"
                      className="secondary"
                      disabled={busy || triageReason.trim().length < 3}
                      onClick={() => void clearTriage()}
                    >
                      {t(lang, "clearTriage")}
                    </button>
                  </div>
                </div>
              ) : null}
            </div>
          ) : null}
          {result ? (
            <>
              <RankingNotice lang={lang} ranking={result.ranking} />
              <p className="muted">
                {t(lang, "matcherVersion")}: {result.matcher_version}
              </p>
              {result.offers.length === 0 ? (
                <p className="empty" role="status">
                  {t(lang, "noOptions")}
                </p>
              ) : (
                <ul
                  className="result-list offers"
                  aria-label={t(lang, "stageOptions")}
                >
                  {result.offers.map((o) => (
                    <OfferCard
                      key={o.id}
                      lang={lang}
                      offer={o}
                      busy={busy}
                      onAction={onOffer}
                    />
                  ))}
                </ul>
              )}
              {rejected.length > 0 ? (
                <details className="secondary">
                  <summary>
                    {t(lang, "whyNotOthers")} (
                    {rejected.reduce((n, [, c]) => n + c, 0)})
                  </summary>
                  <ul>
                    {rejected.map(([code, n]) => (
                      <li key={code}>
                        {rejectionLabel(lang, code)}: {n}
                      </li>
                    ))}
                  </ul>
                </details>
              ) : null}
              {mode === "staff" ? (
                <ReasonField
                  id={`${ids}-override`}
                  lang={lang}
                  value={overrideReason}
                  onChange={setOverrideReason}
                  label={t(lang, "overrideReasonOptional")}
                />
              ) : null}
            </>
          ) : null}
          {decline ? (
            <div
              className="confirm-box"
              role="group"
              aria-label={t(lang, "declineOption")}
            >
              <strong>{t(lang, "declineOption")}</strong>
              <ReasonField
                id={`${ids}-decline`}
                lang={lang}
                value={decline.reason}
                onChange={(r) => setDecline({ ...decline, reason: r })}
              />
              <div className="visit-actions">
                <button
                  type="button"
                  className="primary"
                  disabled={busy}
                  onClick={() => void confirmDecline()}
                >
                  {t(lang, "confirm")}
                </button>
                <button
                  type="button"
                  className="tertiary"
                  disabled={busy}
                  onClick={() => setDecline(null)}
                >
                  {t(lang, "cancel")}
                </button>
              </div>
            </div>
          ) : null}
          <div className="visit-actions">
            <button
              type="button"
              className="secondary"
              disabled={busy}
              onClick={() => {
                setStage("need");
                setMessage(null);
              }}
            >
              {t(lang, "changeConstraints")}
            </button>
            {request && request.status !== "needs_clinical_triage" ? (
              <button
                type="button"
                className="tertiary"
                disabled={busy}
                onClick={() => void rematch()}
              >
                {t(lang, "rerunMatching")}
              </button>
            ) : null}
            <button
              type="button"
              className="tertiary"
              disabled={busy}
              onClick={reset}
            >
              {t(lang, "newSearch")}
            </button>
          </div>
        </div>
      ) : null}

      {stage === "booked" && appointment ? (
        <div className="stack" data-testid="booking-confirmed">
          <p className="success" role="status">
            {t(lang, "appointmentBooked")}
          </p>
          <p>
            <strong>
              {formatRange(
                lang,
                appointment.starts_at,
                appointment.ends_at,
                appointment.time_zone,
              )}
            </strong>
            <br />
            {appointment.service
              ? lang === "es"
                ? appointment.service.name_es
                : appointment.service.name_en
              : appointment.service_code}
            {appointment.facility_name ? ` · ${appointment.facility_name}` : ""}
            {appointment.primary_resource
              ? ` · ${appointment.primary_resource.name}`
              : ""}
          </p>
          {appointment.confirmation_required &&
          !appointment.patient_confirmed_at ? (
            <p className="warn-text">{t(lang, "confirmationRequiredNotice")}</p>
          ) : null}
          {preparationText(lang, appointment.service) ? (
            <div className="advisory">
              <strong>{t(lang, "preparation")}</strong>
              <p>{preparationText(lang, appointment.service)}</p>
            </div>
          ) : null}
          <div className="visit-actions">
            <a
              className="button secondary"
              href={api.icsPath(appointment.id, lang)}
              download
            >
              {t(lang, "downloadIcs")}
            </a>
            <button type="button" className="tertiary" onClick={reset}>
              {t(lang, "newSearch")}
            </button>
          </div>
        </div>
      ) : null}
    </section>
  );
}

/** Weekly preferred windows (weekday + local time range). */
export function WindowsEditor({
  lang,
  idPrefix,
  windows,
  onChange,
  legend,
}: {
  lang: Lang;
  idPrefix: string;
  windows: WeeklyWindow[];
  onChange: (w: WeeklyWindow[]) => void;
  legend?: string;
}) {
  const [draft, setDraft] = useState<WeeklyWindow>({
    weekday: 1,
    start: "09:00",
    end: "13:00",
  });
  return (
    <fieldset>
      <legend>{legend ?? t(lang, "preferredWindows")}</legend>
      {windows.length > 0 ? (
        <ul className="plain">
          {windows.map((w, i) => (
            <li key={i}>
              {weekdayLabel(lang, w.weekday)} {w.start}–{w.end}{" "}
              <button
                type="button"
                className="tertiary"
                onClick={() => onChange(windows.filter((_, j) => j !== i))}
                aria-label={`${t(lang, "remove")} ${weekdayLabel(lang, w.weekday)} ${w.start}`}
              >
                {t(lang, "remove")}
              </button>
            </li>
          ))}
        </ul>
      ) : (
        <p className="muted">{t(lang, "noWindows")}</p>
      )}
      <div className="filters">
        <div>
          <label htmlFor={`${idPrefix}-day`}>{t(lang, "weekday")}</label>
          <select
            id={`${idPrefix}-day`}
            value={draft.weekday}
            onChange={(e) =>
              setDraft({ ...draft, weekday: Number(e.target.value) })
            }
          >
            {[1, 2, 3, 4, 5, 6, 7].map((d) => (
              <option key={d} value={d}>
                {weekdayLabel(lang, d)}
              </option>
            ))}
          </select>
        </div>
        <div>
          <label htmlFor={`${idPrefix}-start`}>{t(lang, "from")}</label>
          <input
            id={`${idPrefix}-start`}
            type="time"
            value={draft.start}
            onChange={(e) => setDraft({ ...draft, start: e.target.value })}
          />
        </div>
        <div>
          <label htmlFor={`${idPrefix}-end`}>{t(lang, "to")}</label>
          <input
            id={`${idPrefix}-end`}
            type="time"
            value={draft.end}
            onChange={(e) => setDraft({ ...draft, end: e.target.value })}
          />
        </div>
        <div className="filters-actions">
          <button
            type="button"
            className="secondary"
            disabled={!draft.start || !draft.end || draft.start >= draft.end}
            onClick={() => onChange([...windows, draft])}
          >
            {t(lang, "addWindow")}
          </button>
        </div>
      </div>
    </fieldset>
  );
}
