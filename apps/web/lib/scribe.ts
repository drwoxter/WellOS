// Consultation scribe: the recording state machine and the merge rules that
// decide how a proposed section enters the clinician's draft. Both are pure so
// they can be tested without a browser recorder or a server.

import type { Recording, RecorderErrorKind } from "./recorder";

export type Confidence = "low" | "medium" | "high";

export type TranscriptSegment = {
  index: number;
  start_ms: number;
  end_ms: number;
  speaker: string | null;
  text: string;
  confidence: Confidence;
};

export type ScribeSection = {
  section: string;
  text: string;
  confidence: Confidence;
  review_needed: boolean;
  reasons: string[];
  segments: number[];
};

export type ScribeFlag = {
  kind: "contradiction" | "uncertain";
  message: string;
  segments: number[];
  sections: string[];
};

export type ProviderInfo = {
  provider: string;
  model: string;
  model_version: string;
};

export type ScribeDraftOutput = {
  schema_version: string;
  encounter_id: string;
  source_note_version: number | null;
  language: string;
  transcript: TranscriptSegment[];
  sections: ScribeSection[];
  flags: ScribeFlag[];
  transcription: ProviderInfo;
  extraction: ProviderInfo;
  generated_at: string;
  limitations: string[];
};

export type ScribeArtifact = {
  id: string;
  status: string;
  output: ScribeDraftOutput | null;
  limitations: string[];
  model: string | null;
  model_version: string | null;
  route: string | null;
  generated_at: string | null;
  review_decision: string | null;
  review_detail: { applied?: { section: string; mode: string }[] } | null;
  note_version: number | null;
  stale: boolean;
};

export type ScribePhase =
  | "idle"
  | "consent_required"
  | "requesting_permission"
  | "recording"
  | "paused"
  | "processing"
  | "ready"
  | "error"
  | "discarded";

export type ScribeErrorKind =
  | RecorderErrorKind
  | "transcription_failed"
  | "consent_failed"
  | "too_short"
  | "too_long"
  | "too_large"
  | "encounter_closed"
  | "rate_limited";

export type ScribeState = {
  phase: ScribePhase;
  /** Finished audio kept in memory so a failed transcription can be retried. */
  recording: Recording | null;
  error: { kind: ScribeErrorKind; retryable: boolean } | null;
  /** True while the patient's consent has been recorded for this encounter. */
  consented: boolean;
};

export type ScribeEvent =
  | { type: "start_requested" }
  | { type: "consent_given" }
  | { type: "consent_failed" }
  | { type: "consent_cancelled" }
  | { type: "permission_granted" }
  | { type: "permission_failed"; kind: RecorderErrorKind }
  | { type: "pause" }
  | { type: "resume" }
  | { type: "finish" }
  | { type: "captured"; recording: Recording }
  | { type: "capture_failed" }
  | { type: "transcribed" }
  | { type: "transcription_failed"; kind: ScribeErrorKind; retryable: boolean }
  | { type: "retry" }
  | { type: "discard" }
  | { type: "reset" };

export const MIN_DURATION_MS = 1_000;
export const MAX_DURATION_MS = 20 * 60 * 1_000;
export const MAX_AUDIO_BYTES = 6 * 1024 * 1024;

export function initialScribeState(consented: boolean): ScribeState {
  return { phase: "idle", recording: null, error: null, consented };
}

