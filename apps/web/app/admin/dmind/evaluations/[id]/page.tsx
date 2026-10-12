"use client";

import Link from "next/link";
import { useParams } from "next/navigation";
import { useCallback, useState } from "react";
import { AppShell } from "../../../../chrome";
import { useSession } from "@/lib/session";
import { t } from "@/lib/i18n";
import {
  REVIEW_DECISIONS,
  canReadEvaluations,
  createReview,
  decisionLabel,
  loadRun,
  outcomeTone,
  passRate,
  statusLabel,
  statusTone,
  verificationLabel,
  type EvalExecution,
  type EvalRunDetail,
  type EvalTotals,
  type ReviewDecision,
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
} from "../../../../scheduling/shared";

export default function EvaluationRunPage() {
  return (
    <AppShell>
      <RunDetail />
    </AppShell>
  );
}

function RunDetail() {
  const { lang, meta } = useSession();
  const params = useParams<{ id: string }>();
  const id = params?.id ?? "";
  const roles = meta?.user.roles ?? [];
  const allowed = canReadEvaluations(roles) && id.length > 0;
  const load = useCallback(() => loadRun(id), [id]);
  const detail = useLoader(load, `eval-run-${id}`, allowed);
  const [filter, setFilter] = useState<string>("all");

  if (meta && !canReadEvaluations(roles)) {
    return (
      <div className="page" aria-labelledby="eval-title">
        <h1 id="eval-title">{t(lang, "evalDetail")}</h1>
        <EmptyState title={t(lang, "evalDenied")} />
      </div>
    );
  }

  return (
    <div className="page" aria-labelledby="eval-title">
      <nav aria-label={t(lang, "evalBack")}>
        <Link href="/admin/dmind/evaluations" className="btn ghost small">
          ← {t(lang, "evalBack")}
        </Link>
      </nav>
      <p
        className="banner warn"
        role="note"
        data-testid="eval-synthetic-banner"
      >
        {t(lang, "evalSyntheticBanner")}
      </p>
      <PanelState
        lang={lang}
        state={detail}
        emptyKey="evalNotFound"
        isEmpty={(d) => !d.run}
      >
        {(d) => (
          <>
            <RunHeader d={d} />
            <Card>
              <CardHead
                title={t(lang, "evalExecutions")}
                actions={
                  <label className="field inline">
                    <span>{t(lang, "evalStatus")}</span>
                    <select
                      value={filter}
                      onChange={(e) => setFilter(e.target.value)}
                      aria-label={t(lang, "evalStatus")}
                    >
                      <option value="all">{t(lang, "evalFilterAll")}</option>
                      <option value="failed">
                        {statusLabel(lang, "failed")}
                      </option>
                      <option value="needs_review">
                        {statusLabel(lang, "needs_review")}
                      </option>
                      <option value="passed">
                        {statusLabel(lang, "passed")}
                      </option>
                      <option value="provider_unavailable_fallback_verified">
                        {statusLabel(
                          lang,
                          "provider_unavailable_fallback_verified",
                        )}
                      </option>
                      <option value="aborted">
                        {statusLabel(lang, "aborted")}
                      </option>
                    </select>
                  </label>
                }
              />
              <ul className="eval-executions" data-testid="eval-executions">
                {d.executions
                  .filter((e) => filter === "all" || e.status === filter)
                  .map((e) => (
                    <ExecutionItem
                      key={e.id}
                      execution={e}
                      canManage={d.can_manage}
                      onReviewed={detail.reload}
                    />
                  ))}
              </ul>
            </Card>
          </>
        )}
      </PanelState>
    </div>
  );
}

function fmtCost(
  cost: string | null | undefined,
  currency: string | null | undefined,
) {
  if (!cost || cost === "0") return "0";
  return `${cost} ${currency ?? ""}`.trim();
}

