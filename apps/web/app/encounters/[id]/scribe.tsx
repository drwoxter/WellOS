"use client";

import { useCallback, useEffect, useReducer, useRef, useState } from "react";
import { t } from "@/lib/i18n";
import type { Lang, TKey } from "@/lib/i18n";
import { ApiRequestError, apiFetch } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import {
  RecorderError,
  blobToBase64,
  createMediaStreamRecorder,
  formatTimecode,
  isRecordingSupported,
} from "@/lib/recorder";
import type { AudioRecorder, RecorderFactory } from "@/lib/recorder";
import {
  appliedSections,
  defaultApplyAll,
  initialScribeState,
  isNoteSection,
  scribeReducer,
} from "@/lib/scribe";
import type {
  ApplyMode,
  NoteSectionKey,
  ScribeArtifact,
  ScribeErrorKind,
  ScribeSection,
  TranscriptSegment,
} from "@/lib/scribe";

function errorKey(kind: ScribeErrorKind): TKey {
  switch (kind) {
    case "permission_denied":
      return "micDenied";
    case "unsupported":
      return "micUnsupported";
    case "no_device":
      return "micNoDevice";
    case "failed":
      return "recordingFailed";
    case "transcription_failed":
      return "transcriptionFailed";
    case "consent_failed":
      return "consentFailed";
    case "too_short":
      return "recordingTooShort";
    case "too_long":
      return "recordingTooLong";
    case "too_large":
      return "recordingTooLarge";
    case "encounter_closed":
      return "encounterClosedScribe";
    case "rate_limited":
      return "scribeRateLimited";
  }
}

function transcriptionFailure(err: unknown): {
  kind: ScribeErrorKind;
  retryable: boolean;
} {
  if (err instanceof ApiRequestError) {
    switch (err.code) {
      case "recording_too_short":
        return { kind: "too_short", retryable: false };
      case "recording_too_long":
        return { kind: "too_long", retryable: false };
      case "recording_too_large":
        return { kind: "too_large", retryable: false };
      case "encounter_not_active":
        return { kind: "encounter_closed", retryable: false };
      case "consent_required":
        return { kind: "consent_failed", retryable: true };
      case "unsupported_media_type":
      case "validation_failed":
      case "audio_rejected":
        return { kind: "transcription_failed", retryable: false };
    }
    if (err.status === 429) return { kind: "rate_limited", retryable: true };
  }
  return { kind: "transcription_failed", retryable: true };
}

export type RecordingDockProps = {
  encounterId: string;
  lang: Lang;
  consented: boolean;
  /** Recording is offered only while the note can still be documented. */
  enabled: boolean;
  recorderFactory?: RecorderFactory;
  onConsentRecorded: () => void;
  onDraft: (artifact: ScribeArtifact) => void;
};

/**
 * Sticky recording surface. Audio lives in the recorder (and, after Finish,
 * in this component's memory for a retry) until the transcript arrives, then
 * it is dropped. Nothing is ever written to browser storage.
 */
