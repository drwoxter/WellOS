"use client";

import Link from "next/link";
import { useCallback, useEffect, useRef, useState } from "react";
import { AppShell } from "../chrome";
import { t } from "@/lib/i18n";
import type { Lang } from "@/lib/i18n";
import { ApiRequestError, apiFetch, useSession } from "@/lib/session";
import { ageYears, formatDateTime, patientName } from "@/lib/clinical";
import {
  RISK_DOMAINS,
  canReadRisk,
  domainKey,
  factorText,
  gapText,
  levelKey,
  reviewKey,
  trendKey,
} from "@/lib/risk";
import type { RiskLevel, Worklist, WorklistItem } from "@/lib/risk";
import {
  DomainList,
  LevelBadge,
  ReviewBadge,
  RiskActions,
  TrendBadge,
} from "./risk-view";

type Filters = {
  domain: string;
  service: string;
  assignee: string;
  review: string;
  trend: string;
  include_low: boolean;
};

const EMPTY: Filters = {
  domain: "",
  service: "",
  assignee: "",
  review: "",
  trend: "",
  include_low: false,
};

const REVIEW_OPTIONS = ["unreviewed", "acknowledged", "reviewed"] as const;
const TREND_OPTIONS = ["worsening", "stable", "improving", "unknown"] as const;

function query(f: Filters): string {
  const p = new URLSearchParams();
  if (f.domain) p.set("domain", f.domain);
  if (f.service) p.set("service", f.service);
  if (f.assignee) p.set("assignee", f.assignee);
  if (f.review) p.set("review", f.review);
  if (f.trend) p.set("trend", f.trend);
  if (f.include_low) p.set("include_low", "true");
  const s = p.toString();
  return s ? `?${s}` : "";
}

/** One patient on the worklist: who, how severe, why, links, actions. */
function WorklistCard({
  lang,
  item,
  focusDomain,
  professionals,
  onChanged,
}: {
  lang: Lang;
  item: WorklistItem;
  focusDomain: string;
  professionals: { id: string; display_name: string }[];
  onChanged: () => Promise<unknown>;
}) {
  const p = item.patient;
  const headingId = `risk-item-${item.assessment_id}`;
  const insufficient = item.focus_level === "insufficient_data";
  const elevated = (Object.entries(item.domain_levels) as [string, RiskLevel][])
    .filter(([, lvl]) => lvl !== "low")
    .sort(
      (a, b) =>
        RISK_DOMAINS.indexOf(a[0] as (typeof RISK_DOMAINS)[number]) -
        RISK_DOMAINS.indexOf(b[0] as (typeof RISK_DOMAINS)[number]),
    );
  return (
    <li
      className={`result-card risk-item ${item.focus_level === "critical" ? "critical" : ""}`}
      aria-labelledby={headingId}
      data-level={item.focus_level}
    >
      <div className="grow">
        <div className="risk-domain-head">
          <h3 id={headingId} className="title">
            {patientName(p)}
          </h3>
          <span className="muted">
            {p.identifier} · {ageYears(p.birth_date)} {t(lang, "ageYears")} ·{" "}
            {p.facility}
          </span>
        </div>
        <div className="risk-domain-head">
          <LevelBadge lang={lang} level={item.focus_level} />
          {focusDomain !== "overall" ? (
            <span className="muted">{t(lang, domainKey(focusDomain))}</span>
          ) : null}
          <TrendBadge lang={lang} trend={item.trend} />
          <ReviewBadge lang={lang} review={item.review} />
          {item.service ? (
            <span className="badge neutral">{item.service}</span>
          ) : null}
        </div>
        <p className="muted" style={{ margin: "0.2rem 0" }}>
          {t(lang, "riskFollowUpOwner")}:{" "}
          {item.owner?.display_name ?? t(lang, "riskUnassigned")}
          {item.treating_professional
            ? ` · ${t(lang, "riskTreating")}: ${item.treating_professional}`
            : ""}
          {" · "}
          {t(lang, "riskCalculatedAt")}{" "}
          {formatDateTime(lang, item.calculated_at)}
        </p>
        {elevated.length > 0 ? (
          <div
            className="risk-item-domains"
            aria-label={t(lang, "riskDomainsTitle")}
          >
            {elevated.map(([d, lvl]) => (
              <span key={d} className="badge neutral" data-level={lvl}>
                {t(lang, domainKey(d))}: {t(lang, levelKey(lvl))}
              </span>
            ))}
          </div>
        ) : null}
        <section aria-label={t(lang, "riskWhy")}>
          <strong>{t(lang, "riskWhy")}:</strong>
          {insufficient ? (
            <p className="muted">{t(lang, "riskInsufficientItem")}</p>
          ) : item.explained.length === 0 ? (
            <p className="muted">{t(lang, "riskNoFactors")}</p>
          ) : (
            <ul className="risk-factors">
              {item.explained.flatMap((d) => [
                ...d.factors.map((f) => (
                  <li key={`${d.domain}-${f.code}`}>
                    {t(lang, domainKey(d.domain))}: {factorText(lang, f)}
                  </li>
                )),
                ...d.missing_data.map((g) => (
                  <li key={`${d.domain}-m-${g.code}`} className="muted">
                    {t(lang, domainKey(d.domain))}: {gapText(lang, g)}
                  </li>
                )),
              ])}
            </ul>
          )}
        </section>
        <details className="risk-technical">
          <summary>{t(lang, "riskTechnicalEvidence")}</summary>
          <DomainList
            lang={lang}
            domains={item.explained}
            patientId={p.id}
            rulesVersion={item.rules_version}
          />
        </details>
        <div className="risk-item-links">
          {item.capabilities.can_open_360 ? (
            <Link className="navlink" href={`/patients/${p.id}/360`}>
              {t(lang, "openPatient360")}
            </Link>
          ) : (
            <span className="muted">{t(lang, "riskNo360Access")}</span>
          )}
        </div>
        <RiskActions
          lang={lang}
          patientId={p.id}
          assessmentId={item.assessment_id}
          domain={focusDomain}
          review={item.review}
          capabilities={item.capabilities}
          professionals={professionals}
          owner={item.owner}
          onChanged={onChanged}
          idPrefix={`risk-${item.assessment_id}`}
        />
      </div>
    </li>
  );
}

