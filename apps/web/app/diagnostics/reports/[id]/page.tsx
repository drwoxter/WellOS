"use client";

import Link from "next/link";
import { use, useState } from "react";
import { AppShell } from "../../../chrome";
import { t, type Lang } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import { aiAvailability } from "@/lib/capabilities";
import {
  DISPOSITIONS,
  NO_DIAGNOSTICS_CAPABILITIES,
  criticalityLabel,
  criticalityTone,
  dispositionLabel,
  draftExplanation,
  interpretationTone,
  isExplanationOutput,
  isSynthesisOutput,
  loadReport,
  releaseReport,
  reportStatusLabel,
  reviewExplanation,
  reviewReport,
  staffDocumentDownloadPath,
  synthesizeReport,
  valueText,
  type Disposition,
  type ReportArtifact,
  type ReportDetail,
  criticalityRuleText,
} from "@/lib/diagnostics";
import {
  ConfirmBox,
  MessageLine,
  PanelState,
  ReasonField,
  StatusBadge,
  useAction,
  useLoader,
} from "../../../scheduling/shared";

function ArtifactMeta({ lang, a }: { lang: Lang; a: ReportArtifact }) {
  return (
    <p className="muted small">
      {a.template}
      {a.model ? ` · ${a.model}` : ""}
      {a.synthetic ? ` · ${t(lang, "aiSyntheticProvider")}` : ""}
      {a.generated_at ? ` · ${formatDateTime(lang, a.generated_at)}` : ""}
      {a.review_decision
        ? ` · ${a.review_decision === "approved" ? t(lang, "dxApproved") : t(lang, "dxRejected")}`
        : ""}
    </p>
  );
}

/** Latest usable synthesis for the current report version, if any. */
export function currentSynthesis(r: ReportDetail): ReportArtifact | null {
  return (
    r.synthesis.find(
      (a) =>
        (a.report_version ?? r.version) === r.version &&
        a.status !== "superseded" &&
        a.status !== "invalidated",
    ) ?? null
  );
}

/** Latest approved explanation for the current report version, if any. */
export function approvedExplanation(r: ReportDetail): ReportArtifact | null {
  return (
    r.explanations.find(
      (a) =>
        (a.report_version ?? r.version) === r.version &&
        a.review_decision === "approved",
    ) ?? null
  );
}

function Synthesis({
  lang,
  report,
  onDone,
}: {
  lang: Lang;
  report: ReportDetail;
  onDone: () => void;
}) {
  const { meta } = useSession();
  const avail = aiAvailability(lang, meta?.ai_capabilities, "model");
  const { busy, message, run } = useAction(lang);
  const current = currentSynthesis(report);
  const out =
    current && isSynthesisOutput(current.output) ? current.output : null;
  return (
    <section
      className="card diag-ai"
      aria-labelledby="dx-synth-h"
      data-testid="dx-synthesis"
    >
      <h3 id="dx-synth-h">{t(lang, "dxSynthesisTitle")}</h3>
      <p className="muted small">{t(lang, "aiAssistiveOnly")}</p>
      {current ? (
        <div data-testid="dx-synthesis-draft">
          <ArtifactMeta lang={lang} a={current} />
          {out ? (
            <>
              <p>{out.summary}</p>
              {out.components.length > 0 ? (
                <ul className="brief-list">
                  {out.components.map((c) => (
                    <li key={c.component_ref}>
                      <StatusBadge
                        label={c.interpretation}
                        tone={interpretationTone(c.interpretation)}
                      />{" "}
                      {c.statement}
                      {c.change_from_prior ? (
                        <span className="muted"> · {c.change_from_prior}</span>
                      ) : null}
                    </li>
                  ))}
                </ul>
              ) : null}
              {out.contradictions.length > 0 ? (
                <p className="warn-text">
                  {t(lang, "dxContradictions")}: {out.contradictions.join("; ")}
                </p>
              ) : null}
              {out.missing_information.length > 0 ? (
                <p className="muted">
                  {t(lang, "dxMissingInformation")}:{" "}
                  {out.missing_information.join("; ")}
                </p>
              ) : null}
              {out.limitations.length > 0 ? (
                <p className="muted small">
                  {t(lang, "limitations")}: {out.limitations.join("; ")}
                </p>
              ) : null}
            </>
          ) : (
            <p className="muted">{t(lang, "aiDraftStale")}</p>
          )}
        </div>
      ) : avail.callable ? (
        <button
          type="button"
          className="secondary"
          disabled={busy}
          data-testid="dx-synthesize"
          onClick={() =>
            void run(async () => {
              await synthesizeReport(report.id, lang);
            }).then((ok) => ok && onDone())
          }
        >
          {t(lang, "dxGenerateSynthesis")}
        </button>
      ) : (
        <p
          className="muted"
          role="status"
          data-testid="dx-synthesis-unavailable"
        >
          {avail.notice ?? t(lang, "aiUnavailable")}
        </p>
      )}
      <MessageLine message={message} />
    </section>
  );
}

