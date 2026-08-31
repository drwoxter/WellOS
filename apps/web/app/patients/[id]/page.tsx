"use client";

import Link from "next/link";
import { useCallback, useEffect, useMemo, useState } from "react";
import { AppShell } from "../../chrome";
import { t } from "@/lib/i18n";
import type { Lang, TKey } from "@/lib/i18n";
import { apiFetch, useSession } from "@/lib/session";
import {
  LAB_TESTS,
  canActClinicallyAt,
  formatDate,
  formatDateTime,
  loopStateShortLabel,
  patientName,
} from "@/lib/clinical";

type Chart = {
  patient: {
    id: string;
    facility_id: string;
    family_name: string;
    given_name: string;
    birth_date: string;
    sex: string;
    identifier: string;
  };
  allergies: { substance: string; criticality: string }[];
  medications: { name: string; status: string }[];
  conditions: { code: string; display: string }[];
  observations: {
    id: string;
    code_loinc: string;
    value: string;
    unit: string;
    status: string;
    effective_at: string;
  }[];
  service_requests: {
    id: string;
    code_loinc: string;
    display: string;
    loop_state: string;
    created_at: string;
  }[];
  encounters: {
    id: string;
    status: string;
    started_at: string;
    practitioner: string;
  }[];
  consents: { purpose: string; status: string }[];
  alerts: { severity: string; message: string; created_at: string }[];
};

const TABS: { id: string; key: TKey }[] = [
  { id: "overview", key: "tabOverview" },
  { id: "problems", key: "tabProblems" },
  { id: "medications", key: "tabMedications" },
  { id: "allergies", key: "tabAllergies" },
  { id: "encounters", key: "tabEncounters" },
  { id: "results", key: "tabResults" },
  { id: "consents", key: "tabConsents" },
];

