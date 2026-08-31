"use client";

import Link from "next/link";
import { useState } from "react";
import { useRouter } from "next/navigation";
import { AppShell } from "../chrome";
import { t } from "@/lib/i18n";
import { apiFetch, useSession } from "@/lib/session";
import { formatDate, patientName } from "@/lib/clinical";

type PatientHit = {
  id: string;
  family_name: string;
  given_name: string;
  birth_date: string;
  sex: string;
  identifier: string;
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
  const [query, setQuery] = useState("");
  const [searched, setSearched] = useState<string | null>(null);
  const [hits, setHits] = useState<PatientHit[] | null>(null);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);

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
    <div className="card">
      <h2>{t(lang, "searchPatients")}</h2>
      <form onSubmit={search}>
        <label htmlFor="patient-query">{t(lang, "searchPatients")}</label>
        <div style={{ display: "flex", gap: "0.5rem" }}>
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
      </form>
      <p className="muted">{t(lang, "searchHint")}</p>
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
        <ul className="result-list" style={{ marginTop: "0.75rem" }}>
          {hits.map((p) => (
            <li key={p.id} className="result-card">
              <div className="grow">
                <div className="title">{patientName(p)}</div>
                <div className="muted">
                  {p.identifier} · {sexLabel(lang, p.sex)} · {t(lang, "born")}{" "}
                  {formatDate(lang, p.birth_date)}
                </div>
              </div>
              <Link className="navlink" href={`/patients/${p.id}`}>
                {t(lang, "openChart")}
              </Link>
            </li>
          ))}
        </ul>
      ) : null}
    </div>
  );
}

function RegisterSection() {
  const { lang, meta } = useSession();
  const router = useRouter();
  const accessible = meta?.facilities.filter((f) => f.accessible) ?? [];
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

  // Users without any registrable facility (e.g. oversight roles) simply
  // don't see the form; the backend stays the authorization boundary.
  if (accessible.length === 0) return null;

  return (
    <div className="card" id="register">
      <h2>{t(lang, "registerPatient")}</h2>
      <form onSubmit={register}>
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
        <p>
          <button className="primary" type="submit" disabled={busy}>
            {t(lang, "register")}
          </button>
        </p>
      </form>
    </div>
  );
}

export default function PatientsPage() {
  const { lang } = useSession();
  return (
    <AppShell>
      <h2 style={{ marginTop: 0 }}>{t(lang, "patientsTitle")}</h2>
      <SearchSection />
      <RegisterSection />
    </AppShell>
  );
}
