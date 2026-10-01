"use client";

import { useCallback, useEffect, useRef, useState } from "react";
import { apiFetch, ApiRequestError, useSession } from "@/lib/session";
import { t, type Lang } from "@/lib/i18n";
import {
  catalogName,
  formatRange,
  loadCatalog,
  minutesUntil,
  offerReasons,
  offerStatusLabel,
  rankingNotice,
  supportiveActionLabel,
  type CatalogEntry,
  type Offer,
  type RankingOutcome,
} from "@/lib/access";

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