function ReviewForm({
  lang,
  report,
  onDone,
}: {
  lang: Lang;
  report: ReportDetail;
  onDone: () => void;
}) {
  const { busy, message, run } = useAction(lang);
  const [assessment, setAssessment] = useState("");
  const [disposition, setDisposition] = useState<Disposition>(
    report.criticality === "critical" ? "immediate_contact" : "no_action",
  );
  const [note, setNote] = useState("");
  const [followUp, setFollowUp] = useState("");
  const [synthDecision, setSynthDecision] = useState<
    "approved" | "rejected" | ""
  >("");
  const synth = currentSynthesis(report);
  const synthPending = synth !== null && synth.review_decision === null;
  const valid =
    assessment.trim().length >= 10 &&
    (disposition !== "other" || note.trim().length >= 3) &&
    (!synthPending || synthDecision !== "");
  return (
    <form
      className="card nested"
      aria-labelledby="dx-review-h"
      data-testid="dx-review-form"
      onSubmit={(e) => {
        e.preventDefault();
        void run(async () => {
          await reviewReport(report.id, {
            report_version: report.version,
            clinical_assessment: assessment.trim(),
            disposition,
            ...(note.trim() ? { disposition_note: note.trim() } : {}),
            follow_ups: followUp.trim()
              ? [
                  {
                    description: followUp.trim(),
                    priority:
                      report.criticality === "critical" ? "urgent" : "routine",
                  },
                ]
              : [],
            ...(synth && synthDecision
              ? {
                  synthesis_artifact_id: synth.id,
                  synthesis_decision: synthDecision,
                }
              : {}),
          });
        }, "dxReviewRecorded").then((ok) => ok && onDone());
      }}
    >
      <h4 id="dx-review-h">{t(lang, "dxProfessionalReview")}</h4>
      {report.criticality === "critical" ? (
        <p
          className="critical-banner"
          role="alert"
          data-testid="dx-critical-banner"
        >
          {t(lang, "dxCriticalReviewRequired")}
        </p>
      ) : null}
      <label>
        {t(lang, "dxClinicalAssessment")} *
        <textarea
          rows={3}
          value={assessment}
          onChange={(e) => setAssessment(e.target.value)}
          required
          data-testid="dx-assessment"
        />
      </label>
      <div className="grid-2">
        <label>
          {t(lang, "dxDisposition")}
          <select
            value={disposition}
            onChange={(e) => setDisposition(e.target.value as Disposition)}
            data-testid="dx-disposition"
          >
            {DISPOSITIONS.map((d) => (
              <option key={d} value={d}>
                {dispositionLabel(lang, d)}
              </option>
            ))}
          </select>
        </label>
        <label>
          {t(lang, "dxDispositionNote")}
          {disposition === "other" ? " *" : ""}
          <input value={note} onChange={(e) => setNote(e.target.value)} />
        </label>
      </div>
      <label>
        {t(lang, "dxFollowUpTask")}
        <input
          value={followUp}
          onChange={(e) => setFollowUp(e.target.value)}
          placeholder={t(lang, "dxFollowUpTaskHint")}
          data-testid="dx-follow-up"
        />
      </label>
      {synthPending ? (
        <fieldset className="dx-synth-decision" data-testid="dx-synth-decision">
          <legend>{t(lang, "dxSynthesisDecision")}</legend>
          <label className="check-option">
            <input
              type="radio"
              name="synth"
              checked={synthDecision === "approved"}
              onChange={() => setSynthDecision("approved")}
            />
            {t(lang, "aiAcceptDraft")}
          </label>
          <label className="check-option">
            <input
              type="radio"
              name="synth"
              checked={synthDecision === "rejected"}
              onChange={() => setSynthDecision("rejected")}
            />
            {t(lang, "aiRejectDraft")}
          </label>
        </fieldset>
      ) : null}
      <button
        type="submit"
        className="primary"
        disabled={busy || !valid}
        data-testid="dx-submit-review"
      >
        {t(lang, "dxRecordReview")}
      </button>
      <MessageLine message={message} />
    </form>
  );
}