export function RecordingDock({
  encounterId,
  lang,
  consented,
  enabled,
  recorderFactory = createMediaStreamRecorder,
  onConsentRecorded,
  onDraft,
}: RecordingDockProps) {
  const [state, dispatch] = useReducer(
    scribeReducer,
    consented,
    initialScribeState,
  );
  const [elapsed, setElapsed] = useState(0);
  const [confirmingDiscard, setConfirmingDiscard] = useState(false);
  const recorder = useRef<AudioRecorder | null>(null);
  // Lifecycle shared by the unmount cleanup and `beginCapture`: a capture
  // attempt only owns its recorder while the dock is mounted and no newer
  // attempt (or a discard) has superseded it.
  const mounted = useRef(true);
  const captureAttempt = useRef(0);
  const transcribing = useRef(false);
  const stateRef = useRef(state);
  stateRef.current = state;

  useEffect(() => {
    if (consented) dispatch({ type: "consent_given" });
  }, [consented]);

  // Elapsed display: a coarse 1 Hz tick, no animation.
  useEffect(() => {
    if (state.phase !== "recording" && state.phase !== "paused") return;
    const tick = () => setElapsed(recorder.current?.elapsedMs() ?? 0);
    tick();
    const handle = setInterval(tick, 1000);
    return () => clearInterval(handle);
  }, [state.phase]);

  // Release the microphone if the workspace unmounts mid-recording. A
  // permission prompt still pending at this point is cancelled: when its
  // `start()` settles, `beginCapture` sees the dock is gone and discards.
  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
      captureAttempt.current += 1;
      recorder.current?.discard();
      recorder.current = null;
    };
  }, []);

  const transcribe = useCallback(async () => {
    const rec = stateRef.current.recording;
    if (!rec || transcribing.current) return;
    transcribing.current = true;
    try {
      const audio_base64 = await blobToBase64(rec.blob);
      const artifact = await apiFetch<ScribeArtifact>(
        `/api/v1/encounters/${encounterId}/scribe`,
        {
          method: "POST",
          body: JSON.stringify({
            audio_base64,
            mime_type: rec.mimeType,
            duration_ms: rec.durationMs,
            language: lang,
          }),
        },
      );
      dispatch({ type: "transcribed" });
      onDraft(artifact);
    } catch (err) {
      const failure = transcriptionFailure(err);
      dispatch({ type: "transcription_failed", ...failure });
    } finally {
      transcribing.current = false;
    }
  }, [encounterId, lang, onDraft]);

  const beginCapture = useCallback(async () => {
    const attempt = ++captureAttempt.current;
    const r = recorderFactory();
    recorder.current = r;
    const owned = () =>
      mounted.current &&
      captureAttempt.current === attempt &&
      recorder.current === r;
    try {
      await r.start();
    } catch (err) {
      // A failed start may still hold a stream (e.g. MediaRecorder refused
      // after the microphone was granted): release it before reporting.
      const current = owned();
      r.discard();
      if (recorder.current === r) recorder.current = null;
      if (!current) return;
      dispatch({
        type: "permission_failed",
        kind: err instanceof RecorderError ? err.kind : "failed",
      });
      return;
    }
    if (!owned()) {
      // Permission resolved after unmount or after this attempt was
      // superseded: nobody can stop this recorder, so drop it now.
      r.discard();
      if (recorder.current === r) recorder.current = null;
      return;
    }
    dispatch({ type: "permission_granted" });
  }, [recorderFactory]);

  // Side effects of the state machine: request the microphone when entering
  // `requesting_permission`, transcribe when audio has been captured.
  useEffect(() => {
    if (state.phase === "requesting_permission" && !recorder.current) {
      void beginCapture();
    }
    if (state.phase === "processing" && state.recording) {
      void transcribe();
    }
  }, [state.phase, state.recording, beginCapture, transcribe]);

  const start = () => {
    if (
      !isRecordingSupported() &&
      recorderFactory === createMediaStreamRecorder
    ) {
      dispatch({ type: "permission_failed", kind: "unsupported" });
      return;
    }
    dispatch({ type: "start_requested" });
  };

  const giveConsent = async () => {
    try {
      await apiFetch(`/api/v1/encounters/${encounterId}/recording-consent`, {
        method: "POST",
        body: JSON.stringify({ granted: true }),
      });
      onConsentRecorded();
      dispatch({ type: "consent_given" });
    } catch {
      dispatch({ type: "consent_failed" });
    }
  };

  const pause = () => {
    recorder.current?.pause();
    dispatch({ type: "pause" });
  };
  const resume = () => {
    recorder.current?.resume();
    dispatch({ type: "resume" });
  };
  const finish = async () => {
    const r = recorder.current;
    if (!r) return;
    dispatch({ type: "finish" });
    try {
      const recording = await r.stop();
      recorder.current = null;
      dispatch({ type: "captured", recording });
    } catch {
      recorder.current = null;
      dispatch({ type: "capture_failed" });
    }
  };
  const discard = () => {
    captureAttempt.current += 1;
    recorder.current?.discard();
    recorder.current = null;
    setConfirmingDiscard(false);
    dispatch({ type: "discard" });
  };
  const retry = () => dispatch({ type: "retry" });

  if (!enabled) return null;

  const live = state.phase === "recording" || state.phase === "paused";

  return (
    <section
      className={`recording-dock phase-${state.phase}`}
      aria-labelledby="recording-dock-title"
      data-phase={state.phase}
    >
      <div className="recording-dock-head">
        <h2 id="recording-dock-title">{t(lang, "recordingDockTitle")}</h2>
        {live ? (
          <span
            className={`recording-status ${state.phase}`}
            role="status"
            aria-live="polite"
          >
            <span className="recording-indicator" aria-hidden="true" />
            {state.phase === "recording"
              ? t(lang, "recordingLive")
              : t(lang, "recordingPaused")}
            {" · "}
            <span aria-label={t(lang, "elapsed")}>
              {formatTimecode(elapsed)}
            </span>
          </span>
        ) : null}
      </div>

      {state.phase === "idle" || state.phase === "ready" ? (
        <div className="recording-row">
          <p className="muted grow">
            {state.phase === "ready"
              ? t(lang, "scribeReady")
              : t(lang, "recordingIdleHelp")}
          </p>
          <button
            type="button"
            className="primary record-button"
            onClick={start}
          >
            {state.phase === "ready"
              ? t(lang, "recordAgain")
              : t(lang, "recordConsultation")}
          </button>
        </div>
      ) : null}

      {state.phase === "consent_required" ? (
        <div
          className="confirm-box"
          role="group"
          aria-labelledby="consent-title"
        >
          <p id="consent-title">
            <strong>{t(lang, "consentTitle")}</strong>
          </p>
          <p>{t(lang, "consentHelp")}</p>
          <button
            type="button"
            className="primary"
            onClick={() => void giveConsent()}
          >
            {t(lang, "consentConfirm")}
          </button>{" "}
          <button
            type="button"
            className="secondary"
            onClick={() => dispatch({ type: "consent_cancelled" })}
          >
            {t(lang, "consentDecline")}
          </button>
        </div>
      ) : null}

      {state.phase === "requesting_permission" ? (
        <p role="status" className="muted">
          {t(lang, "requestingMicrophone")}
        </p>
      ) : null}

      {live ? (
        <div className="recording-row">
          {state.phase === "recording" ? (
            <button type="button" className="secondary" onClick={pause}>
              {t(lang, "pauseRecording")}
            </button>
          ) : (
            <button type="button" className="secondary" onClick={resume}>
              {t(lang, "resumeRecording")}
            </button>
          )}
          <button
            type="button"
            className="primary"
            onClick={() => void finish()}
          >
            {t(lang, "finishRecording")}
          </button>
          {confirmingDiscard ? (
            <span className="recording-row">
              <span>{t(lang, "discardRecordingConfirm")}</span>
              <button type="button" className="tertiary" onClick={discard}>
                {t(lang, "confirm")}
              </button>
              <button
                type="button"
                className="tertiary"
                onClick={() => setConfirmingDiscard(false)}
              >
                {t(lang, "cancel")}
              </button>
            </span>
          ) : (
            <button
              type="button"
              className="tertiary"
              onClick={() => setConfirmingDiscard(true)}
            >
              {t(lang, "discardRecording")}
            </button>
          )}
        </div>
      ) : null}

      {state.phase === "processing" ? (
        <div className="recording-row" aria-busy="true">
          <p role="status" className="grow">
            <strong>{t(lang, "processingAudio")}</strong>{" "}
            <span className="muted">{t(lang, "processingHelp")}</span>
          </p>
        </div>
      ) : null}

      {state.phase === "error" && state.error ? (
        <div className="recording-row">
          <p role="alert" className="error grow">
            {t(lang, errorKey(state.error.kind))}
          </p>
          {state.error.retryable ? (
            <button type="button" className="primary" onClick={retry}>
              {state.recording
                ? t(lang, "retryTranscription")
                : t(lang, "retry")}
            </button>
          ) : null}
          {state.recording ? (
            <button type="button" className="tertiary" onClick={discard}>
              {t(lang, "discardRecording")}
            </button>
          ) : (
            <button
              type="button"
              className="secondary"
              onClick={() => dispatch({ type: "reset" })}
            >
              {t(lang, "recordAgain")}
            </button>
          )}
        </div>
      ) : null}

      {state.phase === "discarded" ? (
        <div className="recording-row">
          <p role="status" className="muted grow">
            {t(lang, "recordingDiscarded")}
          </p>
          <button type="button" className="secondary" onClick={start}>
            {t(lang, "recordAgain")}
          </button>
        </div>
      ) : null}
    </section>
  );
}

