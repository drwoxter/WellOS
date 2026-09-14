"use client";

import Link from "next/link";
import { Component, useCallback, useEffect, useRef, useState } from "react";
import type { ReactNode } from "react";
import { t } from "@/lib/i18n";
import type { Lang } from "@/lib/i18n";
import { ApiRequestError, apiFetch } from "@/lib/session";
import type { AiCapabilities } from "@/lib/capabilities";
import type { RiskSection } from "@/lib/risk";
import {
  DomainList,
  RiskOverviewHeader,
  RiskSummaryPanel,
  errorText,
} from "./risk-view";

type CockpitRiskProps = {
  lang: Lang;
  patientId: string;
  refreshKey: string;
  capabilities: AiCapabilities | undefined;
};

/** Patient 360 summary shown inside the consultation cockpit.
 *
 *  Read-only with respect to the consultation: it never touches the note or
 *  the workspace state. `refreshKey` changes when a confirmed clinical change
 *  (vitals, diagnosis, signing) has been recorded, which re-reads the risk
 *  assessment recalculated by the server. A rendering failure inside the panel
 *  is contained here so it can never take the active consultation down. */
export class CockpitRisk extends Component<
  CockpitRiskProps,
  { failed: boolean }
> {
  state = { failed: false };

  static getDerivedStateFromError(): { failed: boolean } {
    return { failed: true };
  }

  componentDidUpdate(prev: CockpitRiskProps) {
    if (prev.refreshKey !== this.props.refreshKey && this.state.failed) {
      this.setState({ failed: false });
    }
  }

  render(): ReactNode {
    if (this.state.failed) {
      return (
        <section className="card cockpit-360" aria-labelledby="cockpit-360-h">
          <h2 id="cockpit-360-h">{t(this.props.lang, "riskCockpitTitle")}</h2>
          <p role="alert" className="error">
            {t(this.props.lang, "riskCockpitUnavailable")}
          </p>
        </section>
      );
    }
    return <CockpitRiskPanel {...this.props} />;
  }
}

function CockpitRiskPanel({
  lang,
  patientId,
  refreshKey,
  capabilities,
}: CockpitRiskProps) {
  const [risk, setRisk] = useState<RiskSection | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [denied, setDenied] = useState(false);
  const [updated, setUpdated] = useState(false);
  const gen = useRef(0);
  const lastCalculated = useRef<string | null>(null);
  const [open, setOpen] = useState(true);

  const load = useCallback(async () => {
    const g = ++gen.current;
    setError(null);
    try {
      const r = await apiFetch<RiskSection>(
        `/api/v1/patients/${patientId}/risk`,
      );
      if (g !== gen.current) return;
      const calc = r.current?.calculated_at ?? null;
      if (lastCalculated.current !== null && calc !== lastCalculated.current) {
        setUpdated(true);
      }
      lastCalculated.current = calc;
      setRisk(r);
      setDenied(false);
    } catch (err) {
      if (g !== gen.current) return;
      if (err instanceof ApiRequestError && err.status === 403) {
        setDenied(true);
      } else {
        setError(errorText(lang, err));
      }
    }
  }, [lang, patientId]);

  useEffect(() => {
    void load();
    // `refreshKey` intentionally re-runs the read after confirmed changes.
  }, [load, refreshKey]);

  if (denied) return null;

  const elevated =
    risk?.current?.domains.filter((d) => d.level !== "low") ?? [];

  return (
    <section className="card cockpit-360" aria-labelledby="cockpit-360-h">
      <div className="risk-domain-head">
        <h2 id="cockpit-360-h">{t(lang, "riskCockpitTitle")}</h2>
        <button
          type="button"
          className="secondary"
          aria-expanded={open}
          aria-controls="cockpit-360-body"
          onClick={() => setOpen((o) => !o)}
        >
          {open ? t(lang, "hideSection") : t(lang, "showSection")}
        </button>
        <Link className="navlink" href={`/patients/${patientId}/360`}>
          {t(lang, "openPatient360")}
        </Link>
      </div>
      <p className="muted">{t(lang, "riskCockpitHelp")}</p>
      {updated ? (
        <p role="status" className="success">
          {t(lang, "riskCockpitUpdated")}
        </p>
      ) : null}
      <div id="cockpit-360-body" hidden={!open}>
        {error ? (
          <div>
            <p role="alert" className="error">
              {t(lang, "riskCockpitUnavailable")} {error}
            </p>
            <button
              type="button"
              className="secondary"
              onClick={() => void load()}
            >
              {t(lang, "retry")}
            </button>
          </div>
        ) : risk === null ? (
          <p className="muted" role="status">
            {t(lang, "riskCockpitLoading")}
          </p>
        ) : risk.current === null ? (
          <p className="muted">{t(lang, "riskNoAssessment")}</p>
        ) : (
          <>
            <RiskOverviewHeader lang={lang} risk={risk} />
            {elevated.length > 0 ? (
              <DomainList
                lang={lang}
                domains={elevated}
                patientId={patientId}
                rulesVersion={risk.current.rules_version}
              />
            ) : (
              <p className="muted">{t(lang, "riskNoFactors")}</p>
            )}
            <RiskSummaryPanel
              lang={lang}
              patientId={patientId}
              risk={risk}
              capabilities={capabilities}
              onChanged={load}
              compact
            />
          </>
        )}
      </div>
    </section>
  );
}
