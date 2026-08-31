"use client";

import Link from "next/link";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { AppShell } from "../../chrome";
import { t } from "@/lib/i18n";
import type { Lang, TKey } from "@/lib/i18n";
import { ApiRequestError, apiFetch, useSession } from "@/lib/session";
import {
  LAB_TESTS,
  ageYears,
  formatDateTime,
  loopStateShortLabel,
  patientName,
} from "@/lib/clinical";

type VitalSet = {
  id: string;
  encounter_id: string;
  systolic_mmhg: string | null;
  diastolic_mmhg: string | null;
  heart_rate_bpm: string | null;
  respiratory_rate_bpm: string | null;
  temperature_c: string | null;
  spo2_percent: string | null;
  weight_kg: string | null;
  height_cm: string | null;
  bmi: string | null;
  recorded_at: string;
};

type Note = {
  id: string;
  status: string;
  version: number;
  reason_for_encounter: string | null;
  history_present_illness: string | null;
  medical_history: string | null;
  review_of_systems: string | null;
  physical_exam: string | null;
  assessment: string | null;
  plan: string | null;
  follow_up: string | null;
  author: string;
  updated_at: string;
  signed_at: string | null;
  signed_by: string | null;
};

type AiDraft = {
  id: string;
  status: string;
  output: {
    summary: string;
    limitations: string[];
    cited_sources: string[];
  } | null;
  limitations: string[];
  citations: string[];
  model: string | null;
  model_version: string | null;
  generated_at: string | null;
  review_decision: string | null;
};

type Workspace = {
  encounter: {
    id: string;
    status: string;
    encounter_type: string;
    started_at: string;
    completed_at: string | null;
    practitioner: string;
    facility_name: string;
    own: boolean;
  };
  patient: {
    id: string;
    family_name: string;
    given_name: string;
    birth_date: string;
    sex: string;
    identifier: string;
  };
  allergies: { substance: string; criticality: string }[];
  medications: { name: string; status: string }[];
  alerts: { severity: string; message: string; created_at: string }[];
  note: Note | null;
  addenda: { body: string; author: string; created_at: string }[];
  vitals: VitalSet[];
  previous_vitals: VitalSet[];
  diagnoses: {
    id: string;
    code: string;
    display: string;
    status: string;
    recorded_at: string;
    this_encounter: boolean;
  }[];
  service_requests: {
    id: string;
    display: string;
    loop_state: string;
    created_at: string;
  }[];
  ai_draft: AiDraft | null;
  capabilities: {
    can_document: boolean;
    can_sign: boolean;
    can_add_addendum: boolean;
    can_order_lab: boolean;
  };
};

type NoteSections = {
  reason_for_encounter: string;
  history_present_illness: string;
  medical_history: string;
  review_of_systems: string;
  physical_exam: string;
  assessment: string;
  plan: string;
  follow_up: string;
};

const EMPTY_SECTIONS: NoteSections = {
  reason_for_encounter: "",
  history_present_illness: "",
  medical_history: "",
  review_of_systems: "",
  physical_exam: "",
  assessment: "",
  plan: "",
  follow_up: "",
};

function sectionsFromNote(note: Note | null): NoteSections {
  return {
    reason_for_encounter: note?.reason_for_encounter ?? "",
    history_present_illness: note?.history_present_illness ?? "",
    medical_history: note?.medical_history ?? "",
    review_of_systems: note?.review_of_systems ?? "",
    physical_exam: note?.physical_exam ?? "",
    assessment: note?.assessment ?? "",
    plan: note?.plan ?? "",
    follow_up: note?.follow_up ?? "",
  };
}

function statusKey(status: string): TKey {
  switch (status) {
    case "completed":
      return "encStatusCompleted";
    case "cancelled":
      return "encStatusCancelled";
    default:
      return "encStatusInProgress";
  }
}

function statusBadgeClass(status: string): string {
  switch (status) {
    case "completed":
      return "ok";
    case "cancelled":
      return "warn";
    default:
      return "neutral";
  }
}