function RunHeader({ d }: { d: EvalRunDetail }) {
  const { lang } = useSession();
  const r = d.run;
  const s = r.summary;
  const totals = s?.totals;
  return (
    <>
      <header className="page-head">
        <div>
          <h1 id="eval-title">
            {t(lang, "evalDetail")} · {r.suite_key} {r.suite_version}
          </h1>
          <p className="muted">
            {r.provider} / {r.model} ·{" "}
            {r.external
              ? t(lang, "evalExternal")
              : t(lang, "evalSyntheticProvider")}{" "}
            · {r.mode.replace("_", "-")} · {r.environment} · {r.requested_via}
            {r.requested_by ? ` · ${r.requested_by}` : ""} ·{" "}
            <time dateTime={r.created_at}>
              {new Date(r.created_at).toLocaleString(lang)}
            </time>
          </p>
        </div>
        <Pill tone={statusTone(r.status)}>{statusLabel(lang, r.status)}</Pill>
      </header>
      {!d.can_manage && (
        <p className="muted" role="note">
          {t(lang, "evalReadOnly")}
        </p>
      )}
      {totals && (
        <div className="stat-grid" data-testid="eval-totals">
          <StatTile value={totals.cases} label={t(lang, "evalCases")} />
          <StatTile
            value={totals.passed}
            label={t(lang, "evalPassed")}
            tone="ok"
          />
          <StatTile
            value={totals.needs_review}
            label={t(lang, "evalNeedsReview")}
            tone={totals.needs_review > 0 ? "warn" : "neutral"}
          />
          <StatTile
            value={totals.failed}
            label={t(lang, "evalFailed")}
            tone={totals.failed > 0 ? "critical" : "neutral"}
          />
          <StatTile
            value={totals.fallback_verified}
            label={t(lang, "evalFallback")}
          />
          <StatTile value={totals.aborted} label={t(lang, "evalAborted")} />
        </div>
      )}
      {s && (
        <Card compact>
          <dl className="kv-grid">
            <div>
              <dt>{t(lang, "evalCalls")}</dt>
              <dd>{s.calls}</dd>
            </div>
            <div>
              <dt>{t(lang, "evalTokens")}</dt>
              <dd>
                {s.input_tokens} / {s.output_tokens}
              </dd>
            </div>
            <div>
              <dt>{t(lang, "evalCost")}</dt>
              <dd>{fmtCost(s.estimated_cost, s.currency)}</dd>
            </div>
            <div>
              <dt>{t(lang, "evalLatency")}</dt>
              <dd>
                {s.p50_latency_ms ?? "—"} / {s.p95_latency_ms ?? "—"} ms
              </dd>
            </div>
            <div>
              <dt>{t(lang, "evalRubric")}</dt>
              <dd>{r.rubric_version}</dd>
            </div>
            {r.abort_reason && (
              <div>
                <dt>{t(lang, "evalAborted")}</dt>
                <dd>{r.abort_reason}</dd>
              </div>
            )}
          </dl>
        </Card>
      )}
      <Card tone={s && s.safety_failures.length > 0 ? "critical" : undefined}>
        <CardHead title={t(lang, "evalSafetyFailures")} level={3} />
        {!s || s.safety_failures.length === 0 ? (
          <p className="muted">{t(lang, "evalNoSafetyFailures")}</p>
        ) : (
          <ul data-testid="eval-safety-failures">
            {s.safety_failures.map((f, i) => (
              <li key={`${f.case_key}-${f.metric}-${i}`}>
                <strong>{f.case_key}</strong> · {f.metric} — {f.detail}
              </li>
            ))}
          </ul>
        )}
      </Card>
      {s && (
        <div className="two-col">
          <BreakdownTable
            title={t(lang, "evalByOperation")}
            rows={s.operations}
          />
          <BreakdownTable
            title={t(lang, "evalByLanguage")}
            rows={s.languages}
          />
        </div>
      )}
    </>
  );
}

