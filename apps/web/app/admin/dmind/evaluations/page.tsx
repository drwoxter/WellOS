"use client";

import Link from "next/link";
import { useRouter } from "next/navigation";
import { useState } from "react";
import { AppShell } from "../../../chrome";
import { useSession } from "@/lib/session";
import { t } from "@/lib/i18n";
import {
  canReadEvaluations,
  createRun,
  loadRuns,
  loadSuites,
  passRate,
  statusLabel,
  statusTone,
  type EvalRun,
  type EvalSuites,
} from "@/lib/eval";
import {
  Card,
  CardHead,
  EmptyState,
  Pill,
  StatTile,
} from "@/components/ui/primitives";
import {
  MessageLine,
  PanelState,
  errorText,
  useLoader,
  type Msg,
} from "../../../scheduling/shared";

export default function EvaluationsPage() {
  return (
    <AppShell>
      <EvaluationsCockpit />
    </AppShell>
  );
}

function EvaluationsCockpit() {
  const { lang, meta } = useSession();
  const roles = meta?.user.roles ?? [];
  const allowed = canReadEvaluations(roles);
  const suites = useLoader(loadSuites, "eval-suites", allowed);
  const runs = useLoader(loadRuns, "eval-runs", allowed);

  if (meta && !allowed) {
    return (
      <div className="page" aria-labelledby="eval-title">
        <h1 id="eval-title">{t(lang, "evalTitle")}</h1>
        <EmptyState title={t(lang, "evalDenied")} />
      </div>
    );
  }

  return (
    <div className="page" aria-labelledby="eval-title">
      <header className="page-head">
        <div>
          <h1 id="eval-title">{t(lang, "evalTitle")}</h1>
          <p className="muted">{t(lang, "evalSubtitle")}</p>
        </div>
      </header>
      <p
        className="banner warn"
        role="note"
        data-testid="eval-synthetic-banner"
      >
        {t(lang, "evalSyntheticBanner")}
      </p>
      <PanelState
        lang={lang}
        state={suites}
        emptyKey="evalLoadError"
        isEmpty={() => false}
      >
        {(s) => (
          <>
            <SuiteCard s={s} />
            {s.can_manage && s.synthetic_tenant ? (
              <NewRunCard s={s} onCreated={() => runs.reload()} />
            ) : (
              <p className="muted" role="note">
                {s.can_manage
                  ? t(lang, "evalNotSynthetic")
                  : t(lang, "evalReadOnly")}
              </p>
            )}
          </>
        )}
      </PanelState>
      <Card>
        <CardHead title={t(lang, "evalRuns")} />
        <PanelState
          lang={lang}
          state={runs}
          emptyKey="evalNoRuns"
          isEmpty={(d) => d.runs.length === 0}
        >
          {(d) => <RunTable runs={d.runs} />}
        </PanelState>
      </Card>
    </div>
  );
}

function SuiteCard({ s }: { s: EvalSuites }) {
  const { lang } = useSession();
  return (
    <Card>
      <CardHead
        title={`${t(lang, "evalSuite")} ${s.compiled.suite_key} ${s.compiled.suite_version}`}
        eyebrow={`${t(lang, "evalRubric")} ${s.compiled.rubric_version}`}
      />
      <div className="stat-grid">
        <StatTile value={s.compiled.case_count} label={t(lang, "evalCases")} />
        <StatTile
          value={s.compiled.operations.length}
          label={t(lang, "evalOperations")}
        />
        <StatTile
          value={s.compiled.languages.join(" / ").toUpperCase()}
          label={t(lang, "evalLanguage")}
        />
      </div>
      <ul className="chip-list" aria-label={t(lang, "evalOperations")}>
        {s.compiled.operations.map((op) => (
          <li key={op.operation}>
            <Pill
              tone={op.deterministic_baseline ? "ok" : "neutral"}
              icon={false}
            >
              {op.operation} · {op.cases}
            </Pill>
          </li>
        ))}
      </ul>
      <p className="muted small">
        {t(lang, "evalHashes")}: corpus {s.compiled.corpus_hash.slice(0, 16)}…
      </p>
    </Card>
  );
}

