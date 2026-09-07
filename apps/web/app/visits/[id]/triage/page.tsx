"use client";

import Link from "next/link";
import { useParams } from "next/navigation";
import { useCallback, useEffect, useRef, useState } from "react";
import { AppShell } from "../../../chrome";
import { t } from "@/lib/i18n";
import type { Lang, TKey } from "@/lib/i18n";
import { ApiRequestError, apiFetch, useSession } from "@/lib/session";
import {
  formatBloodPressure,
  formatDateTime,
  patientName,
} from "@/lib/clinical";
import {
  CONCERNS,
  PRIORITIES,
  RED_FLAGS,
  SERVICES,
  arrivalKindLabel,
  concernLabel,
  formatWait,
  meetsFloor,
  priorityBadge,
  priorityLabel,
  redFlagLabel,
  safetyRuleLabel,
  serviceLabel,
  visitStatusLabel,
} from "@/lib/visits";
import type { Priority, Service, VisitItem } from "@/lib/visits";
import { useVisitActions, visitErrorMessage } from "../../../access/visit-card";

type VitalField =
  | "systolic_mmhg"
  | "diastolic_mmhg"
  | "heart_rate_bpm"
  | "respiratory_rate_bpm"
  | "temperature_c"
  | "spo2_percent"
  | "weight_kg"
  | "height_cm";

const VITAL_FIELDS: { field: VitalField; labelKey: TKey; unit: string }[] = [
  { field: "systolic_mmhg", labelKey: "systolic", unit: "mmHg" },
  { field: "diastolic_mmhg", labelKey: "diastolic", unit: "mmHg" },
  { field: "heart_rate_bpm", labelKey: "heartRate", unit: "bpm" },
  { field: "respiratory_rate_bpm", labelKey: "respiratoryRate", unit: "/min" },
  { field: "temperature_c", labelKey: "temperature", unit: "°C" },
  { field: "spo2_percent", labelKey: "oxygenSaturation", unit: "%" },
  { field: "weight_kg", labelKey: "weight", unit: "kg" },
  { field: "height_cm", labelKey: "height", unit: "cm" },
];

type VitalSet = Record<VitalField, string | null> & {
  id: string;
  visit_id: string | null;
  encounter_id: string | null;
  bmi: string | null;
  recorded_at: string;
};

type Triage = {
  id: string;
  version: number;
  reason: string | null;
  concerns: string[];
  onset: string | null;
  red_flags: string[];
  note: string | null;
  vital_signs_id: string | null;
  priority: string | null;
  safety_floor: string;
  safety_rules: { rule: string; priority: string }[];
  rules_version: string;
  requested_service: string | null;
  ai_artifact_id: string | null;
  ai_decision: string | null;
  completed_at: string | null;
  updated_at: string;
  author_name: string;
};

type ProposalOutput = {
  proposed_priority: string;
  safety_floor: string;
  raised_to_floor: boolean;
  proposed_service: string;
  important_facts: string[];
  missing_information: string[];
  contradictions: string[];
  handoff_summary: string;
  rationale: string[];
  confidence: string;
  limitations: string[];
  cited_sources: string[];
};

type Proposal = {
  id: string;
  status: string;
  model: string | null;
  model_version: string | null;
  output: ProposalOutput | null;
  limitations: string[];
  citations: string[];
  triage_version: number | null;
  review_decision: string | null;
  review_detail: { decision?: string } | null;
  reviewed_at: string | null;
  generated_at: string | null;
};

type Detail = VisitItem & {
  patient_safety: {
    birth_date: string;
    sex: string | null;
    allergies: { substance: string; criticality: string | null }[];
    alerts: { severity: string; message: string }[];
    conditions: { display: string; status: string }[];
    medications: { name: string; status: string }[];
  };
  triage: Triage | null;
  vitals: VitalSet[];
  proposal: Proposal | null;
  professionals: { id: string; display_name: string }[];
  queues: { id: string; code: string; name: string }[];
  rules_version: string;
};

type Form = {
  reason: string;
  concerns: string[];
  onset: string;
  red_flags: string[];
  note: string;
  vitals: Partial<Record<VitalField, string>>;
  priority: string;
  requested_service: string;
  handoff: string;
};

function formFrom(d: Detail): Form {
  const tr = d.triage;
  return {
    reason: tr?.reason ?? d.reason ?? "",
    concerns: tr?.concerns ?? [],
    onset: tr?.onset ?? "",
    red_flags: tr?.red_flags ?? [],
    note: tr?.note ?? "",
    vitals: {},
    priority: tr?.priority ?? "",
    requested_service: tr?.requested_service ?? d.service,
    handoff: d.handoff_summary ?? "",
  };
}

