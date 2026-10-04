"use client";

import Link from "next/link";
import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { t, type Lang } from "@/lib/i18n";
import { ApiRequestError } from "@/lib/session";
import { aiAvailability, type AiCapabilities } from "@/lib/capabilities";
import {
  FULFILMENT_MODES,
  PRIORITIES,
  categoryLabel,
  confirmOrders,
  findingKindLabel,
  fulfilmentModeLabel,
  newIdempotencyKey,
  orderStatusLabel,
  preflight,
  priorityLabel,
  searchCatalog,
  suggestOrders,
  type Composition,
  type FulfilmentMode,
  type OrderGroup,
  type Orderable,
  type Priority,
  type SafetyEvaluation,
  type SuggestionDraft,
} from "@/lib/diagnostics";
import { MessageLine, StatusBadge, errorText } from "../scheduling/shared";

/**
 * Diagnostic order composer. Every orderable comes from the runtime catalog
 * search; dMind may only propose server-supplied candidates; the deterministic
 * safety preflight must be acknowledged/overridden before the clinician's
 * explicit confirmation creates the ServiceRequests.
 */

export type Facility = {
  id: string;
  name: string;
  can_act_clinically?: boolean;
};

export type SelectedItem = {
  orderable: Orderable;
  fulfilment_mode: FulfilmentMode;
  priority: Priority | "";
};

export const MIN_OVERRIDE_REASON = 10;

/** Compose the exact preflight body for the current selection. */
export function compositionFor(
  items: SelectedItem[],
  answers: Record<string, boolean>,
  facilityId: string,
  priority: Priority,
  lang: Lang,
): Composition {
  return {
    items: items.map((i) => ({
      orderable_id: i.orderable.id,
      fulfilment_mode: i.fulfilment_mode,
      ...(i.priority ? { priority: i.priority } : {}),
    })),
    answers,
    performing_facility_id: facilityId,
    priority,
    lang,
  };
}

/** Whether the confirmation button may be offered for an evaluation. */
export function canConfirm(
  evaluation: SafetyEvaluation | null,
  acknowledged: ReadonlySet<string>,
  overrideReason: string,
  canOverride: boolean,
  indication: string,
): boolean {
  if (!evaluation || indication.trim().length === 0) return false;
  const warnings = evaluation.findings.filter((f) => f.severity === "warning");
  if (warnings.some((w) => !acknowledged.has(w.id))) return false;
  if (evaluation.hard_stops > 0) {
    if (!canOverride) return false;
    if (overrideReason.trim().length < MIN_OVERRIDE_REASON) return false;
  }
  return true;
}

export function defaultMode(o: Orderable): FulfilmentMode {
  return o.fulfilment_modes[0] ?? "scheduled";
}

