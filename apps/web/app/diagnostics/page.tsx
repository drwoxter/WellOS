"use client";

import Link from "next/link";
import { useState } from "react";
import { AppShell } from "../chrome";
import { t, type Lang } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import {
  NO_DIAGNOSTICS_CAPABILITIES,
  ORDER_STATUSES,
  categoryLabel,
  criticalityLabel,
  criticalityTone,
  fulfilmentModeLabel,
  hasDiagnosticsWorkspaceAccess,
  loadReviewWorklist,
  loadWorklist,
  orderStatusLabel,
  orderStatusTone,
  priorityLabel,
  reportStatusLabel,
  type DiagnosticOrder,
  type DiagnosticReport,
  type ReviewWorklistQuery,
} from "@/lib/diagnostics";
import { PanelState, StatusBadge, useLoader } from "../scheduling/shared";

type Tab = "orders" | "reviews";

function patientLabel(o: {
  patient?: { given_name: string; family_name: string } | null;
}) {
  return o.patient ? `${o.patient.family_name}, ${o.patient.given_name}` : "—";
}

export function OrdersWorklist({
  lang,
  facilityId,
}: {
  lang: Lang;
  facilityId: string;
}) {
  const [status, setStatus] = useState<string>("open");
  const [q, setQ] = useState("");
  const [conflicts, setConflicts] = useState(false);
  const state = useLoader(
    () =>
      loadWorklist({
        status: status === "all" ? undefined : status,
        q: q.trim() || undefined,
        conflicts_only: conflicts || undefined,
        facility_id: facilityId || undefined,
        limit: 100,
      }),
    `${status}:${q}:${conflicts}:${facilityId}`,
  );
  return (
    <section
      aria-labelledby="dx-orders-h"
      className="card"
      data-testid="dx-orders"
    >
      <h3 id="dx-orders-h">{t(lang, "dxOrders")}</h3>
      <div className="filters grid-2">
        <label>
          {t(lang, "status")}
          <select
            value={status}
            onChange={(e) => setStatus(e.target.value)}
            data-testid="dx-status-filter"
          >
            <option value="open">{t(lang, "dxStatusOpen")}</option>
            <option value="all">{t(lang, "all")}</option>
            {ORDER_STATUSES.map((s) => (
              <option key={s} value={s}>
                {orderStatusLabel(lang, s)}
              </option>
            ))}
          </select>
        </label>
        <label>
          {t(lang, "search")}
          <input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            placeholder={t(lang, "dxSearchOrdersPlaceholder")}
            data-testid="dx-search"
          />
        </label>
        <label className="check-option">
          <input
            type="checkbox"
            checked={conflicts}
            onChange={(e) => setConflicts(e.target.checked)}
          />
          {t(lang, "dxConflictsOnly")}
        </label>
      </div>
      <PanelState
        lang={lang}
        state={state}
        emptyKey="dxNoOrders"
        isEmpty={(d) => d.items.length === 0}
      >
        {(d) => (
          <ul className="row-list" data-testid="dx-order-rows">
            {d.items.map((o: DiagnosticOrder) => (
              <li
                key={o.id}
                className={`row-card status-${o.order_status}`}
                data-testid="dx-order-row"
                data-status={o.order_status}
              >
                <div className="row-main">
                  <div className="row-head">
                    <Link
                      className="navlink"
                      href={`/diagnostics/orders/${o.id}`}
                    >
                      {o.display}
                    </Link>
                    <StatusBadge
                      label={orderStatusLabel(lang, o.order_status)}
                      tone={orderStatusTone(o.order_status)}
                    />
                    {o.priority !== "routine" ? (
                      <StatusBadge
                        label={priorityLabel(lang, o.priority)}
                        tone={o.priority === "stat" ? "critical" : "warn"}
                      />
                    ) : null}
                    {o.schedule_conflict ? (
                      <StatusBadge
                        label={t(lang, "dxScheduleConflict")}
                        tone="warn"
                      />
                    ) : null}
                  </div>
                  <p className="muted">
                    {patientLabel(o)} · {categoryLabel(lang, o.category_code)} ·{" "}
                    {fulfilmentModeLabel(lang, o.fulfilment_mode)} ·{" "}
                    {formatDateTime(lang, o.created_at)}
                    {o.latest_report_status
                      ? ` · ${t(lang, "dxReport")}: ${reportStatusLabel(
                          lang,
                          o.latest_report_status,
                        )}`
                      : ""}
                  </p>
                </div>
              </li>
            ))}
            {d.has_more ? (
              <li className="muted">{t(lang, "dxMoreResultsNarrow")}</li>
            ) : null}
          </ul>
        )}
      </PanelState>
    </section>
  );
}

