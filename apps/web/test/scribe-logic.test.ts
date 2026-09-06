import { describe, expect, it } from "vitest";
import {
  MAX_AUDIO_BYTES,
  MAX_DURATION_MS,
  appliedSections,
  defaultApplyAll,
  initialScribeState,
  mergeSection,
  scribeReducer,
  type ScribeArtifact,
  type ScribeEvent,
  type ScribeSection,
  type ScribeState,
} from "@/lib/scribe";
import {
  COCKPIT_STORAGE_ITEM,
  defaultConfig,
  loadConfig,
  move,
  parseConfig,
  saveConfig,
  setDensity,
  toggleHidden,
  visibleWidgets,
} from "@/lib/cockpit";
import { formatTimecode } from "@/lib/recorder";

function run(state: ScribeState, ...events: ScribeEvent[]): ScribeState {
  return events.reduce(scribeReducer, state);
}

function recording(durationMs = 5_000, bytes = 1_000) {
  return {
    blob: new Blob([new Uint8Array(bytes)], { type: "audio/webm" }),
    mimeType: "audio/webm",
    durationMs,
  };
}

describe("scribe recording state machine", () => {
  it("asks for consent first, then for the microphone, then records", () => {
    const s0 = initialScribeState(false);
    const s1 = scribeReducer(s0, { type: "start_requested" });
    expect(s1.phase).toBe("consent_required");
    const s2 = scribeReducer(s1, { type: "consent_given" });
    expect(s2.phase).toBe("requesting_permission");
    expect(s2.consented).toBe(true);
    expect(scribeReducer(s2, { type: "permission_granted" }).phase).toBe(
      "recording",
    );
  });

  it("skips the consent step once consent is already recorded", () => {
    const s = scribeReducer(initialScribeState(true), {
      type: "start_requested",
    });
    expect(s.phase).toBe("requesting_permission");
  });

  it("returns to idle when consent is declined", () => {
    const s = run(
      initialScribeState(false),
      { type: "start_requested" },
      { type: "consent_cancelled" },
    );
    expect(s.phase).toBe("idle");
    expect(s.consented).toBe(false);
  });

  it("maps microphone failures to retryable errors except unsupported", () => {
    const base = run(initialScribeState(true), { type: "start_requested" });
    const denied = scribeReducer(base, {
      type: "permission_failed",
      kind: "permission_denied",
    });
    expect(denied.phase).toBe("error");
    expect(denied.error).toEqual({
      kind: "permission_denied",
      retryable: true,
    });
    expect(scribeReducer(denied, { type: "retry" }).phase).toBe(
      "requesting_permission",
    );
    const unsupported = scribeReducer(base, {
      type: "permission_failed",
      kind: "unsupported",
    });
    expect(unsupported.error?.retryable).toBe(false);
    expect(scribeReducer(unsupported, { type: "retry" })).toBe(unsupported);
  });

  it("pauses, resumes and finishes into processing", () => {
    const rec = run(
      initialScribeState(true),
      { type: "start_requested" },
      { type: "permission_granted" },
    );
    const paused = scribeReducer(rec, { type: "pause" });
    expect(paused.phase).toBe("paused");
    expect(scribeReducer(paused, { type: "pause" })).toBe(paused);
    const resumed = scribeReducer(paused, { type: "resume" });
    expect(resumed.phase).toBe("recording");
    expect(scribeReducer(resumed, { type: "finish" }).phase).toBe("processing");
    expect(scribeReducer(paused, { type: "finish" }).phase).toBe("processing");
  });

  it("keeps the captured audio in memory across a retryable failure", () => {
    const processing = run(
      initialScribeState(true),
      { type: "start_requested" },
      { type: "permission_granted" },
      { type: "finish" },
    );
    const captured = scribeReducer(processing, {
      type: "captured",
      recording: recording(),
    });
    expect(captured.recording).not.toBeNull();
    const failed = scribeReducer(captured, {
      type: "transcription_failed",
      kind: "transcription_failed",
      retryable: true,
    });
    expect(failed.phase).toBe("error");
    expect(failed.recording).toBe(captured.recording);
    expect(failed.error?.retryable).toBe(true);
    const retried = scribeReducer(failed, { type: "retry" });
    expect(retried.phase).toBe("processing");
    expect(retried.recording).toBe(captured.recording);
    const done = scribeReducer(retried, { type: "transcribed" });
    expect(done.phase).toBe("ready");
    expect(done.recording).toBeNull();
  });

  it("is not retryable when the failure left no audio to resend", () => {
    const processing = run(
      initialScribeState(true),
      { type: "start_requested" },
      { type: "permission_granted" },
      { type: "finish" },
    );
    const failed = scribeReducer(processing, {
      type: "transcription_failed",
      kind: "transcription_failed",
      retryable: true,
    });
    expect(failed.error?.retryable).toBe(false);
  });

  it("rejects recordings that are too short, too long or too large", () => {
    const processing = run(
      initialScribeState(true),
      { type: "start_requested" },
      { type: "permission_granted" },
      { type: "finish" },
    );
    expect(
      scribeReducer(processing, { type: "captured", recording: recording(500) })
        .error,
    ).toEqual({ kind: "too_short", retryable: false });
    expect(
      scribeReducer(processing, {
        type: "captured",
        recording: recording(MAX_DURATION_MS + 1),
      }).error,
    ).toEqual({ kind: "too_long", retryable: false });
    expect(
      scribeReducer(processing, {
        type: "captured",
        recording: recording(5_000, MAX_AUDIO_BYTES + 1),
      }).error,
    ).toEqual({ kind: "too_large", retryable: false });
  });

  it("discard drops the audio and allows a fresh start", () => {
    const paused = run(
      initialScribeState(true),
      { type: "start_requested" },
      { type: "permission_granted" },
      { type: "pause" },
    );
    const discarded = scribeReducer(paused, { type: "discard" });
    expect(discarded.phase).toBe("discarded");
    expect(discarded.recording).toBeNull();
    expect(scribeReducer(discarded, { type: "start_requested" }).phase).toBe(
      "requesting_permission",
    );
  });

  it("ignores start while a recording is in progress", () => {
    const rec = run(
      initialScribeState(true),
      { type: "start_requested" },
      { type: "permission_granted" },
    );
    expect(scribeReducer(rec, { type: "start_requested" })).toBe(rec);
  });
});