function toggle(list: string[], code: string): string[] {
  return list.includes(code) ? list.filter((c) => c !== code) : [...list, code];
}

function triageError(lang: Lang, err: unknown): string {
  if (err instanceof ApiRequestError) {
    if (err.code === "priority_below_safety_floor")
      return t(lang, "belowFloor");
    if (err.code === "triage_not_started") return t(lang, "saveBeforeDmind");
    if (err.code === "proposal_awaiting_review")
      return t(lang, "decideProposalFirst");
    if (err.code === "artifact_stale" || err.code === "invalid_artifact_state")
      return t(lang, "proposalStale");
    if (err.code === "ai_unavailable") return t(lang, "aiUnavailable");
    if (err.code === "value_out_of_range") return err.message;
  }
  return visitErrorMessage(lang, err);
}

function SafetyHeader({ lang, d }: { lang: Lang; d: Detail }) {
  const s = d.patient_safety;
  return (
    <section className="card safety-header" aria-label={t(lang, "patient")}>
      <div className="visit-selected">
        <h2 style={{ margin: 0 }}>{patientName(d.patient)}</h2>
        <span className="muted">
          {d.patient.identifier} · {d.patient.age_years} {t(lang, "yearsShort")}
          {s.sex ? ` · ${s.sex}` : ""}
        </span>
        <span className="badge ok">{visitStatusLabel(lang, d.status)}</span>
        <span className="muted">
          {arrivalKindLabel(lang, d.arrival_kind)} ·{" "}
          {serviceLabel(lang, d.service)} · {d.facility.name}
        </span>
        {d.wait_minutes !== null ? (
          <span className="muted">
            {t(lang, "waiting")} {formatWait(lang, d.wait_minutes)}
          </span>
        ) : null}
      </div>
      <div className="visit-meta">
        {s.alerts.map((a, i) => (
          <span
            key={i}
            className={`badge ${a.severity === "high" || a.severity === "critical" ? "critical" : "warn"}`}
          >
            {a.message}
          </span>
        ))}
        {s.allergies.length === 0 ? (
          <span className="badge neutral">{t(lang, "noKnownAllergies")}</span>
        ) : (
          s.allergies.map((a) => (
            <span key={a.substance} className="badge warn">
              {t(lang, "allergy")}: {a.substance}
            </span>
          ))
        )}
        {s.conditions.map((c) => (
          <span key={c.display} className="badge neutral">
            {c.display}
          </span>
        ))}
      </div>
    </section>
  );
}