function errMessage(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

function SafetyHeader({ ws, lang }: { ws: Workspace; lang: Lang }) {
  const p = ws.patient;
  const age = ageYears(p.birth_date);
  return (
    <div className="card">
      <div className="patient-header">
        <h2>{patientName(p)}</h2>
        <span className="badge neutral">{p.identifier}</span>
        <span className="muted">
          {t(lang, "age")}: {Number.isNaN(age) ? "—" : age}
          {t(lang, "yearsShort")}
        </span>
        <span className={`badge ${statusBadgeClass(ws.encounter.status)}`}>
          {t(lang, statusKey(ws.encounter.status))}
        </span>
      </div>
      <p className="muted" style={{ margin: "0.4rem 0 0" }}>
        {t(lang, "consultation")} · {ws.encounter.practitioner} ·{" "}
        {ws.encounter.facility_name} ·{" "}
        {formatDateTime(lang, ws.encounter.started_at)}
      </p>
      <div style={{ marginTop: "0.5rem" }}>
        <strong style={{ fontSize: "0.9rem" }}>{t(lang, "allergies")}:</strong>{" "}
        {ws.allergies.length === 0 ? (
          <span className="muted">{t(lang, "noKnownAllergies")}</span>
        ) : (
          ws.allergies.map((a) => (
            <span
              key={a.substance}
              className={`badge ${a.criticality === "high" ? "critical" : "warn"}`}
              style={{ marginRight: "0.4rem" }}
            >
              {a.substance}
            </span>
          ))
        )}
      </div>
      {ws.alerts.length > 0 ? (
        <ul className="result-list" style={{ marginTop: "0.5rem" }}>
          {ws.alerts.map((a, i) => (
            <li key={i} className="result-card critical">
              <div className="grow title">{a.message}</div>
              <span className="badge critical">{t(lang, "critical")}</span>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

function VitalValue({
  label,
  value,
  unit,
}: {
  label: string;
  value: string | null;
  unit: string;
}) {
  if (value === null) return null;
  return (
    <div className="vital-value">
      <span className="muted">{label}</span>
      <strong>
        {value} {unit}
      </strong>
    </div>
  );
}

function VitalsTable({ v, lang }: { v: VitalSet; lang: Lang }) {
  const bp =
    v.systolic_mmhg !== null && v.diastolic_mmhg !== null
      ? `${v.systolic_mmhg}/${v.diastolic_mmhg}`
      : null;
  return (
    <div>
      <p className="muted" style={{ margin: "0 0 0.3rem" }}>
        {t(lang, "recordedAt")}: {formatDateTime(lang, v.recorded_at)}
      </p>
      <div className="vitals-grid">
        <VitalValue label={t(lang, "bloodPressure")} value={bp} unit="mmHg" />
        <VitalValue
          label={t(lang, "heartRate")}
          value={v.heart_rate_bpm}
          unit="bpm"
        />
        <VitalValue
          label={t(lang, "respiratoryRate")}
          value={v.respiratory_rate_bpm}
          unit="/min"
        />
        <VitalValue
          label={t(lang, "temperature")}
          value={v.temperature_c}
          unit="°C"
        />
        <VitalValue
          label={t(lang, "oxygenSaturation")}
          value={v.spo2_percent}
          unit="%"
        />
        <VitalValue label={t(lang, "weight")} value={v.weight_kg} unit="kg" />
        <VitalValue label={t(lang, "height")} value={v.height_cm} unit="cm" />
        <VitalValue
          label={t(lang, "bmiCalculated")}
          value={v.bmi}
          unit="kg/m²"
        />
      </div>
    </div>
  );
}

const VITAL_FIELDS: {
  field: keyof Omit<VitalSet, "id" | "encounter_id" | "bmi" | "recorded_at">;
  labelKey: TKey;
  unit: string;
}[] = [
  { field: "systolic_mmhg", labelKey: "systolic", unit: "mmHg" },
  { field: "diastolic_mmhg", labelKey: "diastolic", unit: "mmHg" },
  { field: "heart_rate_bpm", labelKey: "heartRate", unit: "bpm" },
  { field: "respiratory_rate_bpm", labelKey: "respiratoryRate", unit: "/min" },
  { field: "temperature_c", labelKey: "temperature", unit: "°C" },
  { field: "spo2_percent", labelKey: "oxygenSaturation", unit: "%" },
  { field: "weight_kg", labelKey: "weight", unit: "kg" },
  { field: "height_cm", labelKey: "height", unit: "cm" },
];

function VitalsForm({
  encounterId,
  lang,
  onSaved,
}: {
  encounterId: string;
  lang: Lang;
  onSaved: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [values, setValues] = useState<Record<string, string>>({});
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [needsConfirm, setNeedsConfirm] = useState(false);
  const [success, setSuccess] = useState(false);

  async function submit(confirm: boolean) {
    setBusy(true);
    setError(null);
    setSuccess(false);
    try {
      const body: Record<string, unknown> = { confirm_unusual: confirm };
      for (const f of VITAL_FIELDS) {
        const raw = values[f.field]?.trim();
        if (raw) body[f.field] = raw;
      }
      await apiFetch(`/api/v1/encounters/${encounterId}/vitals`, {
        method: "POST",
        body: JSON.stringify(body),
      });
      setNeedsConfirm(false);
      setValues({});
      setOpen(false);
      setSuccess(true);
      onSaved();
    } catch (err) {
      if (err instanceof ApiRequestError && err.code === "unusual_values") {
        setNeedsConfirm(true);
        setError(t(lang, "unusualValues"));
      } else if (
        err instanceof ApiRequestError &&
        err.code === "value_out_of_range"
      ) {
        setNeedsConfirm(false);
        setError(`${t(lang, "valueOutOfRange")} ${err.message}`);
      } else {
        setNeedsConfirm(false);
        setError(errMessage(err));
      }
    } finally {
      setBusy(false);
    }
  }

  return (
    <div style={{ marginTop: "0.6rem" }}>
      {success ? (
        <p role="status" className="success">
          {t(lang, "vitalsRecorded")}
        </p>
      ) : null}
      <button
        className="secondary"
        aria-expanded={open}
        onClick={() => {
          setOpen((o) => !o);
          setSuccess(false);
        }}
      >
        {t(lang, "recordVitals")}
      </button>
      {open ? (
        <form
          onSubmit={(e) => {
            e.preventDefault();
            void submit(needsConfirm);
          }}
          style={{ marginTop: "0.6rem" }}
        >
          <div className="vitals-form-grid">
            {VITAL_FIELDS.map((f) => (
              <div key={f.field}>
                <label htmlFor={`vital-${f.field}`}>
                  {t(lang, f.labelKey)} ({f.unit})
                </label>
                <input
                  id={`vital-${f.field}`}
                  inputMode="decimal"
                  value={values[f.field] ?? ""}
                  onChange={(e) => {
                    setValues((v) => ({ ...v, [f.field]: e.target.value }));
                    setNeedsConfirm(false);
                  }}
                />
              </div>
            ))}
          </div>
          {error ? (
            <p role="alert" className={needsConfirm ? "warn-text" : "error"}>
              {error}
            </p>
          ) : null}
          <p>
            <button className="primary" type="submit" disabled={busy}>
              {needsConfirm
                ? t(lang, "confirmUnusualSave")
                : t(lang, "recordVitals")}
            </button>
          </p>
        </form>
      ) : null}
    </div>
  );
}

function DiagnosisForm({
  encounterId,
  lang,
  onSaved,
}: {
  encounterId: string;
  lang: Lang;
  onSaved: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [display, setDisplay] = useState("");
  const [code, setCode] = useState("");
  const [status, setStatus] = useState("active");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!display.trim()) {
      setError(t(lang, "requiredField"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await apiFetch(`/api/v1/encounters/${encounterId}/diagnoses`, {
        method: "POST",
        body: JSON.stringify({
          display: display.trim(),
          code: code.trim() || null,
          status,
        }),
      });
      setDisplay("");
      setCode("");
      setStatus("active");
      setOpen(false);
      setSuccess(true);
      onSaved();
    } catch (err) {
      setError(errMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div style={{ marginTop: "0.6rem" }}>
      {success ? (
        <p role="status" className="success">
          {t(lang, "diagnosisAdded")}
        </p>
      ) : null}
      <button
        className="secondary"
        aria-expanded={open}
        onClick={() => {
          setOpen((o) => !o);
          setSuccess(false);
        }}
      >
        {t(lang, "addDiagnosis")}
      </button>
      {open ? (
        <form onSubmit={submit} style={{ marginTop: "0.6rem" }}>
          <label htmlFor="dx-display">{t(lang, "diagnosisName")}</label>
          <input
            id="dx-display"
            value={display}
            placeholder={t(lang, "diagnosisNamePlaceholder")}
            onChange={(e) => setDisplay(e.target.value)}
            required
          />
          <label htmlFor="dx-code">{t(lang, "diagnosisCode")}</label>
          <input
            id="dx-code"
            value={code}
            onChange={(e) => setCode(e.target.value)}
          />
          <label htmlFor="dx-status">{t(lang, "diagnosisStatus")}</label>
          <select
            id="dx-status"
            value={status}
            onChange={(e) => setStatus(e.target.value)}
          >
            <option value="active">{t(lang, "dxActive")}</option>
            <option value="provisional">{t(lang, "dxProvisional")}</option>
            <option value="resolved">{t(lang, "dxResolved")}</option>
          </select>
          {error ? (
            <p role="alert" className="error">
              {error}
            </p>
          ) : null}
          <p>
            <button className="primary" type="submit" disabled={busy}>
              {t(lang, "addDiagnosis")}
            </button>
          </p>
        </form>
      ) : null}
    </div>
  );
}

function LabOrderForm({
  encounterId,
  lang,
  onSaved,
}: {
  encounterId: string;
  lang: Lang;
  onSaved: () => void;
}) {
  const [open, setOpen] = useState(false);
  const [test, setTest] = useState(LAB_TESTS[0].code_loinc);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    try {
      const selected = LAB_TESTS.find((x) => x.code_loinc === test);
      if (!selected) return;
      await apiFetch("/api/v1/service-requests", {
        method: "POST",
        body: JSON.stringify({
          encounter_id: encounterId,
          code_loinc: selected.code_loinc,
          display: selected.display,
        }),
      });
      setOpen(false);
      setSuccess(true);
      onSaved();
    } catch (err) {
      setError(errMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div style={{ marginTop: "0.6rem" }}>
      {success ? (
        <p role="status" className="success">
          {t(lang, "orderPlaced")}
        </p>
      ) : null}
      <button
        className="secondary"
        aria-expanded={open}
        onClick={() => {
          setOpen((o) => !o);
          setSuccess(false);
        }}
      >
        {t(lang, "orderLab")}
      </button>
      {open ? (
        <form onSubmit={submit} style={{ marginTop: "0.6rem" }}>
          <label htmlFor="enc-lab-test">{t(lang, "labTest")}</label>
          <select
            id="enc-lab-test"
            value={test}
            onChange={(e) => setTest(e.target.value)}
          >
            {LAB_TESTS.map((x) => (
              <option key={x.code_loinc} value={x.code_loinc}>
                {x.display}
              </option>
            ))}
          </select>
          {error ? (
            <p role="alert" className="error">
              {error}
            </p>
          ) : null}
          <p>
            <button className="primary" type="submit" disabled={busy}>
              {t(lang, "orderLab")}
            </button>
          </p>
        </form>
      ) : null}
    </div>
  );
}

function AiDocAid({
  encounterId,
  lang,
  draft,
  canDocument,
  onAccepted,
  onChanged,
}: {
  encounterId: string;
  lang: Lang;
  draft: AiDraft | null;
  canDocument: boolean;
  onAccepted: (text: string) => void;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [message, setMessage] = useState<string | null>(null);

  const awaiting = draft?.status === "awaiting_review";

  async function generate() {
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      await apiFetch(`/api/v1/encounters/${encounterId}/ai-draft`, {
        method: "POST",
        body: JSON.stringify({ language: lang }),
      });
      onChanged();
    } catch (err) {
      setError(errMessage(err));
    } finally {
      setBusy(false);
    }
  }

  async function review(decision: "approved" | "rejected") {
    if (!draft) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      await apiFetch(`/api/v1/ai-artifacts/${draft.id}/review`, {
        method: "POST",
        body: JSON.stringify({ decision }),
      });
      if (decision === "approved" && draft.output) {
        onAccepted(draft.output.summary);
        setMessage(t(lang, "aiDraftAccepted"));
      } else {
        setMessage(t(lang, "aiDraftRejected"));
      }
      onChanged();
    } catch (err) {
      setError(errMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="card ai-section">
      <h2>{t(lang, "aiDocAid")}</h2>
      <p className="muted">{t(lang, "aiDocAidHelp")}</p>
      {error ? (
        <p role="alert" className="error">
          {error}
        </p>
      ) : null}
      {message ? (
        <p role="status" className="success">
          {message}
        </p>
      ) : null}
      {canDocument ? (
        <button
          className="secondary"
          disabled={busy}
          onClick={() => void generate()}
        >
          {t(lang, "aiGenerateDraft")}
        </button>
      ) : null}
      {draft && awaiting && draft.output ? (
        <div style={{ marginTop: "0.6rem" }}>
          <p>
            <span className="badge warn">{t(lang, "aiAssistiveDraft")}</span>
          </p>
          <blockquote className="ai-draft-text">
            {draft.output.summary}
          </blockquote>
          <p className="muted">
            {t(lang, "aiModel")}: {draft.model} {draft.model_version}
            {draft.generated_at
              ? ` · ${t(lang, "generatedAt")}: ${formatDateTime(lang, draft.generated_at)}`
              : null}
          </p>
          <details>
            <summary>{t(lang, "aiFactsUsed")}</summary>
            <ul>
              {draft.citations.map((c, i) => (
                <li key={i}>
                  <code>{c}</code>
                </li>
              ))}
            </ul>
          </details>
          <details>
            <summary>{t(lang, "limitations")}</summary>
            <ul>
              {draft.limitations.map((l, i) => (
                <li key={i}>{l}</li>
              ))}
            </ul>
          </details>
          {canDocument ? (
            <p style={{ display: "flex", gap: "0.6rem", flexWrap: "wrap" }}>
              <button
                className="primary"
                disabled={busy}
                onClick={() => void review("approved")}
              >
                {t(lang, "aiAcceptDraft")}
              </button>
              <button
                className="secondary"
                disabled={busy}
                onClick={() => void review("rejected")}
              >
                {t(lang, "aiRejectDraft")}
              </button>
            </p>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

const NOTE_FIELDS: {
  field: keyof NoteSections;
  labelKey: TKey;
  placeholderKey: TKey;
  optional: boolean;
}[] = [
  {
    field: "reason_for_encounter",
    labelKey: "reasonForEncounter",
    placeholderKey: "reasonPlaceholder",
    optional: false,
  },
  {
    field: "history_present_illness",
    labelKey: "historyPresentIllness",
    placeholderKey: "hpiPlaceholder",
    optional: false,
  },
  {
    field: "medical_history",
    labelKey: "medicalHistory",
    placeholderKey: "medicalHistoryPlaceholder",
    optional: true,
  },
  {
    field: "review_of_systems",
    labelKey: "reviewOfSystems",
    placeholderKey: "rosPlaceholder",
    optional: true,
  },
  {
    field: "physical_exam",
    labelKey: "physicalExam",
    placeholderKey: "examPlaceholder",
    optional: false,
  },
  {
    field: "assessment",
    labelKey: "assessment",
    placeholderKey: "assessmentPlaceholder",
    optional: false,
  },
  {
    field: "plan",
    labelKey: "plan",
    placeholderKey: "planPlaceholder",
    optional: false,
  },
  {
    field: "follow_up",
    labelKey: "followUpInstructions",
    placeholderKey: "followUpPlaceholder",
    optional: true,
  },
];

function SignedNoteView({
  ws,
  lang,
  onChanged,
}: {
  ws: Workspace;
  lang: Lang;
  onChanged: () => void;
}) {
  const note = ws.note;
  const [addendum, setAddendum] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);
  const [showForm, setShowForm] = useState(false);
  if (!note) return null;

  async function submitAddendum(e: React.FormEvent) {
    e.preventDefault();
    if (!addendum.trim()) {
      setError(t(lang, "requiredField"));
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await apiFetch(`/api/v1/encounters/${ws.encounter.id}/addenda`, {
        method: "POST",
        body: JSON.stringify({ body: addendum.trim() }),
      });
      setAddendum("");
      setShowForm(false);
      setSuccess(true);
      onChanged();
    } catch (err) {
      setError(errMessage(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="card">
      <h2>{t(lang, "signedSummary")}</h2>
      <p>
        <span className="badge ok">{t(lang, "signedBadge")}</span>{" "}
        <span className="muted">
          {t(lang, "readOnlyRecord")} · {t(lang, "author")}: {note.author}
          {note.signed_at
            ? ` · ${t(lang, "signedAt")}: ${formatDateTime(lang, note.signed_at)}`
            : null}
        </span>
      </p>
      {NOTE_FIELDS.map((f) => {
        const value = note[f.field];
        if (!value) return null;
        return (
          <section key={f.field} style={{ marginBottom: "0.7rem" }}>
            <h3 style={{ fontSize: "0.9rem", margin: "0 0 0.2rem" }}>
              {t(lang, f.labelKey)}
            </h3>
            <p style={{ whiteSpace: "pre-wrap", margin: 0 }}>{value}</p>
          </section>
        );
      })}
      {ws.addenda.length > 0 ? (
        <section>
          <h3 style={{ fontSize: "0.9rem" }}>{t(lang, "addenda")}</h3>
          <ul className="result-list">
            {ws.addenda.map((a, i) => (
              <li key={i} className="result-card addendum">
                <div className="grow">
                  <span className="badge warn">{t(lang, "addendumLabel")}</span>
                  <p style={{ whiteSpace: "pre-wrap", margin: "0.3rem 0 0" }}>
                    {a.body}
                  </p>
                  <p className="muted" style={{ margin: "0.2rem 0 0" }}>
                    {a.author} · {formatDateTime(lang, a.created_at)}
                  </p>
                </div>
              </li>
            ))}
          </ul>
        </section>
      ) : null}
      {success ? (
        <p role="status" className="success">
          {t(lang, "addendumAdded")}
        </p>
      ) : null}
      {error ? (
        <p role="alert" className="error">
          {error}
        </p>
      ) : null}
      {ws.capabilities.can_add_addendum ? (
        <>
          <button
            className="secondary"
            aria-expanded={showForm}
            onClick={() => {
              setShowForm((s) => !s);
              setSuccess(false);
            }}
          >
            {t(lang, "addAddendum")}
          </button>
          {showForm ? (
            <form onSubmit={submitAddendum} style={{ marginTop: "0.6rem" }}>
              <label htmlFor="addendum-body">{t(lang, "addendumLabel")}</label>
              <textarea
                id="addendum-body"
                rows={3}
                value={addendum}
                placeholder={t(lang, "addendumPlaceholder")}
                onChange={(e) => setAddendum(e.target.value)}
              />
              <p>
                <button className="primary" type="submit" disabled={busy}>
                  {t(lang, "addAddendum")}
                </button>
              </p>
            </form>
          ) : null}
        </>
      ) : null}
    </div>
  );
}

function EncounterWorkspace({ id }: { id: string }) {
  const { lang, authenticated } = useSession();
  const [ws, setWs] = useState<Workspace | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [sections, setSections] = useState<NoteSections>(EMPTY_SECTIONS);
  const [noteVersion, setNoteVersion] = useState<number | null>(null);
  const [dirty, setDirty] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveMessage, setSaveMessage] = useState<string | null>(null);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [confirmingSign, setConfirmingSign] = useState(false);
  const [confirmingCancel, setConfirmingCancel] = useState(false);
  const [collapsed, setCollapsed] = useState<Record<string, boolean>>({
    medical_history: true,
    review_of_systems: true,
    follow_up: true,
  });
  const hydratedNote = useRef(false);

  const load = useCallback(() => {
    setLoadError(null);
    apiFetch<Workspace>(`/api/v1/encounters/${id}`)
      .then((data) => {
        setWs(data);
        setNoteVersion(data.note?.version ?? null);
        if (!hydratedNote.current) {
          setSections(sectionsFromNote(data.note));
          hydratedNote.current = true;
        }
      })
      .catch((e) => setLoadError(errMessage(e)));
  }, [id]);

  useEffect(() => {
    if (authenticated) load();
  }, [authenticated, load]);

  // Protect against losing unsaved documentation on navigation.
  useEffect(() => {
    if (!dirty) return;
    const handler = (e: BeforeUnloadEvent) => {
      e.preventDefault();
    };
    window.addEventListener("beforeunload", handler);
    return () => window.removeEventListener("beforeunload", handler);
  }, [dirty]);

  const editable = Boolean(
    ws && ws.capabilities.can_document && ws.note?.status !== "signed",
  );

  const setSection = useCallback((field: keyof NoteSections, value: string) => {
    setSections((s) => ({ ...s, [field]: value }));
    setDirty(true);
    setSaveMessage(null);
  }, []);

  const save = useCallback(async (): Promise<number | null> => {
    setSaving(true);
    setSaveError(null);
    setSaveMessage(null);
    try {
      const body: Record<string, unknown> = { ...sections };
      if (noteVersion !== null) body.version = noteVersion;
      const res = await apiFetch<{ version: number }>(
        `/api/v1/encounters/${id}/note`,
        { method: "POST", body: JSON.stringify(body) },
      );
      setNoteVersion(res.version);
      setDirty(false);
      setSaveMessage(t(lang, "draftSaved"));
      return res.version;
    } catch (err) {
      if (err instanceof ApiRequestError && err.code === "version_conflict") {
        setSaveError(t(lang, "versionConflict"));
      } else if (err instanceof ApiRequestError && err.code === "note_signed") {
        setSaveError(t(lang, "noteImmutable"));
      } else {
        setSaveError(errMessage(err));
      }
      return null;
    } finally {
      setSaving(false);
    }
  }, [id, lang, noteVersion, sections]);

  const sign = useCallback(async () => {
    setConfirmingSign(false);
    setSaving(true);
    setSaveError(null);
    try {
      let version = noteVersion;
      if (dirty || version === null) {
        setSaving(false);
        version = await save();
        setSaving(true);
        if (version === null) return;
      }
      await apiFetch(`/api/v1/encounters/${id}/sign`, {
        method: "POST",
        body: JSON.stringify({ version }),
      });
      setSaveMessage(t(lang, "noteSigned"));
      setDirty(false);
      hydratedNote.current = false;
      load();
    } catch (err) {
      if (
        err instanceof ApiRequestError &&
        err.code === "sign_requires_reason"
      ) {
        setSaveError(t(lang, "signRequiresReason"));
      } else if (
        err instanceof ApiRequestError &&
        err.code === "sign_requires_assessment_or_plan"
      ) {
        setSaveError(t(lang, "signRequiresAssessment"));
      } else if (
        err instanceof ApiRequestError &&
        err.code === "version_conflict"
      ) {
        setSaveError(t(lang, "versionConflict"));
      } else {
        setSaveError(errMessage(err));
      }
    } finally {
      setSaving(false);
    }
  }, [dirty, id, lang, load, noteVersion, save]);

  const cancelEncounter = useCallback(async () => {
    setConfirmingCancel(false);
    setSaving(true);
    setSaveError(null);
    try {
      await apiFetch(`/api/v1/encounters/${id}/cancel`, { method: "POST" });
      setSaveMessage(t(lang, "encounterCancelled"));
      setDirty(false);
      load();
    } catch (err) {
      setSaveError(errMessage(err));
    } finally {
      setSaving(false);
    }
  }, [id, lang, load]);

  const acceptAiDraft = useCallback((text: string) => {
    setSections((s) => ({
      ...s,
      assessment: s.assessment ? `${s.assessment}\n\n${text}` : text,
    }));
    setDirty(true);
  }, []);

  const currentVitals = useMemo(() => ws?.vitals ?? [], [ws]);

  if (loadError) {
    const denied =
      loadError.includes("404") || loadError.toLowerCase().includes("not");
    return (
      <div className="card">
        <p role="alert" className="error">
          {denied ? t(lang, "notAuthorized") : loadError}
        </p>
        <button className="secondary" onClick={load}>
          {t(lang, "retry")}
        </button>
      </div>
    );
  }
  if (!ws) {
    return (
      <p className="muted" role="status">
        {t(lang, "loading")}
      </p>
    );
  }

  const signed = ws.note?.status === "signed";

  return (
    <>
      <SafetyHeader ws={ws} lang={lang} />

      <div className="encounter-layout">
        <div className="encounter-main">
          {signed ? (
            <SignedNoteView ws={ws} lang={lang} onChanged={load} />
          ) : ws.capabilities.can_document ? (
            <div className="card">
              <h2>{t(lang, "clinicalNote")}</h2>
              <p>
                <span className="badge neutral">{t(lang, "draftBadge")}</span>{" "}
                {dirty ? (
                  <span className="badge warn">
                    {t(lang, "unsavedChanges")}
                  </span>
                ) : null}
              </p>
              {NOTE_FIELDS.map((f) => {
                const isCollapsed = f.optional && collapsed[f.field];
                return (
                  <div key={f.field} style={{ marginBottom: "0.6rem" }}>
                    <label htmlFor={`note-${f.field}`}>
                      {t(lang, f.labelKey)}
                      {f.optional ? (
                        <>
                          {" "}
                          <span className="muted">
                            ({t(lang, "optional")})
                          </span>{" "}
                          <button
                            type="button"
                            className="linklike"
                            aria-expanded={!isCollapsed}
                            onClick={() =>
                              setCollapsed((c) => ({
                                ...c,
                                [f.field]: !c[f.field],
                              }))
                            }
                          >
                            {isCollapsed
                              ? t(lang, "showSection")
                              : t(lang, "hideSection")}
                          </button>
                        </>
                      ) : null}
                    </label>
                    {!isCollapsed ? (
                      <textarea
                        id={`note-${f.field}`}
                        rows={f.field === "reason_for_encounter" ? 2 : 3}
                        value={sections[f.field]}
                        placeholder={t(lang, f.placeholderKey)}
                        onChange={(e) => setSection(f.field, e.target.value)}
                      />
                    ) : null}
                  </div>
                );
              })}
              {saveError ? (
                <p role="alert" className="error">
                  {saveError}
                </p>
              ) : null}
              {saveMessage ? (
                <p role="status" className="success">
                  {saveMessage}
                </p>
              ) : null}
              {confirmingSign ? (
                <div className="confirm-box">
                  <p>{t(lang, "confirmSign")}</p>
                  <button
                    className="primary"
                    disabled={saving}
                    onClick={() => void sign()}
                  >
                    {t(lang, "confirm")}
                  </button>{" "}
                  <button
                    className="secondary"
                    onClick={() => setConfirmingSign(false)}
                  >
                    {t(lang, "cancel")}
                  </button>
                </div>
              ) : confirmingCancel ? (
                <div className="confirm-box">
                  <p>{t(lang, "confirmCancelEncounter")}</p>
                  <button
                    className="primary"
                    disabled={saving}
                    onClick={() => void cancelEncounter()}
                  >
                    {t(lang, "confirm")}
                  </button>{" "}
                  <button
                    className="secondary"
                    onClick={() => setConfirmingCancel(false)}
                  >
                    {t(lang, "cancel")}
                  </button>
                </div>
              ) : (
                <div
                  style={{ display: "flex", gap: "0.6rem", flexWrap: "wrap" }}
                >
                  <button
                    className="secondary"
                    disabled={saving || !editable}
                    onClick={() => void save()}
                  >
                    {saving ? t(lang, "savingDraft") : t(lang, "saveDraft")}
                  </button>
                  {ws.capabilities.can_sign ? (
                    <button
                      className="primary"
                      disabled={saving}
                      onClick={() => setConfirmingSign(true)}
                    >
                      {t(lang, "signComplete")}
                    </button>
                  ) : null}
                  <button
                    className="tertiary"
                    disabled={saving}
                    onClick={() => setConfirmingCancel(true)}
                  >
                    {t(lang, "cancelEncounter")}
                  </button>
                </div>
              )}
            </div>
          ) : (
            <div className="card">
              <h2>{t(lang, "clinicalNote")}</h2>
              {ws.note ? (
                <>
                  <p>
                    <span className="badge neutral">
                      {t(lang, "draftBadge")}
                    </span>{" "}
                    <span className="muted">
                      {t(lang, "author")}: {ws.note.author}
                    </span>
                  </p>
                  {NOTE_FIELDS.map((f) => {
                    const value = ws.note ? ws.note[f.field] : null;
                    if (!value) return null;
                    return (
                      <section key={f.field} style={{ marginBottom: "0.7rem" }}>
                        <h3
                          style={{ fontSize: "0.9rem", margin: "0 0 0.2rem" }}
                        >
                          {t(lang, f.labelKey)}
                        </h3>
                        <p style={{ whiteSpace: "pre-wrap", margin: 0 }}>
                          {value}
                        </p>
                      </section>
                    );
                  })}
                </>
              ) : (
                <p className="muted">{t(lang, "noNoteYet")}</p>
              )}
            </div>
          )}

          {!signed && editable ? (
            <AiDocAid
              encounterId={id}
              lang={lang}
              draft={ws.ai_draft}
              canDocument={ws.capabilities.can_document}
              onAccepted={acceptAiDraft}
              onChanged={load}
            />
          ) : null}
        </div>

        <div className="encounter-side">
          <div className="card">
            <h2>{t(lang, "vitalSigns")}</h2>
            {currentVitals.length === 0 ? (
              <p className="muted">{t(lang, "noVitals")}</p>
            ) : (
              <VitalsTable v={currentVitals[0]} lang={lang} />
            )}
            {editable ? (
              <VitalsForm encounterId={id} lang={lang} onSaved={load} />
            ) : null}
            {ws.previous_vitals.length > 0 ? (
              <details style={{ marginTop: "0.6rem" }}>
                <summary>{t(lang, "previousVitals")}</summary>
                {ws.previous_vitals.map((v) => (
                  <div key={v.id} style={{ marginTop: "0.5rem" }}>
                    <VitalsTable v={v} lang={lang} />
                  </div>
                ))}
              </details>
            ) : null}
          </div>

          <div className="card">
            <h2>{t(lang, "diagnoses")}</h2>
            {ws.diagnoses.length === 0 ? (
              <p className="muted">{t(lang, "noDiagnoses")}</p>
            ) : (
              <ul className="result-list">
                {ws.diagnoses.map((d) => (
                  <li key={d.id} className="result-card">
                    <div className="grow">
                      <div className="title">{d.display}</div>
                      <div className="muted">
                        {d.code ? `${t(lang, "code")}: ${d.code} · ` : null}
                        {d.status === "active"
                          ? t(lang, "dxActive")
                          : d.status === "provisional"
                            ? t(lang, "dxProvisional")
                            : d.status === "resolved"
                              ? t(lang, "dxResolved")
                              : d.status}
                      </div>
                    </div>
                    {d.this_encounter ? (
                      <span className="badge ok">
                        {t(lang, "thisConsultation")}
                      </span>
                    ) : null}
                  </li>
                ))}
              </ul>
            )}
            {editable ? (
              <DiagnosisForm encounterId={id} lang={lang} onSaved={load} />
            ) : null}
          </div>

          <div className="card">
            <h2>{t(lang, "medications")}</h2>
            {ws.medications.length === 0 ? (
              <p className="muted">{t(lang, "noMedications")}</p>
            ) : (
              <ul className="result-list">
                {ws.medications.map((m) => (
                  <li key={m.name} className="result-card">
                    <div className="grow title">{m.name}</div>
                    <span className="badge ok">{m.status}</span>
                  </li>
                ))}
              </ul>
            )}
          </div>

          <div className="card">
            <h2>{t(lang, "serviceRequests")}</h2>
            {ws.service_requests.length === 0 ? (
              <p className="muted">{t(lang, "noLabResults")}</p>
            ) : (
              <ul className="result-list">
                {ws.service_requests.map((sr) => (
                  <li key={sr.id} className="result-card">
                    <div className="grow">
                      <div className="title">{sr.display}</div>
                      <div className="muted">
                        {formatDateTime(lang, sr.created_at)}
                      </div>
                    </div>
                    <span className="badge neutral">
                      {loopStateShortLabel(lang, sr.loop_state)}
                    </span>
                    <Link className="navlink" href={`/requests/${sr.id}`}>
                      {t(lang, "openResult")}
                    </Link>
                  </li>
                ))}
              </ul>
            )}
            {ws.capabilities.can_order_lab && !signed ? (
              <LabOrderForm encounterId={id} lang={lang} onSaved={load} />
            ) : null}
          </div>

          <p>
            <Link className="navlink" href={`/patients/${ws.patient.id}`}>
              ← {patientName(ws.patient)}
            </Link>
          </p>
        </div>
      </div>
    </>
  );
}

export default function EncounterPage({ params }: { params: { id: string } }) {
  return (
    <AppShell>
      <EncounterWorkspace id={params.id} />
    </AppShell>
  );
}