// ---------------------------------------------------------------------------
// Structured draft review
// ---------------------------------------------------------------------------

const SECTION_LABEL: Record<NoteSectionKey, TKey> = {
  reason_for_encounter: "reasonForEncounter",
  history_present_illness: "historyPresentIllness",
  medical_history: "medicalHistory",
  review_of_systems: "reviewOfSystems",
  physical_exam: "physicalExam",
  assessment: "assessment",
  plan: "plan",
  follow_up: "followUpInstructions",
};

function confidenceKey(c: ScribeSection["confidence"]): TKey {
  return c === "high"
    ? "confidenceHigh"
    : c === "medium"
      ? "confidenceMedium"
      : "confidenceLow";
}

function speakerLabel(lang: Lang, speaker: string | null): string {
  if (speaker === "clinician") return t(lang, "speakerClinician");
  if (speaker === "patient") return t(lang, "speakerPatient");
  return t(lang, "speakerUnknown");
}

export type ApplyOutcome =
  "applied" | "conflict" | "stale" | "closed" | "error";

export type ScribeReviewProps = {
  lang: Lang;
  artifact: ScribeArtifact;
  current: Record<NoteSectionKey, string>;
  /** True while another note mutation is running or the note is signing. */
  locked: boolean;
  onApply: (
    selection: { section: NoteSectionKey; mode: ApplyMode }[],
  ) => Promise<ApplyOutcome>;
  onDismiss: () => Promise<boolean>;
};

