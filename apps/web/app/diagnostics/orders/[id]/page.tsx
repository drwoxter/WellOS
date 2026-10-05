"use client";

import Link from "next/link";
import { use, useMemo, useState } from "react";
import { AppShell } from "../../../chrome";
import { t, type Lang } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import {
  FULFILMENT_MODES,
  NO_DIAGNOSTICS_CAPABILITIES,
  RESULT_TYPES,
  availableTransitions,
  categoryLabel,
  criticalityLabel,
  criticalityTone,
  findingKindLabel,
  fulfilmentModeLabel,
  issueReport,
  loadOrder,
  newIdempotencyKey,
  orderStatusLabel,
  orderStatusTone,
  parseValue,
  preparationFor,
  priorityLabel,
  recordSpecimen,
  reportStatusLabel,
  scheduleOrder,
  specimenEvent,
  specimenEventLabel,
  specimenEventsFor,
  specimenStatusLabel,
  staffDocumentDownloadPath,
  transitionLabel,
  transitionNeedsReason,
  transitionOrder,
  type ComponentInput,
  type FulfilmentMode,
  type OrderDetail,
  type ResultType,
  type Specimen,
  type Transition,
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

const IMMEDIATE_MODES: FulfilmentMode[] = FULFILMENT_MODES.filter(
  (m) => m !== "scheduled",
);

function Transitions({
  lang,
  order,
  onDone,
}: {
  lang: Lang;
  order: OrderDetail;
  onDone: () => void;
}) {
  const { busy, message, run } = useAction(lang);
  const [pending, setPending] = useState<Transition | null>(null);
  const [reason, setReason] = useState("");
  const [mode, setMode] = useState<FulfilmentMode>(
    order.fulfilment_mode === "scheduled" ? "immediate" : order.fulfilment_mode,
  );
  const available = availableTransitions(order.order_status);
  const startWithoutAppointment =
    pending === "start" &&
    order.fulfilment_mode === "scheduled" &&
    !order.appointment_id;
  const needsReason = pending ? transitionNeedsReason(pending) : false;
  const reasonOk = !needsReason || reason.trim().length >= 3;

  async function submit() {
    if (!pending) return;
    const tr = pending;
    const ok = await run(async () => {
      await transitionOrder(order.id, {
        transition: tr,
        version: order.version,
        ...(needsReason ? { reason: reason.trim() } : {}),
        ...(startWithoutAppointment ? { fulfilment_mode: mode } : {}),
      });
    }, "dxTransitionRecorded");
    if (ok) {
      setPending(null);
      setReason("");
      onDone();
    }
  }

  if (available.length === 0) return null;
  return (
    <div className="dx-transitions" data-testid="dx-transitions">
      <div className="visit-actions">
        {available.map((tr) => (
          <button
            key={tr}
            type="button"
            className={
              tr === "cancel" || tr === "reject" || tr === "enter_in_error"
                ? "tertiary"
                : "secondary"
            }
            disabled={busy}
            onClick={() => {
              setPending(tr);
              setReason("");
            }}
            data-testid={`dx-tr-${tr}`}
          >
            {transitionLabel(lang, tr)}
          </button>
        ))}
      </div>
      {pending ? (
        <ConfirmBox
          lang={lang}
          title={transitionLabel(lang, pending)}
          onConfirm={() => void submit()}
          onCancel={() => setPending(null)}
          busy={busy}
          disabled={!reasonOk}
          confirmLabel={transitionLabel(lang, pending)}
        >
          {startWithoutAppointment ? (
            <label>
              {t(lang, "dxFulfilmentMode")}
              <select
                value={mode}
                onChange={(e) => setMode(e.target.value as FulfilmentMode)}
                data-testid="dx-start-mode"
              >
                {IMMEDIATE_MODES.map((m) => (
                  <option key={m} value={m}>
                    {fulfilmentModeLabel(lang, m)}
                  </option>
                ))}
              </select>
              <span className="muted small">
                {" "}
                {t(lang, "dxStartWithoutAppointmentHint")}
              </span>
            </label>
          ) : null}
          {needsReason ? (
            <ReasonField
              id="dx-tr-reason"
              lang={lang}
              value={reason}
              onChange={setReason}
              required
            />
          ) : null}
        </ConfirmBox>
      ) : null}
      <MessageLine message={message} />
    </div>
  );
}

function Scheduling({
  lang,
  order,
  onDone,
}: {
  lang: Lang;
  order: OrderDetail;
  onDone: () => void;
}) {
  const { busy, message, run } = useAction(lang);
  const canRequest =
    order.fulfilment_mode === "scheduled" &&
    !order.appointment_id &&
    !order.access_request_id &&
    (order.order_status === "placed" || order.order_status === "accepted");
  return (
    <section className="card nested" aria-labelledby="dx-sched-h">
      <h4 id="dx-sched-h">{t(lang, "dxScheduling")}</h4>
      {order.appointment ? (
        <p data-testid="dx-appointment">
          <strong>{formatDateTime(lang, order.appointment.starts_at)}</strong> ·{" "}
          {order.appointment.facility_name} ·{" "}
          <StatusBadge label={order.appointment.status} tone="neutral" />{" "}
          <Link className="navlink" href="/scheduling">
            {t(lang, "navScheduling")}
          </Link>
        </p>
      ) : order.access_request_id ? (
        <p className="muted" data-testid="dx-access-request">
          {t(lang, "dxAccessRequestOpen")}{" "}
          <Link
            className="navlink"
            href={`/scheduling/requests/${order.access_request_id}`}
          >
            {t(lang, "open")}
          </Link>
        </p>
      ) : (
        <p className="muted">{t(lang, "dxNoAppointment")}</p>
      )}
      {order.schedule_conflict ? (
        <p className="advisory" role="status" data-testid="dx-conflict">
          {t(lang, "dxScheduleConflict")}: {order.schedule_conflict}
        </p>
      ) : null}
      {canRequest ? (
        <button
          type="button"
          className="secondary"
          disabled={busy}
          data-testid="dx-request-appointment"
          onClick={() =>
            void run(async () => {
              await scheduleOrder(order.id, {
                version: order.version,
                ...(order.performing_facility_id
                  ? { facility_id: order.performing_facility_id }
                  : {}),
                lang,
              });
            }, "dxAccessRequestCreated").then((ok) => ok && onDone())
          }
        >
          {t(lang, "dxRequestAppointment")}
        </button>
      ) : null}
      <MessageLine message={message} />
    </section>
  );
}

function SpecimenRow({
  lang,
  specimen,
  canHandle,
  onDone,
}: {
  lang: Lang;
  specimen: Specimen;
  canHandle: boolean;
  onDone: () => void;
}) {
  const { busy, message, run } = useAction(lang);
  const [reason, setReason] = useState("");
  const events = canHandle ? specimenEventsFor(specimen.status) : [];
  return (
    <li
      className="row-card"
      data-testid="dx-specimen"
      data-status={specimen.status}
    >
      <div className="row-main">
        <div className="row-head">
          <code>{specimen.identifier}</code>
          <StatusBadge
            label={specimenStatusLabel(lang, specimen.status)}
            tone={specimen.status === "rejected" ? "warn" : "neutral"}
          />
        </div>
        <p className="muted">
          {specimen.specimen_type_code}
          {specimen.container_code ? ` · ${specimen.container_code}` : ""}
          {specimen.collected_at
            ? ` · ${t(lang, "dxCollectedAt")} ${formatDateTime(lang, specimen.collected_at)}`
            : ""}
          {specimen.rejection_reason ? ` · ${specimen.rejection_reason}` : ""}
        </p>
        {events.length > 0 ? (
          <div className="visit-actions">
            {events.includes("rejected") ? (
              <ReasonField
                id={`sp-reason-${specimen.id}`}
                lang={lang}
                value={reason}
                onChange={setReason}
                label={t(lang, "dxRejectionReason")}
              />
            ) : null}
            {events.map((ev) => (
              <button
                key={ev}
                type="button"
                className={ev === "rejected" ? "tertiary" : "secondary"}
                disabled={
                  busy || (ev === "rejected" && reason.trim().length < 3)
                }
                data-testid={`dx-sp-${ev}`}
                onClick={() =>
                  void run(async () => {
                    await specimenEvent(specimen.id, {
                      event: ev,
                      version: specimen.version,
                      ...(ev === "rejected" ? { reason: reason.trim() } : {}),
                    });
                  }, "dxSpecimenUpdated").then((ok) => ok && onDone())
                }
              >
                {specimenEventLabel(lang, ev)}
              </button>
            ))}
          </div>
        ) : null}
        <MessageLine message={message} />
      </div>
    </li>
  );
}

function RecordSpecimen({
  lang,
  order,
  onDone,
}: {
  lang: Lang;
  order: OrderDetail;
  onDone: () => void;
}) {
  const { busy, message, run } = useAction(lang);
  const [type, setType] = useState("blood_venous");
  const [container, setContainer] = useState("");
  const [identifier, setIdentifier] = useState("");
  const [collected, setCollected] = useState(true);
  return (
    <form
      className="card nested"
      aria-labelledby="dx-rec-sp-h"
      data-testid="dx-record-specimen"
      onSubmit={(e) => {
        e.preventDefault();
        void run(async () => {
          await recordSpecimen(order.id, {
            specimen_type_code: type.trim(),
            ...(container.trim() ? { container_code: container.trim() } : {}),
            ...(identifier.trim() ? { identifier: identifier.trim() } : {}),
            collected,
            ...(order.performing_facility_id
              ? { collection_facility_id: order.performing_facility_id }
              : {}),
          });
        }, "dxSpecimenRecorded").then((ok) => ok && onDone());
      }}
    >
      <h4 id="dx-rec-sp-h">{t(lang, "dxRecordSpecimen")}</h4>
      <div className="grid-2">
        <label>
          {t(lang, "dxSpecimenType")}
          <input
            value={type}
            onChange={(e) => setType(e.target.value)}
            required
            data-testid="dx-sp-type"
          />
        </label>
        <label>
          {t(lang, "dxContainer")}
          <input
            value={container}
            onChange={(e) => setContainer(e.target.value)}
          />
        </label>
        <label>
          {t(lang, "dxSpecimenIdentifier")}
          <input
            value={identifier}
            onChange={(e) => setIdentifier(e.target.value)}
            placeholder={t(lang, "dxSpecimenIdentifierHint")}
          />
        </label>
        <label className="check-option">
          <input
            type="checkbox"
            checked={collected}
            onChange={(e) => setCollected(e.target.checked)}
          />
          {t(lang, "dxCollectedNow")}
        </label>
      </div>
      <button
        type="submit"
        className="secondary"
        disabled={busy || !type.trim()}
      >
        {t(lang, "dxRecordSpecimen")}
      </button>
      <MessageLine message={message} />
    </form>
  );
}

type ComponentForm = {
  code: string;
  display: string;
  result_type: ResultType;
  value: string;
  unit: string;
  reference_range: string;
};

function emptyComponent(order: OrderDetail): ComponentForm {
  return {
    code: order.code_loinc ?? order.orderable_code ?? "",
    display: order.display,
    result_type: order.expected_result_type ?? "quantity",
    value: "",
    unit: "",
    reference_range: "",
  };
}

export function ResultEntry({
  lang,
  order,
  onDone,
}: {
  lang: Lang;
  order: OrderDetail;
  onDone: () => void;
}) {
  const { busy, message, run } = useAction(lang);
  const [components, setComponents] = useState<ComponentForm[]>([
    emptyComponent(order),
  ]);
  const [status, setStatus] = useState<"preliminary" | "final">("final");
  const [conclusion, setConclusion] = useState("");
  const [sign, setSign] = useState(true);
  const [idem, setIdem] = useState(newIdempotencyKey);
  const parsed = useMemo(
    () =>
      components.map((c) => ({
        form: c,
        value: c.code.trim()
          ? parseValue(c.result_type, c.value, c.unit)
          : null,
      })),
    [components],
  );
  const valid = parsed.length > 0 && parsed.every((p) => p.value !== null);
  const update = (i: number, patch: Partial<ComponentForm>) =>
    setComponents((cs) => cs.map((c, j) => (j === i ? { ...c, ...patch } : c)));

  async function submit(e: React.FormEvent) {
    e.preventDefault();
    const body: ComponentInput[] = [];
    for (const p of parsed) {
      if (!p.value) return;
      body.push({
        code: p.form.code.trim(),
        display: p.form.display.trim() || undefined,
        value: p.value,
        ...(p.form.reference_range.trim()
          ? { reference_range: p.form.reference_range.trim() }
          : {}),
      });
    }
    const ok = await run(async () => {
      await issueReport(order.id, {
        status,
        components: body,
        ...(conclusion.trim() ? { conclusion: conclusion.trim() } : {}),
        conclusion_codes: [],
        idempotency_key: idem,
        sign,
      });
    }, "dxReportIssued");
    if (ok) {
      setIdem(newIdempotencyKey());
      setComponents([emptyComponent(order)]);
      setConclusion("");
      onDone();
    }
  }

  return (
    <form
      className="card nested dx-result-entry"
      aria-labelledby="dx-result-h"
      data-testid="dx-result-entry"
      onSubmit={(e) => void submit(e)}
    >
      <h4 id="dx-result-h">{t(lang, "dxEnterResults")}</h4>
      <p className="muted small">{t(lang, "dxEnterResultsHint")}</p>
      <table className="data-table dx-components">
        <caption className="sr-only">{t(lang, "dxComponents")}</caption>
        <thead>
          <tr>
            <th scope="col">{t(lang, "dxComponentCode")}</th>
            <th scope="col">{t(lang, "dxComponentDisplay")}</th>
            <th scope="col">{t(lang, "dxResultType")}</th>
            <th scope="col">{t(lang, "value")}</th>
            <th scope="col">{t(lang, "dxUnit")}</th>
            <th scope="col">{t(lang, "referenceRange")}</th>
            <th scope="col">
              <span className="sr-only">{t(lang, "actions")}</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {components.map((c, i) => (
            <tr key={i} data-testid="dx-component-row">
              <td>
                <input
                  aria-label={t(lang, "dxComponentCode")}
                  value={c.code}
                  onChange={(e) => update(i, { code: e.target.value })}
                  required
                />
              </td>
              <td>
                <input
                  aria-label={t(lang, "dxComponentDisplay")}
                  value={c.display}
                  onChange={(e) => update(i, { display: e.target.value })}
                />
              </td>
              <td>
                <select
                  aria-label={t(lang, "dxResultType")}
                  value={c.result_type}
                  onChange={(e) =>
                    update(i, { result_type: e.target.value as ResultType })
                  }
                >
                  {RESULT_TYPES.map((r) => (
                    <option key={r} value={r}>
                      {r}
                    </option>
                  ))}
                </select>
              </td>
              <td>
                <input
                  aria-label={t(lang, "value")}
                  value={c.value}
                  onChange={(e) => update(i, { value: e.target.value })}
                  data-testid="dx-component-value"
                  aria-invalid={c.value.trim().length > 0 && !parsed[i].value}
                />
              </td>
              <td>
                <input
                  aria-label={t(lang, "dxUnit")}
                  value={c.unit}
                  onChange={(e) => update(i, { unit: e.target.value })}
                  disabled={c.result_type !== "quantity"}
                />
              </td>
              <td>
                <input
                  aria-label={t(lang, "referenceRange")}
                  value={c.reference_range}
                  onChange={(e) =>
                    update(i, { reference_range: e.target.value })
                  }
                />
              </td>
              <td>
                <button
                  type="button"
                  className="tertiary"
                  disabled={components.length === 1}
                  aria-label={t(lang, "remove")}
                  onClick={() =>
                    setComponents((cs) => cs.filter((_, j) => j !== i))
                  }
                >
                  ×
                </button>
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      <div className="visit-actions">
        <button
          type="button"
          className="tertiary"
          onClick={() =>
            setComponents((cs) => [
              ...cs,
              { ...emptyComponent(order), code: "", display: "" },
            ])
          }
        >
          {t(lang, "dxAddComponent")}
        </button>
      </div>
      <label>
        {t(lang, "dxConclusion")}
        <textarea
          rows={2}
          value={conclusion}
          onChange={(e) => setConclusion(e.target.value)}
          data-testid="dx-conclusion"
        />
      </label>
      <div className="grid-2">
        <label>
          {t(lang, "status")}
          <select
            value={status}
            onChange={(e) =>
              setStatus(e.target.value as "preliminary" | "final")
            }
          >
            <option value="final">{reportStatusLabel(lang, "final")}</option>
            <option value="preliminary">
              {reportStatusLabel(lang, "preliminary")}
            </option>
          </select>
        </label>
        <label className="check-option">
          <input
            type="checkbox"
            checked={sign}
            onChange={(e) => setSign(e.target.checked)}
          />
          {t(lang, "dxSignReport")}
        </label>
      </div>
      <button
        type="submit"
        className="primary"
        disabled={busy || !valid}
        data-testid="dx-issue-report"
      >
        {t(lang, "dxIssueReport")}
      </button>
      <MessageLine message={message} />
    </form>
  );
}

function OrderView({ id }: { id: string }) {
  const { lang, meta } = useSession();
  const caps = meta?.diagnostics_capabilities ?? NO_DIAGNOSTICS_CAPABILITIES;
  const [tick, setTick] = useState(0);
  const state = useLoader(() => loadOrder(id), `${id}:${tick}`);
  const reload = () => setTick((n) => n + 1);
  return (
    <PanelState
      lang={lang}
      state={state}
      emptyKey="dxNoOrders"
      isEmpty={() => false}
    >
      {(o) => {
        const prep = preparationFor(lang, o);
        const terminal = [
          "completed",
          "cancelled",
          "rejected",
          "entered_in_error",
        ].includes(o.order_status);
        const canFulfil = caps.can_fulfil || caps.can_order;
        return (
          <div
            className="dx-order"
            data-testid="dx-order-detail"
            data-status={o.order_status}
          >
            <header className="page-head">
              <div>
                <h2 style={{ marginBottom: 4 }}>{o.display}</h2>
                <p className="muted">
                  {o.patient ? (
                    <Link
                      className="navlink"
                      href={`/patients/${o.patient_id}/360`}
                    >
                      {o.patient.family_name}, {o.patient.given_name}
                    </Link>
                  ) : null}{" "}
                  · {categoryLabel(lang, o.category_code)} ·{" "}
                  {fulfilmentModeLabel(lang, o.fulfilment_mode)} ·{" "}
                  {priorityLabel(lang, o.priority)} · v{o.version}
                </p>
              </div>
              <div className="row-head">
                <StatusBadge
                  label={orderStatusLabel(lang, o.order_status)}
                  tone={orderStatusTone(o.order_status)}
                />
                {o.schedule_conflict ? (
                  <StatusBadge
                    label={t(lang, "dxScheduleConflict")}
                    tone="warn"
                  />
                ) : null}
              </div>
            </header>
            <section className="card" aria-labelledby="dx-clinical-h">
              <h3 id="dx-clinical-h">{t(lang, "dxClinicalContext")}</h3>
              <dl className="kv">
                <dt>{t(lang, "dxClinicalIndication")}</dt>
                <dd>{o.clinical_indication ?? "—"}</dd>
                <dt>{t(lang, "dxClinicalQuestion")}</dt>
                <dd>{o.clinical_question ?? "—"}</dd>
                {prep ? (
                  <>
                    <dt>{t(lang, "dxPreparation")}</dt>
                    <dd>{prep}</dd>
                  </>
                ) : null}
                {o.hold_reason ? (
                  <>
                    <dt>{t(lang, "dxHoldReason")}</dt>
                    <dd>{o.hold_reason}</dd>
                  </>
                ) : null}
                {o.cancellation_reason ? (
                  <>
                    <dt>{t(lang, "cancellationReason")}</dt>
                    <dd>{o.cancellation_reason}</dd>
                  </>
                ) : null}
                {o.rejection_reason ? (
                  <>
                    <dt>{t(lang, "dxRejectionReason")}</dt>
                    <dd>{o.rejection_reason}</dd>
                  </>
                ) : null}
                {o.encounter_id ? (
                  <>
                    <dt>{t(lang, "consultation")}</dt>
                    <dd>
                      <Link
                        className="navlink"
                        href={`/encounters/${o.encounter_id}`}
                      >
                        {t(lang, "open")}
                      </Link>
                    </dd>
                  </>
                ) : null}
              </dl>
              {canFulfil && !terminal ? (
                <Transitions lang={lang} order={o} onDone={reload} />
              ) : null}
            </section>

            {o.safety ? (
              <section
                className="card"
                aria-labelledby="dx-safety-h"
                data-testid="dx-safety"
              >
                <h3 id="dx-safety-h">{t(lang, "dxSafetyEvaluation")}</h3>
                <p className="muted small">
                  {o.safety.engine_version} ·{" "}
                  {formatDateTime(lang, o.safety.evaluated_at)} ·{" "}
                  {t(lang, "dxWarnings")}: {o.safety.warnings} ·{" "}
                  {t(lang, "dxHardStops")}: {o.safety.hard_stops}
                </p>
                {o.safety.findings.length === 0 ? (
                  <p className="muted">{t(lang, "dxNoFindings")}</p>
                ) : (
                  <ul className="brief-list">
                    {o.safety.findings.map((f) => (
                      <li key={f.id}>
                        <StatusBadge
                          label={findingKindLabel(lang, f.kind)}
                          tone={
                            f.severity === "hard_stop" ? "critical" : "warn"
                          }
                        />{" "}
                        {lang === "es" ? f.text_es : f.text_en}
                        {o.safety?.acknowledged_ids.includes(f.id)
                          ? ` · ${t(lang, "dxAcknowledged")}`
                          : ""}
                      </li>
                    ))}
                  </ul>
                )}
                {o.safety.override_reason ? (
                  <p className="advisory" role="status">
                    {t(lang, "dxOverrideRecorded")}: {o.safety.override_reason}
                  </p>
                ) : null}
              </section>
            ) : null}

            {!terminal ? (
              <Scheduling lang={lang} order={o} onDone={reload} />
            ) : null}

            <section className="card" aria-labelledby="dx-specimens-h">
              <h3 id="dx-specimens-h">{t(lang, "dxSpecimens")}</h3>
              {o.specimens.length === 0 ? (
                <p className="muted">{t(lang, "dxNoSpecimens")}</p>
              ) : (
                <ul className="row-list">
                  {o.specimens.map((s) => (
                    <SpecimenRow
                      key={s.id}
                      lang={lang}
                      specimen={s}
                      canHandle={caps.can_handle_specimens && !terminal}
                      onDone={reload}
                    />
                  ))}
                </ul>
              )}
              {caps.can_handle_specimens &&
              !terminal &&
              o.order_status !== "placed" &&
              o.order_status !== "on_hold" ? (
                <RecordSpecimen lang={lang} order={o} onDone={reload} />
              ) : null}
            </section>

            <section className="card" aria-labelledby="dx-reports-h">
              <h3 id="dx-reports-h">{t(lang, "dxReports")}</h3>
              {o.reports.length === 0 ? (
                <p className="muted">{t(lang, "dxNoReports")}</p>
              ) : (
                <ul className="row-list" data-testid="dx-order-reports">
                  {o.reports.map((r) => (
                    <li key={r.id} className="row-card">
                      <div className="row-main">
                        <div className="row-head">
                          <Link
                            className="navlink"
                            href={`/diagnostics/reports/${r.id}`}
                          >
                            {t(lang, "dxReport")} v{r.version}
                          </Link>
                          <StatusBadge
                            label={reportStatusLabel(lang, r.status)}
                            tone="neutral"
                          />
                          <StatusBadge
                            label={criticalityLabel(lang, r.criticality)}
                            tone={criticalityTone(r.criticality)}
                          />
                        </div>
                        <p className="muted">
                          {formatDateTime(lang, r.issued_at ?? r.created_at)}
                          {r.conclusion ? ` · ${r.conclusion}` : ""}
                        </p>
                      </div>
                    </li>
                  ))}
                </ul>
              )}
              {caps.can_write_reports &&
              (o.order_status === "in_progress" ||
                o.order_status === "completed" ||
                o.order_status === "scheduled" ||
                o.order_status === "accepted") ? (
                <ResultEntry lang={lang} order={o} onDone={reload} />
              ) : null}
            </section>

            {o.documents.length > 0 || o.imaging_studies.length > 0 ? (
              <section className="card" aria-labelledby="dx-docs-h">
                <h3 id="dx-docs-h">{t(lang, "dxDocumentsAndImaging")}</h3>
                <ul className="brief-list" data-testid="dx-documents">
                  {o.documents.map((d) => (
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
                      <span className="muted">
                        {" "}
                        · {d.mime_type}
                        {d.released ? ` · ${t(lang, "dxReleased")}` : ""}
                      </span>
                    </li>
                  ))}
                  {o.imaging_studies.map((s) => (
                    <li key={s.id}>
                      <strong>{s.modality_code}</strong> {s.description ?? ""} ·{" "}
                      <code>{s.accession_number ?? s.study_instance_uid}</code>{" "}
                      · <span className="muted">{s.status}</span>
                    </li>
                  ))}
                </ul>
              </section>
            ) : null}

            <section className="card" aria-labelledby="dx-history-h">
              <h3 id="dx-history-h">{t(lang, "history")}</h3>
              <ol className="timeline" data-testid="dx-history">
                {o.history.map((h) => (
                  <li key={h.id}>
                    <strong>{orderStatusLabel(lang, h.to_status)}</strong>
                    {h.from_status
                      ? ` (${t(lang, "from")} ${orderStatusLabel(lang, h.from_status)})`
                      : ""}{" "}
                    · {formatDateTime(lang, h.recorded_at)}
                    {h.actor ? ` · ${h.actor}` : ""}
                    {h.reason ? ` · ${h.reason}` : ""}
                  </li>
                ))}
              </ol>
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

export default function DiagnosticOrderPage({
  params,
}: {
  params: Promise<{ id: string }>;
}) {
  const { id } = use(params);
  return (
    <AppShell>
      <OrderView id={id} />
    </AppShell>
  );
}