function BreakdownTable({
  title,
  rows,
}: {
  title: string;
  rows: Record<string, EvalTotals>;
}) {
  const { lang } = useSession();
  return (
    <Card compact>
      <CardHead title={title} level={3} />
      <div className="table-wrap">
        <table className="data">
          <thead>
            <tr>
              <th scope="col">{title}</th>
              <th scope="col">{t(lang, "evalCases")}</th>
              <th scope="col">{t(lang, "evalPassed")}</th>
              <th scope="col">{t(lang, "evalNeedsReview")}</th>
              <th scope="col">{t(lang, "evalFailed")}</th>
              <th scope="col">{t(lang, "evalPassRate")}</th>
            </tr>
          </thead>
          <tbody>
            {Object.entries(rows).map(([k, v]) => {
              const rate = passRate(v);
              return (
                <tr key={k}>
                  <th scope="row">{k}</th>
                  <td>{v.cases}</td>
                  <td>{v.passed}</td>
                  <td>{v.needs_review}</td>
                  <td>{v.failed}</td>
                  <td>{rate === null ? "—" : `${rate}%`}</td>
                </tr>
              );
            })}
          </tbody>
        </table>
      </div>
    </Card>
  );
}

function kindLabel(
  lang: Parameters<typeof t>[0],
  k: EvalExecution["execution_kind"],
) {
  switch (k) {
    case "fresh":
      return t(lang, "evalKindFresh");
    case "retried":
      return t(lang, "evalKindRetried");
    case "reused":
      return t(lang, "evalKindReused");
    case "cached":
      return t(lang, "evalKindCached");
    default:
      return t(lang, "evalKindSkipped");
  }
}