function Explanation({
  lang,
  report,
  onDone,
}: {
  lang: Lang;
  report: ReportDetail;
  onDone: () => void;
}) {
  const { meta } = useSession();
  const avail = aiAvailability(lang, meta?.ai_capabilities, "model");
  const { busy, message, run } = useAction(lang);
  const [note, setNote] = useState("");
  const pending =
    report.explanations.find(
      (a) =>
        (a.report_version ?? report.version) === report.version &&
        a.review_decision === null &&
        a.status !== "superseded",
    ) ?? null;
  const approved = approvedExplanation(report);
  const shown = approved ?? pending;
  const out = shown && isExplanationOutput(shown.output) ? shown.output : null;
  return (
    <section
      className="card diag-ai"
      aria-labelledby="dx-expl-h"
      data-testid="dx-explanation"
    >
      <h3 id="dx-expl-h">{t(lang, "dxPatientExplanation")}</h3>
      <p className="muted small">{t(lang, "dxPatientExplanationHint")}</p>
      {shown ? (
        <div
          data-testid={
            approved ? "dx-explanation-approved" : "dx-explanation-pending"
          }
        >
          <ArtifactMeta lang={lang} a={shown} />
          {out ? (
            <>
              <p lang="en">{out.explanation_en}</p>
              <p lang="es">{out.explanation_es}</p>
              <p className="muted small" lang="en">
                {out.next_steps_en}
              </p>
              <p className="muted small" lang="es">
                {out.next_steps_es}
              </p>
            </>
          ) : null}
          {pending && !approved ? (
            <div className="visit-actions">
              <ReasonField
                id="dx-expl-note"
                lang={lang}
                value={note}
                onChange={setNote}
                label={t(lang, "dxNote")}
              />
              <button
                type="button"
                className="secondary"
                disabled={busy}
                data-testid="dx-approve-explanation"
                onClick={() =>
                  void run(async () => {
                    await reviewExplanation(report.id, pending.id, {
                      decision: "approved",
                      ...(note.trim() ? { note: note.trim() } : {}),
                    });
                  }, "aiDraftAccepted").then((ok) => ok && onDone())
                }
              >
                {t(lang, "aiAcceptDraft")}
              </button>
              <button
                type="button"
                className="tertiary"
                disabled={busy}
                data-testid="dx-reject-explanation"
                onClick={() =>
                  void run(async () => {
                    await reviewExplanation(report.id, pending.id, {
                      decision: "rejected",
                      ...(note.trim() ? { note: note.trim() } : {}),
                    });
                  }, "aiDraftRejected").then((ok) => ok && onDone())
                }
              >
                {t(lang, "aiRejectDraft")}
              </button>
            </div>
          ) : null}
        </div>
      ) : avail.callable ? (
        <button
          type="button"
          className="secondary"
          disabled={busy}
          data-testid="dx-draft-explanation"
          onClick={() =>
            void run(async () => {
              await draftExplanation(report.id);
            }).then((ok) => ok && onDone())
          }
        >
          {t(lang, "dxDraftExplanation")}
        </button>
      ) : (
        <p
          className="muted"
          role="status"
          data-testid="dx-explanation-unavailable"
        >
          {avail.notice ?? t(lang, "aiUnavailable")}
        </p>
      )}
      <MessageLine message={message} />
    </section>
  );
}