export function ScribeReview({
  lang,
  artifact,
  current,
  locked,
  onApply,
  onDismiss,
}: ScribeReviewProps) {
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [focusSegment, setFocusSegment] = useState<number | null>(null);
  const transcriptRefs = useRef(new Map<number, HTMLLIElement>());
  const output = artifact.output;

  useEffect(() => {
    if (focusSegment === null) return;
    const el = transcriptRefs.current.get(focusSegment);
    el?.scrollIntoView({ block: "nearest" });
    el?.focus();
  }, [focusSegment]);

  if (!output) return null;

  const applied = appliedSections(artifact);
  const actionable =
    (artifact.status === "awaiting_review" || artifact.status === "approved") &&
    !locked;
  const proposals = output.sections.filter((s) => isNoteSection(s.section));
  const applyAll = defaultApplyAll(proposals, current, applied);
  const segmentById = new Map<number, TranscriptSegment>(
    output.transcript.map((s) => [s.index, s]),
  );

  async function run(
    selection: { section: NoteSectionKey; mode: ApplyMode }[],
  ) {
    if (busy || selection.length === 0) return;
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      const outcome = await onApply(selection);
      switch (outcome) {
        case "applied":
          setMessage(t(lang, "scribeApplied"));
          break;
        case "conflict":
          setError(t(lang, "sectionNotEmpty"));
          break;
        case "stale":
          setError(t(lang, "scribeStale"));
          break;
        case "closed":
          setError(t(lang, "encounterClosedScribe"));
          break;
        default:
          setError(t(lang, "error"));
      }
    } finally {
      setBusy(false);
    }
  }

  async function dismiss() {
    if (busy) return;
    setBusy(true);
    setError(null);
    try {
      if (await onDismiss()) setMessage(t(lang, "scribeDismissed"));
      else setError(t(lang, "error"));
    } finally {
      setBusy(false);
    }
  }

  const dismissed =
    artifact.status === "rejected" || artifact.status === "superseded";

  return (
    <section
      className="card ai-section scribe-review"
      aria-labelledby="scribe-review-title"
      aria-busy={busy}
    >
      <h2 id="scribe-review-title">{t(lang, "scribeReviewTitle")}</h2>
      <p className="muted">
        <span className="badge neutral">{t(lang, "aiAssistiveDraft")}</span>{" "}
        {t(lang, "scribeReviewHelp")}
      </p>
      {artifact.stale && !dismissed ? (
        <p className="warn-text" role="status">
          {t(lang, "scribeStale")}
        </p>
      ) : null}
      {dismissed ? (
        <p className="muted" role="status">
          {artifact.status === "rejected"
            ? t(lang, "scribeDismissed")
            : t(lang, "artifactSuperseded")}
        </p>
      ) : null}

      {output.flags.length > 0 ? (
        <div
          className="scribe-flags"
          role="region"
          aria-label={t(lang, "flagsTitle")}
        >
          <h3>{t(lang, "flagsTitle")}</h3>
          <ul>
            {output.flags.map((f, i) => (
              <li key={i}>
                <span
                  className={`badge ${f.kind === "contradiction" ? "critical" : "warn"}`}
                >
                  {f.kind === "contradiction"
                    ? t(lang, "flagContradiction")
                    : t(lang, "flagUncertain")}
                </span>{" "}
                {f.message}{" "}
                {f.segments.map((idx) => (
                  <SegmentLink
                    key={idx}
                    lang={lang}
                    segment={segmentById.get(idx)}
                    onJump={() => setFocusSegment(idx)}
                  />
                ))}
              </li>
            ))}
          </ul>
        </div>
      ) : null}

      <h3>{t(lang, "proposedSections")}</h3>
      {!dismissed ? (
        <div className="recording-row" style={{ marginBottom: "0.6rem" }}>
          <button
            type="button"
            className="primary"
            disabled={!actionable || busy || applyAll.length === 0}
            onClick={() => void run(applyAll)}
          >
            {t(lang, "applyAllEmpty")}
            {applyAll.length > 0 ? ` (${applyAll.length})` : ""}
          </button>
          {applyAll.length === 0 &&
          proposals.some((p) => !applied.has(p.section)) ? (
            <span className="muted">{t(lang, "applyAllNone")}</span>
          ) : null}
          <button
            type="button"
            className="tertiary"
            disabled={
              !actionable || busy || artifact.status !== "awaiting_review"
            }
            onClick={() => void dismiss()}
          >
            {t(lang, "dismissDraft")}
          </button>
        </div>
      ) : null}
      <ul className="scribe-sections">
        {proposals.map((s) => {
          const key = s.section as NoteSectionKey;
          const isApplied = applied.has(key);
          const empty = current[key].trim() === "";
          return (
            <li
              key={key}
              className={`scribe-section${s.review_needed ? " needs-review" : ""}`}
            >
              <div className="scribe-section-head">
                <strong>{t(lang, SECTION_LABEL[key])}</strong>
                <span className={`badge confidence-${s.confidence}`}>
                  {t(lang, confidenceKey(s.confidence))}
                </span>
                {s.review_needed ? (
                  <span className="badge warn">{t(lang, "reviewNeeded")}</span>
                ) : null}
                {isApplied ? (
                  <span className="badge ok">{t(lang, "alreadyApplied")}</span>
                ) : null}
              </div>
              <p className="ai-draft-text">{s.text}</p>
              {s.reasons.length > 0 ? (
                <ul className="muted scribe-reasons">
                  {s.reasons.map((r, i) => (
                    <li key={i}>{r}</li>
                  ))}
                </ul>
              ) : null}
              <div className="recording-row">
                <span className="muted">{t(lang, "sourceSegments")}:</span>
                {s.segments.map((idx) => (
                  <SegmentLink
                    key={idx}
                    lang={lang}
                    segment={segmentById.get(idx)}
                    onJump={() => setFocusSegment(idx)}
                  />
                ))}
              </div>
              {!dismissed && !isApplied ? (
                <div className="recording-row">
                  {empty ? (
                    <button
                      type="button"
                      className="secondary"
                      disabled={!actionable || busy}
                      onClick={() => void run([{ section: key, mode: "fill" }])}
                    >
                      {t(lang, "insertIntoSection")}
                    </button>
                  ) : (
                    <button
                      type="button"
                      className="secondary"
                      disabled={!actionable || busy}
                      onClick={() =>
                        void run([{ section: key, mode: "append" }])
                      }
                    >
                      {t(lang, "appendToSection")}
                    </button>
                  )}
                </div>
              ) : null}
            </li>
          );
        })}
      </ul>

      {error ? (
        <p role="alert" className="error">
          {error}
        </p>
      ) : null}
      {message ? (
        <p role="status" className="success">
          {message}
        </p>
      ) : null}

      <details className="scribe-transcript" open>
        <summary>{t(lang, "transcript")}</summary>
        <ol className="transcript-list">
          {output.transcript.map((seg) => (
            <li
              key={seg.index}
              id={`transcript-seg-${seg.index}`}
              tabIndex={-1}
              ref={(el) => {
                if (el) transcriptRefs.current.set(seg.index, el);
                else transcriptRefs.current.delete(seg.index);
              }}
              className={`transcript-seg confidence-${seg.confidence}${
                focusSegment === seg.index ? " highlighted" : ""
              }`}
            >
              <span className="timecode">{formatTimecode(seg.start_ms)}</span>{" "}
              <span className="speaker">
                {speakerLabel(lang, seg.speaker)}:
              </span>{" "}
              <span>{seg.text}</span>
              {seg.confidence === "low" ? (
                <span className="badge warn" style={{ marginLeft: "0.4rem" }}>
                  {t(lang, "confidenceLow")}
                </span>
              ) : null}
            </li>
          ))}
        </ol>
      </details>

      <p className="muted" style={{ fontSize: "0.85rem" }}>
        {t(lang, "transcriptionProvider")}: {output.transcription.provider}/
        {output.transcription.model} {output.transcription.model_version} ·{" "}
        {t(lang, "extractionProvider")}: {output.extraction.provider}/
        {output.extraction.model} {output.extraction.model_version} ·{" "}
        {t(lang, "generatedAt")}: {formatDateTime(lang, output.generated_at)}
        {artifact.note_version !== null
          ? ` · ${t(lang, "noteVersion")} ${artifact.note_version}`
          : ""}
      </p>
      {output.limitations.length > 0 ? (
        <details>
          <summary>{t(lang, "limitations")}</summary>
          <ul className="muted">
            {output.limitations.map((l, i) => (
              <li key={i}>{l}</li>
            ))}
          </ul>
        </details>
      ) : null}
    </section>
  );
}

function SegmentLink({
  lang,
  segment,
  onJump,
}: {
  lang: Lang;
  segment: TranscriptSegment | undefined;
  onJump: () => void;
}) {
  if (!segment) return null;
  return (
    <button
      type="button"
      className="linklike timecode-link"
      aria-label={`${t(lang, "jumpToSegment")} ${formatTimecode(segment.start_ms)}`}
      onClick={onJump}
    >
      {formatTimecode(segment.start_ms)}
    </button>
  );
}
