"use client";

import Link from "next/link";
import { useCallback, useEffect, useMemo, useState } from "react";
import { AppShell } from "../chrome";
import { t } from "@/lib/i18n";
import { apiFetch, useSession } from "@/lib/session";
import {
  formatDateTime,
  loopStateShortLabel,
  patientName,
} from "@/lib/clinical";

type WorklistItem = {
  id: string;
  display: string;
  code_loinc: string;
  loop_state: string;
  has_open_alert: boolean;
  created_at: string;
  patient: { family_name: string; given_name: string; identifier: string };
};

function ResultsContent() {
  const { lang, authenticated } = useSession();
  const [items, setItems] = useState<WorklistItem[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [criticality, setCriticality] = useState<"all" | "critical">("all");
  const [state, setState] = useState("all");
  const [query, setQuery] = useState("");

  const load = useCallback(() => {
    setError(null);
    apiFetch<{ items: WorklistItem[] }>("/api/v1/worklist")
      .then((d) => setItems(d.items))
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, []);

  useEffect(() => {
    if (authenticated) load();
  }, [authenticated, load]);

  const filtered = useMemo(() => {
    if (!items) return null;
    const q = query.trim().toLowerCase();
    return items.filter((item) => {
      if (criticality === "critical" && !item.has_open_alert) return false;
      if (state !== "all" && item.loop_state !== state) return false;
      if (q) {
        const hay =
          `${item.patient.given_name} ${item.patient.family_name} ${item.patient.identifier}`.toLowerCase();
        if (!hay.includes(q)) return false;
      }
      return true;
    });
  }, [items, criticality, state, query]);

  const hasFilters = criticality !== "all" || state !== "all" || query !== "";

  function resetFilters() {
    setCriticality("all");
    setState("all");
    setQuery("");
  }

  if (error) {
    return (
      <div className="card">
        <p role="alert" className="error">
          {error}
        </p>
        <button className="secondary" onClick={load}>
          {t(lang, "retry")}
        </button>
      </div>
    );
  }
  if (!filtered) {
    return (
      <p className="muted" role="status">
        {t(lang, "loading")}
      </p>
    );
  }

  return (
    <>
      <h2 style={{ marginTop: 0 }}>{t(lang, "resultsTitle")}</h2>
      <div className="card">
        <div className="filters">
          <div>
            <label htmlFor="f-criticality">
              {t(lang, "filterCriticality")}
            </label>
            <select
              id="f-criticality"
              value={criticality}
              onChange={(e) =>
                setCriticality(e.target.value as "all" | "critical")
              }
            >
              <option value="all">{t(lang, "filterAll")}</option>
              <option value="critical">{t(lang, "filterCritical")}</option>
            </select>
          </div>
          <div>
            <label htmlFor="f-state">{t(lang, "filterState")}</label>
            <select
              id="f-state"
              value={state}
              onChange={(e) => setState(e.target.value)}
            >
              <option value="all">{t(lang, "filterAll")}</option>
              <option value="ordered">{t(lang, "shortOrdered")}</option>
              <option value="received">{t(lang, "shortReceived")}</option>
              <option value="reviewed">{t(lang, "shortReviewed")}</option>
              <option value="notified">{t(lang, "shortNotified")}</option>
            </select>
          </div>
          <div>
            <label htmlFor="f-query">{t(lang, "searchByPatient")}</label>
            <input
              id="f-query"
              value={query}
              onChange={(e) => setQuery(e.target.value)}
              autoComplete="off"
            />
          </div>
          <div style={{ flex: "0 0 auto", minWidth: "auto" }}>
            <button
              className="secondary"
              onClick={resetFilters}
              disabled={!hasFilters}
            >
              {t(lang, "resetFilters")}
            </button>
          </div>
        </div>

        {filtered.length === 0 ? (
          <p className="muted">
            {hasFilters
              ? t(lang, "noMatchingResults")
              : t(lang, "noOpenResults")}
          </p>
        ) : (
          <>
            <div className="table-wrap desktop-only">
              <table>
                <thead>
                  <tr>
                    <th scope="col">{t(lang, "patient")}</th>
                    <th scope="col">{t(lang, "result")}</th>
                    <th scope="col">{t(lang, "criticality")}</th>
                    <th scope="col">{t(lang, "status")}</th>
                    <th scope="col">{t(lang, "ordered")}</th>
                    <th scope="col">
                      <span className="sr-only">{t(lang, "openResult")}</span>
                    </th>
                  </tr>
                </thead>
                <tbody>
                  {filtered.map((item) => (
                    <tr key={item.id}>
                      <td>
                        <strong>{patientName(item.patient)}</strong>
                        <br />
                        <span className="muted">{item.patient.identifier}</span>
                      </td>
                      <td>
                        {item.display}
                        <br />
                        <span className="muted">LOINC {item.code_loinc}</span>
                      </td>
                      <td>
                        {item.has_open_alert ? (
                          <span className="badge critical">
                            {t(lang, "critical")}
                          </span>
                        ) : (
                          <span className="badge ok">{t(lang, "routine")}</span>
                        )}
                      </td>
                      <td>
                        <span className="badge neutral">
                          {loopStateShortLabel(lang, item.loop_state)}
                        </span>
                      </td>
                      <td>{formatDateTime(lang, item.created_at)}</td>
                      <td>
                        <Link href={`/requests/${item.id}`}>
                          {t(lang, "openResult")}
                        </Link>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
            <ul className="result-list mobile-only">
              {filtered.map((item) => (
                <li
                  key={item.id}
                  className={`result-card ${item.has_open_alert ? "critical" : "routine"}`}
                >
                  <div className="grow">
                    <div className="title">{patientName(item.patient)}</div>
                    <div className="muted">
                      {item.display} · {item.patient.identifier}
                    </div>
                    <div className="muted">
                      {formatDateTime(lang, item.created_at)}
                    </div>
                  </div>
                  {item.has_open_alert ? (
                    <span className="badge critical">
                      {t(lang, "critical")}
                    </span>
                  ) : (
                    <span className="badge ok">{t(lang, "routine")}</span>
                  )}
                  <span className="badge neutral">
                    {loopStateShortLabel(lang, item.loop_state)}
                  </span>
                  <Link className="navlink" href={`/requests/${item.id}`}>
                    {t(lang, "openResult")}
                  </Link>
                </li>
              ))}
            </ul>
          </>
        )}
      </div>
    </>
  );
}

export default function ResultsPage() {
  return (
    <AppShell>
      <ResultsContent />
    </AppShell>
  );
}
