"use client";

/**
 * `/operations/flow` — the emergency → admission → placement flow. Lists
 * open episodes, the placement queue and live escalations for the selected
 * facility; for a request it shows deterministic candidates (feasible and
 * rejected, with score decomposition), lets an authorized human hold and
 * confirm, and offers the advisory dMind ranking of already-feasible
 * candidates. The admission decision itself (creating an episode and a
 * placement request) is a human action recorded with its reason.
 */
import Link from "next/link";
import { useCallback, useEffect, useState } from "react";
import { AppShell } from "../../chrome";
import { t, type Lang } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import {
  ageMinutes,
  confirmPlacement,
  createEpisode,
  createPlacementRequest,
  generateCandidates,
  holdCandidate,
  loadEpisodes,
  loadEscalations,
  loadRequest,
  matchFromCandidates,
  loadRequests,
  requestRanking,
  type AiOutcome,
  type Candidate,
  type Episode,
  type MatchResult,
  type PlacementRankingOut,
  type PlacementRequest,
} from "@/lib/care-ops";
import { Card, CardHead, EmptyState, Pill } from "@/components/ui/primitives";
import {
  MessageLine,
  PanelState,
  useAction,
  useLoader,
} from "../../scheduling/shared";
import {
  AiOutcomePanel,
  FacilityPicker,
  StatusPill,
  TextList,
  fill,
  useCareOpsCaps,
  useFacility,
} from "../shared";

export function CandidateRow({
  lang,
  c,
  selected,
  onSelect,
  rankNote,
}: {
  lang: Lang;
  c: Candidate;
  selected: boolean;
  onSelect?: () => void;
  rankNote?: string;
}) {
  const [open, setOpen] = useState(false);
  return (
    <li
      className={`candidate${c.feasible ? "" : " rejected"}`}
      data-testid="flow-candidate"
    >
      <div className="row wrap gap">
        {onSelect ? (
          <input
            type="radio"
            name="candidate"
            aria-label={c.label}
            checked={selected}
            onChange={onSelect}
          />
        ) : null}
        <strong>{c.label}</strong>
        <Pill tone={c.feasible ? "ok" : "critical"} icon>
          {c.feasible ? t(lang, "flowFeasible") : t(lang, "flowRejected")}
        </Pill>
        {c.feasible && c.score !== null ? (
          <span className="small">
            #{c.rank} · {t(lang, "flowScore")} {c.score}
          </span>
        ) : null}
        {c.rejection_codes.map((code) => (
          <code key={code}>{code}</code>
        ))}
        <button
          type="button"
          className="link"
          aria-expanded={open}
          onClick={() => setOpen((v) => !v)}
        >
          {t(lang, "flowWhy")}
        </button>
      </div>
      {rankNote ? (
        <p className="small">
          <Pill tone="dmind" outline>
            dMind
          </Pill>{" "}
          {rankNote}
        </p>
      ) : null}
      {open ? (
        <div className="small">
          {c.factors.length > 0 ? (
            <table className="data compact">
              <caption className="sr-only">
                {t(lang, "flowScoreFactors")}
              </caption>
              <thead>
                <tr>
                  <th scope="col">{t(lang, "flowFactor")}</th>
                  <th scope="col">{t(lang, "flowValue")}</th>
                  <th scope="col">{t(lang, "flowWeight")}</th>
                  <th scope="col">{t(lang, "flowPoints")}</th>
                </tr>
              </thead>
              <tbody>
                {c.factors.map((f) => (
                  <tr key={f.code}>
                    <th scope="row">
                      {f.code} <span className="muted">{f.detail}</span>
                    </th>
                    <td>{f.value}</td>
                    <td>{f.weight}</td>
                    <td>{f.points}</td>
                  </tr>
                ))}
              </tbody>
            </table>
          ) : null}
          {c.reasons.length > 0 ? (
            <ul>
              {c.reasons.map((r, i) => (
                <li key={i}>{r}</li>
              ))}
            </ul>
          ) : null}
        </div>
      ) : null}
    </li>
  );
}

