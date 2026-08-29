"use client";

import Link from "next/link";
import { useCallback, useEffect, useState } from "react";
import { useParams } from "next/navigation";
import { AppHeader } from "../../chrome";
import { t } from "@/lib/i18n";
import { apiFetch, useSession } from "@/lib/session";

type Detail = {
  service_request: {
    id: string;
    display: string;
    code_loinc: string;
    loop_state: string;
    version: number;
    patient: {
      id: string;
      family_name: string;
      given_name: string;
      identifier: string;
    };
  };
  observations: {
    id: string;
    value: string;
    unit: string;
    status: string;
    amends: string | null;
    effective_at: string;
  }[];
  rule_evaluations: {
    rule_id: string;
    rule_version: string;
    outcome: { kind?: string } & Record<string, unknown>;
    evaluated_at: string;
  }[];
  ai_artifacts: {
    id: string;
    status: string;
    autonomy_level: string;
    model: string | null;
    output: {
      summary?: string;
      cited_sources?: string[];
      limitations?: string[];
      suggested_next_step_categories?: string[];
    } | null;
  }[];
  alerts: { id: string; severity: string; message: string; status: string }[];
  data_quality_issues: { issue: string; created_at: string }[];
};

export default function RequestDetailPage() {
  const { token, lang } = useSession();
  const params = useParams<{ id: string }>();
  const [data, setData] = useState<Detail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [note, setNote] = useState("");
  const [busy, setBusy] = useState(false);

  const load = useCallback(() => {
    if (!token) return;
    apiFetch<Detail>(token, `/api/v1/service-requests/${params.id}`)
      .then(setData)
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, [token, params.id]);

  useEffect(load, [load]);

  async function transition(kind: "review" | "notify" | "close") {
    if (!token || !data) return;
    setBusy(true);
    setError(null);
    try {
      await apiFetch(token, `/api/v1/service-requests/${params.id}/${kind}`, {
        method: "POST",
        body: JSON.stringify({
          version: data.service_request.version,
          note: note || null,
        }),
      });
      setNote("");
      load();
    } catch (e) {
      setError(e instanceof Error ? e.message : String(e));
    } finally {
      setBusy(false);
    }
  }

  const sr = data?.service_request;
  const artifact = data?.ai_artifacts[data.ai_artifacts.length - 1];

  return (
    <>
      <AppHeader subtitle={sr?.display} />
      <main>
        <p>
          <Link href="/worklist">← {t(lang, "backToWorklist")}</Link>
        </p>
        {error ? (
          <p role="alert" className="error">
            {error}
          </p>
        ) : null}
        {!data || !sr ? (
          <p className="muted">{t(lang, "loading")}</p>
        ) : (
          <>
            <div className="card">
              <h2>
                {sr.patient.family_name}, {sr.patient.given_name} (
                {sr.patient.identifier}) — {sr.display} [{sr.code_loinc}]
              </h2>
              <p>
                {t(lang, "loopState")}:{" "}
                <span className="badge">{sr.loop_state}</span>
              </p>
              {data.alerts
                .filter((a) => a.status === "open")
                .map((a) => (
                  <p key={a.id} role="alert" className="error">
                    {a.severity.toUpperCase()}: {a.message}
                  </p>
                ))}
              {data.data_quality_issues.map((d, i) => (
                <p key={i} className="error">
                  {d.issue}
                </p>
              ))}
            </div>

            <div className="card">
              <h2>{t(lang, "observations")}</h2>
              <table>
                <thead>
                  <tr>
                    <th scope="col">{t(lang, "value")}</th>
                    <th scope="col">{t(lang, "unit")}</th>
                    <th scope="col">{t(lang, "state")}</th>
                  </tr>
                </thead>
                <tbody>
                  {data.observations.map((o) => (
                    <tr key={o.id}>
                      <td>{o.value}</td>
                      <td>{o.unit}</td>
                      <td>
                        <span className="badge">
                          {o.status}
                          {o.amends ? ` (${t(lang, "amended")})` : ""}
                        </span>
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>

            <div className="card">
              <h2>{t(lang, "ruleEvaluations")}</h2>
              <ul>
                {data.rule_evaluations.map((r, i) => (
                  <li key={i}>
                    {r.rule_id}@{r.rule_version}:{" "}
                    {String(r.outcome.kind ?? JSON.stringify(r.outcome))}
                  </li>
                ))}
              </ul>
            </div>

            <div className="card">
              <h2>{t(lang, "aiSummary")}</h2>
              <p className="muted">{t(lang, "aiDisclaimer")}</p>
              {!artifact || artifact.status === "unavailable" ? (
                <p>{t(lang, "aiUnavailable")}</p>
              ) : (
                <>
                  <p>
                    <span className="badge">{artifact.status}</span>{" "}
                    <span className="badge">{artifact.autonomy_level}</span>{" "}
                    <span className="muted">{artifact.model}</span>
                  </p>
                  <p>{artifact.output?.summary}</p>
                  <h3>{t(lang, "citations")}</h3>
                  <ul>
                    {(artifact.output?.cited_sources ?? []).map((c) => (
                      <li key={c}>{c}</li>
                    ))}
                  </ul>
                  <h3>{t(lang, "limitations")}</h3>
                  <ul>
                    {(artifact.output?.limitations ?? []).map((c) => (
                      <li key={c}>{c}</li>
                    ))}
                  </ul>
                  <h3>{t(lang, "nextSteps")}</h3>
                  <ul>
                    {(
                      artifact.output?.suggested_next_step_categories ?? []
                    ).map((c) => (
                      <li key={c}>{c}</li>
                    ))}
                  </ul>
                </>
              )}
            </div>

            <div className="card">
              <label htmlFor="note">
                {sr.loop_state === "received"
                  ? t(lang, "reviewNote")
                  : sr.loop_state === "reviewed"
                    ? t(lang, "notifyNote")
                    : t(lang, "closeNote")}
              </label>
              <textarea
                id="note"
                value={note}
                onChange={(e) => setNote(e.target.value)}
                rows={2}
              />
              <p>
                {sr.loop_state === "received" ? (
                  <button
                    className="primary"
                    disabled={busy}
                    onClick={() => transition("review")}
                  >
                    {t(lang, "review")}
                  </button>
                ) : sr.loop_state === "reviewed" ? (
                  <button
                    className="primary"
                    disabled={busy}
                    onClick={() => transition("notify")}
                  >
                    {t(lang, "notify")}
                  </button>
                ) : sr.loop_state === "notified" ? (
                  <button
                    className="primary"
                    disabled={busy}
                    onClick={() => transition("close")}
                  >
                    {t(lang, "close")}
                  </button>
                ) : null}
              </p>
            </div>
          </>
        )}
      </main>
    </>
  );
}
