"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useCallback, useEffect, useMemo, useState, use } from "react";
import { AppShell } from "../../../chrome";
import { t } from "@/lib/i18n";
import type { Lang, TKey } from "@/lib/i18n";
import { ApiRequestError, apiFetch, useSession } from "@/lib/session";
import {
  ageYears,
  formatDate,
  formatDateTime,
  loopStateShortLabel,
  patientName,
} from "@/lib/clinical";
import type { VisitItem } from "@/lib/visits";
import type { Brief, Diagnostics } from "../../../encounters/[id]/brief";
import { DiagnosticHistory } from "../../../encounters/[id]/brief";
import type { RiskSection } from "@/lib/risk";
import { factorText, gapText } from "@/lib/risk";
import {
  DomainList,
  LevelBadge,
  RiskActions,
  RiskHistory,
  RiskOverviewHeader,
  RiskSummaryPanel,
  errorText,
} from "../../../risk/risk-view";

/** Composite payload of `GET /patients/:id/360`: the existing chart plus the
 *  Patient Brief, diagnostic history, care team, operational alerts and the
 *  risk section. Nothing here is a copy of a clinical record. */
export type Patient360 = {
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
  conditions: { code: string; display: string; status: string }[];
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
    encounter_type: string;
    started_at: string;
    completed_at: string | null;
    practitioner: string;
    own: boolean;
    note_status: string | null;
    addenda_count: number;
  }[];
  alerts: { severity: string; message: string; created_at: string }[];
  visit: VisitItem | null;
  brief: Brief;
  diagnostics: Diagnostics;
  care_team: {
    id: string;
    function: string;
    source: string;
    since: string;
    assignee_user_id: string | null;
    assignee: string | null;
    queue: string | null;
  }[];
  responsible_professional: {
    assignee: string | null;
    since: string;
  } | null;
  internal_alerts: {
    id: string;
    kind: string;
    priority: string;
    status: string;
    visit_id: string;
    created_at: string;
  }[];
  risk: RiskSection | null;
  capabilities: {
    can_start_consultation: boolean;
    open_consultation_id: string | null;
    can_view_risk: boolean;
  };
  generated_at: string;
};

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

function careFunctionKey(fn: string): TKey {
  switch (fn) {
    case "treating_professional":
    case "treating_physician":
      return "careFunctionTreating";
    case "triage_nurse":
      return "careFunctionTriage";
    case "registration":
      return "careFunctionRegistration";
    case "risk_follow_up":
      return "riskFollowUpOwner";
    default:
      return "careFunctionOther";
  }
}

function encounterStatusKey(status: string): TKey {
  switch (status) {
    case "completed":
      return "encStatusCompleted";
    case "cancelled":
      return "encStatusCancelled";
    default:
      return "encStatusInProgress";
  }
}

function Section({
  id,
  titleKey,
  children,
  count,
  className = "",
}: {
  id: string;
  titleKey: TKey;
  children: React.ReactNode;
  count?: number;
  className?: string;
}) {
  const { lang } = useSession();
  return (
    <section
      id={id}
      className={`card p360-section ${className}`.trim()}
      aria-labelledby={`${id}-h`}
    >
      <h2 id={`${id}-h`}>
        {t(lang, titleKey)}
        {typeof count === "number" ? (
          <span className="badge neutral p360-count">{count}</span>
        ) : null}
      </h2>
      {children}
    </section>
  );
}

/** Header: identity, demographics, direct consultation action. */
function Header({
  data,
  lang,
  onError,
}: {
  data: Patient360;
  lang: Lang;
  onError: (msg: string | null) => void;
}) {
  const router = useRouter();
  const [busy, setBusy] = useState(false);
  const p = data.patient;
  const open = data.capabilities.open_consultation_id;

  async function start() {
    setBusy(true);
    onError(null);
    try {
      const enc = await apiFetch<{ id: string }>("/api/v1/encounters", {
        method: "POST",
        body: JSON.stringify({ patient_id: p.id }),
      });
      router.push(`/encounters/${enc.id}`);
    } catch (err) {
      onError(errorText(lang, err));
    } finally {
      setBusy(false);
    }
  }

  return (
    <header className="card p360-header">
      <div className="grow">
        <p className="muted p360-kicker">{t(lang, "patient360")}</p>
        <h1>{patientName(p)}</h1>
        <p className="muted">
          {t(lang, "identifier")}: {p.identifier} · {sexLabel(lang, p.sex)} ·{" "}
          {t(lang, "born")} {formatDate(lang, p.birth_date)} (
          {ageYears(p.birth_date)} {t(lang, "ageYears")})
        </p>
        <p className="muted">
          {t(lang, "responsibleProfessional")}:{" "}
          {data.responsible_professional?.assignee ?? (
            <em>{t(lang, "noResponsibleProfessional")}</em>
          )}
        </p>
      </div>
      <div className="p360-actions">
        {open ? (
          <Link className="primary button" href={`/encounters/${open}`}>
            {t(lang, "resumeConsultation")}
          </Link>
        ) : data.capabilities.can_start_consultation ? (
          <button
            type="button"
            className="primary"
            disabled={busy}
            aria-busy={busy}
            onClick={() => void start()}
          >
            {t(lang, "startConsultation")}
          </button>
        ) : null}
        <Link className="secondary button" href={`/patients/${p.id}`}>
          {t(lang, "openChart")}
        </Link>
      </div>
    </header>
  );
}

