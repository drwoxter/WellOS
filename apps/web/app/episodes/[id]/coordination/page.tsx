"use client";

/**
 * `/episodes/[id]/coordination` — one episode's operational coordination:
 * versioned state machine with human transitions, multidisciplinary tasks,
 * barriers, escalations, theatre cases, shift handoffs (human or dMind draft
 * accepted/rejected by a human), append-only history and the dMind
 * artifacts bound to the episode. Discharge readiness here is operational,
 * never medical clearance.
 */
import Link from "next/link";
import { useParams } from "next/navigation";
import { useState } from "react";
import { AppShell } from "../../../chrome";
import { t, type Lang, type TKey } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import {
  createTask,
  decideHandoff,
  loadEpisode,
  requestDischargeCoordination,
  requestHandoffDraft,
  releasePlacement,
  transitionEpisode,
  transitionTask,
  type AiOutcome,
  type DischargeCoordinationOut,
  type EpisodeDetail,
  type Evidence,
  type Handoff,
  type ShiftHandoffDraftOut,
  type Task,
} from "@/lib/care-ops";
import { Card, CardHead, Pill } from "@/components/ui/primitives";
import {
  MessageLine,
  PanelState,
  useAction,
  useLoader,
} from "../../../scheduling/shared";
import {
  AiOutcomePanel,
  StatusPill,
  TextList,
  fill,
  useCareOpsCaps,
} from "../../../operations/shared";

const EPISODE_TRANSITIONS: Record<string, { key: TKey; transition: string }[]> =
  {
    planned: [
      { key: "coordActivate", transition: "activate" },
      { key: "coordCancel", transition: "cancel" },
    ],
    active: [
      {
        key: "coordStartDischargePrep",
        transition: "start_discharge_preparation",
      },
      { key: "coordCancel", transition: "cancel" },
    ],
    discharge_preparation: [
      { key: "coordMarkDischargeReady", transition: "mark_discharge_ready" },
      { key: "coordCancel", transition: "cancel" },
    ],
    discharge_ready: [
      { key: "coordComplete", transition: "complete" },
      { key: "coordReopenPrep", transition: "reopen_preparation" },
    ],
  };

const TASK_TRANSITIONS: Record<string, { key: TKey; transition: string }[]> = {
  open: [
    { key: "coordTaskStart", transition: "start" },
    { key: "coordTaskBlock", transition: "block" },
    { key: "coordTaskComplete", transition: "complete" },
    { key: "coordTaskCancel", transition: "cancel" },
  ],
  in_progress: [
    { key: "coordTaskBlock", transition: "block" },
    { key: "coordTaskComplete", transition: "complete" },
    { key: "coordTaskCancel", transition: "cancel" },
  ],
  blocked: [
    { key: "coordTaskUnblock", transition: "unblock" },
    { key: "coordTaskCancel", transition: "cancel" },
  ],
};

function EvidenceList({
  lang,
  titleKey,
  items,
}: {
  lang: Lang;
  titleKey: TKey;
  items: Evidence[];
}) {
  if (items.length === 0) return null;
  return (
    <div>
      <h4>{t(lang, titleKey)}</h4>
      <ul>
        {items.map((e, i) => (
          <li key={i}>
            {e.text}
            {e.cited_sources.length ? (
              <span className="muted small">
                {" "}
                ({e.cited_sources.join(", ")})
              </span>
            ) : null}
          </li>
        ))}
      </ul>
    </div>
  );
}