function ExecutionItem({
  execution: e,
  canManage,
  onReviewed,
}: {
  execution: EvalExecution;
  canManage: boolean;
  onReviewed: () => void;
}) {
  const { lang } = useSession();
  const [showOutputs, setShowOutputs] = useState(false);
  const failing = e.metrics.filter(
    (m) => m.outcome === "fail" || m.outcome === "review",
  );
  const headingId = `exec-${e.id}`;
  return (
    <li
      className="eval-execution"
      data-status={e.status}
      data-testid="eval-execution"
    >
      <Card as="article" compact aria-labelledby={headingId}>
        <CardHead
          id={headingId}
          level={3}
          title={`${e.case_key} — ${e.title}`}
          eyebrow={`${e.operation} · ${e.language.toUpperCase()} · ${e.category} · ${kindLabel(lang, e.execution_kind)}`}
          actions={
            <Pill tone={statusTone(e.status)}>
              {statusLabel(lang, e.status)}
            </Pill>
          }
        />
        <dl className="kv-grid small">
          <div>
            <dt>{t(lang, "evalProvider")}</dt>
            <dd>
              {e.provider} / {e.model}
            </dd>
          </div>
          <div>
            <dt>{t(lang, "evalLatency")}</dt>
            <dd>{e.latency_ms ?? "—"} ms</dd>
          </div>
          <div>
            <dt>{t(lang, "evalTokens")}</dt>
            <dd>
              {e.input_tokens ?? "—"} / {e.output_tokens ?? "—"}
            </dd>
          </div>
          <div>
            <dt>{t(lang, "evalRetries")}</dt>
            <dd>{e.retry_count}</dd>
          </div>
          <div>
            <dt>{t(lang, "evalCost")}</dt>
            <dd>{fmtCost(e.estimated_cost, e.currency)}</dd>
          </div>
          <div>
            <dt>{t(lang, "evalHashes")}</dt>
            <dd className="mono">
              in {e.input_hash.slice(0, 12)} · base{" "}
              {e.baseline_hash.slice(0, 12)}
            </dd>
          </div>
        </dl>
        <details className="eval-metrics">
          <summary>
            {t(lang, "evalMetrics")} ({e.metrics.length})
            {failing.length > 0
              ? ` · ${failing.map((m) => m.metric).join(", ")}`
              : ""}
          </summary>
          <div className="table-wrap">
            <table className="data">
              <thead>
                <tr>
                  <th scope="col">{t(lang, "evalMetric")}</th>
                  <th scope="col">{t(lang, "evalVerification")}</th>
                  <th scope="col">{t(lang, "evalSeverity")}</th>
                  <th scope="col">{t(lang, "evalOutcome")}</th>
                  <th scope="col">{t(lang, "evalComment")}</th>
                </tr>
              </thead>
              <tbody>
                {e.metrics.map((m) => (
                  <tr key={m.metric}>
                    <th scope="row">{m.metric}</th>
                    <td>{verificationLabel(lang, m.verification)}</td>
                    <td>{m.severity}</td>
                    <td>
                      <Pill tone={outcomeTone(m.outcome)} icon={false}>
                        {m.outcome}
                      </Pill>
                    </td>
                    <td className="muted small">{m.detail ?? ""}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          </div>
        </details>
        <button
          type="button"
          className="btn ghost small"
          aria-expanded={showOutputs}
          onClick={() => setShowOutputs((v) => !v)}
        >
          {showOutputs
            ? t(lang, "evalHideOutputs")
            : t(lang, "evalShowOutputs")}
        </button>
        {showOutputs && (
          <div className="two-col">
            <section aria-label={t(lang, "evalBaselineOutput")}>
              <h4>{t(lang, "evalBaselineOutput")}</h4>
              <pre className="code-block">
                {e.baseline_output
                  ? JSON.stringify(e.baseline_output, null, 2)
                  : "—"}
              </pre>
            </section>
            <section aria-label={t(lang, "evalProviderOutput")}>
              <h4>{t(lang, "evalProviderOutput")}</h4>
              <pre className="code-block">
                {e.provider_output
                  ? JSON.stringify(e.provider_output, null, 2)
                  : "—"}
              </pre>
            </section>
          </div>
        )}
        <section aria-label={t(lang, "evalReviews")} className="eval-reviews">
          <h4>{t(lang, "evalReviews")}</h4>
          {e.reviews.length === 0 ? (
            <p className="muted small">{t(lang, "evalNoReviews")}</p>
          ) : (
            <ol className="timeline">
              {e.reviews.map((rv) => (
                <li key={rv.id}>
                  <Pill
                    tone={rv.decision === "accept_result" ? "ok" : "warn"}
                    icon={false}
                  >
                    {decisionLabel(lang, rv.decision)}
                  </Pill>{" "}
                  <span className="muted small">
                    v{rv.version} · {rv.reviewer} ·{" "}
                    <time dateTime={rv.created_at}>
                      {new Date(rv.created_at).toLocaleString(lang)}
                    </time>
                  </span>
                  {rv.comment && <p>{rv.comment}</p>}
                </li>
              ))}
            </ol>
          )}
          {canManage && (
            <ReviewForm executionId={e.id} onReviewed={onReviewed} />
          )}
        </section>
      </Card>
    </li>
  );
}

function ReviewForm({
  executionId,
  onReviewed,
}: {
  executionId: string;
  onReviewed: () => void;
}) {
  const { lang } = useSession();
  const [decision, setDecision] = useState<ReviewDecision>("accept_result");
  const [comment, setComment] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<Msg>(null);
  const needsComment = decision !== "accept_result";

  const submit = async (ev: React.FormEvent) => {
    ev.preventDefault();
    if (needsComment && comment.trim().length === 0) {
      setMessage({ kind: "error", text: t(lang, "evalCommentRequired") });
      return;
    }
    setBusy(true);
    setMessage(null);
    try {
      await createReview(executionId, decision, comment.trim());
      setComment("");
      setMessage({ kind: "success", text: t(lang, "evalReviewSaved") });
      onReviewed();
    } catch (err) {
      setMessage({ kind: "error", text: errorText(err) });
    } finally {
      setBusy(false);
    }
  };

  return (
    <form
      onSubmit={submit}
      className="eval-review-form"
      aria-label={t(lang, "evalAddReview")}
    >
      <label className="field">
        <span>{t(lang, "evalDecision")}</span>
        <select
          value={decision}
          onChange={(ev) => setDecision(ev.target.value as ReviewDecision)}
        >
          {REVIEW_DECISIONS.map((d) => (
            <option key={d} value={d}>
              {decisionLabel(lang, d)}
            </option>
          ))}
        </select>
      </label>
      <label className="field">
        <span>{t(lang, "evalComment")}</span>
        <textarea
          value={comment}
          maxLength={1000}
          rows={2}
          required={needsComment}
          onChange={(ev) => setComment(ev.target.value)}
        />
      </label>
      <div className="actions">
        <button
          type="submit"
          className="btn primary small"
          disabled={busy}
          aria-busy={busy}
        >
          {t(lang, "evalSubmitReview")}
        </button>
      </div>
      <MessageLine message={message} />
    </form>
  );
}