function Alerts({ data, lang }: { data: Patient360; lang: Lang }) {
  const high = data.allergies.filter((a) => a.criticality === "high");
  const total = data.alerts.length + data.internal_alerts.length + high.length;
  return (
    <Section id="alerts" titleKey="unresolvedSafety" count={total}>
      {total === 0 ? (
        <p className="muted">{t(lang, "noActiveAlerts")}</p>
      ) : (
        <ul className="brief-list">
          {data.alerts.map((a, i) => (
            <li key={`a-${i}`}>
              <span
                className={`badge ${a.severity === "critical" ? "critical" : "warn"}`}
              >
                {a.severity === "critical"
                  ? t(lang, "critical")
                  : t(lang, "alerts")}
              </span>{" "}
              {a.message}
              <span className="muted">
                {" "}
                · {formatDateTime(lang, a.created_at)}
              </span>
            </li>
          ))}
          {data.internal_alerts.map((a) => (
            <li key={a.id}>
              <span
                className={`badge ${a.priority === "critical" || a.priority === "high" ? "critical" : "warn"}`}
              >
                {t(lang, "internalAlertsTitle")}
              </span>{" "}
              {a.kind.replace(/_/g, " ")} · {a.status}
              <span className="muted">
                {" "}
                · {formatDateTime(lang, a.created_at)}
              </span>
            </li>
          ))}
          {high.map((a) => (
            <li key={`al-${a.substance}`}>
              <span className="badge critical">{t(lang, "allergies")}</span>{" "}
              {a.substance}
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

function CareTeam({ data, lang }: { data: Patient360; lang: Lang }) {
  return (
    <Section
      id="care-team"
      titleKey="careTeamTitle"
      count={data.care_team.length}
    >
      {data.care_team.length === 0 ? (
        <p className="muted">{t(lang, "noCareTeam")}</p>
      ) : (
        <ul className="brief-list">
          {data.care_team.map((c) => (
            <li key={c.id}>
              <strong>{t(lang, careFunctionKey(c.function))}</strong>:{" "}
              {c.assignee ?? c.queue ?? t(lang, "unassigned")}
              <span className="muted">
                {" "}
                · {t(lang, "careTeamSince")} {formatDate(lang, c.since)}
              </span>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

function Conditions({ data, lang }: { data: Patient360; lang: Lang }) {
  const active = data.conditions.filter((c) => c.status !== "resolved");
  const past = data.conditions.filter((c) => c.status === "resolved");
  return (
    <Section id="conditions" titleKey="conditionsHistory" count={active.length}>
      {data.conditions.length === 0 ? (
        <p className="muted">{t(lang, "noConditions")}</p>
      ) : (
        <>
          <ul className="brief-list">
            {active.map((c) => (
              <li key={`${c.code}-${c.display}`}>
                {c.display}{" "}
                <span className="muted">
                  {c.code}
                  {c.status === "provisional"
                    ? ` · ${t(lang, "dxProvisional")}`
                    : ""}
                </span>
              </li>
            ))}
          </ul>
          {past.length > 0 ? (
            <details className="secondary">
              <summary>
                {t(lang, "conditionResolved")} ({past.length})
              </summary>
              <ul className="brief-list">
                {past.map((c) => (
                  <li key={`${c.code}-${c.display}`}>
                    {c.display} <span className="muted">{c.code}</span>
                  </li>
                ))}
              </ul>
            </details>
          ) : null}
        </>
      )}
    </Section>
  );
}

function MedicationSafety({ data, lang }: { data: Patient360; lang: Lang }) {
  const domain = data.risk?.current?.domains.find(
    (d) => d.domain === "medication_allergy_safety",
  );
  const active = data.medications.filter((m) => m.status === "active");
  return (
    <Section id="medications" titleKey="medicationSafety" count={active.length}>
      {domain && domain.factors.length > 0 ? (
        <ul className="brief-list p360-flags">
          {domain.factors.map((f) => (
            <li key={f.code}>
              <LevelBadge lang={lang} level={f.level} /> {factorText(lang, f)}
            </li>
          ))}
        </ul>
      ) : null}
      <h3>{t(lang, "allergies")}</h3>
      {data.allergies.length === 0 ? (
        <p className="muted">{t(lang, "noKnownAllergies")}</p>
      ) : (
        <ul className="brief-list">
          {data.allergies.map((a) => (
            <li key={a.substance}>
              <span
                className={`badge ${a.criticality === "high" ? "critical" : "warn"}`}
              >
                {t(lang, "criticality")}: {a.criticality}
              </span>{" "}
              {a.substance}
            </li>
          ))}
        </ul>
      )}
      <h3>{t(lang, "medications")}</h3>
      {active.length === 0 ? (
        <p className="muted">{t(lang, "noMedications")}</p>
      ) : (
        <ul className="brief-list">
          {active.map((m, i) => (
            <li key={`${m.name}-${i}`}>{m.name}</li>
          ))}
        </ul>
      )}
    </Section>
  );
}

function Encounters({ data, lang }: { data: Patient360; lang: Lang }) {
  const recent = useMemo(
    () =>
      [...data.encounters]
        .sort((a, b) => (a.started_at < b.started_at ? 1 : -1))
        .slice(0, 5),
    [data.encounters],
  );
  const notes = new Map(
    data.brief.recent_notes.map((n) => [n.encounter_id, n]),
  );
  return (
    <Section
      id="encounters"
      titleKey="recentEncounters"
      count={data.encounters.length}
    >
      {recent.length === 0 ? (
        <p className="muted">{t(lang, "noRecentEncounters")}</p>
      ) : (
        <ul className="brief-list">
          {recent.map((e) => {
            const note = notes.get(e.id);
            return (
              <li key={e.id}>
                <Link className="navlink" href={`/encounters/${e.id}`}>
                  {formatDateTime(lang, e.started_at)} —{" "}
                  {t(
                    lang,
                    e.encounter_type === "consultation"
                      ? "consultation"
                      : "orderOnlyEncounter",
                  )}
                </Link>{" "}
                <span className="badge neutral">
                  {t(lang, encounterStatusKey(e.status))}
                </span>
                <span className="muted"> · {e.practitioner}</span>
                {note?.reason_for_encounter ? (
                  <div className="muted">{note.reason_for_encounter}</div>
                ) : null}
                {note?.assessment ? (
                  <div className="muted brief-clamp">{note.assessment}</div>
                ) : null}
              </li>
            );
          })}
        </ul>
      )}
    </Section>
  );
}

function Pending({ data, lang }: { data: Patient360; lang: Lang }) {
  const tasks = data.brief.open_tasks;
  const requests = data.brief.open_requests;
  const total = tasks.length + requests.length + (data.visit ? 1 : 0);
  return (
    <Section id="pending" titleKey="pendingWork" count={total}>
      {total === 0 ? (
        <p className="muted">{t(lang, "noPendingWork")}</p>
      ) : (
        <ul className="brief-list">
          {data.visit ? (
            <li>
              <span className="badge neutral">{t(lang, "todaysVisit")}</span>{" "}
              <Link className="navlink" href="/access">
                {data.visit.service} ·{" "}
                {formatDateTime(
                  lang,
                  data.visit.scheduled_at ??
                    data.visit.arrived_at ??
                    data.visit.updated_at,
                )}
              </Link>
            </li>
          ) : null}
          {tasks.map((task) => (
            <li key={task.id}>
              <span
                className={`badge ${task.status === "overdue" ? "critical" : "warn"}`}
              >
                {task.status === "overdue"
                  ? t(lang, "overdue")
                  : t(lang, "followUpTasks")}
              </span>{" "}
              {task.service_request_id ? (
                <Link
                  className="navlink"
                  href={`/requests/${task.service_request_id}`}
                >
                  {task.description}
                </Link>
              ) : (
                task.description
              )}
              {task.source === "risk_suggestion" ? (
                <>
                  {" "}
                  <span className="badge neutral">
                    {t(lang, "riskAiGenerated")}
                  </span>
                </>
              ) : null}
              {task.due_at ? (
                <span className="muted">
                  {" "}
                  · {t(lang, "dueAt")} {formatDate(lang, task.due_at)}
                </span>
              ) : null}
            </li>
          ))}
          {requests.map((r) => (
            <li key={r.id}>
              <span className="badge neutral">
                {loopStateShortLabel(lang, r.loop_state)}
              </span>{" "}
              <Link className="navlink" href={`/requests/${r.id}`}>
                {r.display}
              </Link>
              <span className="muted"> · {formatDate(lang, r.created_at)}</span>
            </li>
          ))}
        </ul>
      )}
    </Section>
  );
}

function Results({ data, lang }: { data: Patient360; lang: Lang }) {
  const abnormal = data.brief.recent_abnormal;
  return (
    <Section id="results" titleKey="diagnosticTrends" className="p360-wide">
      <h3>{t(lang, "abnormalAwaitingReview")}</h3>
      {abnormal.length === 0 ? (
        <p className="muted">{t(lang, "noAbnormalAwaiting")}</p>
      ) : (
        <ul className="brief-list">
          {abnormal.map((r) => (
            <li key={r.id}>
              <span className={`badge ${r.critical ? "critical" : "warn"}`}>
                {r.critical ? t(lang, "critical") : t(lang, "abnormalFlag")}{" "}
                <span aria-hidden="true">
                  {r.abnormal === "high" ? "↑" : "↓"}
                </span>
              </span>{" "}
              <Link
                className="navlink"
                href={`/requests/${r.service_request_id}`}
              >
                {r.display}
              </Link>{" "}
              <strong>
                {r.value} {r.unit}
              </strong>
              {r.reference_range ? (
                <span className="muted"> ({r.reference_range})</span>
              ) : null}
              <span className="muted">
                {" "}
                · {formatDate(lang, r.effective_at)}
              </span>
            </li>
          ))}
        </ul>
      )}
      {data.diagnostics.tests.length === 0 ? (
        <p className="muted">{t(lang, "trendsUnavailable")}</p>
      ) : (
        <DiagnosticHistory lang={lang} diagnostics={data.diagnostics} />
      )}
    </Section>
  );
}

function Preventive({ data, lang }: { data: Patient360; lang: Lang }) {
  const domain = data.risk?.current?.domains.find(
    (d) => d.domain === "preventive_care",
  );
  const gaps = domain ? [...domain.missing_data, ...domain.stale_data] : [];
  const count = (domain?.factors.length ?? 0) + gaps.length;
  return (
    <Section
      id="preventive"
      titleKey="preventiveGaps"
      count={domain ? count : undefined}
    >
      {!data.risk ? (
        <p className="muted">{t(lang, "riskNoAccess")}</p>
      ) : !domain ? (
        <p className="muted">{t(lang, "riskNoAssessment")}</p>
      ) : count === 0 ? (
        <p className="muted">{t(lang, "noPreventiveGaps")}</p>
      ) : (
        <>
          <p>
            <LevelBadge lang={lang} level={domain.level} />
          </p>
          <ul className="brief-list">
            {domain.factors.map((f) => (
              <li key={f.code}>{factorText(lang, f)}</li>
            ))}
            {gaps.map((g) => (
              <li key={`${g.code}-${g.record_type}`} className="muted">
                {gapText(lang, g)}
              </li>
            ))}
          </ul>
        </>
      )}
    </Section>
  );
}

/** Current risk, domains, actions, evolution and the dMind panel. */
function Risk({
  data,
  lang,
  onChanged,
}: {
  data: Patient360;
  lang: Lang;
  onChanged: () => Promise<unknown>;
}) {
  const [busy, setBusy] = useState(false);
  const [msg, setMsg] = useState<{ kind: "error" | "ok"; text: string } | null>(
    null,
  );
  const risk = data.risk;
  const requestIds = useMemo(() => {
    const m: Record<string, string> = {};
    for (const test of data.diagnostics.tests) {
      for (const r of test.results) m[r.id] = r.service_request_id;
    }
    for (const r of data.brief.recent_abnormal) m[r.id] = r.service_request_id;
    return m;
  }, [data]);

  async function recalc() {
    setBusy(true);
    setMsg(null);
    try {
      await apiFetch(`/api/v1/patients/${data.patient.id}/risk/recalculate`, {
        method: "POST",
        body: "{}",
      });
      setMsg({ kind: "ok", text: t(lang, "riskRecalculated") });
      await onChanged();
    } catch (err) {
      setMsg({ kind: "error", text: errorText(lang, err) });
    } finally {
      setBusy(false);
    }
  }

  if (!risk) {
    return (
      <Section id="risk" titleKey="riskSummaryTitle" className="p360-wide">
        <p className="muted">{t(lang, "riskNoAccess")}</p>
      </Section>
    );
  }
  const current = risk.current;
  return (
    <Section id="risk" titleKey="riskSummaryTitle" className="p360-wide">
      <p className="muted">{t(lang, "riskHelp")}</p>
      {msg ? (
        <p
          role={msg.kind === "error" ? "alert" : "status"}
          className={msg.kind === "error" ? "error" : "success"}
        >
          {msg.text}
        </p>
      ) : null}
      {!current ? (
        <p className="muted">{t(lang, "riskNoAssessment")}</p>
      ) : (
        <>
          <RiskOverviewHeader lang={lang} risk={risk} />
          <RiskActions
            lang={lang}
            patientId={data.patient.id}
            assessmentId={current.id}
            domain="overall"
            review={current.review}
            capabilities={risk.capabilities}
            professionals={risk.professionals}
            owner={risk.follow_up_owner}
            onChanged={onChanged}
            idPrefix="p360-risk"
          />
          <h3>{t(lang, "riskDomainsTitle")}</h3>
          <DomainList
            lang={lang}
            domains={current.domains}
            patientId={data.patient.id}
            requestIds={requestIds}
            rulesVersion={current.rules_version}
          />
        </>
      )}
      {risk.capabilities.can_recalculate ? (
        <p>
          <button
            type="button"
            className="secondary"
            disabled={busy}
            aria-busy={busy}
            onClick={() => void recalc()}
          >
            {t(lang, "riskRecalculate")}
          </button>
        </p>
      ) : null}
    </Section>
  );
}

/** Risk evolution and the governed dMind explanation, after the clinical
 *  facts so the record stays primary. */
function RiskExtras({
  data,
  lang,
  onChanged,
}: {
  data: Patient360;
  lang: Lang;
  onChanged: () => Promise<unknown>;
}) {
  const { meta } = useSession();
  const risk = data.risk;
  if (!risk || !risk.current) return null;
  return (
    <>
      <Section
        id="risk-evolution"
        titleKey="riskEvolution"
        className="p360-wide"
      >
        <RiskHistory lang={lang} history={risk.history} />
      </Section>
      <div className="p360-wide">
        <RiskSummaryPanel
          lang={lang}
          patientId={data.patient.id}
          risk={risk}
          capabilities={meta?.ai_capabilities}
          onChanged={onChanged}
        />
      </div>
    </>
  );
}

function Patient360View({ id }: { id: string }) {
  const { lang, authenticated } = useSession();
  const [data, setData] = useState<Patient360 | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [denied, setDenied] = useState(false);
  const [actionError, setActionError] = useState<string | null>(null);

  const load = useCallback(async () => {
    setError(null);
    try {
      setData(
        await apiFetch<Patient360>(
          `/api/v1/patients/${id}/360?lang=${encodeURIComponent(lang)}`,
        ),
      );
    } catch (e) {
      if (
        e instanceof ApiRequestError &&
        (e.status === 403 || e.status === 404)
      ) {
        setDenied(true);
      } else {
        setError(e instanceof Error ? e.message : String(e));
      }
    }
  }, [id, lang]);

  useEffect(() => {
    if (authenticated) void load();
  }, [authenticated, load]);

  if (denied) {
    return (
      <div className="card">
        <p role="alert" className="error">
          {t(lang, "notAuthorized")}
        </p>
        <Link className="navlink" href="/patients">
          {t(lang, "navPatients")}
        </Link>
      </div>
    );
  }
  if (error) {
    return (
      <div className="card">
        <p role="alert" className="error">
          {error}
        </p>
        <button type="button" className="secondary" onClick={() => void load()}>
          {t(lang, "retry")}
        </button>
      </div>
    );
  }
  if (!data) {
    return (
      <p className="muted" role="status" aria-live="polite">
        {t(lang, "loading")}
      </p>
    );
  }
  return (
    <div className="p360">
      <Header data={data} lang={lang} onError={setActionError} />
      {actionError ? (
        <p role="alert" className="error">
          {actionError}
        </p>
      ) : null}
      <p className="muted p360-generated">
        {t(lang, "patient360Help")} · {formatDateTime(lang, data.generated_at)}
      </p>
      <div className="p360-grid">
        <Alerts data={data} lang={lang} />
        <CareTeam data={data} lang={lang} />
        <Risk data={data} lang={lang} onChanged={load} />
        <Conditions data={data} lang={lang} />
        <MedicationSafety data={data} lang={lang} />
        <Encounters data={data} lang={lang} />
        <Pending data={data} lang={lang} />
        <Results data={data} lang={lang} />
        <Preventive data={data} lang={lang} />
        <RiskExtras data={data} lang={lang} onChanged={load} />
      </div>
    </div>
  );
}

export default function Patient360Page({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = use(params);
  return (
    <AppShell>
      <Patient360View id={id} />
    </AppShell>
  );
}