export function ReviewsWorklist({
  lang,
  facilityId,
}: {
  lang: Lang;
  facilityId: string;
}) {
  const [stateFilter, setStateFilter] =
    useState<NonNullable<ReviewWorklistQuery["state"]>>("pending");
  const [criticality, setCriticality] = useState("");
  const state = useLoader(
    () =>
      loadReviewWorklist({
        state: stateFilter,
        criticality: criticality || undefined,
        facility_id: facilityId || undefined,
        limit: 100,
      }),
    `${stateFilter}:${criticality}:${facilityId}`,
  );
  return (
    <section
      aria-labelledby="dx-reviews-h"
      className="card"
      data-testid="dx-reviews"
    >
      <h3 id="dx-reviews-h">{t(lang, "dxReportsToReview")}</h3>
      <div className="filters grid-2">
        <label>
          {t(lang, "status")}
          <select
            value={stateFilter}
            onChange={(e) =>
              setStateFilter(
                e.target.value as NonNullable<ReviewWorklistQuery["state"]>,
              )
            }
            data-testid="dx-review-state"
          >
            <option value="pending">{t(lang, "dxReviewPending")}</option>
            <option value="reviewed">{t(lang, "dxReviewReviewed")}</option>
            <option value="released">{t(lang, "dxReviewReleased")}</option>
            <option value="all">{t(lang, "all")}</option>
          </select>
        </label>
        <label>
          {t(lang, "dxCriticality")}
          <select
            value={criticality}
            onChange={(e) => setCriticality(e.target.value)}
          >
            <option value="">{t(lang, "all")}</option>
            <option value="critical">
              {criticalityLabel(lang, "critical")}
            </option>
            <option value="abnormal">
              {criticalityLabel(lang, "abnormal")}
            </option>
            <option value="normal">{criticalityLabel(lang, "normal")}</option>
          </select>
        </label>
      </div>
      <PanelState
        lang={lang}
        state={state}
        emptyKey="dxNoReportsToReview"
        isEmpty={(d) => d.items.length === 0}
      >
        {(d) => (
          <ul className="row-list" data-testid="dx-review-rows">
            {d.items.map((r: DiagnosticReport) => (
              <li
                key={r.id}
                className={`row-card criticality-${r.criticality}`}
                data-testid="dx-review-row"
                data-criticality={r.criticality}
              >
                <div className="row-main">
                  <div className="row-head">
                    <Link
                      className="navlink"
                      href={`/diagnostics/reports/${r.id}`}
                    >
                      {r.order_display ?? t(lang, "dxReport")} · v{r.version}
                    </Link>
                    <StatusBadge
                      label={criticalityLabel(lang, r.criticality)}
                      tone={criticalityTone(r.criticality)}
                    />
                    <StatusBadge
                      label={reportStatusLabel(lang, r.status)}
                      tone="neutral"
                    />
                    {r.released ? (
                      <StatusBadge label={t(lang, "dxReleased")} tone="ok" />
                    ) : r.reviewed ? (
                      <StatusBadge
                        label={t(lang, "dxReviewReviewed")}
                        tone="ok"
                      />
                    ) : null}
                  </div>
                  <p className="muted">
                    {patientLabel(r)} ·{" "}
                    {r.priority ? priorityLabel(lang, r.priority) : ""} ·{" "}
                    {formatDateTime(lang, r.issued_at ?? r.created_at)}
                  </p>
                </div>
              </li>
            ))}
          </ul>
        )}
      </PanelState>
    </section>
  );
}

