"use client";

/**
 * Shared pieces of the Care Operations surfaces: facility selection from the
 * trusted session, honest freshness labels and the advisory dMind outcome
 * panel with explicit human accept/reject. Nothing here mutates operational
 * state; every button posts to an API that re-authorizes and re-validates.
 */
import { useEffect, useMemo, useState } from "react";
import { t, type Lang, type TKey } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import {
  ageMinutes,
  reviewArtifact,
  type AiOutcome,
  type CareOpsCapabilities,
  NO_CARE_OPS_CAPABILITIES,
} from "@/lib/care-ops";
import { Pill, type Tone } from "@/components/ui/primitives";
import { MessageLine, useAction } from "../scheduling/shared";

export const STALE_AFTER_MINUTES = 10;

/** Fill `{name}` placeholders in a translated string. */
export function fill(text: string, vars: Record<string, string | number>) {
  return Object.entries(vars).reduce(
    (acc, [k, v]) => acc.replaceAll(`{${k}}`, String(v)),
    text,
  );
}

export function useCareOpsCaps(): CareOpsCapabilities {
  const { meta } = useSession();
  return meta?.care_ops_capabilities ?? NO_CARE_OPS_CAPABILITIES;
}

/** Accessible facilities with a persisted selection (per browser, not per user). */
export function useFacility(): {
  facilityId: string | null;
  setFacilityId: (id: string) => void;
  facilities: { id: string; name: string }[];
} {
  const { meta } = useSession();
  const facilities = useMemo(
    () => (meta?.facilities ?? []).filter((f) => f.accessible),
    [meta],
  );
  const [facilityId, setFacilityId] = useState<string | null>(null);
  useEffect(() => {
    if (facilities.length === 0) return;
    const saved =
      typeof window !== "undefined"
        ? window.sessionStorage.getItem("wellos.ops.facility")
        : null;
    const pick = facilities.find((f) => f.id === saved)?.id ?? facilities[0].id;
    if (!facilityId || !facilities.some((f) => f.id === facilityId)) {
      setFacilityId(pick);
    }
  }, [facilities, facilityId]);
  const set = (id: string) => {
    window.sessionStorage.setItem("wellos.ops.facility", id);
    setFacilityId(id);
  };
  return { facilityId, setFacilityId: set, facilities };
}

export function FacilityPicker({
  lang,
  facilityId,
  facilities,
  onChange,
}: {
  lang: Lang;
  facilityId: string | null;
  facilities: { id: string; name: string }[];
  onChange: (id: string) => void;
}) {
  if (facilities.length === 0) {
    return (
      <p role="status" className="muted">
        {t(lang, "opsNoFacility")}
      </p>
    );
  }
  return (
    <label className="field inline">
      <span>{t(lang, "opsFacility")}</span>
      <select
        value={facilityId ?? ""}
        onChange={(e) => onChange(e.target.value)}
        data-testid="ops-facility"
      >
        {facilities.map((f) => (
          <option key={f.id} value={f.id}>
            {f.name}
          </option>
        ))}
      </select>
    </label>
  );
}

/** "Generated N min ago", flagged (text + role, not colour only) when stale. */
export function Freshness({
  lang,
  generatedAt,
  now,
}: {
  lang: Lang;
  generatedAt: string;
  now?: number;
}) {
  const min = ageMinutes(generatedAt, now);
  const stale = min >= STALE_AFTER_MINUTES;
  return (
    <p
      className={stale ? "warning" : "muted"}
      role={stale ? "alert" : "status"}
      data-testid="ops-freshness"
    >
      {stale
        ? fill(t(lang, "opsStale"), { min })
        : fill(t(lang, "opsFreshness"), { min })}{" "}
      ({formatDateTime(lang, generatedAt)})
    </p>
  );
}

export function statusTone(status: string): Tone {
  switch (status) {
    case "completed":
    case "placed":
    case "accepted":
    case "approved":
    case "ready":
    case "covered":
      return "ok";
    case "held":
    case "discharge_preparation":
    case "discharge_ready":
    case "candidates_ready":
    case "awaiting_review":
    case "draft":
      return "warn";
    case "cancelled":
    case "error":
    case "expired":
    case "rejected":
    case "breached":
    case "insufficient_history":
      return "critical";
    default:
      return "neutral";
  }
}