function sexLabel(lang: Lang, sex: string): string {
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

function AllergiesList({ chart, lang }: { chart: Chart; lang: Lang }) {
  if (chart.allergies.length === 0) {
    return <p className="muted">{t(lang, "noKnownAllergies")}</p>;
  }
  return (
    <ul className="result-list">
      {chart.allergies.map((a) => (
        <li
          key={a.substance}
          className={`result-card ${a.criticality === "high" ? "critical" : ""}`}
        >
          <div className="grow title">{a.substance}</div>
          <span
            className={`badge ${a.criticality === "high" ? "critical" : "warn"}`}
          >
            {t(lang, "criticality")}: {a.criticality}
          </span>
        </li>
      ))}
    </ul>
  );
}

function Timeline({ chart, lang }: { chart: Chart; lang: Lang }) {
  const events = useMemo(() => {
    const evts: { when: string; text: string; href?: string }[] = [];
    for (const e of chart.encounters) {
      evts.push({
        when: e.started_at,
        text: `${t(lang, "encounters")}: ${e.practitioner}`,
      });
    }
    for (const sr of chart.service_requests) {
      evts.push({
        when: sr.created_at,
        text: `${sr.display} — ${loopStateShortLabel(lang, sr.loop_state)}`,
        href: `/requests/${sr.id}`,
      });
    }
    for (const o of chart.observations) {
      evts.push({
        when: o.effective_at,
        text: `${t(lang, "result")}: ${o.value} ${o.unit}`,
      });
    }
    return evts.sort((a, b) => (a.when < b.when ? 1 : -1)).slice(0, 20);
  }, [chart, lang]);

  if (events.length === 0) {
    return <p className="muted">{t(lang, "noEncounters")}</p>;
  }
  return (
    <ol className="timeline">
      {events.map((e, i) => (
        <li key={i}>
          <span className="when">{formatDateTime(lang, e.when)}</span>
          <br />
          {e.href ? <Link href={e.href}>{e.text}</Link> : e.text}
        </li>
      ))}
    </ol>
  );
}

function Actions({
  chart,
  lang,
  onChanged,
}: {
  chart: Chart;
  lang: Lang;
  onChanged: () => void;
}) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [success, setSuccess] = useState<string | null>(null);
  const [showOrder, setShowOrder] = useState(false);
  const [test, setTest] = useState(LAB_TESTS[0].code_loinc);
  const [encounterId, setEncounterId] = useState("new");

  async function startEncounter() {
    setBusy(true);
    setError(null);
    setSuccess(null);
    try {
      await apiFetch("/api/v1/encounters", {
        method: "POST",
        body: JSON.stringify({ patient_id: chart.patient.id }),
      });
      setSuccess(t(lang, "encounterStarted"));
      onChanged();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  async function orderLab(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    setSuccess(null);
    try {
      let encId = encounterId;
      if (encId === "new") {
        const enc = await apiFetch<{ id: string }>("/api/v1/encounters", {
          method: "POST",
          body: JSON.stringify({ patient_id: chart.patient.id }),
        });
        encId = enc.id;
        // Reuse this encounter if the order below fails and is retried,
        // instead of creating another empty encounter per attempt.
        setEncounterId(encId);
      }
      const selected = LAB_TESTS.find((x) => x.code_loinc === test);
      if (!selected) return;
      await apiFetch("/api/v1/service-requests", {
        method: "POST",
        body: JSON.stringify({
          encounter_id: encId,
          code_loinc: selected.code_loinc,
          display: selected.display,
        }),
      });
      setSuccess(t(lang, "orderPlaced"));
      setShowOrder(false);
      onChanged();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <div className="card">
      <h2>{t(lang, "quickActions")}</h2>
      {error ? (
        <p role="alert" className="error">
          {error}
        </p>
      ) : null}
      {success ? (
        <p role="status" className="success">
          {success}
        </p>
      ) : null}
      <div style={{ display: "flex", gap: "0.6rem", flexWrap: "wrap" }}>
        <button
          className="primary"
          disabled={busy}
          onClick={() => void startEncounter()}
        >
          {t(lang, "startEncounter")}
        </button>
        <button
          className="secondary"
          disabled={busy}
          aria-expanded={showOrder}
          onClick={() => setShowOrder((s) => !s)}
        >
          {t(lang, "orderLab")}
        </button>
      </div>
      {showOrder ? (
        <form onSubmit={orderLab} style={{ marginTop: "0.6rem" }}>
          <label htmlFor="lab-test">{t(lang, "labTest")}</label>
          <select
            id="lab-test"
            value={test}
            onChange={(e) => setTest(e.target.value)}
          >
            {LAB_TESTS.map((x) => (
              <option key={x.code_loinc} value={x.code_loinc}>
                {x.display}
              </option>
            ))}
          </select>
          <label htmlFor="lab-encounter">{t(lang, "selectEncounter")}</label>
          <select
            id="lab-encounter"
            value={encounterId}
            onChange={(e) => setEncounterId(e.target.value)}
          >
            <option value="new">{t(lang, "newEncounter")}</option>
            {chart.encounters.map((enc) => (
              <option key={enc.id} value={enc.id}>
                {formatDateTime(lang, enc.started_at)} — {enc.practitioner}
              </option>
            ))}
          </select>
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

function PatientWorkspace({ id }: { id: string }) {
  const { lang, authenticated, meta } = useSession();
  const [chart, setChart] = useState<Chart | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [tab, setTab] = useState("overview");

  const load = useCallback(() => {
    setError(null);
    apiFetch<Chart>(`/api/v1/patients/${id}`)
      .then(setChart)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, [id]);

  useEffect(() => {
    if (authenticated) load();
  }, [authenticated, load]);

  if (error) {
    const denied = error.includes("404") || error.toLowerCase().includes("not");
    return (
      <div className="card">
        <p role="alert" className="error">
          {denied ? t(lang, "notAuthorized") : error}
        </p>
        <button className="secondary" onClick={load}>
          {t(lang, "retry")}
        </button>
      </div>
    );
  }
  if (!chart) {
    return (
      <p className="muted" role="status">
        {t(lang, "loading")}
      </p>
    );
  }

  const p = chart.patient;

  return (
    <>
      <div className="card">
        <div className="patient-header">
          <h2>{patientName(p)}</h2>
          <span className="badge neutral">{p.identifier}</span>
          <span className="muted">
            {sexLabel(lang, p.sex)} · {t(lang, "born")}{" "}
            {formatDate(lang, p.birth_date)}
          </span>
        </div>
      </div>

      <div className="card">
        <h2>{t(lang, "safetyInformation")}</h2>
        <h3 style={{ fontSize: "0.95rem", margin: "0.4rem 0" }}>
          {t(lang, "allergies")}
        </h3>
        <AllergiesList chart={chart} lang={lang} />
        <h3 style={{ fontSize: "0.95rem", margin: "0.8rem 0 0.4rem" }}>
          {t(lang, "activeAlerts")}
        </h3>
        {chart.alerts.length === 0 ? (
          <p className="muted">{t(lang, "noActiveAlerts")}</p>
        ) : (
          <ul className="result-list">
            {chart.alerts.map((a, i) => (
              <li key={i} className="result-card critical">
                <div className="grow">
                  <div className="title">{a.message}</div>
                  <div className="muted">
                    {formatDateTime(lang, a.created_at)}
                  </div>
                </div>
                <span className="badge critical">{t(lang, "critical")}</span>
              </li>
            ))}
          </ul>
        )}
      </div>

      {meta &&
      canActClinicallyAt(meta.facilities, chart.patient.facility_id) ? (
        <Actions chart={chart} lang={lang} onChanged={load} />
      ) : null}

      <div className="card">
        <div className="tabs" role="tablist">
          {TABS.map((tb) => (
            <button
              key={tb.id}
              role="tab"
              aria-selected={tab === tb.id}
              onClick={() => setTab(tb.id)}
            >
              {t(lang, tb.key)}
            </button>
          ))}
        </div>

        {tab === "overview" ? (
          <>
            <h3 style={{ fontSize: "0.95rem" }}>
              {t(lang, "clinicalTimeline")}
            </h3>
            <Timeline chart={chart} lang={lang} />
          </>
        ) : null}

        {tab === "problems" ? (
          chart.conditions.length === 0 ? (
            <p className="muted">{t(lang, "noConditions")}</p>
          ) : (
            <ul className="result-list">
              {chart.conditions.map((c) => (
                <li key={c.code} className="result-card">
                  <div className="grow title">{c.display}</div>
                  <span className="badge neutral">
                    {t(lang, "code")}: {c.code}
                  </span>
                </li>
              ))}
            </ul>
          )
        ) : null}

        {tab === "medications" ? (
          chart.medications.length === 0 ? (
            <p className="muted">{t(lang, "noMedications")}</p>
          ) : (
            <ul className="result-list">
              {chart.medications.map((m) => (
                <li key={m.name} className="result-card">
                  <div className="grow title">{m.name}</div>
                  <span className="badge ok">{m.status}</span>
                </li>
              ))}
            </ul>
          )
        ) : null}

        {tab === "allergies" ? (
          <AllergiesList chart={chart} lang={lang} />
        ) : null}

        {tab === "encounters" ? (
          chart.encounters.length === 0 ? (
            <p className="muted">{t(lang, "noEncounters")}</p>
          ) : (
            <ul className="result-list">
              {chart.encounters.map((e) => (
                <li key={e.id} className="result-card">
                  <div className="grow">
                    <div className="title">{e.practitioner}</div>
                    <div className="muted">
                      {t(lang, "started")}: {formatDateTime(lang, e.started_at)}
                    </div>
                  </div>
                  <span className="badge neutral">{e.status}</span>
                </li>
              ))}
            </ul>
          )
        ) : null}

        {tab === "results" ? (
          chart.service_requests.length === 0 ? (
            <p className="muted">{t(lang, "noLabResults")}</p>
          ) : (
            <ul className="result-list">
              {chart.service_requests.map((sr) => (
                <li key={sr.id} className="result-card">
                  <div className="grow">
                    <div className="title">{sr.display}</div>
                    <div className="muted">
                      {t(lang, "ordered")}:{" "}
                      {formatDateTime(lang, sr.created_at)} · LOINC{" "}
                      {sr.code_loinc}
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
          )
        ) : null}

        {tab === "consents" ? (
          chart.consents.length === 0 ? (
            <p className="muted">{t(lang, "noConsents")}</p>
          ) : (
            <ul className="result-list">
              {chart.consents.map((c) => (
                <li key={c.purpose} className="result-card">
                  <div className="grow title">{c.purpose}</div>
                  <span
                    className={`badge ${c.status === "active" ? "ok" : "warn"}`}
                  >
                    {c.status === "active"
                      ? t(lang, "active")
                      : t(lang, "revoked")}
                  </span>
                </li>
              ))}
            </ul>
          )
        ) : null}
      </div>
    </>
  );
}

export default function PatientPage({ params }: { params: { id: string } }) {
  return (
    <AppShell>
      <PatientWorkspace id={params.id} />
    </AppShell>
  );
}
