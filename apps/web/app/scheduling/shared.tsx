"use client";

import {
  useCallback,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from "react";
import { apiFetch, ApiRequestError, useSession } from "@/lib/session";
import { t, type Lang } from "@/lib/i18n";
import {
  catalogName,
  factorLabel,
  formatRange,
  loadCatalog,
  minutesUntil,
  offerReasons,
  offerStatusLabel,
  preparationText,
  rankingNotice,
  supportiveActionLabel,
  type CatalogEntry,
  type Offer,
  type RankingOutcome,
} from "@/lib/access";
import { Combobox, type ComboOption } from "@/components/ui/combobox";

/** User-visible outcome of the last action on a panel. */
export type Msg = { kind: "error" | "success"; text: string } | null;

export function errorText(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

export function isDenied(err: unknown): boolean {
  return err instanceof ApiRequestError && err.status === 403;
}

export type Loaded<T> = {
  data: T | null;
  loading: boolean;
  error: string | null;
  denied: boolean;
  reload: () => void;
};

/**
 * Load one panel's data whenever `key` changes. Responses from a superseded
 * load are dropped so a slow request cannot overwrite the current view.
 */
export function useLoader<T>(
  load: () => Promise<T>,
  key: string,
  enabled = true,
): Loaded<T> {
  const [data, setData] = useState<T | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [denied, setDenied] = useState(false);
  const [tick, setTick] = useState(0);
  const gen = useRef(0);
  const loadRef = useRef(load);
  loadRef.current = load;
  useEffect(() => {
    if (!enabled) return;
    const g = ++gen.current;
    setLoading(true);
    setError(null);
    loadRef
      .current()
      .then((d) => {
        if (g !== gen.current) return;
        setData(d);
        setDenied(false);
      })
      .catch((err) => {
        if (g !== gen.current) return;
        if (isDenied(err)) setDenied(true);
        else setError(errorText(err));
      })
      .finally(() => {
        if (g === gen.current) setLoading(false);
      });
    return () => {
      // A newer effect run owns the panel now.
    };
  }, [key, tick, enabled]);
  const reload = useCallback(() => setTick((n) => n + 1), []);
  return { data, loading, error, denied, reload };
}

/** Standard loading / error / denied / empty rendering around a list panel. */
export function PanelState<T>({
  lang,
  state,
  emptyKey,
  isEmpty,
  children,
}: {
  lang: Lang;
  state: Loaded<T>;
  emptyKey: Parameters<typeof t>[1];
  isEmpty: (d: T) => boolean;
  children: (d: T) => React.ReactNode;
}) {
  if (state.denied) {
    return (
      <p role="alert" className="error">
        {t(lang, "notAuthorized")}
      </p>
    );
  }
  if (state.error) {
    return (
      <div>
        <p role="alert" className="error">
          {state.error}
        </p>
        <button type="button" className="secondary" onClick={state.reload}>
          {t(lang, "retry")}
        </button>
      </div>
    );
  }
  if (state.data === null) {
    return (
      <p className="muted" role="status">
        {t(lang, "loading")}
      </p>
    );
  }
  if (isEmpty(state.data)) {
    return (
      <p className="muted empty" role="status">
        {t(lang, emptyKey)}
      </p>
    );
  }
  return (
    <div aria-busy={state.loading || undefined}>{children(state.data)}</div>
  );
}

/**
 * Active catalog entries of one kind for the current tenant. Catalogs are
 * open data: the UI never hardcodes services, specialties or professions.
 */
export function useCatalog(kind: string): {
  entries: CatalogEntry[];
  error: string | null;
  reload: () => void;
} {
  const { authenticated } = useSession();
  const [entries, setEntries] = useState<CatalogEntry[]>([]);
  const [error, setError] = useState<string | null>(null);
  const [tick, setTick] = useState(0);
  useEffect(() => {
    if (!authenticated) return;
    let cancelled = false;
    setError(null);
    loadCatalog({ kind, limit: 200 })
      .then((page) => {
        if (!cancelled) setEntries(page.items);
      })
      .catch((err) => {
        if (!cancelled) setError(errorText(err));
      });
    return () => {
      cancelled = true;
    };
  }, [authenticated, kind, tick]);
  const reload = useCallback(() => setTick((n) => n + 1), []);
  return { entries, error, reload };
}

/** Localized label for a catalog code, falling back to the raw code. */
export function nameFor(
  lang: Lang,
  entries: CatalogEntry[],
  code: string | null | undefined,
): string {
  if (!code) return "—";
  const e = entries.find((x) => x.code === code);
  return e ? catalogName(lang, e) : code;
}

/**
 * Runs an async action, tracking a busy flag and surfacing the outcome. The
 * generation guard discards results of actions started before the latest one.
 */
export function useAction(lang: Lang): {
  busy: boolean;
  message: Msg;
  setMessage: (m: Msg) => void;
  run: (
    fn: () => Promise<void>,
    successKey?: Parameters<typeof t>[1],
  ) => Promise<boolean>;
} {
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<Msg>(null);
  const gen = useRef(0);
  const run = useCallback(
    async (fn: () => Promise<void>, successKey?: Parameters<typeof t>[1]) => {
      const g = ++gen.current;
      setBusy(true);
      setMessage(null);
      try {
        await fn();
        if (g === gen.current && successKey) {
          setMessage({ kind: "success", text: t(lang, successKey) });
        }
        return true;
      } catch (err) {
        if (g === gen.current) {
          setMessage({ kind: "error", text: errorText(err) });
        }
        return false;
      } finally {
        if (g === gen.current) setBusy(false);
      }
    },
    [lang],
  );
  return { busy, message, setMessage, run };
}

export function MessageLine({ message }: { message: Msg }) {
  if (!message) return null;
  return (
    <p
      role={message.kind === "error" ? "alert" : "status"}
      className={message.kind === "error" ? "error" : "success"}
    >
      {message.text}
    </p>
  );
}

export function StatusBadge({
  label,
  tone,
}: {
  label: string;
  tone?: "ok" | "warn" | "critical" | "neutral";
}) {
  return <span className={`badge ${tone ?? "neutral"}`}>{label}</span>;
}

export function offerTone(
  status: string,
): "ok" | "warn" | "critical" | "neutral" {
  switch (status) {
    case "held":
      return "warn";
    case "accepted":
      return "ok";
    case "expired":
    case "revoked":
    case "declined":
      return "critical";
    default:
      return "neutral";
  }
}

export function appointmentTone(
  status: string,
): "ok" | "warn" | "critical" | "neutral" {
  switch (status) {
    case "confirmed":
      return "ok";
    case "fulfilled":
      return "neutral";
    case "no_show":
    case "cancelled":
      return "critical";
    default:
      return "warn";
  }
}

/** Reason that must accompany a staff override; empty until typed. */
export function ReasonField({
  id,
  lang,
  value,
  onChange,
  required,
  label,
}: {
  id: string;
  lang: Lang;
  value: string;
  onChange: (v: string) => void;
  required?: boolean;
  label?: string;
}) {
  return (
    <div>
      <label htmlFor={id}>
        {label ?? t(lang, "reason")}
        {required ? " *" : ""}
      </label>
      <input
        id={id}
        value={value}
        onChange={(e) => onChange(e.target.value)}
        maxLength={500}
        required={required}
      />
    </div>
  );
}

export function RankingNotice({
  lang,
  ranking,
}: {
  lang: Lang;
  ranking: RankingOutcome;
}) {
  const synthetic = ranking.mode === "dmind" && ranking.synthetic;
  return (
    <p
      className={synthetic ? "advisory synthetic-notice" : "advisory"}
      role="status"
      data-testid="ranking-notice"
    >
      {rankingNotice(lang, ranking)}
      {ranking.reused ? ` ${t(lang, "rankingReused")}` : ""}
    </p>
  );
}

export type OfferAction = "hold" | "release" | "accept" | "decline";

/**
 * One ranked option. The explanation always lists the deterministic reasons;
 * dMind text (when present) is clearly labelled and never replaces them.
 */
export function OfferCard({
  lang,
  offer,
  busy,
  onAction,
  showPatient,
  timeZone,
  compact,
}: {
  lang: Lang;
  offer: Offer;
  busy: boolean;
  onAction?: (offer: Offer, action: OfferAction) => void;
  showPatient?: boolean;
  timeZone?: string;
  compact?: boolean;
}) {
  const reasons = offerReasons(lang, offer);
  const holdMinutes = minutesUntil(offer.hold_expires_at);
  const offerMinutes = minutesUntil(offer.offer_expires_at);
  const live = offer.status === "offered" || offer.status === "held";
  const serviceName = offer.service
    ? lang === "es"
      ? offer.service.name_es
      : offer.service.name_en
    : offer.service_code;
  const headingId = `offer-${offer.id}-h`;
  const compromises = (offer.score?.factors ?? []).filter((f) => f.points < 0);
  const preparation = preparationText(lang, offer.service);
  return (
    <li className="result-card offer-card" aria-labelledby={headingId}>
      <div className="offer-head">
        <div>
          <strong id={headingId}>
            {offer.rank !== null ? `#${offer.rank} · ` : ""}
            {formatRange(lang, offer.starts_at, offer.ends_at, timeZone)}
          </strong>
          <div className="muted">
            {serviceName} · {offer.modality_code}
            {offer.facility_name ? ` · ${offer.facility_name}` : ""}
          </div>
          {showPatient && offer.patient ? (
            <div>
              {offer.patient.family_name}, {offer.patient.given_name}
            </div>
          ) : null}
        </div>
        <StatusBadge
          label={offerStatusLabel(lang, offer.status)}
          tone={offerTone(offer.status)}
        />
      </div>
      {offer.resources.length > 0 ? (
        <p className="muted">
          {offer.resources.map((r) => r.name ?? r.resource_id).join(" · ")}
        </p>
      ) : null}
      {offer.status === "held" && holdMinutes !== null ? (
        <p className="warn-text">
          {t(lang, "holdExpiresIn").replace("{min}", String(holdMinutes))}
        </p>
      ) : offer.status === "offered" && offerMinutes !== null ? (
        <p className="muted">
          {t(lang, "offerExpiresIn").replace("{min}", String(offerMinutes))}
        </p>
      ) : null}
      {!compact && reasons.length > 0 ? (
        <details className="secondary" open={offer.rank === 1}>
          <summary>{t(lang, "whyThisOption")}</summary>
          {offer.explanation ? (
            <p className="muted">
              {offer.explanation.synthetic
                ? t(lang, "explanationSynthetic")
                : t(lang, "explanationDmind")}
            </p>
          ) : null}
          <ul className="offer-reasons">
            {reasons.map((r, i) => (
              <li key={i}>{r}</li>
            ))}
          </ul>
          {offer.score && offer.score.supportive_actions.length > 0 ? (
            <p className="muted">
              {t(lang, "supportiveActions")}:{" "}
              {offer.score.supportive_actions
                .map((a) => supportiveActionLabel(lang, a))
                .join(", ")}
            </p>
          ) : null}
          {compromises.length > 0 ? (
            <p className="muted">
              {t(lang, "offerCompromises")}:{" "}
              {compromises
                .map((f) => f.detail || factorLabel(lang, f.code))
                .join("; ")}
            </p>
          ) : null}
          {preparation ? (
            <p className="muted">
              {t(lang, "offerPreparation")}: {preparation}
            </p>
          ) : null}
          {offer.score?.travel ? (
            <p className="muted">
              {t(lang, "travelEstimate")
                .replace("{km}", offer.score.travel.distance_km.toFixed(1))
                .replace("{min}", String(offer.score.travel.minutes))}{" "}
              ({offer.score.travel.provenance})
            </p>
          ) : null}
          {offer.score ? (
            <p className="muted">
              {t(lang, "score")}: {offer.score.score.toFixed(1)}
            </p>
          ) : null}
        </details>
      ) : null}
      {onAction && live ? (
        <div className="visit-actions">
          {offer.status === "offered" ? (
            <button
              type="button"
              className="secondary"
              disabled={busy}
              onClick={() => onAction(offer, "hold")}
            >
              {t(lang, "holdOption")}
            </button>
          ) : (
            <button
              type="button"
              className="tertiary"
              disabled={busy}
              onClick={() => onAction(offer, "release")}
            >
              {t(lang, "releaseHold")}
            </button>
          )}
          <button
            type="button"
            className="primary"
            disabled={busy}
            onClick={() => onAction(offer, "accept")}
          >
            {t(lang, "confirmAppointment")}
          </button>
          <button
            type="button"
            className="tertiary"
            disabled={busy}
            onClick={() => onAction(offer, "decline")}
          >
            {t(lang, "declineOption")}
          </button>
        </div>
      ) : null}
    </li>
  );
}

/** Confirm dialog shared by destructive scheduling actions. */
export function ConfirmBox({
  lang,
  title,
  children,
  onConfirm,
  onCancel,
  busy,
  confirmLabel,
  disabled,
}: {
  lang: Lang;
  title: string;
  children?: React.ReactNode;
  onConfirm: () => void;
  onCancel: () => void;
  busy: boolean;
  confirmLabel?: string;
  disabled?: boolean;
}) {
  return (
    <div className="confirm-box" role="group" aria-label={title}>
      <strong>{title}</strong>
      {children}
      <div className="visit-actions">
        <button
          type="button"
          className="primary"
          disabled={busy || disabled}
          onClick={onConfirm}
        >
          {confirmLabel ?? t(lang, "confirm")}
        </button>
        <button
          type="button"
          className="tertiary"
          disabled={busy}
          onClick={onCancel}
        >
          {t(lang, "cancel")}
        </button>
      </div>
    </div>
  );
}

export async function postJson<T>(path: string, body: unknown): Promise<T> {
  return apiFetch<T>(path, { method: "POST", body: JSON.stringify(body) });
}

/** Administrative scheduling surfaces act under the `operations` purpose of use. */
export const OPERATIONS_PURPOSE = { "x-purpose-of-use": "operations" } as const;

export async function opsFetch<T>(
  path: string,
  init?: RequestInit,
): Promise<T> {
  return apiFetch<T>(path, {
    ...init,
    headers: { ...OPERATIONS_PURPOSE, ...(init?.headers ?? {}) },
  });
}

export async function postOps<T>(path: string, body: unknown): Promise<T> {
  return opsFetch<T>(path, { method: "POST", body: JSON.stringify(body) });
}

/**
 * Searchable single-select over a catalog kind (services, specialties,
 * professions…). Long lists get type-ahead; the value stays the catalog
 * code so request payloads are unchanged.
 */
export function CatalogCombobox({
  id,
  lang,
  label,
  entries,
  value,
  onChange,
  placeholder,
  required,
  disabled,
  describedBy,
}: {
  id: string;
  lang: Lang;
  label: string;
  entries: CatalogEntry[];
  value: string;
  onChange: (code: string) => void;
  placeholder?: string;
  required?: boolean;
  disabled?: boolean;
  describedBy?: string;
}) {
  const options = useMemo<ComboOption[]>(
    () =>
      entries.map((e) => ({
        id: e.code,
        label: catalogName(lang, e, e.code),
        sub: e.code,
        keywords: `${e.name_en} ${e.name_es} ${e.synonyms.join(" ")}`,
      })),
    [entries, lang],
  );
  const selected = options.find((o) => o.id === value) ?? null;
  return (
    <Combobox
      id={id}
      label={label}
      placeholder={placeholder ?? t(lang, "comboHint")}
      options={options}
      value={selected}
      onChange={(o) => onChange(o?.id ?? "")}
      emptyText={t(lang, "comboNoMatches")}
      loadingText={t(lang, "comboLoading")}
      clearLabel={t(lang, "comboClear")}
      required={required}
      disabled={disabled}
      describedBy={describedBy}
    />
  );
}

/** Master–detail worklist: a compact selectable list beside the full record.
 *  Selection survives reloads by key and falls back to the first item. */
export function Worklist<T>({
  lang,
  label,
  items,
  keyOf,
  renderRow,
  renderDetail,
  rowData,
}: {
  lang: Lang;
  label: string;
  items: T[];
  keyOf: (item: T) => string;
  renderRow: (item: T, selected: boolean) => ReactNode;
  renderDetail: (item: T) => ReactNode;
  rowData?: (item: T) => Record<`data-${string}`, string | undefined>;
}) {
  const [picked, setPicked] = useState<string | null>(null);
  const rowRefs = useRef<(HTMLButtonElement | null)[]>([]);
  const detailRef = useRef<HTMLDivElement | null>(null);
  const selected = items.find((it) => keyOf(it) === picked) ?? items[0] ?? null;
  const selectedKey = selected ? keyOf(selected) : null;

  function pick(item: T) {
    setPicked(keyOf(item));
    if (
      typeof window !== "undefined" &&
      typeof window.matchMedia === "function" &&
      window.matchMedia("(max-width: 1000px)").matches
    ) {
      const reduce = window.matchMedia(
        "(prefers-reduced-motion: reduce)",
      ).matches;
      detailRef.current?.scrollIntoView({
        block: "start",
        behavior: reduce ? "auto" : "smooth",
      });
    }
  }

  function onKey(e: KeyboardEvent<HTMLButtonElement>, i: number) {
    const last = items.length - 1;
    let next: number | null = null;
    if (e.key === "ArrowDown") next = i >= last ? 0 : i + 1;
    else if (e.key === "ArrowUp") next = i <= 0 ? last : i - 1;
    else if (e.key === "Home") next = 0;
    else if (e.key === "End") next = last;
    if (next === null) return;
    e.preventDefault();
    rowRefs.current[next]?.focus();
  }

  return (
    <div className="master-detail worklist">
      <ul className="worklist-rows" aria-label={label}>
        {items.map((item, i) => {
          const k = keyOf(item);
          const sel = k === selectedKey;
          return (
            <li key={k}>
              <button
                type="button"
                ref={(el) => {
                  rowRefs.current[i] = el;
                }}
                className={sel ? "worklist-row selected" : "worklist-row"}
                aria-current={sel ? "true" : undefined}
                tabIndex={sel ? 0 : -1}
                data-testid="worklist-row"
                data-id={k}
                {...(rowData ? rowData(item) : null)}
                onClick={() => pick(item)}
                onKeyDown={(e) => onKey(e, i)}
              >
                {renderRow(item, sel)}
              </button>
            </li>
          );
        })}
      </ul>
      <div className="worklist-detail" ref={detailRef}>
        {selected ? (
          <ul className="plain">{renderDetail(selected)}</ul>
        ) : (
          <p className="muted">{t(lang, "worklistSelectHint")}</p>
        )}
      </div>
    </div>
  );
}

/** Standard row content for `Worklist`: title, one meta line and badges. */
export function WorklistRow({
  title,
  meta,
  badges,
}: {
  title: ReactNode;
  meta?: ReactNode;
  badges?: ReactNode;
}) {
  return (
    <>
      <span className="worklist-row-main">
        <span className="worklist-row-title">{title}</span>
        {meta ? <span className="worklist-row-meta muted">{meta}</span> : null}
      </span>
      {badges ? <span className="worklist-row-badges">{badges}</span> : null}
    </>
  );
}