function ProposalPanel({
  lang,
  d,
  form,
  dirty,
  busy,
  onAsk,
  onReview,
}: {
  lang: Lang;
  d: Detail;
  form: Form;
  dirty: boolean;
  busy: string | null;
  onAsk: () => void;
  onReview: (decision: "accept" | "override" | "reject") => void;
}) {
  const p = d.proposal;
  const out = p?.output ?? null;
  const awaiting = p?.status === "awaiting_review";
  const stale =
    awaiting && d.triage !== null && p?.triage_version !== d.triage.version;
  const canDecide = awaiting && !stale && d.capabilities.can_triage && !dirty;
  const canAsk =
    d.capabilities.can_triage && d.triage !== null && !dirty && !busy;
  const overrideReady =
    form.priority !== "" &&
    form.requested_service !== "" &&
    (form.priority !== out?.proposed_priority ||
      form.requested_service !== out?.proposed_service);

  return (
    <section className="card dmind-panel" aria-labelledby="dmind-h">
      <h3 id="dmind-h" style={{ marginTop: 0 }}>
        {t(lang, "dmindTriage")}
      </h3>
      <p className="muted">{t(lang, "dmindTriageHelp")}</p>
      <p>
        <button
          type="button"
          className="secondary"
          disabled={!canAsk}
          onClick={onAsk}
        >
          {busy === "propose" ? t(lang, "askingDmind") : t(lang, "askDmind")}
        </button>
        {d.triage === null || dirty ? (
          <span className="muted" style={{ marginLeft: "0.6rem" }}>
            {t(lang, "saveBeforeDmind")}
          </span>
        ) : null}
      </p>
      {p && out ? (
        <div className="proposal" aria-live="polite">
          <div className="visit-meta">
            <span className="badge neutral">{t(lang, "aiAssistiveOnly")}</span>
            <span className={`badge ${priorityBadge(out.proposed_priority)}`}>
              {t(lang, "proposedPriority")}:{" "}
              {priorityLabel(lang, out.proposed_priority)}
              {out.raised_to_floor ? ` (${t(lang, "raisedToFloor")})` : ""}
            </span>
            <span className="badge neutral">
              {t(lang, "proposedService")}:{" "}
              {serviceLabel(lang, out.proposed_service)}
            </span>
            <span className="badge neutral">
              {t(
                lang,
                out.confidence === "high"
                  ? "confidenceHigh"
                  : out.confidence === "medium"
                    ? "confidenceMedium"
                    : "confidenceLow",
              )}
            </span>
          </div>
          <p>{out.handoff_summary}</p>
          {out.important_facts.length > 0 ? (
            <>
              <h4>{t(lang, "importantFacts")}</h4>
              <ul className="brief-list">
                {out.important_facts.map((f, i) => (
                  <li key={i}>{f}</li>
                ))}
              </ul>
            </>
          ) : null}
          {out.rationale.length > 0 ? (
            <>
              <h4>{t(lang, "rationale")}</h4>
              <ul className="brief-list">
                {out.rationale.map((f, i) => (
                  <li key={i}>{f}</li>
                ))}
              </ul>
            </>
          ) : null}
          {out.missing_information.length > 0 ? (
            <>
              <h4>{t(lang, "missingInformation")}</h4>
              <ul className="brief-list">
                {out.missing_information.map((f, i) => (
                  <li key={i}>{f}</li>
                ))}
              </ul>
            </>
          ) : null}
          {out.contradictions.length > 0 ? (
            <>
              <h4>{t(lang, "contradictions")}</h4>
              <ul className="brief-list">
                {out.contradictions.map((f, i) => (
                  <li key={i}>{f}</li>
                ))}
              </ul>
            </>
          ) : null}
          <h4>{t(lang, "limitations")}</h4>
          <ul className="brief-list">
            {out.limitations.map((f, i) => (
              <li key={i}>{f}</li>
            ))}
          </ul>
          <p className="muted">
            {t(lang, "factsUsed")}: {out.cited_sources.join(", ")} ·{" "}
            {p.model ?? "—"} {p.model_version ?? ""}
            {p.generated_at ? ` · ${formatDateTime(lang, p.generated_at)}` : ""}
          </p>
          {awaiting ? (
            stale ? (
              <p role="status" className="error">
                {t(lang, "proposalStale")}
              </p>
            ) : (
              <div
                className="visit-actions"
                style={{ justifyContent: "start" }}
              >
                <button
                  type="button"
                  className="primary"
                  disabled={!canDecide || busy !== null}
                  onClick={() => onReview("accept")}
                >
                  {t(lang, "acceptProposal")}
                </button>
                <button
                  type="button"
                  className="secondary"
                  disabled={!canDecide || busy !== null || !overrideReady}
                  onClick={() => onReview("override")}
                >
                  {t(lang, "overrideProposal")}
                </button>
                <button
                  type="button"
                  className="tertiary"
                  disabled={!canDecide || busy !== null}
                  onClick={() => onReview("reject")}
                >
                  {t(lang, "rejectProposal")}
                </button>
                <span className="muted">
                  {dirty ? t(lang, "saveBeforeDmind") : t(lang, "overrideHelp")}
                </span>
              </div>
            )
          ) : (
            <p role="status">
              <span className="badge neutral">
                {p.review_detail?.decision === "override"
                  ? t(lang, "proposalOverridden")
                  : p.review_decision === "approved"
                    ? t(lang, "proposalAccepted")
                    : p.review_decision === "rejected"
                      ? t(lang, "proposalRejected")
                      : t(lang, "proposalStale")}
              </span>{" "}
              {p.reviewed_at ? formatDateTime(lang, p.reviewed_at) : null}
            </p>
          )}
        </div>
      ) : null}
    </section>
  );
}