export function DischargeView({
  lang,
  out,
}: {
  lang: Lang;
  out: DischargeCoordinationOut;
}) {
  return (
    <div>
      <p>{out.summary}</p>
      <p className="small">
        <Pill tone={out.operational_only ? "ok" : "critical"} icon>
          {out.operational_only
            ? t(lang, "coordOperationalOnly")
            : t(lang, "coordNotOperationalOnly")}
        </Pill>
      </p>
      {out.barriers.length ? (
        <>
          <h4>{t(lang, "coordBarriers")}</h4>
          <ul>
            {out.barriers.map((b, i) => (
              <li key={i}>
                <strong>{b.category}</strong>: {b.explanation}
              </li>
            ))}
          </ul>
        </>
      ) : null}
      {out.suggested_steps.length ? (
        <>
          <h4>{t(lang, "coordSuggestedSteps")}</h4>
          <ul>
            {out.suggested_steps.map((s, i) => (
              <li key={i}>
                {s.text}{" "}
                {s.requires_confirmation ? (
                  <Pill tone="warn" outline>
                    {t(lang, "coordRequiresConfirmation")}
                  </Pill>
                ) : null}
              </li>
            ))}
          </ul>
        </>
      ) : null}
      <TextList
        lang={lang}
        titleKey="opsBriefMissing"
        items={out.missing_information}
      />
      <TextList
        lang={lang}
        titleKey="opsBriefLimitations"
        items={out.limitations}
      />
    </div>
  );
}

export function HandoffDraftView({
  lang,
  out,
}: {
  lang: Lang;
  out: ShiftHandoffDraftOut;
}) {
  return (
    <div>
      <h4>{t(lang, "coordSituation")}</h4>
      <p>{out.situation}</p>
      <h4>{t(lang, "coordBackground")}</h4>
      <p>{out.background}</p>
      <EvidenceList lang={lang} titleKey="coordPending" items={out.pending} />
      <EvidenceList lang={lang} titleKey="coordRisks" items={out.risks} />
      <EvidenceList lang={lang} titleKey="coordBarriers" items={out.barriers} />
      <EvidenceList
        lang={lang}
        titleKey="coordNextSteps"
        items={out.next_steps}
      />
      <TextList
        lang={lang}
        titleKey="opsBriefMissing"
        items={out.missing_information}
      />
    </div>
  );
}

function HandoffRow({
  lang,
  h,
  canDecide,
  onChanged,
}: {
  lang: Lang;
  h: Handoff;
  canDecide: boolean;
  onChanged: () => void;
}) {
  const action = useAction(lang);
  const decide = (decision: "accept" | "reject") =>
    action.run(async () => {
      await decideHandoff(h.id, { decision, expected_version: h.version });
      onChanged();
    });
  const c = h.content;
  const text = (k: string) =>
    typeof c[k] === "string" ? (c[k] as string) : null;
  return (
    <li
      data-testid="coord-handoff"
      data-origin={h.origin}
      data-status={h.status}
    >
      <p>
        <strong>{h.shift_label}</strong> <StatusPill status={h.status} />{" "}
        <Pill tone={h.origin === "dmind_draft" ? "dmind" : "neutral"} outline>
          {h.origin === "dmind_draft"
            ? t(lang, "coordOriginDmind")
            : t(lang, "coordOriginHuman")}
        </Pill>{" "}
        <span className="muted small">
          {formatDateTime(lang, h.created_at)}
        </span>
      </p>
      {text("situation") ? <p className="small">{text("situation")}</p> : null}
      {canDecide && h.status === "draft" ? (
        <div className="row wrap gap">
          <button
            type="button"
            className="primary"
            disabled={action.busy}
            onClick={() => void decide("accept")}
          >
            {t(lang, "coordAcceptHandoff")}
          </button>
          <button
            type="button"
            className="secondary"
            disabled={action.busy}
            onClick={() => void decide("reject")}
          >
            {t(lang, "coordRejectHandoff")}
          </button>
        </div>
      ) : null}
      <MessageLine message={action.message} />
    </li>
  );
}

