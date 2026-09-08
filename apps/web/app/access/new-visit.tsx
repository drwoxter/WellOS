"use client";

import { useState } from "react";
import { t } from "@/lib/i18n";
import type { Lang } from "@/lib/i18n";
import { apiFetch } from "@/lib/session";
import { patientName } from "@/lib/clinical";
import {
  ARRIVAL_KINDS,
  SERVICES,
  arrivalKindLabel,
  isSchedulableAt,
  localDateTimeToIso,
  serviceLabel,
} from "@/lib/visits";
import type { ArrivalKind, Service } from "@/lib/visits";
import { visitErrorMessage } from "./visit-card";

export type PatientHit = {
  id: string;
  family_name: string;
  given_name: string;
  identifier: string;
  birth_date: string;
};

function errText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** Registers an appointment or an arrival for a patient found by name or
 *  identifier. Urgent arrivals are alerted server-side on creation. */
export function NewVisit({
  lang,
  onCreated,
  fixedPatient,
  headingId = "new-visit-h",
}: {
  lang: Lang;
  onCreated: () => Promise<unknown>;
  /** Register for one known patient (chart context): no search, no change. */
  fixedPatient?: PatientHit;
  headingId?: string;
}) {
  const [query, setQuery] = useState("");
  const [hits, setHits] = useState<PatientHit[] | null>(null);
  const [searched, setSearched] = useState<string | null>(null);
  const [searching, setSearching] = useState(false);
  const [patient, setPatient] = useState<PatientHit | null>(
    fixedPatient ?? null,
  );
  const [kind, setKind] = useState<ArrivalKind>("walk_in");
  const [service, setService] = useState<Service>("general_medicine");
  const [scheduledAt, setScheduledAt] = useState("");
  const [reason, setReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);

  async function search(e: React.FormEvent) {
    e.preventDefault();
    const q = query.trim();
    if (q.length < 2) return;
    setSearching(true);
    setError(null);
    try {
      const res = await apiFetch<{ patients: PatientHit[] }>(
        `/api/v1/patients?query=${encodeURIComponent(q)}`,
      );
      setHits(res.patients);
      setSearched(q);
    } catch (err) {
      setError(errText(err));
    } finally {
      setSearching(false);
    }
  }

  function choose(p: PatientHit) {
    setPatient(p);
    setHits(null);
    setQuery("");
    setSuccess(null);
    setError(null);
  }

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    if (!patient) return;
    setError(null);
    setSuccess(null);
    let scheduled_at: string | null = null;
    if (kind === "scheduled") {
      scheduled_at = localDateTimeToIso(scheduledAt);
      if (!scheduled_at) {
        setError(t(lang, "requiredField"));
        return;
      }
      if (!isSchedulableAt(scheduled_at)) {
        setError(t(lang, "scheduledOutOfWindow"));
        return;
      }
    }
    setBusy(true);
    try {
      await apiFetch("/api/v1/visits", {
        method: "POST",
        body: JSON.stringify({
          patient_id: patient.id,
          arrival_kind: kind,
          service,
          reason: reason.trim() || null,
          scheduled_at,
        }),
      });
      setSuccess(
        `${t(lang, "visitRegistered")} ${patientName(patient)} — ${arrivalKindLabel(lang, kind)}, ${serviceLabel(lang, service)}.`,
      );
      setPatient(fixedPatient ?? null);
      setReason("");
      setScheduledAt("");
      await onCreated();
    } catch (err) {
      setError(visitErrorMessage(lang, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <section className="card new-visit" aria-labelledby={headingId}>
      <h2 id={headingId}>{t(lang, "newVisit")}</h2>
      {!fixedPatient ? (
        <p className="muted">{t(lang, "newVisitHelp")}</p>
      ) : null}
      {success ? (
        <p className="success" role="status">
          {success}
        </p>
      ) : null}
      {!patient ? (
        <>
          <form onSubmit={search} className="picker-form" role="search">
            <label htmlFor="visit-patient-q">{t(lang, "findPatient")}</label>
            <div className="recording-row">
              <input
                id="visit-patient-q"
                className="grow"
                type="search"
                value={query}
                placeholder={t(lang, "searchPlaceholder")}
                autoComplete="off"
                onChange={(e) => setQuery(e.target.value)}
              />
              <button
                type="submit"
                className="secondary"
                disabled={searching || query.trim().length < 2}
              >
                {searching ? t(lang, "loading") : t(lang, "search")}
              </button>
            </div>
          </form>
          {hits ? (
            hits.length === 0 ? (
              <p className="muted" role="status">
                {t(lang, "noResultsFor")} “{searched}”.
              </p>
            ) : (
              <ul className="result-list picker-results">
                {hits.map((p) => (
                  <li key={p.id} className="result-card routine">
                    <div className="grow">
                      <div className="title">{patientName(p)}</div>
                      <div className="muted">
                        {p.identifier} · {t(lang, "born")} {p.birth_date}
                      </div>
                    </div>
                    <button
                      type="button"
                      className="primary"
                      onClick={() => choose(p)}
                    >
                      {t(lang, "choose")}
                    </button>
                  </li>
                ))}
              </ul>
            )
          ) : null}
        </>
      ) : (
        <form onSubmit={submit} className="new-visit-form">
          <div className="visit-selected">
            <span>
              <strong>{t(lang, "patient")}:</strong> {patientName(patient)}{" "}
              <span className="muted">· {patient.identifier}</span>
            </span>
            {!fixedPatient ? (
              <button
                type="button"
                className="linklike"
                onClick={() => setPatient(null)}
              >
                {t(lang, "changePatient")}
              </button>
            ) : null}
          </div>
          <fieldset className="arrival-kinds">
            <legend>{t(lang, "arrivalKind")}</legend>
            {ARRIVAL_KINDS.map((k) => (
              <label key={k} className="radio-option">
                <input
                  type="radio"
                  name="arrival_kind"
                  value={k}
                  checked={kind === k}
                  onChange={() => setKind(k)}
                />
                {arrivalKindLabel(lang, k)}
              </label>
            ))}
          </fieldset>
          <div className="vitals-form-grid">
            <div>
              <label htmlFor="visit-service">{t(lang, "service")}</label>
              <select
                id="visit-service"
                value={service}
                onChange={(e) => setService(e.target.value as Service)}
              >
                {SERVICES.map((s) => (
                  <option key={s} value={s}>
                    {serviceLabel(lang, s)}
                  </option>
                ))}
              </select>
            </div>
            {kind === "scheduled" ? (
              <div>
                <label htmlFor="visit-scheduled">
                  {t(lang, "scheduledFor")}
                </label>
                <input
                  id="visit-scheduled"
                  type="datetime-local"
                  required
                  value={scheduledAt}
                  onChange={(e) => setScheduledAt(e.target.value)}
                />
              </div>
            ) : null}
          </div>
          <label htmlFor="visit-reason">
            {t(lang, "reasonForVisit")}{" "}
            <span className="muted">({t(lang, "optional")})</span>
          </label>
          <input
            id="visit-reason"
            type="text"
            maxLength={500}
            value={reason}
            placeholder={t(lang, "reasonForVisitPlaceholder")}
            onChange={(e) => setReason(e.target.value)}
          />
          {error ? (
            <p role="alert" className="error">
              {error}
            </p>
          ) : null}
          <p>
            <button type="submit" className="primary" disabled={busy}>
              {busy ? t(lang, "registering") : t(lang, "registerVisit")}
            </button>
          </p>
        </form>
      )}
      {!patient && error ? (
        <p role="alert" className="error">
          {error}
        </p>
      ) : null}
    </section>
  );
}
