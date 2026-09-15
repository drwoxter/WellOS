import type { Lang } from "./i18n";
import { t } from "./i18n";

export type CapabilityState =
  "ready" | "degraded" | "disabled" | "invalid_configuration";

/** Server-reported status of one AI capability (`/ready`, tenant metadata). */
export type CapabilityStatus = {
  state: CapabilityState;
  provider: string;
  model: string | null;
  reason: string | null;
  external: boolean;
  synthetic: boolean;
};

export type AiCapabilities = {
  model: CapabilityStatus;
  transcription: CapabilityStatus;
  structured_note: CapabilityStatus;
  transcription_languages: string[];
};

export type AiCapabilityKind = keyof Omit<
  AiCapabilities,
  "transcription_languages"
>;

export type CapabilityAvailability = {
  /** The action may be offered; `degraded` still allows an attempt. */
  callable: boolean;
  state: CapabilityState | "unknown";
  /** Plain-language explanation to show beside a disabled or degraded action. */
  notice: string | null;
  /** Output from this capability is deterministic fixture data, not a real model. */
  synthetic: boolean;
};

/**
 * Availability of one AI action from the trusted server status. When the
 * status has not loaded the action is withheld (never optimistically
 * enabled) and no reason is shown yet.
 */
export function aiAvailability(
  lang: Lang,
  caps: AiCapabilities | null | undefined,
  kind: AiCapabilityKind,
): CapabilityAvailability {
  const status = caps?.[kind];
  if (!status) {
    return {
      callable: false,
      state: "unknown",
      notice: t(lang, "aiCapabilityUnknown"),
      synthetic: false,
    };
  }
  switch (status.state) {
    case "ready":
      return {
        callable: true,
        state: status.state,
        notice: status.synthetic ? t(lang, "aiSyntheticProvider") : null,
        synthetic: status.synthetic,
      };
    case "degraded":
      return {
        callable: true,
        state: status.state,
        notice: withReason(t(lang, "aiCapabilityDegraded"), status.reason),
        synthetic: status.synthetic,
      };
    case "disabled":
      return {
        callable: false,
        state: status.state,
        notice: t(lang, "aiCapabilityDisabled"),
        synthetic: false,
      };
    case "invalid_configuration":
      return {
        callable: false,
        state: status.state,
        notice: t(lang, "aiCapabilityInvalid"),
        synthetic: false,
      };
  }
}

function withReason(base: string, reason: string | null): string {
  return reason ? `${base} (${reason})` : base;
}