function TaskRow({
  lang,
  task,
  canManage,
  onChanged,
}: {
  lang: Lang;
  task: Task;
  canManage: boolean;
  onChanged: () => void;
}) {
  const action = useAction(lang);
  const title =
    lang === "es"
      ? task.title_es || task.title_en
      : task.title_en || task.title_es;
  const overdue = task.due_at
    ? Date.parse(task.due_at) < Date.now() && task.status !== "completed"
    : false;
  return (
    <li data-testid="coord-task" data-status={task.status}>
      <p>
        <strong>{title}</strong> <StatusPill status={task.status} />{" "}
        {task.profession_code ? <code>{task.profession_code}</code> : null}{" "}
        {task.barrier_category_code ? (
          <Pill tone="warn" outline>
            {t(lang, "coordBarrier")}: {task.barrier_category_code}
          </Pill>
        ) : null}{" "}
        {task.patient_visible ? (
          <Pill tone="neutral" outline>
            {t(lang, "coordPatientVisible")}
          </Pill>
        ) : (
          <Pill tone="neutral" outline>
            {t(lang, "coordStaffOnly")}
          </Pill>
        )}
      </p>
      {task.due_at ? (
        <p
          className={overdue ? "warning small" : "muted small"}
          role={overdue ? "alert" : undefined}
        >
          {overdue ? t(lang, "coordOverdue") : t(lang, "coordDue")}:{" "}
          {formatDateTime(lang, task.due_at)}
        </p>
      ) : null}
      {canManage && TASK_TRANSITIONS[task.status] ? (
        <div className="row wrap gap">
          {TASK_TRANSITIONS[task.status].map((tr) => (
            <button
              key={tr.transition}
              type="button"
              className="secondary"
              disabled={action.busy}
              onClick={() =>
                void action.run(async () => {
                  await transitionTask(task.id, {
                    transition: tr.transition,
                    expected_version: task.version,
                  });
                  onChanged();
                })
              }
            >
              {t(lang, tr.key)}
            </button>
          ))}
        </div>
      ) : null}
      <MessageLine message={action.message} />
    </li>
  );
}

function NewTaskForm({
  lang,
  episodeId,
  userId,
  onCreated,
}: {
  lang: Lang;
  episodeId: string;
  userId?: string;
  onCreated: () => void;
}) {
  const action = useAction(lang);
  const [accountable, setAccountable] = useState<"me" | "queue">(
    userId ? "me" : "queue",
  );
  const [queueId, setQueueId] = useState("");
  const [titleEn, setTitleEn] = useState("");
  const [titleEs, setTitleEs] = useState("");
  const [profession, setProfession] = useState("");
  const [barrier, setBarrier] = useState("");
  const [visible, setVisible] = useState(false);
  return (
    <form
      className="stack"
      data-testid="coord-new-task"
      onSubmit={(e) => {
        e.preventDefault();
        void action.run(async () => {
          await createTask({
            episode_id: episodeId,
            title_en: titleEn.trim(),
            title_es: titleEs.trim() || titleEn.trim(),
            accountable_user_id:
              accountable === "me" && userId ? userId : undefined,
            accountable_queue_id:
              accountable === "queue" ? queueId.trim() || undefined : undefined,
            profession_code: profession.trim() || undefined,
            barrier_category_code: barrier.trim() || undefined,
            patient_visible: visible,
            patient_text_en: visible ? titleEn.trim() : undefined,
            patient_text_es: visible
              ? titleEs.trim() || titleEn.trim()
              : undefined,
          });
          setTitleEn("");
          setTitleEs("");
          onCreated();
        }, "coordTaskCreated");
      }}
    >
      <div className="grid two">
        <label className="field">
          <span>{t(lang, "coordTaskTitleEn")}</span>
          <input
            required
            value={titleEn}
            onChange={(e) => setTitleEn(e.target.value)}
            maxLength={200}
          />
        </label>
        <label className="field">
          <span>{t(lang, "coordTaskTitleEs")}</span>
          <input
            value={titleEs}
            onChange={(e) => setTitleEs(e.target.value)}
            maxLength={200}
          />
        </label>
        <label className="field">
          <span>{t(lang, "coordProfession")}</span>
          <input
            value={profession}
            onChange={(e) => setProfession(e.target.value)}
          />
        </label>
        <label className="field">
          <span>{t(lang, "coordBarrier")}</span>
          <input value={barrier} onChange={(e) => setBarrier(e.target.value)} />
        </label>
        <label className="field">
          <span>{t(lang, "coordAccountable")}</span>
          <select
            value={accountable}
            onChange={(e) =>
              setAccountable(e.target.value === "queue" ? "queue" : "me")
            }
          >
            <option value="me" disabled={!userId}>
              {t(lang, "coordAccountableMe")}
            </option>
            <option value="queue">{t(lang, "coordAccountableQueue")}</option>
          </select>
        </label>
        {accountable === "queue" ? (
          <label className="field">
            <span>{t(lang, "coordQueueId")}</span>
            <input
              required
              value={queueId}
              onChange={(e) => setQueueId(e.target.value)}
            />
          </label>
        ) : null}
      </div>
      <label className="check">
        <input
          type="checkbox"
          checked={visible}
          onChange={(e) => setVisible(e.target.checked)}
        />
        <span>{t(lang, "coordPatientVisible")}</span>
      </label>
      <button type="submit" className="secondary" disabled={action.busy}>
        {t(lang, "coordAddTask")}
      </button>
      <MessageLine message={action.message} />
    </form>
  );
}