function RiskWorklist() {
  const { lang, authenticated, meta } = useSession();
  const roles = meta?.user.roles ?? [];
  const [filters, setFilters] = useState<Filters>(EMPTY);
  const [data, setData] = useState<Worklist | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [denied, setDenied] = useState(false);
  // Responses from a superseded load are dropped so a slow request cannot
  // overwrite the list for the filters currently shown.
  const generation = useRef(0);

  const load = useCallback(async () => {
    const gen = ++generation.current;
    setError(null);
    try {
      const w = await apiFetch<Worklist>(
        `/api/v1/risk/worklist${query(filters)}`,
      );
      if (gen !== generation.current) return;
      setData(w);
      setDenied(false);
    } catch (err) {
      if (gen !== generation.current) return;
      if (err instanceof ApiRequestError && err.status === 403) {
        setDenied(true);
      } else {
        setError(err instanceof Error ? err.message : String(err));
      }
    }
  }, [filters]);

  const allowed = canReadRisk(roles);

  useEffect(() => {
    if (!authenticated || !meta || !allowed) return;
    setData(null);
    void load();
  }, [authenticated, meta, allowed, load]);

  if (meta && !allowed) {
    return (
      <div className="card">
        <p role="alert" className="error">
          {t(lang, "notAuthorized")}
        </p>
      </div>
    );
  }

  const focusDomain = filters.domain || "overall";
  const set = (patch: Partial<Filters>) =>
    setFilters((f) => ({ ...f, ...patch }));
  const active = Object.values({ ...filters, include_low: "" }).some(Boolean);

  return (
    <>
      <h2 style={{ marginTop: 0 }}>{t(lang, "riskWorklist")}</h2>
      <p className="muted">{t(lang, "riskWorklistHelp")}</p>
      {denied ? (
        <div className="card">
          <p role="alert" className="error">
            {t(lang, "notAuthorized")}
          </p>
        </div>
      ) : (
        <section className="card" aria-labelledby="risk-board-h">
          <h2 id="risk-board-h" className="sr-only">
            {t(lang, "riskWorklist")}
          </h2>
          <form
            className="risk-filters"
            aria-label={t(lang, "riskWorklist")}
            onSubmit={(e) => e.preventDefault()}
          >
            <label htmlFor="risk-f-domain">
              {t(lang, "riskFilterDomain")}
              <select
                id="risk-f-domain"
                value={filters.domain}
                onChange={(e) => set({ domain: e.target.value })}
              >
                <option value="">{t(lang, "riskAllDomains")}</option>
                {(data?.domains ?? RISK_DOMAINS).map((d) => (
                  <option key={d} value={d}>
                    {t(lang, domainKey(d))}
                  </option>
                ))}
              </select>
            </label>
            <label htmlFor="risk-f-service">
              {t(lang, "riskFilterService")}
              <select
                id="risk-f-service"
                value={filters.service}
                onChange={(e) => set({ service: e.target.value })}
              >
                <option value="">{t(lang, "riskAllServices")}</option>
                {(data?.services ?? []).map((s) => (
                  <option key={s} value={s}>
                    {s}
                  </option>
                ))}
              </select>
            </label>
            <label htmlFor="risk-f-assignee">
              {t(lang, "riskFilterAssignee")}
              <select
                id="risk-f-assignee"
                value={filters.assignee}
                onChange={(e) => set({ assignee: e.target.value })}
              >
                <option value="">{t(lang, "riskAnyAssignee")}</option>
                <option value="unassigned">
                  {t(lang, "riskUnassignedOnly")}
                </option>
                {(data?.professionals ?? []).map((p) => (
                  <option key={p.id} value={p.id}>
                    {p.display_name}
                  </option>
                ))}
              </select>
            </label>
            <label htmlFor="risk-f-review">
              {t(lang, "riskFilterReview")}
              <select
                id="risk-f-review"
                value={filters.review}
                onChange={(e) => set({ review: e.target.value })}
              >
                <option value="">{t(lang, "riskAnyReview")}</option>
                {REVIEW_OPTIONS.map((r) => (
                  <option key={r} value={r}>
                    {t(lang, reviewKey(r))}
                  </option>
                ))}
              </select>
            </label>
            <label htmlFor="risk-f-trend">
              {t(lang, "riskFilterTrend")}
              <select
                id="risk-f-trend"
                value={filters.trend}
                onChange={(e) => set({ trend: e.target.value })}
              >
                <option value="">{t(lang, "riskAnyTrend")}</option>
                {TREND_OPTIONS.map((tr) => (
                  <option key={tr} value={tr}>
                    {t(lang, trendKey(tr))}
                  </option>
                ))}
              </select>
            </label>
            <label htmlFor="risk-f-low" className="checkbox">
              <input
                id="risk-f-low"
                type="checkbox"
                checked={filters.include_low}
                onChange={(e) => set({ include_low: e.target.checked })}
              />
              {t(lang, "riskIncludeLow")}
            </label>
            {active ? (
              <button
                type="button"
                className="secondary"
                onClick={() => setFilters(EMPTY)}
              >
                {t(lang, "resetFilters")}
              </button>
            ) : null}
          </form>
          <div aria-live="polite">
            {error ? (
              <div>
                <p role="alert" className="error">
                  {error}
                </p>
                <button
                  type="button"
                  className="secondary"
                  onClick={() => void load()}
                >
                  {t(lang, "retry")}
                </button>
              </div>
            ) : data === null ? (
              <p className="muted" role="status">
                {t(lang, "loading")}
              </p>
            ) : data.items.length === 0 ? (
              <p className="muted" role="status">
                {active || filters.include_low
                  ? t(lang, "riskEmpty")
                  : t(lang, "riskEmptyAll")}
              </p>
            ) : (
              <>
                <p className="muted" role="status">
                  {data.items.length} {t(lang, "riskItemsShown")} ·{" "}
                  {t(lang, "riskRulesVersion")} {data.rules_version}
                </p>
                {data.items.length >= data.limit ? (
                  <p className="muted" role="status">
                    {t(lang, "riskLimitReached").replace(
                      "{n}",
                      String(data.limit),
                    )}
                  </p>
                ) : null}
                <ul className="risk-worklist">
                  {data.items.map((item) => (
                    <WorklistCard
                      key={item.assessment_id}
                      lang={lang}
                      item={item}
                      focusDomain={focusDomain}
                      professionals={data.professionals}
                      onChanged={load}
                    />
                  ))}
                </ul>
              </>
            )}
          </div>
        </section>
      )}
    </>
  );
}

export default function RiskPage() {
  return (
    <AppShell>
      <RiskWorklist />
    </AppShell>
  );
}