function Release({
  lang,
  report,
  onDone,
}: {
  lang: Lang;
  report: ReportDetail;
  onDone: () => void;
}) {
  const { busy, message, run } = useAction(lang);
  const review =
    report.reviews.find((r) => r.report_version === report.version) ?? null;
  const approved = approvedExplanation(report);
  const approvedOut =
    approved && isExplanationOutput(approved.output) ? approved.output : null;
  const [decision, setDecision] = useState<"release" | "withhold">("release");
  const [withholdReason, setWithholdReason] = useState("");
  const [notify, setNotify] = useState(true);
  const [releaseDocs, setReleaseDocs] = useState(false);
  const [explEn, setExplEn] = useState(approvedOut?.explanation_en ?? "");
  const [explEs, setExplEs] = useState(approvedOut?.explanation_es ?? "");
  const [confirming, setConfirming] = useState(false);
  const needsExplanation = report.criticality !== "normal";
  const explanationOk =
    !needsExplanation ||
    approved !== null ||
    (explEn.trim().length >= 10 && explEs.trim().length >= 10);
  const valid =
    review !== null &&
    (decision === "withhold"
      ? withholdReason.trim().length >= 3
      : explanationOk);
  async function submit() {
    if (!review) return;
    const ok = await run(
      async () => {
        await releaseReport(report.id, {
          report_version: report.version,
          review_id: review.id,
          decision,
          notify_patient: decision === "release" && notify,
          ...(decision === "withhold"
            ? { withhold_reason: withholdReason.trim() }
            : {}),
          ...(decision === "release"
            ? {
                ...(approved ? { explanation_artifact_id: approved.id } : {}),
                ...(explEn.trim() ? { explanation_en: explEn.trim() } : {}),
                ...(explEs.trim() ? { explanation_es: explEs.trim() } : {}),
                release_documents: releaseDocs,
              }
            : {}),
        });
      },
      decision === "release" ? "dxReleased" : "dxWithheld",
    );
    setConfirming(false);
    if (ok) onDone();
  }
  return (
    <section
      className="card nested"
      aria-labelledby="dx-release-h"
      data-testid="dx-release"
    >
      <h4 id="dx-release-h">{t(lang, "dxReleaseDecision")}</h4>
      {!review ? (
        <p className="muted" data-testid="dx-release-needs-review">
          {t(lang, "dxReleaseNeedsReview")}
        </p>
      ) : (
        <>
          <p className="muted small">{t(lang, "dxReleaseHint")}</p>
          <div className="grid-2">
            <label>
              {t(lang, "dxDecision")}
              <select
                value={decision}
                onChange={(e) =>
                  setDecision(e.target.value as "release" | "withhold")
                }
                data-testid="dx-release-decision"
              >
                <option value="release">{t(lang, "dxDecisionRelease")}</option>
                <option value="withhold">
                  {t(lang, "dxDecisionWithhold")}
                </option>
              </select>
            </label>
            {decision === "release" ? (
              <>
                <label className="check-option">
                  <input
                    type="checkbox"
                    checked={notify}
                    onChange={(e) => setNotify(e.target.checked)}
                    data-testid="dx-notify-patient"
                  />
                  {t(lang, "dxNotifyPatient")}
                </label>
                {report.documents.length > 0 ? (
                  <label className="check-option">
                    <input
                      type="checkbox"
                      checked={releaseDocs}
                      onChange={(e) => setReleaseDocs(e.target.checked)}
                    />
                    {t(lang, "dxReleaseDocuments")}
                  </label>
                ) : null}
              </>
            ) : (
              <ReasonField
                id="dx-withhold-reason"
                lang={lang}
                value={withholdReason}
                onChange={setWithholdReason}
                required
                label={t(lang, "dxWithholdReason")}
              />
            )}
          </div>
          {decision === "release" && !approved ? (
            <div className="grid-2">
              <label>
                {t(lang, "dxExplanationEn")}
                {needsExplanation ? " *" : ""}
                <textarea
                  rows={3}
                  value={explEn}
                  onChange={(e) => setExplEn(e.target.value)}
                  data-testid="dx-explanation-en"
                />
              </label>
              <label>
                {t(lang, "dxExplanationEs")}
                {needsExplanation ? " *" : ""}
                <textarea
                  rows={3}
                  value={explEs}
                  onChange={(e) => setExplEs(e.target.value)}
                  data-testid="dx-explanation-es"
                />
              </label>
            </div>
          ) : decision === "release" && approved ? (
            <p className="muted small">
              {t(lang, "dxUsingApprovedExplanation")}
            </p>
          ) : null}
          {confirming ? (
            <ConfirmBox
              lang={lang}
              title={
                decision === "release"
                  ? t(lang, "dxDecisionRelease")
                  : t(lang, "dxDecisionWithhold")
              }
              onConfirm={() => void submit()}
              onCancel={() => setConfirming(false)}
              busy={busy}
              disabled={!valid}
            >
              <p>{t(lang, "dxReleaseConfirm")}</p>
            </ConfirmBox>
          ) : (
            <button
              type="button"
              className="primary"
              disabled={busy || !valid}
              onClick={() => setConfirming(true)}
              data-testid="dx-release-submit"
            >
              {decision === "release"
                ? t(lang, "dxDecisionRelease")
                : t(lang, "dxDecisionWithhold")}
            </button>
          )}
        </>
      )}
      <MessageLine message={message} />
    </section>
  );
}