export function scribeReducer(
  state: ScribeState,
  event: ScribeEvent,
): ScribeState {
  switch (event.type) {
    case "start_requested":
      if (
        state.phase !== "idle" &&
        state.phase !== "discarded" &&
        state.phase !== "ready" &&
        state.phase !== "error"
      ) {
        return state;
      }
      return {
        ...state,
        error: null,
        recording: null,
        phase: state.consented ? "requesting_permission" : "consent_required",
      };
    case "consent_given":
      return {
        ...state,
        consented: true,
        error: null,
        phase:
          state.phase === "consent_required"
            ? "requesting_permission"
            : state.phase,
      };
    case "consent_failed":
      return {
        ...state,
        phase: "error",
        error: { kind: "consent_failed", retryable: true },
      };
    case "consent_cancelled":
      return state.phase === "consent_required"
        ? { ...state, phase: "idle" }
        : state;
    case "permission_granted":
      return state.phase === "requesting_permission"
        ? { ...state, phase: "recording" }
        : state;
    case "permission_failed":
      return {
        ...state,
        phase: "error",
        error: { kind: event.kind, retryable: event.kind !== "unsupported" },
      };
    case "pause":
      return state.phase === "recording"
        ? { ...state, phase: "paused" }
        : state;
    case "resume":
      return state.phase === "paused"
        ? { ...state, phase: "recording" }
        : state;
    case "finish":
      return state.phase === "recording" || state.phase === "paused"
        ? { ...state, phase: "processing" }
        : state;
    case "captured": {
      if (state.phase !== "processing") return state;
      const { durationMs, blob } = event.recording;
      if (durationMs < MIN_DURATION_MS) {
        return {
          ...state,
          recording: null,
          phase: "error",
          error: { kind: "too_short", retryable: false },
        };
      }
      if (durationMs > MAX_DURATION_MS) {
        return {
          ...state,
          recording: null,
          phase: "error",
          error: { kind: "too_long", retryable: false },
        };
      }
      if (blob.size > MAX_AUDIO_BYTES) {
        return {
          ...state,
          recording: null,
          phase: "error",
          error: { kind: "too_large", retryable: false },
        };
      }
      return { ...state, recording: event.recording };
    }
    case "capture_failed":
      return {
        ...state,
        recording: null,
        phase: "error",
        error: { kind: "failed", retryable: false },
      };
    case "transcribed":
      return state.phase === "processing"
        ? { ...state, phase: "ready", recording: null, error: null }
        : state;
    case "transcription_failed":
      return {
        ...state,
        phase: "error",
        error: {
          kind: event.kind,
          retryable: event.retryable && state.recording !== null,
        },
      };
    case "retry":
      if (state.phase !== "error" || !state.error?.retryable) return state;
      if (state.error.kind === "consent_failed") {
        return { ...state, phase: "consent_required", error: null };
      }
      if (state.recording)
        return { ...state, phase: "processing", error: null };
      return { ...state, phase: "requesting_permission", error: null };
    case "discard":
      return { ...state, phase: "discarded", recording: null, error: null };
    case "reset":
      return { ...state, phase: "idle", recording: null, error: null };
    default:
      return state;
  }
}

/** Sections of the structured note, in display order. */
export const NOTE_SECTION_ORDER = [
  "reason_for_encounter",
  "history_present_illness",
  "medical_history",
  "review_of_systems",
  "physical_exam",
  "assessment",
  "plan",
  "follow_up",
] as const;

export type NoteSectionKey = (typeof NOTE_SECTION_ORDER)[number];

export function isNoteSection(s: string): s is NoteSectionKey {
  return (NOTE_SECTION_ORDER as readonly string[]).includes(s);
}

export type ApplyMode = "fill" | "append";

/** Same rule as the server: an empty section takes the proposal, a filled one
 *  only accepts an explicit append below the clinician's text. */
export function mergeSection(
  current: string,
  proposal: string,
  mode: ApplyMode,
): string | null {
  if (current.trim() === "") return proposal;
  if (mode !== "append") return null;
  return `${current.trimEnd()}\n\n${proposal}`;
}

/** Sections Apply-all selects by default: proposed, not yet applied, and
 *  still empty in the clinician's draft — so nothing the clinician wrote is
 *  ever touched without an explicit per-section choice. */
export function defaultApplyAll(
  proposals: ScribeSection[],
  current: Record<NoteSectionKey, string>,
  applied: ReadonlySet<string>,
): { section: NoteSectionKey; mode: ApplyMode }[] {
  return proposals
    .filter(
      (p) =>
        isNoteSection(p.section) &&
        !applied.has(p.section) &&
        p.text.trim() !== "" &&
        current[p.section].trim() === "",
    )
    .map((p) => ({ section: p.section as NoteSectionKey, mode: "fill" }));
}

export function appliedSections(artifact: ScribeArtifact | null): Set<string> {
  return new Set(
    (artifact?.review_detail?.applied ?? []).map((a) => a.section),
  );
}