export default function DiagnosticsPage() {
  const { lang, meta, authenticated } = useSession();
  const caps = meta?.diagnostics_capabilities ?? NO_DIAGNOSTICS_CAPABILITIES;
  const [tab, setTab] = useState<Tab>("orders");
  const [facilityId, setFacilityId] = useState("");
  const tabs: Tab[] = caps.can_review ? ["orders", "reviews"] : ["orders"];
  const facilities = meta?.facilities ?? [];
  return (
    <AppShell>
      <div className="diagnostics-page" data-testid="diagnostics-page">
        <header className="page-head">
          <div>
            <h2 style={{ marginBottom: 4 }}>{t(lang, "navDiagnostics")}</h2>
            <p className="muted">{t(lang, "dxWorkspaceIntro")}</p>
          </div>
          <div className="row-actions">
            {facilities.length > 1 ? (
              <label>
                {t(lang, "facility")}
                <select
                  value={facilityId}
                  onChange={(e) => setFacilityId(e.target.value)}
                  data-testid="dx-facility-filter"
                >
                  <option value="">{t(lang, "all")}</option>
                  {facilities.map((f) => (
                    <option key={f.id} value={f.id}>
                      {f.name}
                    </option>
                  ))}
                </select>
              </label>
            ) : null}
            {caps.can_manage_catalog ? (
              <Link className="secondary button" href="/diagnostics/catalog">
                {t(lang, "dxCatalogAdmin")}
              </Link>
            ) : null}
          </div>
        </header>
        {authenticated === false ? (
          <p className="advisory" role="status">
            {t(lang, "signInRequired")}
          </p>
        ) : meta && !hasDiagnosticsWorkspaceAccess(caps) ? (
          <p role="alert" className="error" data-testid="dx-unauthorized">
            {t(lang, "unauthorizedPanel")}
          </p>
        ) : meta ? (
          <>
            {tabs.length > 1 ? (
              <div
                className="tabs"
                role="tablist"
                aria-label={t(lang, "navDiagnostics")}
              >
                {tabs.map((tb, i) => (
                  <button
                    key={tb}
                    type="button"
                    role="tab"
                    id={`dx-tab-${tb}`}
                    aria-selected={tab === tb}
                    aria-controls={`dx-panel-${tb}`}
                    tabIndex={tab === tb ? 0 : -1}
                    className={tab === tb ? "tab active" : "tab"}
                    onClick={() => setTab(tb)}
                    onKeyDown={(e) => {
                      if (e.key !== "ArrowRight" && e.key !== "ArrowLeft")
                        return;
                      e.preventDefault();
                      const n =
                        (i + (e.key === "ArrowRight" ? 1 : -1) + tabs.length) %
                        tabs.length;
                      setTab(tabs[n]);
                      document.getElementById(`dx-tab-${tabs[n]}`)?.focus();
                    }}
                  >
                    {tb === "orders"
                      ? t(lang, "dxOrders")
                      : t(lang, "dxReportsToReview")}
                  </button>
                ))}
              </div>
            ) : null}
            <div
              id={`dx-panel-${tab}`}
              role={tabs.length > 1 ? "tabpanel" : undefined}
              aria-labelledby={tabs.length > 1 ? `dx-tab-${tab}` : undefined}
            >
              {tab === "orders" ? (
                <OrdersWorklist lang={lang} facilityId={facilityId} />
              ) : (
                <ReviewsWorklist lang={lang} facilityId={facilityId} />
              )}
            </div>
          </>
        ) : (
          <p className="muted" role="status">
            {t(lang, "loading")}
          </p>
        )}
      </div>
    </AppShell>
  );
}