function TriageWorkspace({ visitId }: { visitId: string }) {
  const { lang, authenticated, meta } = useSession();
  const [d, setDetail] = useState<Detail | null>(null);
  const [form, setForm] = useState<Form | null>(null);
  const [dirty, setDirty] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [denied, setDenied] = useState(false);
  const [notFound, setNotFound] = useState(false);
  const [busy, setBusy] = useState<string | null>(null);
  const [message, setMessage] = useState<{
    kind: "error" | "success";
    text: string;
  } | null>(null);
  const [needsConfirm, setNeedsConfirm] = useState(false);
  const [routeKind, setRouteKind] = useState<"queue" | "professional">("queue");
  const [routeTarget, setRouteTarget] = useState("");
  const dirtyRef = useRef(false);
  dirtyRef.current = dirty;

  const load = useCallback(async () => {
    setError(null);
    try {
      const res = await apiFetch<Detail>(`/api/v1/visits/${visitId}`);
      setDetail(res);
      setDenied(false);
      setNotFound(false);
      // Unsaved edits survive a background refresh; the server copy only
      // replaces the form when nothing is pending locally.
      if (!dirtyRef.current) setForm(formFrom(res));
    } catch (err) {
      if (err instanceof ApiRequestError && err.status === 403) setDenied(true);
      else if (err instanceof ApiRequestError && err.status === 404)
        setNotFound(true);
      else setError(err instanceof Error ? err.message : String(err));
    }
  }, [visitId]);

  useEffect(() => {
    if (!authenticated || !meta) return;
    void load();
  }, [authenticated, meta, load]);

  const consult = useVisitActions(lang, load);

  const editable = Boolean(
    d &&
    d.capabilities.can_triage &&
    (d.status === "arrived" ||
      d.status === "triage_in_progress" ||
      d.status === "ready_for_consultation"),
  );
  const inTriage = d?.status === "triage_in_progress";
  const floor = d?.triage?.safety_floor ?? null;

  function update(patch: Partial<Form>) {
    setForm((f) => (f ? { ...f, ...patch } : f));
    setDirty(true);
    setMessage(null);
  }

  function vitalsBody(f: Form, confirm: boolean): Record<string, unknown> {
    const body: Record<string, unknown> = { confirm_unusual: confirm };
    for (const v of VITAL_FIELDS) {
      const raw = f.vitals[v.field]?.trim();
      if (raw) body[v.field] = raw;
    }
    return body;
  }

  /** Saves the displayed form against the version it was loaded from and
   *  returns the visit version the saved facts now carry, or null when the
   *  save did not land (validation, floor, conflict). */
  async function save(confirm = false): Promise<number | null> {
    if (!d || !form) return null;
    setBusy("save");
    setMessage(null);
    const submit = (priority: string | null) =>
      apiFetch<{ version: number }>(`/api/v1/visits/${visitId}/triage`, {
        method: "POST",
        body: JSON.stringify({
          version: d.version,
          reason: form.reason.trim() || null,
          concerns: form.concerns,
          onset: form.onset.trim() || null,
          red_flags: form.red_flags,
          note: form.note.trim() || null,
          vitals: vitalsBody(form, confirm),
          priority,
          requested_service: form.requested_service || null,
        }),
      });
    try {
      let priorityDropped = false;
      let saved: { version: number };
      try {
        saved = await submit(form.priority || null);
      } catch (err) {
        // New facts raised the floor above the chosen priority: keep the
        // facts, clear the now-invalid priority and let the clinician
        // choose again against the updated floor.
        if (
          err instanceof ApiRequestError &&
          err.code === "priority_below_safety_floor" &&
          form.priority
        ) {
          saved = await submit(null);
          priorityDropped = true;
        } else {
          throw err;
        }
      }
      setNeedsConfirm(false);
      setDirty(false);
      dirtyRef.current = false;
      // Saved vitals are now on the record; the inputs start blank again.
      setForm((f) =>
        f
          ? { ...f, vitals: {}, priority: priorityDropped ? "" : f.priority }
          : f,
      );
      await load();
      // The handoff summary is only recorded at completion, so the typed
      // text outlives the refresh.
      const handoff = form.handoff;
      setForm((f) => (f && !f.handoff && handoff ? { ...f, handoff } : f));
      setMessage(
        priorityDropped
          ? { kind: "error", text: t(lang, "belowFloorReselect") }
          : { kind: "success", text: t(lang, "triageSaved") },
      );
      return priorityDropped ? null : saved.version;
    } catch (err) {
      if (err instanceof ApiRequestError && err.code === "unusual_values") {
        setNeedsConfirm(true);
        setMessage({ kind: "error", text: t(lang, "unusualValues") });
      } else {
        setMessage({ kind: "error", text: triageError(lang, err) });
        if (err instanceof ApiRequestError && err.status === 409) {
          setDirty(false);
          dirtyRef.current = false;
          await load().catch(() => undefined);
        }
      }
      return null;
    } finally {
      setBusy(null);
    }
  }

  async function ask() {
    if (!d) return;
    setBusy("propose");
    setMessage(null);
    try {
      await apiFetch(`/api/v1/visits/${visitId}/triage/proposal`, {
        method: "POST",
        body: JSON.stringify({ language: lang }),
      });
      await load();
    } catch (err) {
      setMessage({ kind: "error", text: triageError(lang, err) });
    } finally {
      setBusy(null);
    }
  }

  async function review(decision: "accept" | "override" | "reject") {
    if (!d || !d.proposal || !form) return;
    setBusy("review");
    setMessage(null);
    try {
      const body: Record<string, unknown> = { version: d.version, decision };
      if (decision === "override") {
        body.priority = form.priority || null;
        body.requested_service = form.requested_service || null;
      }
      const res = await apiFetch<{
        applied_priority: string | null;
        applied_service: string | null;
      }>(`/api/v1/visits/${visitId}/triage/proposal/${d.proposal.id}/review`, {
        method: "POST",
        body: JSON.stringify(body),
      });
      const out = d.proposal.output;
      await load();
      // The clinician's decision lands in the form so completion uses it.
      setForm((f) =>
        f
          ? {
              ...f,
              priority: res.applied_priority ?? f.priority,
              requested_service: res.applied_service ?? f.requested_service,
              handoff:
                decision === "accept" && !f.handoff && out
                  ? out.handoff_summary
                  : f.handoff,
            }
          : f,
      );
      setMessage({
        kind: "success",
        text: t(lang, "proposalDecisionRecorded"),
      });
    } catch (err) {
      setMessage({ kind: "error", text: triageError(lang, err) });
      if (err instanceof ApiRequestError && err.status === 409)
        await load().catch(() => undefined);
    } finally {
      setBusy(null);
    }
  }

  async function reassign() {
    if (!d || !routeTarget) return;
    setBusy("assign");
    setMessage(null);
    try {
      await apiFetch(`/api/v1/visits/${visitId}/assign`, {
        method: "POST",
        body: JSON.stringify({
          version: d.version,
          assignee_user_id: routeKind === "professional" ? routeTarget : null,
          queue_id: routeKind === "queue" ? routeTarget : null,
        }),
      });
      setRouteTarget("");
      await load();
      setMessage({ kind: "success", text: t(lang, "reassigned") });
    } catch (err) {
      setMessage({ kind: "error", text: triageError(lang, err) });
      if (err instanceof ApiRequestError && err.status === 409)
        await load().catch(() => undefined);
    } finally {
      setBusy(null);
    }
  }

  async function complete() {
    if (!d || !form) return;
    if (!form.priority) {
      setMessage({ kind: "error", text: t(lang, "priorityRequired") });
      return;
    }
    if (floor && !meetsFloor(form.priority, floor)) {
      setMessage({ kind: "error", text: t(lang, "belowFloor") });
      return;
    }
    // The completion is bound to the version the displayed facts belong to:
    // the one the form was loaded from, or the one the save just produced.
    // Anything another clinician changed in between surfaces as a conflict
    // and reloads the workspace for review instead of being overwritten.
    let version = d.version;
    if (dirty) {
      const saved = await save();
      if (saved === null) return;
      version = saved;
    }
    setBusy("complete");
    setMessage(null);
    try {
      await apiFetch(`/api/v1/visits/${visitId}/triage/complete`, {
        method: "POST",
        body: JSON.stringify({
          version,
          priority: form.priority,
          requested_service: form.requested_service,
          handoff_summary: form.handoff.trim() || null,
          assignee_user_id:
            routeKind === "professional" && routeTarget ? routeTarget : null,
          queue_id: routeKind === "queue" && routeTarget ? routeTarget : null,
        }),
      });
      setRouteTarget("");
      await load();
      setMessage({ kind: "success", text: t(lang, "triageCompleted") });
    } catch (err) {
      setMessage({ kind: "error", text: triageError(lang, err) });
      if (err instanceof ApiRequestError && err.status === 409)
        await load().catch(() => undefined);
    } finally {
      setBusy(null);
    }
  }

  if (denied) {
    return (
      <div className="card">
        <p role="alert" className="error">
          {t(lang, "notAuthorized")}
        </p>
        <Link className="navlink" href="/access">
          {t(lang, "backToAccess")}
        </Link>
      </div>
    );
  }
  if (notFound) {
    return (
      <div className="card">
        <p role="alert" className="error">
          {t(lang, "notFound")}
        </p>
        <Link className="navlink" href="/access">
          {t(lang, "backToAccess")}
        </Link>
      </div>
    );
  }
  if (error && !d) {
    return (
      <div className="card">
        <p role="alert" className="error">
          {error}
        </p>
        <button className="secondary" onClick={() => void load()}>
          {t(lang, "retry")}
        </button>
      </div>
    );
  }
  if (!d || !form) {
    return (
      <p className="muted" role="status">
        {t(lang, "loading")}
      </p>
    );
  }

  const priorityOk =
    !form.priority || !floor || meetsFloor(form.priority, floor);
  const latestVitals = d.vitals[0] ?? null;
  const anyBusy = busy !== null || consult.busy !== null;

  return (
    <>
      <p>
        <Link className="navlink" href="/access">
          ← {t(lang, "backToAccess")}
        </Link>
      </p>
      <h2 style={{ marginTop: 0 }}>{t(lang, "triageTitle")}</h2>
      <p className="muted">{t(lang, "triageIntro")}</p>
      <SafetyHeader lang={lang} d={d} />
      {!editable ? (
        <p role="status" className="muted">
          {d.status === "arrived" ||
          d.status === "triage_in_progress" ||
          d.status === "ready_for_consultation"
            ? t(lang, "triageReadOnly")
            : t(lang, "triageClosed")}
        </p>
      ) : null}
      {message ? (
        <p
          role={message.kind === "error" ? "alert" : "status"}
          className={message.kind === "error" ? "error" : "success"}
        >
          {message.text}
        </p>
      ) : null}
      {error ? (
        <p role="alert" className="error">
          {error}
        </p>
      ) : null}
      {d.status === "ready_for_consultation" ||
      d.status === "in_consultation" ? (
        <section className="card handoff-card">
          <div className="visit-selected">
            <span className={`badge ${priorityBadge(d.priority)}`}>
              {priorityLabel(lang, d.priority)}
            </span>
            <span>{serviceLabel(lang, d.service)}</span>
            {d.assignment ? (
              <span className="muted">
                {d.assignment.kind === "professional"
                  ? `${t(lang, "assignedTo")} ${d.assignment.display_name ?? ""}`
                  : `${t(lang, "queue")}: ${d.assignment.name ?? ""}`}
              </span>
            ) : null}
            {d.capabilities.can_start_consultation ||
            d.capabilities.can_resume_consultation ? (
              <button
                type="button"
                className="primary"
                disabled={anyBusy}
                onClick={() => void consult.run(d, "start")}
              >
                {d.capabilities.can_resume_consultation
                  ? t(lang, "resumeConsultation")
                  : t(lang, "startConsultation")}
              </button>
            ) : null}
          </div>
          {d.handoff_summary ? <p>{d.handoff_summary}</p> : null}
          {consult.message ? (
            <p role="alert" className="error">
              {consult.message.text}
            </p>
          ) : null}
        </section>
      ) : null}

      <div className="triage-layout">
        <div className="triage-main">
          <form
            className="card"
            aria-labelledby="assessment-h"
            onSubmit={(e) => {
              e.preventDefault();
              void save(needsConfirm);
            }}
          >
            <h3 id="assessment-h" style={{ marginTop: 0 }}>
              {t(lang, "reasonForVisit")}
            </h3>
            <label htmlFor="tr-reason">{t(lang, "reasonForVisit")}</label>
            <input
              id="tr-reason"
              type="text"
              maxLength={500}
              value={form.reason}
              disabled={!editable}
              onChange={(e) => update({ reason: e.target.value })}
            />
            <fieldset className="choice-group" disabled={!editable}>
              <legend>{t(lang, "concerns")}</legend>
              {CONCERNS.map((c) => (
                <label key={c} className="check-option">
                  <input
                    type="checkbox"
                    checked={form.concerns.includes(c)}
                    onChange={() =>
                      update({ concerns: toggle(form.concerns, c) })
                    }
                  />
                  {concernLabel(lang, c)}
                </label>
              ))}
            </fieldset>
            <label htmlFor="tr-onset">{t(lang, "onset")}</label>
            <input
              id="tr-onset"
              type="text"
              maxLength={200}
              value={form.onset}
              placeholder={t(lang, "onsetPlaceholder")}
              disabled={!editable}
              onChange={(e) => update({ onset: e.target.value })}
            />
            <fieldset className="choice-group red-flags" disabled={!editable}>
              <legend>{t(lang, "redFlags")}</legend>
              <p className="muted" style={{ flexBasis: "100%", margin: 0 }}>
                {t(lang, "redFlagsHelp")}
              </p>
              {RED_FLAGS.map((c) => (
                <label key={c} className="check-option">
                  <input
                    type="checkbox"
                    checked={form.red_flags.includes(c)}
                    onChange={() =>
                      update({ red_flags: toggle(form.red_flags, c) })
                    }
                  />
                  {redFlagLabel(lang, c)}
                </label>
              ))}
            </fieldset>

            <h3>{t(lang, "vitalSigns")}</h3>
            {latestVitals ? (
              <p className="muted">
                {t(lang, "previousVitals")} (
                {formatDateTime(lang, latestVitals.recorded_at)}):{" "}
                {formatBloodPressure(
                  latestVitals.systolic_mmhg,
                  latestVitals.diastolic_mmhg,
                )}{" "}
                mmHg
                {latestVitals.heart_rate_bpm
                  ? ` · ${latestVitals.heart_rate_bpm} bpm`
                  : ""}
                {latestVitals.respiratory_rate_bpm
                  ? ` · ${latestVitals.respiratory_rate_bpm} /min`
                  : ""}
                {latestVitals.temperature_c
                  ? ` · ${latestVitals.temperature_c} °C`
                  : ""}
                {latestVitals.spo2_percent
                  ? ` · SpO₂ ${latestVitals.spo2_percent} %`
                  : ""}
              </p>
            ) : (
              <p className="muted">{t(lang, "noVitals")}</p>
            )}
            <div className="vitals-form-grid">
              {VITAL_FIELDS.map((f) => (
                <div key={f.field}>
                  <label htmlFor={`tr-vital-${f.field}`}>
                    {t(lang, f.labelKey)} ({f.unit})
                  </label>
                  <input
                    id={`tr-vital-${f.field}`}
                    inputMode="decimal"
                    value={form.vitals[f.field] ?? ""}
                    disabled={!editable}
                    onChange={(e) =>
                      update({
                        vitals: { ...form.vitals, [f.field]: e.target.value },
                      })
                    }
                  />
                </div>
              ))}
            </div>

            <label htmlFor="tr-note">{t(lang, "triageNote")}</label>
            <textarea
              id="tr-note"
              rows={3}
              maxLength={4000}
              value={form.note}
              placeholder={t(lang, "triageNotePlaceholder")}
              disabled={!editable}
              onChange={(e) => update({ note: e.target.value })}
            />

            <div className="vitals-form-grid">
              <div>
                <label htmlFor="tr-priority">
                  {t(lang, "operationalPriority")}
                </label>
                <select
                  id="tr-priority"
                  value={form.priority}
                  disabled={!editable}
                  aria-invalid={!priorityOk}
                  onChange={(e) => update({ priority: e.target.value })}
                >
                  <option value="">—</option>
                  {PRIORITIES.map((p: Priority) => (
                    <option
                      key={p}
                      value={p}
                      disabled={floor !== null && !meetsFloor(p, floor)}
                    >
                      {priorityLabel(lang, p)}
                    </option>
                  ))}
                </select>
                {!priorityOk ? (
                  <p className="error" role="alert">
                    {t(lang, "belowFloor")}
                  </p>
                ) : null}
              </div>
              <div>
                <label htmlFor="tr-service">
                  {t(lang, "destinationService")}
                </label>
                <select
                  id="tr-service"
                  value={form.requested_service}
                  disabled={!editable}
                  onChange={(e) =>
                    update({ requested_service: e.target.value as Service })
                  }
                >
                  {SERVICES.map((s) => (
                    <option key={s} value={s}>
                      {serviceLabel(lang, s)}
                    </option>
                  ))}
                </select>
              </div>
            </div>

            {editable ? (
              <p className="visit-actions" style={{ justifyContent: "start" }}>
                <button
                  type="submit"
                  className={inTriage ? "secondary" : "primary"}
                  disabled={anyBusy || !dirty}
                >
                  {busy === "save"
                    ? t(lang, "savingTriage")
                    : needsConfirm
                      ? t(lang, "confirmUnusualSave")
                      : t(lang, "saveTriage")}
                </button>
                <span className="muted" role="status">
                  {dirty ? t(lang, "unsavedChanges") : ""}
                </span>
              </p>
            ) : null}
          </form>

          {editable && inTriage ? (
            <section className="card" aria-labelledby="complete-h">
              <h3 id="complete-h" style={{ marginTop: 0 }}>
                {t(lang, "completeTriage")}
              </h3>
              <label htmlFor="tr-handoff">{t(lang, "handoffSummary")}</label>
              <textarea
                id="tr-handoff"
                rows={2}
                maxLength={4000}
                value={form.handoff}
                placeholder={t(lang, "handoffPlaceholder")}
                onChange={(e) =>
                  setForm((f) => (f ? { ...f, handoff: e.target.value } : f))
                }
              />
              <RoutePicker
                lang={lang}
                d={d}
                kind={routeKind}
                target={routeTarget}
                onKind={setRouteKind}
                onTarget={setRouteTarget}
              />
              <p className="visit-actions" style={{ justifyContent: "start" }}>
                <button
                  type="button"
                  className="primary"
                  disabled={anyBusy || !form.priority || !priorityOk}
                  onClick={() => void complete()}
                >
                  {busy === "complete"
                    ? t(lang, "completingTriage")
                    : t(lang, "completeTriage")}
                </button>
                {d.capabilities.can_assign && routeTarget ? (
                  <button
                    type="button"
                    className="tertiary"
                    disabled={anyBusy}
                    onClick={() => void reassign()}
                  >
                    {t(lang, "reassign")}
                  </button>
                ) : null}
              </p>
            </section>
          ) : null}
        </div>

        <div className="triage-side">
          <section className="card safety-floor" aria-labelledby="floor-h">
            <h3 id="floor-h" style={{ marginTop: 0 }}>
              {t(lang, "safetyFloor")}
            </h3>
            <p className="floor-value">
              <span className={`badge ${priorityBadge(floor)}`}>
                {floor ? priorityLabel(lang, floor) : "—"}
              </span>
            </p>
            <p className="muted">{t(lang, "safetyFloorHelp")}</p>
            <h4>{t(lang, "safetyRules")}</h4>
            {d.triage && d.triage.safety_rules.length > 0 ? (
              <ul className="brief-list">
                {d.triage.safety_rules.map((r) => (
                  <li key={r.rule}>
                    {safetyRuleLabel(lang, r.rule)} →{" "}
                    <span className={`badge ${priorityBadge(r.priority)}`}>
                      {priorityLabel(lang, r.priority)}
                    </span>
                  </li>
                ))}
              </ul>
            ) : (
              <p className="muted">{t(lang, "noSafetyRules")}</p>
            )}
            <p className="muted">
              {t(lang, "rulesVersion")}: {d.rules_version}
              {d.triage
                ? ` · ${t(lang, "triagedBy")} ${d.triage.author_name} · ${formatDateTime(lang, d.triage.updated_at)}`
                : ""}
            </p>
          </section>
          <ProposalPanel
            lang={lang}
            d={d}
            form={form}
            dirty={dirty}
            busy={busy}
            onAsk={() => void ask()}
            onReview={(decision) => void review(decision)}
          />
        </div>
      </div>
    </>
  );
}