function NewRunCard({
  s,
  onCreated,
}: {
  s: EvalSuites;
  onCreated: () => void;
}) {
  const { lang } = useSession();
  const router = useRouter();
  const [mode, setMode] = useState<"dry-run" | "offline">("offline");
  const [operations, setOperations] = useState<string[]>([]);
  const [stopOnHigh, setStopOnHigh] = useState(true);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<Msg>(null);

  const toggle = (op: string) =>
    setOperations((prev) =>
      prev.includes(op) ? prev.filter((o) => o !== op) : [...prev, op],
    );

  const submit = async (e: React.FormEvent) => {
    e.preventDefault();
    setBusy(true);
    setMessage(null);
    try {
      const res = await createRun({
        mode,
        operations,
        stop_on_high_severity: stopOnHigh,
      });
      setMessage({
        kind: res.totals.failed > 0 ? "error" : "success",
        text: `${t(lang, "evalRunCreated")}: ${res.totals.cases} ${t(lang, "evalCases").toLowerCase()} · ${t(lang, "evalFailed").toLowerCase()} ${res.totals.failed}`,
      });
      onCreated();
      router.push(`/admin/dmind/evaluations/${res.run_id}`);
    } catch (err) {
      setMessage({ kind: "error", text: errorText(err) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <Card>
      <CardHead title={t(lang, "evalNewRun")} />
      <form
        onSubmit={submit}
        className="form-grid"
        aria-label={t(lang, "evalNewRun")}
      >
        <fieldset>
          <legend>{t(lang, "evalMode")}</legend>
          <label className="check">
            <input
              type="radio"
              name="eval-mode"
              value="offline"
              checked={mode === "offline"}
              onChange={() => setMode("offline")}
            />
            {t(lang, "evalModeOffline")}
          </label>
          <label className="check">
            <input
              type="radio"
              name="eval-mode"
              value="dry-run"
              checked={mode === "dry-run"}
              onChange={() => setMode("dry-run")}
            />
            {t(lang, "evalModeDryRun")}
          </label>
          <p className="muted small">{t(lang, "evalLiveCliOnly")}</p>
        </fieldset>
        <fieldset>
          <legend>{t(lang, "evalOperations")}</legend>
          <label className="check">
            <input
              type="checkbox"
              checked={operations.length === 0}
              onChange={() => setOperations([])}
            />
            {t(lang, "evalAllOperations")}
          </label>
          <div className="check-grid">
            {s.compiled.operations.map((op) => (
              <label key={op.operation} className="check">
                <input
                  type="checkbox"
                  checked={operations.includes(op.operation)}
                  onChange={() => toggle(op.operation)}
                />
                {op.operation}
              </label>
            ))}
          </div>
        </fieldset>
        <label className="check">
          <input
            type="checkbox"
            checked={stopOnHigh}
            onChange={(e) => setStopOnHigh(e.target.checked)}
          />
          {t(lang, "evalStopOnHigh")}
        </label>
        <div className="actions">
          <button
            type="submit"
            className="btn primary"
            disabled={busy}
            aria-busy={busy}
          >
            {busy ? t(lang, "evalStarting") : t(lang, "evalStart")}
          </button>
        </div>
        <MessageLine message={message} />
      </form>
    </Card>
  );
}

function RunTable({ runs }: { runs: EvalRun[] }) {
  const { lang } = useSession();
  return (
    <div className="table-wrap">
      <table className="data" data-testid="eval-run-table">
        <thead>
          <tr>
            <th scope="col">{t(lang, "evalRequested")}</th>
            <th scope="col">{t(lang, "evalMode")}</th>
            <th scope="col">{t(lang, "evalProvider")}</th>
            <th scope="col">{t(lang, "evalStatus")}</th>
            <th scope="col">{t(lang, "evalCases")}</th>
            <th scope="col">{t(lang, "evalPassRate")}</th>
            <th scope="col">{t(lang, "evalSafetyFailures")}</th>
            <th scope="col">
              <span className="sr-only">{t(lang, "evalOpenRun")}</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {runs.map((r) => {
            const totals = r.summary?.totals;
            const rate = totals ? passRate(totals) : null;
            const safety = r.summary?.safety_failures.length ?? 0;
            return (
              <tr key={r.id}>
                <td>
                  <time dateTime={r.created_at}>
                    {new Date(r.created_at).toLocaleString(lang)}
                  </time>
                  <div className="muted small">
                    {r.requested_by ?? r.requested_via} · {r.requested_via}
                  </div>
                </td>
                <td>{r.mode.replace("_", "-")}</td>
                <td>
                  {r.provider} / {r.model}
                  <div className="muted small">
                    {r.external
                      ? t(lang, "evalExternal")
                      : t(lang, "evalSyntheticProvider")}
                  </div>
                </td>
                <td>
                  <Pill tone={statusTone(r.status)}>
                    {statusLabel(lang, r.status)}
                  </Pill>
                </td>
                <td>{totals?.cases ?? "—"}</td>
                <td>{rate === null ? "—" : `${rate}%`}</td>
                <td>
                  {safety > 0 ? (
                    <Pill tone="critical">{safety}</Pill>
                  ) : (
                    <span className="muted">0</span>
                  )}
                </td>
                <td>
                  <Link
                    className="btn ghost small"
                    href={`/admin/dmind/evaluations/${r.id}`}
                  >
                    {t(lang, "evalOpenRun")}
                  </Link>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
