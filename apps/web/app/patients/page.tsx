"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { AppShell } from "../chrome";
import { ExpectedToday } from "../dashboard/expected-today";
import { t } from "@/lib/i18n";
import { apiFetch, useSession } from "@/lib/session";
import {
  canRegisterPatients,
  formatDate,
  patientName,
  registrableFacilities,
} from "@/lib/clinical";
import { canReadVisits } from "@/lib/visits";
import type { VisitItem } from "@/lib/visits";
import { Icon } from "@/components/ui/icons";
import { Pill } from "@/components/ui/primitives";

type PatientHit = {
  id: string;
  family_name: string;
  given_name: string;
  birth_date: string;
  sex: string;
  identifier: string;
  can_open_chart: boolean;
  can_start_encounter: boolean;
};

function sexLabel(lang: "en" | "es", sex: string): string {
  switch (sex) {
    case "female":
      return t(lang, "sexFemale");
    case "male":
      return t(lang, "sexMale");
    case "other":
      return t(lang, "sexOther");
    default:
      return t(lang, "sexUnknown");
  }
}

function SearchSection() {
  const { lang } = useSession();
  const router = useRouter();
  const [query, setQuery] = useState("");
  const [searched, setSearched] = useState<string | null>(null);
  const [hits, setHits] = useState<PatientHit[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [starting, setStarting] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Physician chart reads require an established care relationship, so a
  // clinician's entry point from search is starting an encounter; the chart
  // opens once that relationship exists.
  async function startEncounter(patientId: string) {
    setStarting(patientId);
    setError(null);
    try {
      await apiFetch<{ id: string }>("/api/v1/encounters", {
        method: "POST",
        body: JSON.stringify({ patient_id: patientId }),
      });
      router.push(`/patients/${patientId}`);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setStarting(null);
    }
  }

  async function search(e: React.FormEvent) {
    e.preventDefault();
    const q = query.trim();
    if (q.length < 2) return;
    setBusy(true);
    setError(null);
    try {
      const res = await apiFetch<{ patients: PatientHit[] }>(
        `/api/v1/patients?query=${encodeURIComponent(q)}`,
      );
      setHits(res.patients);
      setSearched(q);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <>
      <section className="hero patients-hero" aria-labelledby="patients-h">
        <h1 id="patients-h">{t(lang, "patientsTitle")}</h1>
        <p className="hero-lead">{t(lang, "patientsLead")}</p>
        <form onSubmit={search} className="patients-search" role="search">
          <label htmlFor="patient-query" className="sr-only">
            {t(lang, "searchPatients")}
          </label>
          <div className="patients-search-row">
            <span className="patients-search-icon" aria-hidden="true">
              <Icon.Search />
            </span>
            <input
              id="patient-query"
              value={query}
              placeholder={t(lang, "searchPlaceholder")}
              onChange={(e) => setQuery(e.target.value)}
              autoComplete="off"
            />
            <button
              className="primary"
              type="submit"
              disabled={busy || query.trim().length < 2}
            >
              {t(lang, "search")}
            </button>
          </div>
          <p className="patients-search-hint">{t(lang, "searchHint")}</p>
        </form>
      </section>
      <section
        className="card patients-results"
        aria-live="polite"
        hidden={!busy && !error && hits === null}
      >
        {error ? (
          <p role="alert" className="error">
            {error}
          </p>
        ) : null}
        {busy ? (
          <p className="muted" role="status">
            {t(lang, "loading")}
          </p>
        ) : null}
        {hits && hits.length === 0 && searched ? (
          <div>
            <p>
              {t(lang, "noResultsFor")} “{searched}”.
            </p>
            <p className="muted">{t(lang, "checkSpelling")}</p>
          </div>
        ) : null}
        {hits && hits.length > 0 ? (
          <ul className="result-list patient-hits">
            {hits.map((p) => (
              <li key={p.id} className="result-card patient-hit">
                <span className="avatar" aria-hidden="true">
                  {initials(p)}
                </span>
                <div className="grow">
                  <div className="title">{patientName(p)}</div>
                  <div className="muted patient-hit-meta">
                    <Pill tone="neutral">{p.identifier}</Pill>
                    <span>{sexLabel(lang, p.sex)}</span>
                    <span>
                      {t(lang, "born")} {formatDate(lang, p.birth_date)}
                    </span>
                  </div>
                </div>
                <div className="patient-hit-actions">
                  {p.can_start_encounter ? (
                    <button
                      className="secondary"
                      disabled={starting !== null}
                      onClick={() => void startEncounter(p.id)}
                    >
                      {starting === p.id
                        ? t(lang, "loading")
                        : t(lang, "startEncounter")}
                    </button>
                  ) : null}
                  {p.can_open_chart !== false ? (
                    <Link className="navlink" href={`/patients/${p.id}`}>
                      {t(lang, "openChart")}
                    </Link>
                  ) : null}
                </div>
              </li>
            ))}
          </ul>
        ) : null}
      </section>
    </>
  );
}

function initials(p: { given_name: string; family_name: string }): string {
  return `${p.given_name.charAt(0)}${p.family_name.charAt(0)}`.toUpperCase();
}

/** Today's visits for roles allowed to read the access board; hidden (never
 *  fabricated) when the board is unavailable. */
function TodaySection() {
  const { lang, meta } = useSession();
  const roles = meta?.user.roles ?? [];
  const allowed = canReadVisits(roles);
  const [visits, setVisits] = useState<VisitItem[] | null>(null);
  useEffect(() => {
    if (!allowed) return;
    let live = true;
    apiFetch<{ items: VisitItem[] }>("/api/v1/visits?view=access")
      .then((r) => {
        if (live) setVisits(Array.isArray(r.items) ? r.items : null);
      })
      .catch(() => {
        if (live) setVisits(null);
      });
    return () => {
      live = false;
    };
  }, [allowed]);
  if (!allowed || visits === null) return null;
  return <ExpectedToday lang={lang} visits={visits} limit={8} />;
}

function RegisterSection() {
  const { lang, meta } = useSession();
  const router = useRouter();
  const accessible = registrableFacilities(meta?.facilities ?? []);
  const [form, setForm] = useState({
    facility_id: "",
    family_name: "",
    given_name: "",
    birth_date: "",
    sex: "female",
    identifier: "",
  });
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState(false);

  const facilityId = form.facility_id || accessible[0]?.id || "";

  async function register(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setSuccess(false);
    try {
      const res = await apiFetch<{ id: string }>("/api/v1/patients", {
        method: "POST",
        body: JSON.stringify({ ...form, facility_id: facilityId }),
      });
      setSuccess(true);
      router.push(`/patients/${res.id}`);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  // Only roles with registration permission see the form (and only with a
  // registrable facility); the backend stays the authorization boundary.
  if (!meta || !canRegisterPatients(meta.facilities)) return null;
  if (accessible.length === 0) return null;

  return (
    <section className="card register-card" id="register">
      <div className="card-head">
        <h2>{t(lang, "registerPatient")}</h2>
      </div>
      <form onSubmit={register} className="register-form">
        {accessible.length > 1 ? (
          <>
            <label htmlFor="reg-facility">{t(lang, "facility")}</label>
            <select
              id="reg-facility"
              value={facilityId}
              onChange={(e) =>
                setForm({ ...form, facility_id: e.target.value })
              }
            >
              {accessible.map((f) => (
                <option key={f.id} value={f.id}>
                  {f.name}
                </option>
              ))}
            </select>
          </>
        ) : null}
        <label htmlFor="reg-family">{t(lang, "familyName")}</label>
        <input
          id="reg-family"
          required
          value={form.family_name}
          onChange={(e) => setForm({ ...form, family_name: e.target.value })}
        />
        <label htmlFor="reg-given">{t(lang, "givenName")}</label>
        <input
          id="reg-given"
          required
          value={form.given_name}
          onChange={(e) => setForm({ ...form, given_name: e.target.value })}
        />
        <label htmlFor="reg-birth">{t(lang, "birthDate")}</label>
        <input
          id="reg-birth"
          type="date"
          required
          value={form.birth_date}
          onChange={(e) => setForm({ ...form, birth_date: e.target.value })}
        />
        <label htmlFor="reg-sex">{t(lang, "sex")}</label>
        <select
          id="reg-sex"
          value={form.sex}
          onChange={(e) => setForm({ ...form, sex: e.target.value })}
        >
          <option value="female">{t(lang, "sexFemale")}</option>
          <option value="male">{t(lang, "sexMale")}</option>
          <option value="other">{t(lang, "sexOther")}</option>
          <option value="unknown">{t(lang, "sexUnknown")}</option>
        </select>
        <label htmlFor="reg-id">{t(lang, "identifier")}</label>
        <input
          id="reg-id"
          required
          value={form.identifier}
          onChange={(e) => setForm({ ...form, identifier: e.target.value })}
        />
        {error ? (
          <p role="alert" className="error">
            {error}
          </p>
        ) : null}
        {success ? (
          <p role="status" className="success">
            {t(lang, "registered")}
          </p>
        ) : null}
        <p className="register-submit">
          <button className="primary" type="submit" disabled={busy}>
            {t(lang, "register")}
          </button>
        </p>
      </form>
    </section>
  );
}

export default function PatientsPage() {
  return (
    <AppShell>
      <div className="patients-page">
        <div className="patients-main">
          <SearchSection />
        </div>
        <aside className="patients-side">
          <TodaySection />
          <RegisterSection />
        </aside>
      </div>
    </AppShell>
  );
}