function RoutePicker({
  lang,
  d,
  kind,
  target,
  onKind,
  onTarget,
}: {
  lang: Lang;
  d: Detail;
  kind: "queue" | "professional";
  target: string;
  onKind: (k: "queue" | "professional") => void;
  onTarget: (id: string) => void;
}) {
  const current = d.assignment
    ? d.assignment.kind === "professional"
      ? `${t(lang, "assignedTo")} ${d.assignment.display_name ?? ""}`
      : `${t(lang, "queue")}: ${d.assignment.name ?? ""}`
    : t(lang, "unassigned");
  return (
    <div>
      <fieldset className="choice-group">
        <legend>{t(lang, "routeTo")}</legend>
        <p className="muted" style={{ flexBasis: "100%", margin: 0 }}>
          {current}
        </p>
        <label className="radio-option">
          <input
            type="radio"
            name="route-kind"
            checked={kind === "queue"}
            onChange={() => {
              onKind("queue");
              onTarget("");
            }}
          />
          {t(lang, "routeQueue")}
        </label>
        <label className="radio-option">
          <input
            type="radio"
            name="route-kind"
            checked={kind === "professional"}
            onChange={() => {
              onKind("professional");
              onTarget("");
            }}
          />
          {t(lang, "routeProfessional")}
        </label>
      </fieldset>
      <label htmlFor="tr-route-target">
        {kind === "queue"
          ? t(lang, "routeQueue")
          : t(lang, "routeProfessional")}
      </label>
      <select
        id="tr-route-target"
        value={target}
        onChange={(e) => onTarget(e.target.value)}
      >
        <option value="">{t(lang, "routeAuto")}</option>
        {kind === "queue"
          ? d.queues.map((q) => (
              <option key={q.id} value={q.id}>
                {q.name}
              </option>
            ))
          : d.professionals.map((p) => (
              <option key={p.id} value={p.id}>
                {p.display_name}
              </option>
            ))}
      </select>
    </div>
  );
}

export default function TriagePage() {
  const params = useParams<{ id: string }>();
  return (
    <AppShell>
      <TriageWorkspace visitId={params.id} />
    </AppShell>
  );
}
