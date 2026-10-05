"use client";

import { useEffect, useMemo, useState } from "react";
import { AppShell } from "../../chrome";
import { t } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import { formatDate, formatDateTime } from "@/lib/clinical";
import {
  criticalityLabel,
  criticalityTone,
  explanationFor,
  fulfilmentModeLabel,
  loadMyDiagnostics,
  myDocumentDownloadPath,
  orderStatusLabel,
  orderStatusTone,
  preparationFor,
  type MyDiagnostics,
} from "@/lib/diagnostics";
import { PanelState, StatusBadge, useLoader } from "../../scheduling/shared";
import { apiFetch } from "@/lib/session";
import type { Me } from "@/lib/access";

export default function MyDiagnosticsPage() {
  const { lang, meta, authenticated } = useSession();
  const selfService = meta?.diagnostics_capabilities?.self_service ?? false;
  const me = useLoader(
    () => apiFetch<Me>("/api/v1/me"),
    "me",
    authenticated === true && selfService,
  );
  const [patientId, setPatientId] = useState<string | null>(null);
  const patients = useMemo(() => me.data?.patients ?? [], [me.data]);
  useEffect(() => {
    if (!patientId && patients.length > 0) setPatientId(patients[0].patient_id);
    if (patientId && !patients.some((p) => p.patient_id === patientId)) {
      setPatientId(patients[0]?.patient_id ?? null);
    }
  }, [patients, patientId]);
  const dx = useLoader(
    () => loadMyDiagnostics(patientId ?? undefined),
    `${patientId}`,
    authenticated === true && selfService && patientId !== null,
  );
  const current = patients.find((p) => p.patient_id === patientId);

  return (
    <AppShell>
      <div className="my-diagnostics" data-testid="my-diagnostics">
        {authenticated === null ||
        (authenticated && selfService && me.loading) ? (
          <p className="muted" role="status">
            {t(lang, "loading")}
          </p>
        ) : !authenticated ? (
          <p className="advisory" role="status">
            {t(lang, "signInRequired")}
          </p>
        ) : !selfService || me.denied ? (
          <div className="card" data-testid="self-service-unauthorized">
            <h2>{t(lang, "navMyDiagnostics")}</h2>
            <p role="status">{t(lang, "selfServiceUnauthorized")}</p>
          </div>
        ) : me.error ? (
          <p className="error" role="alert">
            {me.error}
          </p>
        ) : patients.length === 0 ? (
          <div className="card" data-testid="no-grants">
            <h2>{t(lang, "navMyDiagnostics")}</h2>
            <p role="status">{t(lang, "noPatientGrants")}</p>
          </div>
        ) : (
          <>
            <header className="page-head">
              <div>
                <h2 style={{ marginBottom: 4 }}>
                  {t(lang, "navMyDiagnostics")}
                </h2>
                <p className="muted">{t(lang, "myDiagnosticsIntro")}</p>
              </div>
              {patients.length > 1 ? (
                <div>
                  <label htmlFor="me-dx-patient">
                    {t(lang, "managingFor")}
                  </label>
                  <select
                    id="me-dx-patient"
                    value={patientId ?? ""}
                    onChange={(e) => setPatientId(e.target.value)}
                    data-testid="patient-switcher"
                  >
                    {patients.map((p) => (
                      <option key={p.patient_id} value={p.patient_id}>
                        {p.patient.given_name} {p.patient.family_name}
                      </option>
                    ))}
                  </select>
                </div>
              ) : current && current.relationship !== "self" ? (
                <p className="muted" data-testid="managing-for">
                  {t(lang, "managingFor")}: {current.patient.given_name}{" "}
                  {current.patient.family_name}
                </p>
              ) : null}
            </header>
            {meta?.environment?.synthetic_data ? (
              <p className="advisory synthetic-notice" role="status">
                {t(lang, "syntheticDataNotice")}
              </p>
            ) : null}
            <p className="advisory" role="note" data-testid="my-dx-disclaimer">
              {t(lang, "myDiagnosticsDisclaimer")}
            </p>
            <PanelState
              lang={lang}
              state={dx}
              emptyKey="myDiagnosticsEmpty"
              isEmpty={(d: MyDiagnostics) =>
                d.released.length === 0 &&
                d.pending.length === 0 &&
                d.under_review.length === 0
              }
            >
              {(d) => (
                <>
                  <section className="card" aria-labelledby="my-dx-pending-h">
                    <h3 id="my-dx-pending-h">
                      {t(lang, "myDiagnosticsPending")}
                    </h3>
                    {d.pending.length === 0 && d.under_review.length === 0 ? (
                      <p className="muted">
                        {t(lang, "myDiagnosticsNoPending")}
                      </p>
                    ) : (
                      <ul className="row-list" data-testid="my-dx-pending">
                        {d.pending.map((p) => (
                          <li
                            key={p.service_request_id}
                            className="row-card"
                            data-testid="my-dx-pending-row"
                          >
                            <div className="row-main">
                              <div className="row-head">
                                <strong>{p.order_display}</strong>
                                <StatusBadge
                                  label={orderStatusLabel(lang, p.status)}
                                  tone={orderStatusTone(p.status)}
                                />
                              </div>
                              <p className="muted">
                                {p.starts_at
                                  ? `${formatDateTime(lang, p.starts_at)} · ${
                                      p.facility_name ?? ""
                                    }`
                                  : fulfilmentModeLabel(
                                      lang,
                                      p.fulfilment_mode,
                                    )}
                              </p>
                              {preparationFor(lang, p) ? (
                                <p className="advisory">
                                  <strong>{t(lang, "dxPreparation")}:</strong>{" "}
                                  {preparationFor(lang, p)}
                                </p>
                              ) : null}
                            </div>
                          </li>
                        ))}
                        {d.under_review.map((u) => (
                          <li
                            key={u.service_request_id}
                            className="row-card"
                            data-testid="my-dx-under-review"
                          >
                            <div className="row-main">
                              <div className="row-head">
                                <strong>{u.order_display}</strong>
                                <StatusBadge
                                  label={t(lang, "myDiagnosticsUnderReview")}
                                  tone="neutral"
                                />
                              </div>
                              <p className="muted">
                                {t(lang, "myDiagnosticsUnderReviewHint")}
                              </p>
                            </div>
                          </li>
                        ))}
                      </ul>
                    )}
                  </section>
                  <section className="card" aria-labelledby="my-dx-released-h">
                    <h3 id="my-dx-released-h">
                      {t(lang, "myDiagnosticsReleased")}
                    </h3>
                    {d.released.length === 0 ? (
                      <p className="muted">
                        {t(lang, "myDiagnosticsNoReleased")}
                      </p>
                    ) : (
                      <ul className="row-list" data-testid="my-dx-released">
                        {d.released.map((r) => {
                          const explanation = explanationFor(lang, r);
                          return (
                            <li
                              key={r.id}
                              className="row-card"
                              data-testid="my-dx-released-row"
                            >
                              <div className="row-main">
                                <div className="row-head">
                                  <strong>{r.order_display}</strong>
                                  <StatusBadge
                                    label={criticalityLabel(
                                      lang,
                                      r.criticality,
                                    )}
                                    tone={criticalityTone(r.criticality)}
                                  />
                                </div>
                                <p className="muted">
                                  {t(lang, "myDiagnosticsReleasedOn")}{" "}
                                  {formatDate(lang, r.released_at)}
                                  {r.effective_at
                                    ? ` · ${t(lang, "dxSampleTaken")} ${formatDate(lang, r.effective_at)}`
                                    : ""}
                                </p>
                                {r.conclusion ? <p>{r.conclusion}</p> : null}
                                {explanation ? (
                                  <p
                                    className="dx-explanation"
                                    data-testid="my-dx-explanation"
                                  >
                                    {explanation}
                                  </p>
                                ) : null}
                                {r.documents && r.documents.length > 0 ? (
                                  <ul className="brief-list">
                                    {r.documents.map((doc) => (
                                      <li key={doc.id}>
                                        <a
                                          className="navlink"
                                          href={myDocumentDownloadPath(
                                            r.id,
                                            doc.id,
                                            patientId ?? undefined,
                                          )}
                                        >
                                          {doc.title}
                                        </a>
                                      </li>
                                    ))}
                                  </ul>
                                ) : null}
                              </div>
                            </li>
                          );
                        })}
                      </ul>
                    )}
                  </section>
                </>
              )}
            </PanelState>
          </>
        )}
      </div>
    </AppShell>
  );
}