function RankingView({ lang, out }: { lang: Lang; out: PlacementRankingOut }) {
  return (
    <div>
      <p>{out.summary}</p>
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
      <p className="small">
        {t(lang, "opsBriefConfidence")}: {out.confidence}
      </p>
    </div>
  );
}

function RequestPanel({
  lang,
  id,
  onChanged,
}: {
  lang: Lang;
  id: string;
  onChanged: () => void;
}) {
  const caps = useCareOpsCaps();
  const { meta } = useSession();
  const detail = useLoader(() => loadRequest(id), id);
  const action = useAction(lang);
  const [selected, setSelected] = useState<string | null>(null);
  const [freshMatch, setFreshMatch] = useState<MatchResult | null>(null);
  const [ranking, setRanking] = useState<AiOutcome<PlacementRankingOut> | null>(
    null,
  );
  useEffect(() => {
    setSelected(null);
    setFreshMatch(null);
    setRanking(null);
  }, [id]);
  const refresh = useCallback(() => {
    detail.reload();
    onChanged();
  }, [detail, onChanged]);
  const req: PlacementRequest | undefined = detail.data?.request;
  const match: MatchResult | null =
    freshMatch ??
    (detail.data
      ? matchFromCandidates(detail.data.request, detail.data.candidates)
      : null);
  const rankNotes = new Map<string, string>();
  ranking?.output?.ranked.forEach((r) =>
    rankNotes.set(r.candidate_ref, r.rationale),
  );
  const run = (fn: () => Promise<unknown>) =>
    action.run(async () => {
      await fn();
      refresh();
    });
  return (
    <PanelState
      lang={lang}
      state={detail}
      emptyKey="flowNoRequests"
      isEmpty={() => false}
    >
      {(d) => (
        <div
          className="stack"
          data-testid="flow-request"
          data-status={d.request.status}
        >
          <p>
            <StatusPill status={d.request.status} />{" "}
            <strong>{d.request.kind}</strong> · {d.request.priority} ·{" "}
            <span className="muted small">
              {fill(t(lang, "flowWaiting"), {
                min: ageMinutes(d.request.requested_from),
              })}
            </span>
          </p>
          <p className="small muted">
            {t(lang, "flowRequirements")}:{" "}
            {Object.entries(d.request.requirements)
              .filter(([, v]) =>
                Array.isArray(v) ? v.length > 0 : v !== null && v !== undefined,
              )
              .map(
                ([k, v]) =>
                  `${k}=${Array.isArray(v) ? v.join("|") : String(v)}`,
              )
              .join(", ") || "—"}
          </p>
          <div className="row wrap gap">
            {caps.can_manage_flow &&
            (d.request.status === "requested" ||
              d.request.status === "candidates_ready") ? (
              <button
                type="button"
                className="secondary"
                disabled={action.busy}
                onClick={() =>
                  void run(async () => {
                    const r = await generateCandidates(d.request.id, {
                      expected_version: d.request.version,
                    });
                    setFreshMatch(r.match);
                  })
                }
              >
                {t(lang, "flowGenerate")}
              </button>
            ) : null}
            {meta?.ai_capabilities && match && match.feasible.length > 0 ? (
              <button
                type="button"
                className="secondary"
                disabled={action.busy}
                onClick={() =>
                  void action.run(async () => {
                    const r = await requestRanking(d.request.id, lang);
                    setRanking(r.ranking);
                  })
                }
              >
                {t(lang, "flowAskRanking")}
              </button>
            ) : null}
          </div>
          <MessageLine message={action.message} />
          {match ? (
            <>
              <p className="muted small">
                {fill(t(lang, "flowMatchMeta"), {
                  version: match.matcher_version,
                  feasible: match.feasible.length,
                  rejected: match.rejected.length,
                })}
              </p>
              {match.feasible.length === 0 ? (
                <p role="alert" className="warning">
                  {t(lang, "flowNoFeasible")}
                </p>
              ) : (
                <ul className="plain" data-testid="flow-feasible">
                  {match.feasible.map((c) => (
                    <CandidateRow
                      key={c.location_id}
                      lang={lang}
                      c={c}
                      selected={selected === c.location_id}
                      onSelect={
                        caps.can_manage_flow && d.request.status !== "placed"
                          ? () => setSelected(c.location_id)
                          : undefined
                      }
                      rankNote={
                        rankNotes.get(c.location_id) ?? rankNotes.get(c.label)
                      }
                    />
                  ))}
                </ul>
              )}
              {caps.can_manage_flow &&
              req &&
              req.status === "candidates_ready" ? (
                <div className="row wrap gap">
                  <button
                    type="button"
                    className="primary"
                    disabled={!selected || action.busy}
                    onClick={() =>
                      void run(() =>
                        holdCandidate(req.id, {
                          location_id: selected ?? "",
                          expected_version: req.version,
                          facts_hash: match.facts_hash,
                        }),
                      )
                    }
                  >
                    {t(lang, "flowHold")}
                  </button>
                  <button
                    type="button"
                    className="primary"
                    disabled={!selected || action.busy}
                    onClick={() =>
                      void run(() =>
                        confirmPlacement(req.id, {
                          location_id: selected ?? "",
                          expected_version: req.version,
                          facts_hash: match.facts_hash,
                        }),
                      )
                    }
                  >
                    {t(lang, "flowConfirm")}
                  </button>
                </div>
              ) : null}
              {caps.can_manage_flow && req && req.status === "held" ? (
                <button
                  type="button"
                  className="primary"
                  disabled={action.busy}
                  onClick={() =>
                    void run(() =>
                      confirmPlacement(req.id, {
                        expected_version: req.version,
                      }),
                    )
                  }
                >
                  {t(lang, "flowConfirmHeld")}
                </button>
              ) : null}
              {ranking ? (
                <AiOutcomePanel
                  lang={lang}
                  outcome={ranking}
                  canReview={caps.can_review_ai}
                  render={(out) => <RankingView lang={lang} out={out} />}
                  testId="flow-ranking"
                />
              ) : null}
              {match.rejected.length > 0 ? (
                <details>
                  <summary>
                    {fill(t(lang, "flowRejectedSummary"), {
                      n: match.rejected.length,
                    })}
                  </summary>
                  <ul className="plain">
                    {match.rejected.map((c) => (
                      <CandidateRow
                        key={c.location_id}
                        lang={lang}
                        c={c}
                        selected={false}
                      />
                    ))}
                  </ul>
                </details>
              ) : null}
            </>
          ) : (
            <p className="muted">{t(lang, "flowNoCandidatesYet")}</p>
          )}
          <Link
            href={`/episodes/${d.request.episode_id}/coordination`}
            className="small"
          >
            {t(lang, "coordTitle")}
          </Link>
        </div>
      )}
    </PanelState>
  );
}