export function StatusPill({ status }: { status: string }) {
  return (
    <Pill tone={statusTone(status)} icon>
      {status.replaceAll("_", " ")}
    </Pill>
  );
}

function artifactLabel(lang: Lang, status: string | null): string {
  if (status === "approved") return t(lang, "opsArtifactApproved");
  if (status === "rejected") return t(lang, "opsArtifactRejected");
  return t(lang, "opsArtifactAwaiting");
}

/**
 * Renders an advisory dMind outcome: the deterministic "skipped" reason when
 * the gateway declined, otherwise the bounded output and, for reviewers, the
 * accept/reject controls that move the AIArtifact out of `awaiting_review`.
 */
export function AiOutcomePanel<T extends Record<string, unknown>>({
  lang,
  outcome,
  canReview,
  render,
  onReviewed,
  testId,
}: {
  lang: Lang;
  outcome: AiOutcome<T>;
  canReview: boolean;
  render: (output: T) => React.ReactNode;
  onReviewed?: (status: string) => void;
  testId?: string;
}) {
  const action = useAction(lang);
  const [status, setStatus] = useState<string | null>(outcome.status);
  const [note, setNote] = useState("");
  useEffect(
    () => setStatus(outcome.status),
    [outcome.status, outcome.artifact_id],
  );
  if (outcome.mode === "skipped" || !outcome.output) {
    return (
      <p role="status" className="muted" data-testid={testId}>
        {fill(t(lang, "opsBriefSkipped"), {
          reason: outcome.reason ?? "unavailable",
        })}
      </p>
    );
  }
  const decide = (decision: "approved" | "rejected") =>
    action.run(async () => {
      if (!outcome.artifact_id) return;
      const r = await reviewArtifact(
        outcome.artifact_id,
        decision,
        note || undefined,
      );
      setStatus(r.status);
      onReviewed?.(r.status);
    });
  return (
    <div
      className="ai-outcome"
      data-testid={testId}
      data-artifact-status={status ?? ""}
    >
      <p className="muted small">
        <Pill tone="dmind" icon>
          dMind
        </Pill>{" "}
        <Pill tone={statusTone(status ?? "awaiting_review")} icon>
          {artifactLabel(lang, status)}
        </Pill>
        {outcome.synthetic ? (
          <>
            {" "}
            <Pill tone="neutral" outline>
              {t(lang, "opsSynthetic")}
            </Pill>
          </>
        ) : null}
      </p>
      {render(outcome.output)}
      <p className="muted small">{t(lang, "opsNoDecisionAI")}</p>
      {canReview && status === "awaiting_review" && outcome.artifact_id ? (
        <div className="row wrap gap">
          <label className="field grow">
            <span>{t(lang, "opsDecisionNote")}</span>
            <input
              value={note}
              onChange={(e) => setNote(e.target.value)}
              maxLength={4000}
            />
          </label>
          <button
            type="button"
            className="primary"
            disabled={action.busy}
            onClick={() => void decide("approved")}
          >
            {t(lang, "opsArtifactApprove")}
          </button>
          <button
            type="button"
            className="secondary"
            disabled={action.busy}
            onClick={() => void decide("rejected")}
          >
            {t(lang, "opsArtifactReject")}
          </button>
        </div>
      ) : null}
      <MessageLine message={action.message} />
    </div>
  );
}

/** Bulleted list helper used for bounded dMind text fields. */
export function TextList({
  lang,
  titleKey,
  items,
}: {
  lang: Lang;
  titleKey: TKey;
  items: string[] | undefined;
}) {
  if (!items || items.length === 0) return null;
  return (
    <div>
      <h4>{t(lang, titleKey)}</h4>
      <ul>
        {items.map((i, n) => (
          <li key={`${n}-${i}`}>{i}</li>
        ))}
      </ul>
    </div>
  );
}