function Coordination({ id }: { id: string }) {
  const { lang, meta } = useSession();
  const caps = useCareOpsCaps();
  const detail = useLoader(() => loadEpisode(id), id);
  const action = useAction(lang);
  const [discharge, setDischarge] =
    useState<AiOutcome<DischargeCoordinationOut> | null>(null);
  const [draft, setDraft] = useState<AiOutcome<ShiftHandoffDraftOut> | null>(
    null,
  );
  const [shiftLabel, setShiftLabel] = useState("");
  const [reason, setReason] = useState("");
  return (
    <section className="page" aria-labelledby="coord-title">
      <header className="page-head">
        <h1 id="coord-title">{t(lang, "coordTitle")}</h1>
        <p className="muted">{t(lang, "coordIntro")}</p>
        <Link href="/operations/flow" className="small">
          {t(lang, "careFlowTitle")}
        </Link>
      </header>
      <PanelState
        lang={lang}
        state={detail}
        emptyKey="flowNoEpisodes"
        isEmpty={() => false}
      >
        {(d: EpisodeDetail) => (
          <div className="stack">
            <Card data-testid="coord-episode" data-status={d.episode.status}>
              <CardHead
                title={`${d.episode.episode_type_code} · ${d.episode.responsible_service_code ?? "—"}`}
                eyebrow={<StatusPill status={d.episode.status} />}
              />
              <dl className="kv">
                <div>
                  <dt>{t(lang, "flowPriority")}</dt>
                  <dd>{d.episode.priority}</dd>
                </div>
                <div>
                  <dt>{t(lang, "coordStarted")}</dt>
                  <dd>
                    {d.episode.started_at
                      ? formatDateTime(lang, d.episode.started_at)
                      : "—"}
                  </dd>
                </div>
                <div>
                  <dt>{t(lang, "coordVersion")}</dt>
                  <dd>{d.episode.version}</dd>
                </div>
                <div>
                  <dt>{t(lang, "coordLinks")}</dt>
                  <dd>{d.links.map((l) => l.link_type).join(", ") || "—"}</dd>
                </div>
              </dl>
              {d.episode.status === "discharge_ready" ? (
                <p role="note" className="small">
                  {t(lang, "coordDischargeReadyNote")}
                </p>
              ) : null}
              {d.placements
                .filter((p) => p.status === "held" || p.status === "occupied")
                .map((p) => (
                  <div
                    key={p.id}
                    className="row wrap gap"
                    data-testid="coord-placement"
                    data-status={p.status}
                  >
                    <span>
                      {t(lang, "coordPlacement")}:{" "}
                      <StatusPill status={p.status} />
                    </span>
                    {caps.can_manage_flow ? (
                      <button
                        type="button"
                        className="secondary"
                        disabled={action.busy}
                        onClick={() =>
                          void action.run(async () => {
                            await releasePlacement(p.id, {
                              expected_version: p.version,
                              reason_code: reason.trim() || undefined,
                            });
                            setReason("");
                            detail.reload();
                          }, "coordPlacementReleased")
                        }
                      >
                        {t(lang, "coordReleasePlacement")}
                      </button>
                    ) : null}
                  </div>
                ))}
              {caps.can_manage_flow && EPISODE_TRANSITIONS[d.episode.status] ? (
                <div className="stack">
                  <label className="field">
                    <span>{t(lang, "flowReason")}</span>
                    <input
                      value={reason}
                      onChange={(e) => setReason(e.target.value)}
                      maxLength={1000}
                    />
                  </label>
                  <div className="row wrap gap">
                    {EPISODE_TRANSITIONS[d.episode.status].map((tr) => (
                      <button
                        key={tr.transition}
                        type="button"
                        className={
                          tr.transition === "cancel" ? "secondary" : "primary"
                        }
                        disabled={action.busy}
                        onClick={() =>
                          void action.run(async () => {
                            await transitionEpisode(d.episode.id, {
                              transition: tr.transition,
                              expected_version: d.episode.version,
                              note: reason.trim() || undefined,
                            });
                            setReason("");
                            detail.reload();
                          })
                        }
                      >
                        {t(lang, tr.key)}
                      </button>
                    ))}
                  </div>
                </div>
              ) : null}
              <MessageLine message={action.message} />
            </Card>

            <div className="grid two">
              <Card>
                <CardHead title={t(lang, "coordTasks")} />
                {d.tasks.length === 0 ? (
                  <p className="muted">{t(lang, "coordNoTasks")}</p>
                ) : (
                  <ul className="plain">
                    {d.tasks.map((task) => (
                      <TaskRow
                        key={task.id}
                        lang={lang}
                        task={task}
                        canManage={caps.can_manage_flow}
                        onChanged={detail.reload}
                      />
                    ))}
                  </ul>
                )}
                {caps.can_manage_flow ? (
                  <details>
                    <summary>{t(lang, "coordAddTask")}</summary>
                    <NewTaskForm
                      lang={lang}
                      episodeId={d.episode.id}
                      userId={meta?.user.id}
                      onCreated={detail.reload}
                    />
                  </details>
                ) : null}
              </Card>

              <Card>
                <CardHead title={t(lang, "coordPlacementAndCases")} />
                <h4>{t(lang, "flowQueue")}</h4>
                {d.placement_requests.length === 0 ? (
                  <p className="muted small">{t(lang, "flowNoRequests")}</p>
                ) : (
                  <ul>
                    {d.placement_requests.map((r) => (
                      <li key={r.id}>
                        <StatusPill status={r.status} /> {r.kind} · {r.priority}
                      </li>
                    ))}
                  </ul>
                )}
                <h4>{t(lang, "opsEscalations")}</h4>
                {d.escalations.length === 0 ? (
                  <p className="muted small">{t(lang, "opsNoEscalations")}</p>
                ) : (
                  <ul>
                    {d.escalations.map((e) => (
                      <li key={e.id}>
                        <StatusPill status={e.status} /> {e.kind} ·{" "}
                        {formatDateTime(lang, e.due_at)}
                      </li>
                    ))}
                  </ul>
                )}
                <h4>{t(lang, "theatreCases")}</h4>
                {d.theatre_cases.length === 0 ? (
                  <p className="muted small">{t(lang, "theatreNoCases")}</p>
                ) : (
                  <ul>
                    {d.theatre_cases.map((c) => (
                      <li key={c.id}>
                        <StatusPill status={c.status} /> {c.procedure_code}
                      </li>
                    ))}
                  </ul>
                )}
                <Link href="/operations/theatres" className="small">
                  {t(lang, "navTheatres")}
                </Link>
              </Card>
            </div>

            {meta?.ai_capabilities ? (
              <Card tone="dmind">
                <CardHead
                  title={t(lang, "coordDischargeCoordination")}
                  actions={
                    <button
                      type="button"
                      className="primary"
                      disabled={action.busy}
                      onClick={() =>
                        void action.run(async () => {
                          const r = await requestDischargeCoordination(
                            d.episode.id,
                            lang,
                          );
                          setDischarge(r.coordination);
                        })
                      }
                    >
                      {t(lang, "coordAskDischarge")}
                    </button>
                  }
                />
                <p className="muted small">{t(lang, "coordDischargeIntro")}</p>
                {discharge ? (
                  <AiOutcomePanel
                    lang={lang}
                    outcome={discharge}
                    canReview={caps.can_review_ai}
                    render={(out) => <DischargeView lang={lang} out={out} />}
                    testId="coord-discharge"
                  />
                ) : null}
              </Card>
            ) : null}

            <Card>
              <CardHead title={t(lang, "coordHandoffs")} />
              {d.handoffs.length === 0 ? (
                <p className="muted">{t(lang, "coordNoHandoffs")}</p>
              ) : (
                <ul className="plain">
                  {d.handoffs.map((h) => (
                    <HandoffRow
                      key={h.id}
                      lang={lang}
                      h={h}
                      canDecide={caps.can_manage_flow}
                      onChanged={detail.reload}
                    />
                  ))}
                </ul>
              )}
              {meta?.ai_capabilities && caps.can_manage_flow ? (
                <form
                  className="row wrap gap"
                  onSubmit={(e) => {
                    e.preventDefault();
                    void action.run(async () => {
                      const r = await requestHandoffDraft(
                        d.episode.id,
                        shiftLabel.trim(),
                        lang,
                      );
                      setDraft(r.draft);
                      detail.reload();
                    });
                  }}
                >
                  <label className="field grow">
                    <span>{t(lang, "coordShiftLabel")}</span>
                    <input
                      required
                      value={shiftLabel}
                      onChange={(e) => setShiftLabel(e.target.value)}
                      maxLength={80}
                    />
                  </label>
                  <button
                    type="submit"
                    className="secondary"
                    disabled={action.busy}
                  >
                    {t(lang, "coordAskHandoffDraft")}
                  </button>
                </form>
              ) : null}
              {draft ? (
                <AiOutcomePanel
                  lang={lang}
                  outcome={draft}
                  canReview={caps.can_review_ai}
                  render={(out) => <HandoffDraftView lang={lang} out={out} />}
                  testId="coord-handoff-draft"
                />
              ) : null}
              <p className="muted small">{t(lang, "coordHandoffDraftNote")}</p>
            </Card>

            <Card>
              <CardHead title={t(lang, "coordHistory")} />
              <ol className="timeline" data-testid="coord-history">
                {d.history.map((h, i) => (
                  <li key={`${h.version}-${h.recorded_at}-${i}`}>
                    <span className="muted small">
                      {formatDateTime(lang, h.recorded_at)}
                    </span>{" "}
                    <strong>{h.change_kind.replace(/_/g, " ")}</strong>{" "}
                    {h.from_status ?? "∅"} → {h.to_status}
                    {h.reason_code || h.note ? (
                      <span className="small">
                        {" "}
                        · {[h.reason_code, h.note].filter(Boolean).join(" — ")}
                      </span>
                    ) : null}{" "}
                    <span className="muted small">v{h.version}</span>
                  </li>
                ))}
              </ol>
              {d.ai_artifacts.length > 0 ? (
                <>
                  <h4>{t(lang, "coordArtifacts")}</h4>
                  <ul className="small">
                    {d.ai_artifacts.map((a) => (
                      <li key={a.id}>
                        {a.artifact_type} <StatusPill status={a.status} />{" "}
                        {formatDateTime(lang, a.created_at)}
                      </li>
                    ))}
                  </ul>
                </>
              ) : null}
              <p className="muted small">
                {fill(t(lang, "coordHistoryNote"), { n: d.history.length })}
              </p>
            </Card>
          </div>
        )}
      </PanelState>
    </section>
  );
}

export default function CoordinationPage() {
  const params = useParams<{ id: string }>();
  return (
    <AppShell>
      <Coordination id={params.id} />
    </AppShell>
  );
}