function AdmissionForm({
  lang,
  facilityId,
  onCreated,
}: {
  lang: Lang;
  facilityId: string;
  onCreated: (requestId: string) => void;
}) {
  const action = useAction(lang);
  const [patientId, setPatientId] = useState("");
  const [episodeType, setEpisodeType] = useState("inpatient");
  const [service, setService] = useState("");
  const [priority, setPriority] = useState("routine");
  const [ageGroup, setAgeGroup] = useState("");
  const [isolation, setIsolation] = useState("");
  const [accessibility, setAccessibility] = useState("");
  const [equipment, setEquipment] = useState("");
  const [reason, setReason] = useState("");
  const split = (v: string) =>
    v
      .split(",")
      .map((s) => s.trim())
      .filter(Boolean);
  return (
    <form
      className="stack"
      data-testid="flow-admission"
      onSubmit={(e) => {
        e.preventDefault();
        void action.run(async () => {
          const ep = await createEpisode({
            facility_id: facilityId,
            patient_id: patientId.trim(),
            episode_type_code: episodeType.trim(),
            responsible_service_code: service.trim() || undefined,
            priority,
            activate: true,
          });
          const r = await createPlacementRequest({
            episode_id: ep.id,
            kind: "admission",
            priority,
            requirements: {
              service_code: service.trim() || undefined,
              age_group_code: ageGroup.trim() || undefined,
              isolation_codes: split(isolation),
              accessibility_codes: split(accessibility),
              equipment_codes: split(equipment),
            },
            reason_code: "human_admission_decision",
            note: reason.trim() || undefined,
          });
          onCreated(r.id);
        }, "flowAdmitted");
      }}
    >
      <p className="muted small">{t(lang, "flowAdmitIntro")}</p>
      <label className="field">
        <span>{t(lang, "flowPatientId")}</span>
        <input
          required
          value={patientId}
          onChange={(e) => setPatientId(e.target.value)}
        />
      </label>
      <div className="grid two">
        <label className="field">
          <span>{t(lang, "flowEpisodeType")}</span>
          <input
            required
            value={episodeType}
            onChange={(e) => setEpisodeType(e.target.value)}
          />
        </label>
        <label className="field">
          <span>{t(lang, "flowService")}</span>
          <input value={service} onChange={(e) => setService(e.target.value)} />
        </label>
        <label className="field">
          <span>{t(lang, "flowPriority")}</span>
          <select
            value={priority}
            onChange={(e) => setPriority(e.target.value)}
          >
            <option value="routine">routine</option>
            <option value="urgent">urgent</option>
            <option value="emergency">emergency</option>
          </select>
        </label>
        <label className="field">
          <span>{t(lang, "flowAgeGroup")}</span>
          <input
            value={ageGroup}
            onChange={(e) => setAgeGroup(e.target.value)}
          />
        </label>
        <label className="field">
          <span>{t(lang, "flowIsolation")}</span>
          <input
            value={isolation}
            onChange={(e) => setIsolation(e.target.value)}
          />
        </label>
        <label className="field">
          <span>{t(lang, "flowAccessibility")}</span>
          <input
            value={accessibility}
            onChange={(e) => setAccessibility(e.target.value)}
          />
        </label>
        <label className="field">
          <span>{t(lang, "flowEquipment")}</span>
          <input
            value={equipment}
            onChange={(e) => setEquipment(e.target.value)}
          />
        </label>
      </div>
      <label className="field">
        <span>{t(lang, "flowReason")}</span>
        <input
          value={reason}
          onChange={(e) => setReason(e.target.value)}
          maxLength={1000}
        />
      </label>
      <button type="submit" className="primary" disabled={action.busy}>
        {t(lang, "flowAdmit")}
      </button>
      <MessageLine message={action.message} />
    </form>
  );
}