const EMPTY = {
  reason_for_encounter: "",
  history_present_illness: "",
  medical_history: "",
  review_of_systems: "",
  physical_exam: "",
  assessment: "",
  plan: "",
  follow_up: "",
};

function proposal(section: string, text = "Proposed text"): ScribeSection {
  return {
    section,
    text,
    confidence: "high",
    review_needed: false,
    reasons: [],
    segments: [0],
  };
}

describe("scribe section merge rules", () => {
  it("fills an empty section and never overwrites clinician text", () => {
    expect(mergeSection("", "Cough", "fill")).toBe("Cough");
    expect(mergeSection("   \n", "Cough", "fill")).toBe("Cough");
    expect(mergeSection("Clinician wrote this", "Cough", "fill")).toBeNull();
  });

  it("appends below the clinician's text only when asked", () => {
    expect(mergeSection("Clinician wrote this \n", "Cough", "append")).toBe(
      "Clinician wrote this\n\nCough",
    );
  });

  it("Apply-all selects only proposed, unapplied, still-empty sections", () => {
    const proposals = [
      proposal("reason_for_encounter"),
      proposal("assessment"),
      proposal("plan", "   "),
      proposal("follow_up"),
      proposal("not_a_section"),
    ];
    const current = { ...EMPTY, assessment: "Clinician assessment" };
    const selected = defaultApplyAll(
      proposals,
      current,
      new Set(["follow_up"]),
    );
    expect(selected).toEqual([
      { section: "reason_for_encounter", mode: "fill" },
    ]);
  });

  it("reads applied sections from the artifact review detail", () => {
    const artifact = {
      review_detail: { applied: [{ section: "plan", mode: "fill" }] },
    } as unknown as ScribeArtifact;
    expect(appliedSections(artifact)).toEqual(new Set(["plan"]));
    expect(appliedSections(null).size).toBe(0);
  });

  it("formats timecodes as mm:ss", () => {
    expect(formatTimecode(0)).toBe("00:00");
    expect(formatTimecode(61_500)).toBe("01:01");
  });
});

describe("dashboard cockpit configuration", () => {
  it("gives role-specific defaults", () => {
    expect(visibleWidgets(defaultConfig(["physician"]))).toEqual([
      "drafts",
      "attention",
      "results",
      "tasks",
      "ai",
    ]);
    // Laboratory professionals and nurses share the results-first layout,
    // matched on the server's role name.
    for (const role of ["laboratory_professional", "nurse"]) {
      expect(visibleWidgets(defaultConfig([role]))).toEqual([
        "results",
        "tasks",
        "attention",
        "ai",
      ]);
      expect(defaultConfig([role]).density).toBe("compact");
    }
    // Any other role gets the minimal generic cockpit.
    expect(visibleWidgets(defaultConfig(["registration_staff"]))).toEqual([
      "results",
      "tasks",
    ]);
  });

  it("hides, shows, reorders and changes density", () => {
    let cfg = defaultConfig(["physician"]);
    cfg = toggleHidden(cfg, "ai");
    expect(visibleWidgets(cfg)).not.toContain("ai");
    cfg = toggleHidden(cfg, "ai");
    expect(visibleWidgets(cfg)).toContain("ai");
    cfg = move(cfg, "results", -1);
    expect(cfg.order.slice(0, 3)).toEqual(["drafts", "results", "attention"]);
    expect(move(cfg, "drafts", -1)).toBe(cfg);
    cfg = setDensity(cfg, "compact");
    expect(cfg.density).toBe("compact");
  });

  it("falls back to defaults for malformed storage and keeps every widget", () => {
    const fallback = defaultConfig(["physician"]);
    expect(parseConfig("not json", fallback)).toBe(fallback);
    expect(parseConfig(JSON.stringify({ order: "x" }), fallback)).toBe(
      fallback,
    );
    const parsed = parseConfig(
      JSON.stringify({
        order: ["tasks", "bogus", "tasks"],
        hidden: ["drafts", "bogus"],
        density: "compact",
      }),
      fallback,
    );
    expect(parsed.order).toEqual([
      "tasks",
      "drafts",
      "attention",
      "results",
      "ai",
    ]);
    expect(parsed.hidden).toEqual(["drafts"]);
    expect(parsed.density).toBe("compact");
  });

  it("round-trips through storage and stores layout only", () => {
    const store = new Map<string, string>();
    const storage = {
      getItem: (k: string) => store.get(k) ?? null,
      setItem: (k: string, v: string) => void store.set(k, v),
    };
    const cfg = setDensity(
      toggleHidden(defaultConfig(["physician"]), "tasks"),
      "compact",
    );
    saveConfig(storage, cfg);
    expect(loadConfig(storage, ["physician"])).toEqual(cfg);
    expect(
      Object.keys(JSON.parse(store.get(COCKPIT_STORAGE_ITEM)!)).sort(),
    ).toEqual(["density", "hidden", "order"]);
    expect(loadConfig(null, ["physician"])).toEqual(
      defaultConfig(["physician"]),
    );
  });
});
