"use client";

import Link from "next/link";
import { useCallback, useEffect, useRef, useState } from "react";
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
  can_open_detail: boolean;
  patient: { family_name: string; given_name: string; identifier: string };
};

function ResultsContent() {
  const { lang, authenticated } = useSession();
  const [items, setItems] = useState<WorklistItem[] | null>(null);
  const [hasMore, setHasMore] = useState(false);
  const [loadingMore, setLoadingMore] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [criticality, setCriticality] = useState<"all" | "critical">("all");
  const [state, setState] = useState("all");
  const [query, setQuery] = useState("");
  const [debouncedQuery, setDebouncedQuery] = useState("");

  useEffect(() => {
    const handle = setTimeout(() => setDebouncedQuery(query.trim()), 300);
    return () => clearTimeout(handle);
  }, [query]);

  // Filters and paging run in the API so every matching open result stays
  // reachable, not just the newest rows. Pages continue from the keyset
  // cursor the API returned, so live changes never shift page boundaries.
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  // Responses are committed only when they belong to the latest filter set,
  // so a slow response for stale filters can never replace current results.
  const requestGeneration = useRef(0);

  const buildUrl = useCallback(
    (cursor: string | null) => {
      const params = new URLSearchParams();
      if (criticality === "critical") params.set("critical", "true");
      if (state !== "all") params.set("state", state);
      if (debouncedQuery) params.set("query", debouncedQuery);
      if (cursor) params.set("cursor", cursor);
      const qs = params.toString();
      return `/api/v1/worklist${qs ? `?${qs}` : ""}`;
    },
    [criticality, state, debouncedQuery],
  );

  type WorklistPage = {
    items: WorklistItem[];
    has_more?: boolean;
    next_cursor?: string | null;
  };

  const load = useCallback(() => {
    const generation = ++requestGeneration.current;
    setError(null);
    setLoadingMore(false);
    setItems(null);
    apiFetch<WorklistPage>(buildUrl(null))
      .then((d) => {
        if (requestGeneration.current !== generation) return;
        setItems(d.items);
        setHasMore(d.has_more === true);
        setNextCursor(d.next_cursor ?? null);
      })
      .catch((e) => {
        if (requestGeneration.current !== generation) return;
        setError(e instanceof Error ? e.message : String(e));
      });
  }, [buildUrl]);

  const loadMore = useCallback(() => {
    if (!items || loadingMore || !nextCursor) return;
    const generation = requestGeneration.current;
    setLoadingMore(true);
    apiFetch<WorklistPage>(buildUrl(nextCursor))
      .then((d) => {
        if (requestGeneration.current !== generation) return;
        setItems((prev) => [...(prev ?? []), ...d.items]);
        setHasMore(d.has_more === true);
        setNextCursor(d.next_cursor ?? null);
      })
      .catch((e) => {
        if (requestGeneration.current !== generation) return;
        setError(e instanceof Error ? e.message : String(e));
      })
      .finally(() => {
        if (requestGeneration.current === generation) setLoadingMore(false);
      });
  }, [buildUrl, items, loadingMore, nextCursor]);

  useEffect(() => {
    if (authenticated) load();
  }, [authenticated, load]);

  const filtered = items;

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

        {!filtered ? (
          <p className="muted" role="status">
            {t(lang, "loading")}
          </p>
        ) : filtered.length === 0 ? (
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
                        {item.can_open_detail ? (
                          <Link href={`/requests/${item.id}`}>
                            {t(lang, "openResult")}
                          </Link>
                        ) : null}
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
                  {item.can_open_detail ? (
                    <Link className="navlink" href={`/requests/${item.id}`}>
                      {t(lang, "openResult")}
                    </Link>
                  ) : null}
                </li>
              ))}
            </ul>
            {hasMore ? (
              <p style={{ textAlign: "center", marginBottom: 0 }}>
                <button
                  className="secondary"
                  onClick={loadMore}
                  disabled={loadingMore}
                >
                  {loadingMore ? t(lang, "loading") : t(lang, "loadMore")}
                </button>
              </p>
            ) : null}
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