function Flow() {
  const { lang } = useSession();
  const caps = useCareOpsCaps();
  const { facilityId, setFacilityId, facilities } = useFacility();
  const fid = facilityId ?? "";
  const requests = useLoader(() => loadRequests(fid), fid, Boolean(facilityId));
  const episodes = useLoader(() => loadEpisodes(fid), fid, Boolean(facilityId));
  const escalations = useLoader(
    () => loadEscalations(fid),
    fid,
    Boolean(facilityId),
  );
  const [selected, setSelected] = useState<string | null>(null);
  const refreshAll = useCallback(() => {
    requests.reload();
    episodes.reload();
    escalations.reload();
  }, [requests, episodes, escalations]);
  return (
    <section className="page" aria-labelledby="flow-title">
      <header className="page-head">
        <h1 id="flow-title">{t(lang, "careFlowTitle")}</h1>
        <p className="muted">{t(lang, "flowIntro")}</p>
        <FacilityPicker
          lang={lang}
          facilityId={facilityId}
          facilities={facilities}
          onChange={setFacilityId}
        />
      </header>
      {!facilityId ? (
        <p className="muted">{t(lang, "opsSelectFacility")}</p>
      ) : (
        <div className="grid two">
          <div className="stack">
            <Card>
              <CardHead title={t(lang, "flowQueue")} />
              <PanelState
                lang={lang}
                state={requests}
                emptyKey="flowNoRequests"
                isEmpty={(d) => d.requests.length === 0}
              >
                {(d) => (
                  <ul className="plain" role="list" data-testid="flow-queue">
                    {d.requests.map((r) => (
                      <li key={r.id}>
                        <button
                          type="button"
                          className={`listrow${selected === r.id ? " selected" : ""}`}
                          aria-pressed={selected === r.id}
                          onClick={() => setSelected(r.id)}
                        >
                          <StatusPill status={r.status} />{" "}
                          <strong>{r.kind}</strong> · {r.priority}
                          <span className="muted small">
                            {" "}
                            ·{" "}
                            {fill(t(lang, "flowWaiting"), {
                              min: ageMinutes(r.requested_from),
                            })}
                          </span>
                        </button>
                      </li>
                    ))}
                  </ul>
                )}
              </PanelState>
            </Card>
            <Card>
              <CardHead title={t(lang, "flowEpisodes")} />
              <PanelState
                lang={lang}
                state={episodes}
                emptyKey="flowNoEpisodes"
                isEmpty={(d) => d.episodes.length === 0}
              >
                {(d) => (
                  <ul className="plain" role="list" data-testid="flow-episodes">
                    {d.episodes.map((e: Episode) => (
                      <li key={e.id}>
                        <Link
                          href={`/episodes/${e.id}/coordination`}
                          className="listrow"
                        >
                          <StatusPill status={e.status} /> {e.episode_type_code}{" "}
                          · {e.responsible_service_code ?? "—"}
                          <span className="muted small">
                            {" "}
                            ·{" "}
                            {e.started_at
                              ? formatDateTime(lang, e.started_at)
                              : formatDateTime(lang, e.created_at)}
                          </span>
                        </Link>
                      </li>
                    ))}
                  </ul>
                )}
              </PanelState>
            </Card>
            <Card>
              <CardHead title={t(lang, "opsEscalations")} />
              <PanelState
                lang={lang}
                state={escalations}
                emptyKey="opsNoEscalations"
                isEmpty={(d) => d.escalations.length === 0}
              >
                {(d) => (
                  <ul data-testid="flow-escalations">
                    {d.escalations.map((e) => (
                      <li key={e.id}>
                        <StatusPill status={e.status} /> {e.kind} ·{" "}
                        {fill(t(lang, "flowEscalationDue"), {
                          min: e.threshold_minutes,
                          due: formatDateTime(lang, e.due_at),
                        })}
                        {e.episode_id ? (
                          <>
                            {" "}
                            <Link
                              href={`/episodes/${e.episode_id}/coordination`}
                              className="small"
                            >
                              {t(lang, "coordTitle")}
                            </Link>
                          </>
                        ) : null}
                      </li>
                    ))}
                  </ul>
                )}
              </PanelState>
            </Card>
          </div>
          <div className="stack">
            <Card>
              <CardHead title={t(lang, "flowPlacement")} />
              {selected ? (
                <RequestPanel
                  lang={lang}
                  id={selected}
                  onChanged={refreshAll}
                />
              ) : (
                <EmptyState inline title={t(lang, "flowSelectRequest")} />
              )}
            </Card>
            {caps.can_manage_flow ? (
              <Card>
                <CardHead title={t(lang, "flowAdmitTitle")} />
                <AdmissionForm
                  lang={lang}
                  facilityId={facilityId}
                  onCreated={(id) => {
                    setSelected(id);
                    refreshAll();
                  }}
                />
              </Card>
            ) : null}
          </div>
        </div>
      )}
    </section>
  );
}

export default function FlowPage() {
  return (
    <AppShell>
      <Flow />
    </AppShell>
  );
}