function ReportView({ id }: { id: string }) {
  const { lang, meta } = useSession();
  const caps = meta?.diagnostics_capabilities ?? NO_DIAGNOSTICS_CAPABILITIES;
  const [tick, setTick] = useState(0);
  const state = useLoader(() => loadReport(id), `${id}:${tick}`);
  const reload = () => setTick((n) => n + 1);
  return (
    <PanelState
      lang={lang}
      state={state}
      emptyKey="dxNoReports"
      isEmpty={() => false}
    >
      {(r) => {
        const released = r.release_decisions.some(
          (d) =>
            d.decision === "release" &&
            d.report_version === r.version &&
            d.superseded_at === null,
        );
        const reviewed = r.reviews.some((v) => v.report_version === r.version);
        const current = r.replaced_by === null;
        return (
          <div
            className="dx-report"
            data-testid="dx-report-detail"
            data-status={r.status}
            data-criticality={r.criticality}
          >
            <header className="page-head">
              <div>
                <h2 style={{ marginBottom: 4 }}>
                  {r.order.display} · {t(lang, "dxReport")} v{r.version}
                </h2>
                <p className="muted">
                  {r.patient ? (
                    <Link
                      className="navlink"
                      href={`/patients/${r.patient_id}/360`}
                    >
                      {r.patient.family_name}, {r.patient.given_name}
                    </Link>
                  ) : null}{" "}
                  ·{" "}
                  <Link
                    className="navlink"
                    href={`/diagnostics/orders/${r.service_request_id}`}
                  >
                    {t(lang, "dxOrder")}
                  </Link>{" "}
                  · {formatDateTime(lang, r.issued_at ?? r.created_at)}
                  {r.signed_at
                    ? ` · ${t(lang, "dxSigned")} ${formatDateTime(lang, r.signed_at)}`
                    : ""}
                </p>
              </div>
              <div className="row-head">
                <StatusBadge
                  label={reportStatusLabel(lang, r.status)}
                  tone="neutral"
                />
                <StatusBadge
                  label={criticalityLabel(lang, r.criticality)}
                  tone={criticalityTone(r.criticality)}
                />
                {released ? (
                  <StatusBadge label={t(lang, "dxReleased")} tone="ok" />
                ) : null}
                {reviewed && !released ? (
                  <StatusBadge label={t(lang, "dxReviewReviewed")} tone="ok" />
                ) : null}
              </div>
            </header>
            {!current ? (
              <p className="advisory" role="status" data-testid="dx-superseded">
                {t(lang, "dxReportSuperseded")}{" "}
                <Link
                  className="navlink"
                  href={`/diagnostics/reports/${r.replaced_by}`}
                >
                  {t(lang, "dxOpenLatestVersion")}
                </Link>
              </p>
            ) : null}
            {r.replaces ? (
              <p className="muted small">
                {t(lang, "dxReplaces")}{" "}
                <Link
                  className="navlink"
                  href={`/diagnostics/reports/${r.replaces}`}
                >
                  v{r.version - 1}
                </Link>
                {r.change_reason ? ` · ${r.change_reason}` : ""}
              </p>
            ) : null}
            {r.criticality === "critical" ? (
              <p className="critical-banner" role="alert">
                {t(lang, "dxCriticalResult")}
                {r.criticality_rules.length > 0
                  ? ` · ${r.criticality_rules.map(criticalityRuleText).join(", ")}`
                  : ""}
              </p>
            ) : null}

            <section className="card" aria-labelledby="dx-comp-h">
              <h3 id="dx-comp-h">{t(lang, "dxComponents")}</h3>
              <table
                className="data-table dx-components"
                data-testid="dx-component-table"
              >
                <caption className="sr-only">{t(lang, "dxComponents")}</caption>
                <thead>
                  <tr>
                    <th scope="col">{t(lang, "dxTest")}</th>
                    <th scope="col">{t(lang, "value")}</th>
                    <th scope="col">{t(lang, "referenceRange")}</th>
                    <th scope="col">{t(lang, "dxInterpretation")}</th>
                    <th scope="col">{t(lang, "status")}</th>
                  </tr>
                </thead>
                <tbody>
                  {r.components.map((c) => (
                    <tr
                      key={c.id}
                      className={c.superseded ? "superseded" : undefined}
                      data-testid="dx-component"
                    >
                      <td>
                        {c.display ?? c.code}
                        <span className="muted small"> {c.code}</span>
                      </td>
                      <td>
                        <strong>{valueText(lang, c.value)}</strong>
                      </td>
                      <td>{c.reference_range ?? "—"}</td>
                      <td>
                        {c.interpretation ? (
                          <StatusBadge
                            label={c.interpretation}
                            tone={interpretationTone(c.interpretation)}
                          />
                        ) : (
                          "—"
                        )}
                      </td>
                      <td>
                        {c.superseded ? t(lang, "superseded") : c.status}
                        {c.amends ? ` · ${t(lang, "dxAmends")}` : ""}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
              {r.conclusion ? (
                <p>
                  <strong>{t(lang, "dxConclusion")}:</strong> {r.conclusion}
                </p>
              ) : null}
              {r.source_system ? (
                <p className="muted small">
                  {t(lang, "dxSource")}: {r.source_system}
                  {r.external_report_id ? ` · ${r.external_report_id}` : ""}
                </p>
              ) : null}
            </section>

            {r.documents.length > 0 || r.imaging_studies.length > 0 ? (
              <section className="card" aria-labelledby="dx-rdocs-h">
                <h3 id="dx-rdocs-h">{t(lang, "dxDocumentsAndImaging")}</h3>
                <ul className="brief-list">
                  {r.documents.map((d) => (
                    <li key={d.id}>
                      {d.downloadable ? (
                        <a
                          className="navlink"
                          href={staffDocumentDownloadPath(d.id)}
                        >
                          {d.title}
                        </a>
                      ) : (
                        <span>{d.title}</span>
                      )}{" "}
                      <StatusBadge
                        label={d.status}
                        tone={d.status === "clean" ? "ok" : "warn"}
                      />
                      {d.released ? (
                        <span className="muted">
                          {" "}
                          · {t(lang, "dxReleased")}
                        </span>
                      ) : null}
                    </li>
                  ))}
                  {r.imaging_studies.map((s) => (
                    <li key={s.id}>
                      <strong>{s.modality_code}</strong> {s.description ?? ""} ·{" "}
                      <code>{s.accession_number ?? s.study_instance_uid}</code>
                    </li>
                  ))}
                </ul>
              </section>
            ) : null}

            {caps.can_review && current && r.reviewable ? (
              <Synthesis lang={lang} report={r} onDone={reload} />
            ) : null}

            <section className="card" aria-labelledby="dx-reviews-h">
              <h3 id="dx-reviews-h">{t(lang, "dxProfessionalReview")}</h3>
              {r.reviews.length === 0 ? (
                <p className="muted">{t(lang, "dxNotReviewed")}</p>
              ) : (
                <ul className="brief-list" data-testid="dx-reviews-list">
                  {r.reviews.map((v) => (
                    <li key={v.id}>
                      <strong>{dispositionLabel(lang, v.disposition)}</strong> ·{" "}
                      {v.reviewer_name ?? v.reviewer_id} ·{" "}
                      {formatDateTime(lang, v.reviewed_at)} · v
                      {v.report_version}
                      <br />
                      {v.clinical_assessment}
                      {v.disposition_note ? ` · ${v.disposition_note}` : ""}
                      {v.follow_up_task_ids.length > 0
                        ? ` · ${t(lang, "dxFollowUpTasks")}: ${v.follow_up_task_ids.length}`
                        : ""}
                    </li>
                  ))}
                </ul>
              )}
              {caps.can_review && current && r.reviewable && !reviewed ? (
                <ReviewForm lang={lang} report={r} onDone={reload} />
              ) : null}
            </section>

            {caps.can_release && current && r.reviewable ? (
              <Explanation lang={lang} report={r} onDone={reload} />
            ) : null}

            <section className="card" aria-labelledby="dx-rel-h">
              <h3 id="dx-rel-h">{t(lang, "dxPatientRelease")}</h3>
              {r.release_decisions.length === 0 ? (
                <p className="muted">{t(lang, "dxNotReleased")}</p>
              ) : (
                <ul className="brief-list" data-testid="dx-release-list">
                  {r.release_decisions.map((d) => (
                    <li key={d.id}>
                      <StatusBadge
                        label={
                          d.decision === "release"
                            ? t(lang, "dxDecisionRelease")
                            : t(lang, "dxDecisionWithhold")
                        }
                        tone={d.decision === "release" ? "ok" : "warn"}
                      />{" "}
                      v{d.report_version} · {formatDateTime(lang, d.decided_at)}
                      {d.decided_by ? ` · ${d.decided_by}` : ""}
                      {d.notify_patient
                        ? ` · ${t(lang, "dxNotifyPatient")}`
                        : ""}
                      {d.withhold_reason ? ` · ${d.withhold_reason}` : ""}
                      {d.superseded_at ? ` · ${t(lang, "superseded")}` : ""}
                    </li>
                  ))}
                </ul>
              )}
              {caps.can_release && current && r.reviewable && !released ? (
                <Release lang={lang} report={r} onDone={reload} />
              ) : null}
            </section>
            <p>
              <Link className="navlink" href="/diagnostics">
                ← {t(lang, "navDiagnostics")}
              </Link>
            </p>
          </div>
        );
      }}
    </PanelState>
  );
}

export default function DiagnosticReportPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = use(params);
  return (
    <AppShell>
      <ReportView id={id} />
    </AppShell>
  );
}