export function OrderComposer({
  encounterId,
  lang,
  facilities,
  defaultFacilityId,
  canOverride,
  aiCapabilities,
  onPlaced,
  compact,
}: {
  encounterId: string;
  lang: Lang;
  facilities: Facility[];
  defaultFacilityId: string | null;
  canOverride: boolean;
  aiCapabilities: AiCapabilities | null | undefined;
  onPlaced?: (group: OrderGroup) => void;
  compact?: boolean;
}) {
  const clinicalFacilities = useMemo(
    () => facilities.filter((f) => f.can_act_clinically !== false),
    [facilities],
  );
  const [facilityId, setFacilityId] = useState(
    defaultFacilityId ?? clinicalFacilities[0]?.id ?? "",
  );
  useEffect(() => {
    if (!facilityId && clinicalFacilities[0]) {
      setFacilityId(clinicalFacilities[0].id);
    }
  }, [facilityId, clinicalFacilities]);

  const [q, setQ] = useState("");
  const [results, setResults] = useState<Orderable[]>([]);
  const [searching, setSearching] = useState(false);
  const [searchError, setSearchError] = useState<string | null>(null);
  const [items, setItems] = useState<SelectedItem[]>([]);
  const [priority, setPriority] = useState<Priority>("routine");
  const [indication, setIndication] = useState("");
  const [question, setQuestion] = useState("");
  const [answers, setAnswers] = useState<Record<string, boolean>>({});
  const [evaluation, setEvaluation] = useState<SafetyEvaluation | null>(null);
  const [acknowledged, setAcknowledged] = useState<Set<string>>(new Set());
  const [overrideReason, setOverrideReason] = useState("");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<{
    kind: "error" | "success";
    text: string;
  } | null>(null);
  const [placed, setPlaced] = useState<OrderGroup | null>(null);
  const [suggestion, setSuggestion] = useState<SuggestionDraft | null>(null);
  const [suggesting, setSuggesting] = useState(false);
  const [suggestError, setSuggestError] = useState<string | null>(null);
  const idemKey = useRef<string | null>(null);
  const searchGen = useRef(0);

  const ai = aiAvailability(lang, aiCapabilities, "model");

  // Any change to the composition invalidates the deterministic evaluation:
  // confirmation is bound server-side to the exact input hash.
  const invalidate = useCallback(() => {
    setEvaluation(null);
    setAcknowledged(new Set());
    idemKey.current = null;
  }, []);

  useEffect(() => {
    const term = q.trim();
    if (term.length < 2) {
      setResults([]);
      return;
    }
    const g = ++searchGen.current;
    setSearching(true);
    setSearchError(null);
    const handle = setTimeout(() => {
      searchCatalog({
        q: term,
        facility_id: facilityId || undefined,
        lang,
        limit: 20,
      })
        .then((r) => {
          if (g !== searchGen.current) return;
          setResults(r.items);
        })
        .catch((err) => {
          if (g !== searchGen.current) return;
          setSearchError(errorText(err));
        })
        .finally(() => {
          if (g === searchGen.current) setSearching(false);
        });
    }, 200);
    return () => clearTimeout(handle);
  }, [q, facilityId, lang]);

  function add(o: Orderable) {
    if (items.some((i) => i.orderable.id === o.id)) return;
    setItems((prev) => [
      ...prev,
      { orderable: o, fulfilment_mode: defaultMode(o), priority: "" },
    ]);
    setQ("");
    setResults([]);
    invalidate();
  }

  function remove(id: string) {
    setItems((prev) => prev.filter((i) => i.orderable.id !== id));
    invalidate();
  }

  function updateItem(id: string, patch: Partial<SelectedItem>) {
    setItems((prev) =>
      prev.map((i) => (i.orderable.id === id ? { ...i, ...patch } : i)),
    );
    invalidate();
  }

  async function runPreflight(nextAnswers = answers) {
    if (items.length === 0 || !facilityId) return;
    setBusy(true);
    setMessage(null);
    try {
      const ev = await preflight(
        encounterId,
        compositionFor(items, nextAnswers, facilityId, priority, lang),
      );
      setEvaluation(ev);
      setAcknowledged(new Set());
      idemKey.current = null;
    } catch (err) {
      setMessage({ kind: "error", text: errorText(err) });
    } finally {
      setBusy(false);
    }
  }

  function answer(id: string, value: boolean) {
    const next = { ...answers, [id]: value };
    setAnswers(next);
    void runPreflight(next);
  }

  async function confirm() {
    if (!evaluation) return;
    setBusy(true);
    setMessage(null);
    if (!idemKey.current) idemKey.current = newIdempotencyKey();
    try {
      const out = await confirmOrders(encounterId, {
        ...compositionFor(items, answers, facilityId, priority, lang),
        safety_evaluation_id: evaluation.id,
        acknowledged_ids: Array.from(acknowledged),
        ...(evaluation.hard_stops > 0
          ? { override_reason: overrideReason.trim() }
          : {}),
        clinical_indication: indication.trim(),
        ...(question.trim() ? { clinical_question: question.trim() } : {}),
        ...(suggestion
          ? { suggestion_artifact_id: suggestion.artifact_id }
          : {}),
        idempotency_key: idemKey.current,
        schedule: true,
      });
      setPlaced(out.group);
      setItems([]);
      setIndication("");
      setQuestion("");
      setAnswers({});
      setSuggestion(null);
      invalidate();
      setOverrideReason("");
      setMessage({ kind: "success", text: t(lang, "dxOrdersPlaced") });
      onPlaced?.(out.group);
    } catch (err) {
      if (err instanceof ApiRequestError && err.status === 409) {
        // Stale evaluation or concurrent change: the clinician re-runs the
        // preflight and re-reads the findings before confirming again.
        invalidate();
        setMessage({ kind: "error", text: t(lang, "dxEvaluationStale") });
      } else {
        setMessage({ kind: "error", text: errorText(err) });
      }
    } finally {
      setBusy(false);
    }
  }

  async function askDmind() {
    setSuggesting(true);
    setSuggestError(null);
    try {
      const s = await suggestOrders(encounterId, {
        lang,
        facility_id: facilityId,
        ...(question.trim() ? { q: question.trim() } : {}),
      });
      setSuggestion(s);
    } catch (err) {
      setSuggestError(errorText(err));
    } finally {
      setSuggesting(false);
    }
  }

  async function addSuggestion(orderableId: string) {
    // The suggestion carries only a server-supplied orderable id; the full
    // orderable is re-read from the catalog so nothing AI-authored is trusted.
    try {
      const r = await searchCatalog({
        facility_id: facilityId || undefined,
        lang,
        limit: 60,
        q: suggestion?.suggestions.find((s) => s.orderable_id === orderableId)
          ?.code,
      });
      const o = r.items.find((x) => x.id === orderableId);
      if (!o) {
        setSuggestError(t(lang, "dxSuggestionNotOrderable"));
        return;
      }
      add(o);
    } catch (err) {
      setSuggestError(errorText(err));
    }
  }

  const warnings =
    evaluation?.findings.filter((f) => f.severity === "warning") ?? [];
  const hardStops =
    evaluation?.findings.filter((f) => f.severity === "hard_stop") ?? [];
  const confirmable = canConfirm(
    evaluation,
    acknowledged,
    overrideReason,
    canOverride,
    indication,
  );

  return (
    <section
      id="diagnostic-orders"
      className={`card dx-composer${compact ? " compact" : ""}`}
      aria-labelledby="dx-composer-h"
      data-testid="order-composer"
    >
      <h3 id="dx-composer-h">{t(lang, "dxComposerTitle")}</h3>
      <p className="muted small">{t(lang, "dxComposerIntro")}</p>

      <div className="grid-2">
        <label>
          {t(lang, "dxPerformingFacility")}
          <select
            value={facilityId}
            onChange={(e) => {
              setFacilityId(e.target.value);
              invalidate();
            }}
            disabled={busy}
          >
            {clinicalFacilities.map((f) => (
              <option key={f.id} value={f.id}>
                {f.name}
              </option>
            ))}
          </select>
        </label>
        <label>
          {t(lang, "priority")}
          <select
            value={priority}
            onChange={(e) => {
              setPriority(e.target.value as Priority);
              invalidate();
            }}
            disabled={busy}
          >
            {PRIORITIES.map((p) => (
              <option key={p} value={p}>
                {priorityLabel(lang, p)}
              </option>
            ))}
          </select>
        </label>
      </div>

      <label>
        {t(lang, "dxSearchCatalog")}
        <input
          type="search"
          value={q}
          onChange={(e) => setQ(e.target.value)}
          placeholder={t(lang, "dxSearchPlaceholder")}
          autoComplete="off"
          aria-describedby="dx-search-help"
          data-testid="dx-search"
        />
      </label>
      <p id="dx-search-help" className="muted small">
        {t(lang, "dxSearchHelp")}
      </p>
      {searchError ? (
        <p role="alert" className="error">
          {searchError}
        </p>
      ) : null}
      {q.trim().length >= 2 ? (
        <ul
          className="picker-results"
          aria-label={t(lang, "dxSearchResults")}
          aria-busy={searching || undefined}
        >
          {results.length === 0 && !searching ? (
            <li className="muted">{t(lang, "dxNoCatalogMatch")}</li>
          ) : null}
          {results.map((o) => (
            <li key={o.id} className="row-card">
              <div className="row-main">
                <strong>{o.name}</strong>{" "}
                <span className="muted small">
                  {o.code} · {categoryLabel(lang, o.category_code)}
                  {o.panel_member_codes.length > 0
                    ? ` · ${t(lang, "dxPanel")} (${o.panel_member_codes.length})`
                    : ""}
                </span>
              </div>
              <button
                type="button"
                className="secondary"
                onClick={() => add(o)}
                disabled={items.some((i) => i.orderable.id === o.id)}
              >
                {t(lang, "dxAdd")}
              </button>
            </li>
          ))}
        </ul>
      ) : null}

      {items.length > 0 ? (
        <table className="dx-items" data-testid="dx-items">
          <caption className="sr-only">
            {t(lang, "dxSelectedOrderables")}
          </caption>
          <thead>
            <tr>
              <th scope="col">{t(lang, "dxOrderable")}</th>
              <th scope="col">{t(lang, "dxFulfilmentMode")}</th>
              <th scope="col">{t(lang, "priority")}</th>
              <th scope="col">
                <span className="sr-only">{t(lang, "actions")}</span>
              </th>
            </tr>
          </thead>
          <tbody>
            {items.map((i) => (
              <tr key={i.orderable.id}>
                <td>
                  <strong>{i.orderable.name}</strong>
                  <div className="muted small">
                    {i.orderable.requires_specimen
                      ? `${t(lang, "dxNeedsSpecimen")} · `
                      : ""}
                    {i.orderable.preparation
                      ? i.orderable.preparation
                      : t(lang, "dxNoPreparation")}
                  </div>
                </td>
                <td>
                  <label className="sr-only" htmlFor={`mode-${i.orderable.id}`}>
                    {t(lang, "dxFulfilmentMode")}
                  </label>
                  <select
                    id={`mode-${i.orderable.id}`}
                    value={i.fulfilment_mode}
                    onChange={(e) =>
                      updateItem(i.orderable.id, {
                        fulfilment_mode: e.target.value as FulfilmentMode,
                      })
                    }
                    disabled={busy}
                  >
                    {FULFILMENT_MODES.filter(
                      (m) =>
                        i.orderable.fulfilment_modes.length === 0 ||
                        i.orderable.fulfilment_modes.includes(m),
                    ).map((m) => (
                      <option key={m} value={m}>
                        {fulfilmentModeLabel(lang, m)}
                      </option>
                    ))}
                  </select>
                </td>
                <td>
                  <label className="sr-only" htmlFor={`prio-${i.orderable.id}`}>
                    {t(lang, "priority")}
                  </label>
                  <select
                    id={`prio-${i.orderable.id}`}
                    value={i.priority}
                    onChange={(e) =>
                      updateItem(i.orderable.id, {
                        priority: e.target.value as Priority | "",
                      })
                    }
                    disabled={busy}
                  >
                    <option value="">{t(lang, "dxGroupPriority")}</option>
                    {PRIORITIES.map((p) => (
                      <option key={p} value={p}>
                        {priorityLabel(lang, p)}
                      </option>
                    ))}
                  </select>
                </td>
                <td>
                  <button
                    type="button"
                    className="tertiary"
                    onClick={() => remove(i.orderable.id)}
                    disabled={busy}
                    aria-label={`${t(lang, "remove")}: ${i.orderable.name}`}
                  >
                    {t(lang, "remove")}
                  </button>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
      ) : (
        <p className="muted empty">{t(lang, "dxNothingSelected")}</p>
      )}

      <label>
        {t(lang, "dxClinicalIndication")} *
        <textarea
          value={indication}
          onChange={(e) => setIndication(e.target.value)}
          rows={2}
          maxLength={2000}
          data-testid="dx-indication"
        />
      </label>
      <label>
        {t(lang, "dxClinicalQuestion")}
        <textarea
          value={question}
          onChange={(e) => setQuestion(e.target.value)}
          rows={2}
          maxLength={2000}
        />
      </label>

      <section className="diag-ai dx-ai" aria-labelledby="dx-ai-h">
        <h4 id="dx-ai-h">{t(lang, "dxDmindSuggestions")}</h4>
        <p className="muted small">{t(lang, "dxDmindIntro")}</p>
        {ai.notice ? (
          <p className="advisory" role="status">
            {ai.notice}
          </p>
        ) : null}
        <button
          type="button"
          className="secondary"
          onClick={() => void askDmind()}
          disabled={!ai.callable || suggesting || busy || !facilityId}
          data-testid="dx-ask-dmind"
        >
          {suggesting ? t(lang, "loading") : t(lang, "dxAskDmind")}
        </button>
        {suggestError ? (
          <p role="alert" className="error">
            {suggestError}
          </p>
        ) : null}
        {suggestion ? (
          <div className="stack" data-testid="dx-suggestions">
            <p className="muted small">
              {t(lang, "dxDraftStatus")}: {suggestion.status} ·{" "}
              {suggestion.autonomy_level}
              {suggestion.synthetic
                ? ` · ${t(lang, "aiSyntheticProvider")}`
                : ""}
            </p>
            {suggestion.suggestions.length === 0 ? (
              <p className="muted">{t(lang, "dxNoSuggestions")}</p>
            ) : (
              <ul className="stack">
                {suggestion.suggestions.map((s) => (
                  <li key={s.orderable_id} className="row-card">
                    <div className="row-main">
                      <strong>{s.name}</strong>{" "}
                      <span className="muted small">{s.code}</span>
                      <p className="small">{s.rationale}</p>
                      {s.cited_sources.length > 0 ? (
                        <p className="muted small">
                          {t(lang, "dxCited")}: {s.cited_sources.join(", ")}
                        </p>
                      ) : null}
                    </div>
                    <button
                      type="button"
                      className="secondary"
                      onClick={() => void addSuggestion(s.orderable_id)}
                      disabled={
                        busy ||
                        items.some((i) => i.orderable.id === s.orderable_id)
                      }
                    >
                      {t(lang, "dxAdd")}
                    </button>
                  </li>
                ))}
              </ul>
            )}
            {suggestion.duplicate_warnings.length > 0 ? (
              <p className="warn-text small">
                {t(lang, "dxDuplicateWarnings")}:{" "}
                {suggestion.duplicate_warnings.map((w) => w.reason).join("; ")}
              </p>
            ) : null}
            {suggestion.missing_information.length > 0 ? (
              <p className="muted small">
                {t(lang, "dxMissingInformation")}:{" "}
                {suggestion.missing_information.join("; ")}
              </p>
            ) : null}
            {suggestion.limitations.length > 0 ? (
              <p className="muted small">
                {t(lang, "aiLimitations")}: {suggestion.limitations.join("; ")}
              </p>
            ) : null}
          </div>
        ) : null}
      </section>

      <div className="visit-actions">
        <button
          type="button"
          className="secondary"
          onClick={() => void runPreflight()}
          disabled={busy || items.length === 0 || !facilityId}
          data-testid="dx-preflight"
        >
          {t(lang, "dxRunPreflight")}
        </button>
      </div>

      {evaluation ? (
        <section
          className="card nested dx-safety"
          aria-labelledby="dx-safety-h"
          data-testid="dx-safety"
        >
          <h4 id="dx-safety-h">{t(lang, "dxSafetyTitle")}</h4>
          <p className="muted small">
            {evaluation.engine_version} · {t(lang, "dxWarnings")}:{" "}
            {evaluation.warnings} · {t(lang, "dxHardStops")}:{" "}
            {evaluation.hard_stops}
          </p>
          <ul className="stack">
            {evaluation.candidates.map((c) => (
              <li key={c.orderable_id} className="small">
                <strong>{c.name}</strong> ·{" "}
                {fulfilmentModeLabel(lang, c.fulfilment_mode)} ·{" "}
                {priorityLabel(lang, c.priority)}
                {c.requires_appointment
                  ? ` · ${t(lang, "dxWillSchedule")}`
                  : ""}
                {c.needs_specimen ? ` · ${t(lang, "dxNeedsSpecimen")}` : ""}
              </li>
            ))}
          </ul>
          {evaluation.findings.length === 0 ? (
            <p className="success" role="status">
              {t(lang, "dxNoFindings")}
            </p>
          ) : null}
          {hardStops.length > 0 ? (
            <div className="critical-banner" role="alert">
              <strong>{t(lang, "dxHardStops")}</strong>
              <ul>
                {hardStops.map((f) => (
                  <li key={f.id}>
                    <StatusBadge
                      label={findingKindLabel(lang, f.kind)}
                      tone="critical"
                    />{" "}
                    {f.text}
                    {f.answerable ? (
                      <span className="chip-row">
                        <button
                          type="button"
                          className="tertiary"
                          onClick={() => answer(f.id, true)}
                          disabled={busy}
                        >
                          {t(lang, "yes")}
                        </button>
                        <button
                          type="button"
                          className="tertiary"
                          onClick={() => answer(f.id, false)}
                          disabled={busy}
                        >
                          {t(lang, "no")}
                        </button>
                      </span>
                    ) : null}
                  </li>
                ))}
              </ul>
              {canOverride ? (
                <label>
                  {t(lang, "dxOverrideReason")}
                  <textarea
                    value={overrideReason}
                    onChange={(e) => setOverrideReason(e.target.value)}
                    rows={2}
                    minLength={MIN_OVERRIDE_REASON}
                    data-testid="dx-override-reason"
                  />
                  <span className="muted small">
                    {t(lang, "dxOverrideHelp")}
                  </span>
                </label>
              ) : (
                <p className="muted small">
                  {t(lang, "dxNoOverridePermission")}
                </p>
              )}
            </div>
          ) : null}
          {warnings.length > 0 ? (
            <fieldset className="dx-warnings">
              <legend>{t(lang, "dxAcknowledgeWarnings")}</legend>
              {warnings.map((f) => (
                <div key={f.id} className="check-option">
                  <label>
                    <input
                      type="checkbox"
                      checked={acknowledged.has(f.id)}
                      onChange={(e) => {
                        const next = new Set(acknowledged);
                        if (e.target.checked) next.add(f.id);
                        else next.delete(f.id);
                        setAcknowledged(next);
                      }}
                      disabled={busy}
                    />{" "}
                    <StatusBadge
                      label={findingKindLabel(lang, f.kind)}
                      tone="warn"
                    />{" "}
                    {f.text}
                  </label>
                  {f.answerable ? (
                    <span className="chip-row">
                      <button
                        type="button"
                        className="tertiary"
                        onClick={() => answer(f.id, true)}
                        disabled={busy}
                      >
                        {t(lang, "yes")}
                      </button>
                      <button
                        type="button"
                        className="tertiary"
                        onClick={() => answer(f.id, false)}
                        disabled={busy}
                      >
                        {t(lang, "no")}
                      </button>
                    </span>
                  ) : null}
                  {f.evidence.length > 0 ? (
                    <div className="muted small">{f.evidence.join(", ")}</div>
                  ) : null}
                </div>
              ))}
            </fieldset>
          ) : null}
          <div className="visit-actions">
            <button
              type="button"
              className="primary"
              onClick={() => void confirm()}
              disabled={busy || !confirmable}
              data-testid="dx-confirm"
            >
              {t(lang, "dxConfirmOrders")}
            </button>
            {!confirmable && indication.trim().length === 0 ? (
              <span className="muted small">
                {t(lang, "dxIndicationRequired")}
              </span>
            ) : null}
          </div>
        </section>
      ) : null}

      <MessageLine message={message} />

      {placed ? (
        <section
          className="card nested"
          aria-labelledby="dx-placed-h"
          data-testid="dx-placed"
        >
          <h4 id="dx-placed-h">{t(lang, "dxPlacedTitle")}</h4>
          <ul className="stack">
            {placed.orders.map((o) => (
              <li key={o.id} className="row-card">
                <div className="row-main">
                  <Link
                    className="navlink"
                    href={`/diagnostics/orders/${o.id}`}
                  >
                    {o.display}
                  </Link>{" "}
                  <StatusBadge
                    label={orderStatusLabel(lang, o.order_status)}
                    tone="neutral"
                  />{" "}
                  <span className="muted small">
                    {fulfilmentModeLabel(lang, o.fulfilment_mode)}
                    {o.access_request_id
                      ? ` · ${t(lang, "dxAccessRequested")}`
                      : ""}
                    {o.schedule_conflict
                      ? ` · ${t(lang, "dxScheduleConflict")}: ${o.schedule_conflict}`
                      : ""}
                  </span>
                </div>
              </li>
            ))}
          </ul>
        </section>
      ) : null}
    </section>
  );
}
