import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import EncounterPage from "@/app/encounters/[id]/page";
import { RecordingDock } from "@/app/encounters/[id]/scribe";
import {
  DiagnosticHistory,
  type DiagnosticResult,
  type Diagnostics,
} from "@/app/encounters/[id]/brief";
import { SessionProvider, useSession } from "@/lib/session";
import type { AudioRecorder, Recording } from "@/lib/recorder";
import { RecorderError } from "@/lib/recorder";
import type { ScribeArtifact, TranscriptSegment } from "@/lib/scribe";

const router = { push: vi.fn(), replace: vi.fn(), prefetch: vi.fn() };
vi.mock("next/navigation", () => ({
  useRouter: () => router,
  usePathname: () => "/encounters/e1",
}));

const META = {
  tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
  user: {
    username: "dr.garcia",
    display_name: "Dr. García",
    roles: ["physician"],
  },
  facilities: [
    {
      id: "f1",
      name: "Central Hospital",
      accessible: true,
      can_register: false,
      can_act_clinically: true,
    },
  ],
};

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function apiError(status: number, code: string, message: string): Response {
  return jsonResponse({ error: { code, message } }, status);
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

/** A scripted recorder: no microphone, no MediaRecorder. */
class FakeRecorder implements AudioRecorder {
  static instances: FakeRecorder[] = [];
  started = false;
  paused = false;
  stopped = false;
  discarded = false;
  durationMs: number;
  bytes: number;
  failStart: RecorderError | null;
  /** When set, `start()` (the permission prompt) waits for this promise. */
  startGate: Promise<void> | null;

  constructor(
    opts: {
      durationMs?: number;
      bytes?: number;
      failStart?: RecorderError | null;
      startGate?: Promise<void>;
    } = {},
  ) {
    this.durationMs = opts.durationMs ?? 5_000;
    this.bytes = opts.bytes ?? 1_000;
    this.failStart = opts.failStart ?? null;
    this.startGate = opts.startGate ?? null;
    FakeRecorder.instances.push(this);
  }
  async start(): Promise<void> {
    if (this.startGate) await this.startGate;
    if (this.failStart) throw this.failStart;
    this.started = true;
  }
  pause(): void {
    this.paused = true;
  }
  resume(): void {
    this.paused = false;
  }
  async stop(): Promise<Recording> {
    this.stopped = true;
    return {
      blob: new Blob([new Uint8Array(this.bytes)], { type: "audio/webm" }),
      mimeType: "audio/webm",
      durationMs: this.durationMs,
    };
  }
  discard(): void {
    this.discarded = true;
  }
  elapsedMs(): number {
    return this.durationMs;
  }
}

const SEGMENTS: TranscriptSegment[] = [
  {
    index: 0,
    start_ms: 0,
    end_ms: 2_000,
    speaker: "clinician",
    text: "What brings you in today?",
    confidence: "high",
  },
  {
    index: 1,
    start_ms: 2_000,
    end_ms: 5_000,
    speaker: "patient",
    text: "I've had a cough for three days.",
    confidence: "high",
  },
  {
    index: 2,
    start_ms: 5_000,
    end_ms: 8_000,
    speaker: "patient",
    text: "No fever. Actually I measured 38 last night.",
    confidence: "medium",
  },
];

function artifact(overrides: Partial<ScribeArtifact> = {}): ScribeArtifact {
  return {
    id: "a1",
    status: "awaiting_review",
    output: {
      schema_version: "scribe-draft.v1",
      encounter_id: "e1",
      source_note_version: 1,
      language: "en",
      transcript: SEGMENTS,
      sections: [
        {
          section: "reason_for_encounter",
          text: "Cough for three days.",
          confidence: "high",
          review_needed: false,
          reasons: [],
          segments: [1],
        },
        {
          section: "history_present_illness",
          text: "Denied fever, later reported 38 °C.",
          confidence: "medium",
          review_needed: true,
          reasons: ["Conflicting statements about fever."],
          segments: [2],
        },
        {
          section: "assessment",
          text: "Likely viral upper respiratory infection.",
          confidence: "high",
          review_needed: true,
          reasons: ["Clinical impression — confirm before signing."],
          segments: [1, 2],
        },
      ],
      flags: [
        {
          kind: "contradiction",
          message: "Fever was first denied and then reported.",
          segments: [2],
          sections: ["history_present_illness"],
        },
      ],
      transcription: {
        provider: "dmind-fake",
        model: "fake-transcribe",
        model_version: "0.1.0",
      },
      extraction: {
        provider: "dmind-scribe-rules",
        model: "section-mapper",
        model_version: "0.1.0",
      },
      generated_at: "2026-08-29T10:00:00Z",
      limitations: ["Assistive draft; verify before signing."],
    },
    limitations: ["Assistive draft; verify before signing."],
    model: "fake-transcribe",
    model_version: "0.1.0",
    route: "dmind-fake",
    generated_at: "2026-08-29T10:00:00Z",
    review_decision: null,
    review_detail: null,
    note_version: 1,
    stale: false,
    ...overrides,
  };
}

const DRAFT_NOTE = {
  id: "n1",
  status: "draft",
  version: 1,
  reason_for_encounter: "",
  history_present_illness: null,
  medical_history: null,
  review_of_systems: null,
  physical_exam: null,
  assessment: "Clinician impression.",
  plan: null,
  follow_up: null,
  author: "Dr. García",
  updated_at: "2026-08-29T09:10:00Z",
  signed_at: null,
  signed_by: null,
};

function workspace(overrides: Record<string, unknown> = {}) {
  return {
    encounter: {
      id: "e1",
      status: "in_progress",
      encounter_type: "consultation",
      started_at: "2026-08-29T09:00:00Z",
      completed_at: null,
      practitioner: "Dr. García",
      facility_name: "Central Hospital",
      own: true,
    },
    patient: {
      id: "p1",
      family_name: "Demopatient",
      given_name: "Alba",
      birth_date: "1990-03-03",
      sex: "female",
      identifier: "SYN-0001",
    },
    allergies: [{ substance: "Penicillin", criticality: "high" }],
    medications: [{ name: "Metformin 850 mg twice daily", status: "active" }],
    alerts: [],
    note: DRAFT_NOTE,
    addenda: [],
    vitals: [],
    previous_vitals: [],
    diagnoses: [
      {
        id: "d1",
        display: "Type 2 diabetes mellitus",
        code: "E11",
        status: "active",
        recorded_at: "2026-08-01T09:00:00Z",
      },
    ],
    service_requests: [],
    ai_draft: null,
    scribe_draft: artifact(),
    recording_consent: { granted: true, recorded_at: "2026-08-29T09:00:00Z" },
    brief: {
      generated_at: "2026-08-29T09:00:00Z",
      recent_notes: [
        {
          encounter_id: "e0",
          started_at: "2026-08-20T08:30:00Z",
          signed_at: "2026-08-20T09:00:00Z",
          signed_by: "Dr. García",
          reason_for_encounter: "Diabetes review",
          assessment: "Suboptimal glycaemic control.",
          plan: null,
        },
      ],
      open_tasks: [],
      open_requests: [],
      recent_abnormal: [
        {
          id: "o1",
          service_request_id: "sr1",
          code: "2823-3",
          display: "Potassium [Moles/volume] in Serum",
          value: "6.8",
          unit: "mmol/L",
          reference_range: "3.5-5.1 mmol/L",
          abnormal: "high",
          critical: true,
          effective_at: "2026-08-28T09:00:00Z",
        },
      ],
    },
    diagnostics: {
      generated_at: "2026-08-29T09:00:00Z",
      tests: [
        {
          code: "2823-3",
          display: "Potassium [Moles/volume] in Serum",
          unit: "mmol/L",
          reference_range: "3.5-5.1 mmol/L",
          latest_value: "6.8",
          latest_abnormal: "high",
          direction: "rising",
          result_count: 2,
          incomparable_count: 0,
          pending_count: 0,
          results: [
            {
              id: "o0",
              service_request_id: "sr0",
              value: "4.9",
              unit: "mmol/L",
              normalized_value: "4.9",
              comparable: true,
              reference_range: "3.5-5.1 mmol/L",
              abnormal: null,
              critical: false,
              status: "final",
              superseded: false,
              loop_state: "closed",
              effective_at: "2026-08-01T09:00:00Z",
              received_at: "2026-08-01T10:00:00Z",
            },
            {
              id: "o1",
              service_request_id: "sr1",
              value: "6.8",
              unit: "mmol/L",
              normalized_value: "6.8",
              comparable: true,
              reference_range: "3.5-5.1 mmol/L",
              abnormal: "high",
              critical: true,
              status: "final",
              superseded: false,
              loop_state: "awaiting_review",
              effective_at: "2026-08-28T09:00:00Z",
              received_at: "2026-08-28T10:00:00Z",
            },
          ],
          pending: [],
        },
      ],
      analysis: {
        schema_version: "trend-analysis.v1",
        provider: {
          provider: "dmind-fake",
          model: "trend-rules",
          model_version: "0.1.0",
        },
        language: "en",
        statements: [
          {
            code: "2823-3",
            text: "Potassium is rising; the latest value is above the reference range.",
            facts: ["o0", "o1"],
          },
        ],
        limitations: ["Assistive commentary only."],
      },
    },
    capabilities: {
      can_document: true,
      can_sign: true,
      can_add_addendum: false,
      can_order_lab: true,
    },
    ...overrides,
  };
}

function setupPage(
  ws: unknown,
  posts: Record<string, (body: unknown) => Response | Promise<Response>> = {},
) {
  const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (url === "/api/session")
      return Promise.resolve(jsonResponse({ authenticated: true }));
    if (url === "/api/v1/meta/tenant")
      return Promise.resolve(jsonResponse(META));
    if (init?.method === "POST") {
      const handler = posts[url];
      if (handler) {
        const body = init.body ? JSON.parse(String(init.body)) : null;
        return Promise.resolve(handler(body));
      }
      return Promise.resolve(jsonResponse({}));
    }
    if (url.startsWith("/api/v1/encounters/e1?"))
      return Promise.resolve(
        jsonResponse(
          typeof ws === "function" ? (ws as (url: string) => unknown)(url) : ws,
        ),
      );
    return Promise.resolve(jsonResponse({}));
  });
  vi.stubGlobal("fetch", fetchMock);
  render(
    <SessionProvider>
      <EncounterPage params={{ id: "e1" }} />
    </SessionProvider>,
  );
  return fetchMock;
}

function setupDock(
  props: {
    consented?: boolean;
    recorder?: () => AudioRecorder;
    lang?: "en" | "es";
  } = {},
  posts: Record<string, (body: unknown) => Response | Promise<Response>> = {},
) {
  const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (url === "/api/session")
      return Promise.resolve(jsonResponse({ authenticated: true }));
    if (url === "/api/v1/meta/tenant")
      return Promise.resolve(jsonResponse(META));
    if (init?.method === "POST") {
      const handler = posts[url];
      if (handler) {
        const body = init.body ? JSON.parse(String(init.body)) : null;
        return Promise.resolve(handler(body));
      }
      return Promise.resolve(jsonResponse({}));
    }
    return Promise.resolve(jsonResponse({}));
  });
  vi.stubGlobal("fetch", fetchMock);
  const onConsentRecorded = vi.fn();
  const onDraft = vi.fn();
  const factory = props.recorder ?? (() => new FakeRecorder());
  const view = render(
    <SessionProvider>
      <RecordingDock
        encounterId="e1"
        lang={props.lang ?? "en"}
        consented={props.consented ?? true}
        enabled
        recorderFactory={factory}
        onConsentRecorded={onConsentRecorded}
        onDraft={onDraft}
      />
    </SessionProvider>,
  );
  return { fetchMock, onConsentRecorded, onDraft, view };
}

async function recordAndFinish(user: ReturnType<typeof userEvent.setup>) {
  await user.click(screen.getByRole("button", { name: "Record consultation" }));
  await screen.findByRole("button", { name: "Pause" });
  await user.click(screen.getByRole("button", { name: "Finish" }));
}

describe("recording dock", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    FakeRecorder.instances = [];
  });

  it("asks for consent before the microphone and records it server-side", async () => {
    const user = userEvent.setup();
    const consent = vi.fn(() =>
      jsonResponse({ id: "c1", granted: true, recorded_at: "now" }),
    );
    const { onConsentRecorded } = setupDock(
      { consented: false },
      { "/api/v1/encounters/e1/recording-consent": consent },
    );
    await user.click(
      screen.getByRole("button", { name: "Record consultation" }),
    );
    expect(screen.getByText("Patient consent required")).toBeInTheDocument();
    expect(FakeRecorder.instances).toHaveLength(0);
    await user.click(screen.getByRole("button", { name: /Patient consented/ }));
    await screen.findByRole("button", { name: "Pause" });
    expect(consent).toHaveBeenCalledWith({ granted: true });
    expect(onConsentRecorded).toHaveBeenCalled();
    expect(FakeRecorder.instances[0].started).toBe(true);
  });

  it("declining consent returns to idle without touching the microphone", async () => {
    const user = userEvent.setup();
    setupDock({ consented: false });
    await user.click(
      screen.getByRole("button", { name: "Record consultation" }),
    );
    await user.click(screen.getByRole("button", { name: "Not now" }));
    expect(
      screen.getByRole("button", { name: "Record consultation" }),
    ).toBeInTheDocument();
    expect(FakeRecorder.instances).toHaveLength(0);
  });

  it("shows a retryable microphone-permission error", async () => {
    const user = userEvent.setup();
    let attempt = 0;
    setupDock({
      recorder: () =>
        new FakeRecorder({
          failStart:
            attempt++ === 0
              ? new RecorderError("permission_denied", "denied")
              : null,
        }),
    });
    await user.click(
      screen.getByRole("button", { name: "Record consultation" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /Microphone access was denied/,
    );
    await user.click(screen.getByRole("button", { name: "Try again" }));
    await screen.findByRole("button", { name: "Pause" });
    expect(FakeRecorder.instances).toHaveLength(2);
  });

  it("pauses, resumes, finishes and hands the draft to the workspace", async () => {
    const user = userEvent.setup();
    const scribe = vi.fn((body: unknown) => {
      const b = body as Record<string, unknown>;
      expect(b.mime_type).toBe("audio/webm");
      expect(b.duration_ms).toBe(5_000);
      expect(b.language).toBe("en");
      expect(typeof b.audio_base64).toBe("string");
      return jsonResponse(artifact());
    });
    const { onDraft } = setupDock(
      {},
      { "/api/v1/encounters/e1/scribe": scribe },
    );
    await user.click(
      screen.getByRole("button", { name: "Record consultation" }),
    );
    const dock = await screen.findByRole("region", {
      name: "Consultation recording",
    });
    expect(dock).toHaveAttribute("data-phase", "recording");
    expect(screen.getByRole("status")).toHaveTextContent(/Recording/);
    await user.click(screen.getByRole("button", { name: "Pause" }));
    expect(dock).toHaveAttribute("data-phase", "paused");
    expect(FakeRecorder.instances[0].paused).toBe(true);
    expect(screen.getByRole("status")).toHaveTextContent(/Paused/);
    await user.click(screen.getByRole("button", { name: "Resume" }));
    expect(dock).toHaveAttribute("data-phase", "recording");
    await user.click(screen.getByRole("button", { name: "Finish" }));
    await waitFor(() => expect(onDraft).toHaveBeenCalledTimes(1));
    expect(dock).toHaveAttribute("data-phase", "ready");
    expect(scribe).toHaveBeenCalledTimes(1);
    expect(
      screen.getByRole("button", { name: "Record again" }),
    ).toBeInTheDocument();
  });

  it("shows the processing state while transcription is pending", async () => {
    const user = userEvent.setup();
    const pending = deferred<Response>();
    setupDock({}, { "/api/v1/encounters/e1/scribe": () => pending.promise });
    await recordAndFinish(user);
    const dock = screen.getByRole("region", { name: "Consultation recording" });
    await waitFor(() =>
      expect(dock).toHaveAttribute("data-phase", "processing"),
    );
    expect(screen.getByRole("status")).toHaveTextContent(/Transcribing/);
    await act(async () => {
      pending.resolve(jsonResponse(artifact()));
    });
    await waitFor(() => expect(dock).toHaveAttribute("data-phase", "ready"));
  });

  it("discard requires confirmation and releases the recorder", async () => {
    const user = userEvent.setup();
    setupDock();
    await user.click(
      screen.getByRole("button", { name: "Record consultation" }),
    );
    await screen.findByRole("button", { name: "Pause" });
    await user.click(screen.getByRole("button", { name: "Discard" }));
    expect(screen.getByText(/Discard this recording\?/)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Cancel" }));
    expect(FakeRecorder.instances[0].discarded).toBe(false);
    await user.click(screen.getByRole("button", { name: "Discard" }));
    await user.click(screen.getByRole("button", { name: "Confirm" }));
    expect(FakeRecorder.instances[0].discarded).toBe(true);
    expect(screen.getByRole("status")).toHaveTextContent(/discarded/i);
    expect(
      screen.getByRole("button", { name: "Record again" }),
    ).toBeInTheDocument();
  });

  it("keeps the audio for a retry after a transient transcription failure", async () => {
    const user = userEvent.setup();
    let calls = 0;
    const scribe = vi.fn(() =>
      calls++ === 0
        ? apiError(503, "provider_unavailable", "unavailable")
        : jsonResponse(artifact()),
    );
    const { onDraft } = setupDock(
      {},
      { "/api/v1/encounters/e1/scribe": scribe },
    );
    await recordAndFinish(user);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /Transcription failed/,
    );
    await user.click(
      screen.getByRole("button", { name: "Retry transcription" }),
    );
    await waitFor(() => expect(onDraft).toHaveBeenCalledTimes(1));
    expect(scribe).toHaveBeenCalledTimes(2);
    // Same recorder: the audio was resent, not re-recorded.
    expect(FakeRecorder.instances).toHaveLength(1);
  });

  it("does not offer a retry for a closed encounter", async () => {
    const user = userEvent.setup();
    setupDock(
      {},
      {
        "/api/v1/encounters/e1/scribe": () =>
          apiError(409, "encounter_not_active", "closed"),
      },
    );
    await recordAndFinish(user);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /consultation is closed/i,
    );
    expect(
      screen.queryByRole("button", { name: /Retry|Try again/ }),
    ).not.toBeInTheDocument();
  });

  it("rejects a recording shorter than one second locally", async () => {
    const user = userEvent.setup();
    const scribe = vi.fn(() => jsonResponse(artifact()));
    setupDock(
      { recorder: () => new FakeRecorder({ durationMs: 400 }) },
      { "/api/v1/encounters/e1/scribe": scribe },
    );
    await recordAndFinish(user);
    expect(await screen.findByRole("alert")).toHaveTextContent(/too short/i);
    expect(scribe).not.toHaveBeenCalled();
  });

  it("releases the microphone on unmount", async () => {
    const user = userEvent.setup();
    const { view } = setupDock();
    await user.click(
      screen.getByRole("button", { name: "Record consultation" }),
    );
    await screen.findByRole("button", { name: "Pause" });
    view.unmount();
    expect(FakeRecorder.instances[0].discarded).toBe(true);
  });

  it("discards a recorder whose start failed so no track or chunk is kept", async () => {
    const user = userEvent.setup();
    setupDock({
      recorder: () =>
        new FakeRecorder({ failStart: new RecorderError("failed", "boom") }),
    });
    await user.click(
      screen.getByRole("button", { name: "Record consultation" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /Recording failed/,
    );
    expect(FakeRecorder.instances).toHaveLength(1);
    expect(FakeRecorder.instances[0].discarded).toBe(true);
    expect(
      screen.getByRole("button", { name: "Try again" }),
    ).toBeInTheDocument();
  });

  it("drops a recorder whose permission prompt resolves after unmount", async () => {
    const user = userEvent.setup();
    const gate = deferred<void>();
    const { view } = setupDock({
      recorder: () => new FakeRecorder({ startGate: gate.promise }),
    });
    await user.click(
      screen.getByRole("button", { name: "Record consultation" }),
    );
    const rec = FakeRecorder.instances[0];
    expect(rec.started).toBe(false);
    view.unmount();
    // Nothing to stop yet: the stream has not been granted.
    expect(rec.discarded).toBe(true);
    rec.discarded = false;
    await act(async () => {
      gate.resolve();
      await gate.promise;
    });
    // The late grant is released immediately, not left running invisibly.
    expect(rec.started).toBe(true);
    expect(rec.discarded).toBe(true);
  });

  it("a permission failure that lands after unmount is discarded silently", async () => {
    const user = userEvent.setup();
    const gate = deferred<void>();
    const { view } = setupDock({
      recorder: () =>
        new FakeRecorder({
          startGate: gate.promise,
          failStart: new RecorderError("permission_denied", "denied"),
        }),
    });
    await user.click(
      screen.getByRole("button", { name: "Record consultation" }),
    );
    const rec = FakeRecorder.instances[0];
    view.unmount();
    rec.discarded = false;
    await act(async () => {
      gate.resolve();
      await gate.promise;
    });
    expect(rec.discarded).toBe(true);
    expect(screen.queryByRole("alert")).not.toBeInTheDocument();
  });

  it("renders the Spanish controls", async () => {
    const user = userEvent.setup();
    setupDock({ lang: "es", consented: false });
    await user.click(screen.getByRole("button", { name: "Grabar consulta" }));
    expect(
      screen.getByText("Se requiere el consentimiento del paciente"),
    ).toBeInTheDocument();
  });
});

describe("scribe review in the encounter workspace", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    router.push.mockClear();
  });

  it("shows the draft, flags, confidence and transcript with timecodes", async () => {
    setupPage(workspace());
    expect(await screen.findByText("dMind scribe draft")).toBeInTheDocument();
    expect(screen.getByText("Contradiction")).toBeInTheDocument();
    expect(
      screen.getByText("Fever was first denied and then reported."),
    ).toBeInTheDocument();
    expect(screen.getAllByText("Review needed").length).toBe(2);
    expect(screen.getAllByText("High confidence").length).toBeGreaterThan(0);
    expect(screen.getByText("Medium confidence")).toBeInTheDocument();
    expect(
      screen.getByText("I've had a cough for three days."),
    ).toBeInTheDocument();
    expect(
      screen.getAllByRole("button", { name: "Go to transcript segment 00:02" })
        .length,
    ).toBeGreaterThan(0);
    expect(
      screen.getAllByText("Assistive draft — requires your review").length,
    ).toBeGreaterThan(0);
  });

  it("Apply-all targets only empty sections and hydrates the note from the server", async () => {
    const user = userEvent.setup();
    const review = vi.fn((body: unknown) => {
      const b = body as {
        decision: string;
        sections: { section: string; mode: string }[];
        version: number;
        assessment: string;
      };
      expect(b.decision).toBe("apply");
      expect(b.version).toBe(1);
      expect(b.assessment).toBe("Clinician impression.");
      expect(b.sections).toEqual([
        { section: "reason_for_encounter", mode: "fill" },
        { section: "history_present_illness", mode: "fill" },
      ]);
      return jsonResponse({
        id: "a1",
        status: "approved",
        review_decision: "approved",
        review_detail: { applied: b.sections },
        note: {
          id: "n1",
          status: "draft",
          version: 2,
          reason_for_encounter: "Cough for three days.",
          history_present_illness: "Denied fever, later reported 38 °C.",
          assessment: "Clinician impression.",
        },
        note_version: 2,
      });
    });
    setupPage(workspace(), {
      "/api/v1/encounters/e1/scribe/a1/review": review,
    });
    const applyAll = await screen.findByRole("button", {
      name: "Insert all into empty sections (2)",
    });
    await user.click(applyAll);
    await waitFor(() => expect(review).toHaveBeenCalledTimes(1));
    expect(screen.getByLabelText(/Reason for consultation/)).toHaveValue(
      "Cough for three days.",
    );
    expect(
      screen.getByLabelText(/History of presenting complaint/),
    ).toHaveValue("Denied fever, later reported 38 °C.");
    expect(screen.getByLabelText(/^Assessment/)).toHaveValue(
      "Clinician impression.",
    );
    expect(screen.queryByText("Unsaved changes")).not.toBeInTheDocument();
    expect(screen.getAllByText("Inserted").length).toBe(2);
  });

  it("expands a collapsed optional section that receives scribe text", async () => {
    const user = userEvent.setup();
    const review = vi.fn((body: unknown) => {
      const b = body as { sections: { section: string; mode: string }[] };
      expect(b.sections).toEqual([{ section: "follow_up", mode: "fill" }]);
      return jsonResponse({
        id: "a1",
        status: "approved",
        review_decision: "approved",
        review_detail: { applied: b.sections },
        note: { version: 2, follow_up: "Return in two weeks." },
        note_version: 2,
      });
    });
    const art = artifact();
    art.output!.sections = [
      {
        section: "follow_up",
        text: "Return in two weeks.",
        confidence: "high",
        review_needed: false,
        reasons: [],
        segments: [1],
      },
    ];
    setupPage(workspace({ scribe_draft: art }), {
      "/api/v1/encounters/e1/scribe/a1/review": review,
    });
    await screen.findByText("dMind scribe draft");
    expect(
      screen.queryByLabelText(/Follow-up instructions/),
    ).not.toBeInTheDocument();
    await user.click(
      screen.getByRole("button", { name: "Insert into empty section" }),
    );
    await waitFor(() => expect(review).toHaveBeenCalledTimes(1));
    expect(
      screen.getByRole("textbox", { name: /Follow-up instructions/ }),
    ).toHaveValue("Return in two weeks.");
  });

  it("offers Append (never Insert) for a section the clinician already wrote", async () => {
    const user = userEvent.setup();
    const review = vi.fn((body: unknown) => {
      const b = body as { sections: { section: string; mode: string }[] };
      expect(b.sections).toEqual([{ section: "assessment", mode: "append" }]);
      return jsonResponse({
        id: "a1",
        status: "approved",
        review_decision: "approved",
        review_detail: { applied: b.sections },
        note: {
          version: 2,
          assessment:
            "Clinician impression.\n\nLikely viral upper respiratory infection.",
        },
        note_version: 2,
      });
    });
    setupPage(workspace(), {
      "/api/v1/encounters/e1/scribe/a1/review": review,
    });
    await screen.findByText("dMind scribe draft");
    const appendButtons = screen.getAllByRole("button", {
      name: "Append below my text",
    });
    expect(appendButtons).toHaveLength(1);
    await user.click(appendButtons[0]);
    await waitFor(() => expect(review).toHaveBeenCalledTimes(1));
    expect(screen.getByLabelText(/^Assessment/)).toHaveValue(
      "Clinician impression.\n\nLikely viral upper respiratory infection.",
    );
  });

  it("keeps text typed while the apply request is pending and marks it unsaved", async () => {
    const user = userEvent.setup();
    const pending = deferred<Response>();
    setupPage(workspace(), {
      "/api/v1/encounters/e1/scribe/a1/review": () => pending.promise,
    });
    const insert = (
      await screen.findAllByRole("button", {
        name: "Insert into empty section",
      })
    )[0];
    await user.click(insert);
    const plan = screen.getByLabelText(/^Plan/);
    await user.type(plan, "Rest and fluids");
    await act(async () => {
      pending.resolve(
        jsonResponse({
          id: "a1",
          status: "approved",
          review_decision: "approved",
          review_detail: {
            applied: [{ section: "reason_for_encounter", mode: "fill" }],
          },
          note: {
            version: 2,
            reason_for_encounter: "Cough for three days.",
            plan: null,
          },
          note_version: 2,
        }),
      );
    });
    await waitFor(() =>
      expect(screen.getByLabelText(/Reason for consultation/)).toHaveValue(
        "Cough for three days.",
      ),
    );
    expect(plan).toHaveValue("Rest and fluids");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
  });

  it("never overwrites text typed into the target section during the request", async () => {
    const user = userEvent.setup();
    const pending = deferred<Response>();
    setupPage(workspace(), {
      "/api/v1/encounters/e1/scribe/a1/review": () => pending.promise,
    });
    const insert = (
      await screen.findAllByRole("button", {
        name: "Insert into empty section",
      })
    )[0];
    await user.click(insert);
    const reason = screen.getByLabelText(/Reason for consultation/);
    await user.type(reason, "Typed meanwhile");
    await act(async () => {
      pending.resolve(
        jsonResponse({
          id: "a1",
          status: "approved",
          review_decision: "approved",
          review_detail: {
            applied: [{ section: "reason_for_encounter", mode: "fill" }],
          },
          note: { version: 2, reason_for_encounter: "Cough for three days." },
          note_version: 2,
        }),
      );
    });
    await waitFor(() =>
      expect(screen.getByText("Inserted")).toBeInTheDocument(),
    );
    expect(reason).toHaveValue("Typed meanwhile\n\nCough for three days.");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
  });

  it("reports a stale draft when the note version moved on", async () => {
    const user = userEvent.setup();
    setupPage(workspace(), {
      "/api/v1/encounters/e1/scribe/a1/review": () =>
        apiError(409, "version_conflict", "conflict"),
    });
    const insert = (
      await screen.findAllByRole("button", {
        name: "Insert into empty section",
      })
    )[0];
    await user.click(insert);
    const alerts = await screen.findAllByRole("alert");
    expect(
      alerts.some((a) =>
        /changed by someone else|note changed/i.test(a.textContent ?? ""),
      ),
    ).toBe(true);
    expect(screen.getByLabelText(/Reason for consultation/)).toHaveValue("");
  });

  it("dismisses the draft", async () => {
    const user = userEvent.setup();
    const review = vi.fn((body: unknown) => {
      expect(body).toEqual({ decision: "dismiss" });
      return jsonResponse({
        id: "a1",
        status: "rejected",
        review_decision: "rejected",
        review_detail: null,
        note: null,
        note_version: 1,
      });
    });
    setupPage(workspace(), {
      "/api/v1/encounters/e1/scribe/a1/review": review,
    });
    await user.click(
      await screen.findByRole("button", { name: "Dismiss draft" }),
    );
    await waitFor(() => expect(review).toHaveBeenCalledTimes(1));
    expect(
      screen.queryByRole("button", { name: "Insert into empty section" }),
    ).not.toBeInTheDocument();
  });

  it("shows a stale notice and disables insertion (not dismissal) when the artifact is stale", async () => {
    setupPage(workspace({ scribe_draft: artifact({ stale: true }) }));
    await screen.findByText("dMind scribe draft");
    expect(
      screen.getAllByText(/note changed since this draft/i).length,
    ).toBeGreaterThan(0);
    for (const b of screen.getAllByRole("button", {
      name: /Insert into empty section|Append to section|Insert all/,
    })) {
      expect(b).toBeDisabled();
    }
    expect(screen.getByRole("button", { name: "Dismiss draft" })).toBeEnabled();
  });

  it("a save that advances the note version makes the draft stale locally", async () => {
    const user = userEvent.setup();
    // The post-save reload is held back so the assertion sees the local
    // transition, not the server's recomputed flag.
    let releaseReload: () => void = () => {};
    const reloadGate = new Promise<void>((r) => (releaseReload = r));
    let saved = false;
    const fetchMock = vi.fn(
      async (input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url === "/api/session")
          return jsonResponse({ authenticated: true });
        if (url === "/api/v1/meta/tenant") return jsonResponse(META);
        if (init?.method === "POST" && url === "/api/v1/encounters/e1/note") {
          saved = true;
          return jsonResponse({ version: 2 });
        }
        if (url.startsWith("/api/v1/encounters/e1?")) {
          if (!saved) return jsonResponse(workspace());
          await reloadGate;
          return jsonResponse(
            workspace({
              note: { ...DRAFT_NOTE, version: 2, plan: "Rest and fluids." },
              scribe_draft: artifact({ stale: true }),
            }),
          );
        }
        return jsonResponse({});
      },
    );
    vi.stubGlobal("fetch", fetchMock);
    render(
      <SessionProvider>
        <EncounterPage params={{ id: "e1" }} />
      </SessionProvider>,
    );
    const insert = (
      await screen.findAllByRole("button", {
        name: "Insert into empty section",
      })
    )[0];
    expect(insert).toBeEnabled();
    await user.type(screen.getByLabelText(/^Plan/), "Rest and fluids.");
    await user.click(screen.getByRole("button", { name: "Save draft" }));
    await waitFor(() =>
      expect(
        screen.getAllByText(/note changed since this draft/i).length,
      ).toBeGreaterThan(0),
    );
    for (const b of screen.getAllByRole("button", {
      name: /Insert into empty section|Append to section|Insert all/,
    })) {
      expect(b).toBeDisabled();
    }
    releaseReload();
    await waitFor(() =>
      expect(screen.getByText("Draft saved.")).toBeInTheDocument(),
    );
    for (const b of screen.getAllByRole("button", {
      name: /Insert into empty section|Append to section|Insert all/,
    })) {
      expect(b).toBeDisabled();
    }
  });

  it("treats a server artifact_stale refusal as stale and reloads", async () => {
    const user = userEvent.setup();
    let loads = 0;
    const fetchMock = setupPage(
      () => {
        loads += 1;
        return loads === 1
          ? workspace()
          : workspace({ scribe_draft: artifact({ stale: true }) });
      },
      {
        "/api/v1/encounters/e1/scribe/a1/review": () =>
          apiError(409, "artifact_stale", "stale"),
      },
    );
    const insert = (
      await screen.findAllByRole("button", {
        name: "Insert into empty section",
      })
    )[0];
    await user.click(insert);
    const alerts = await screen.findAllByRole("alert");
    expect(
      alerts.some((a) =>
        /note changed since this draft/i.test(a.textContent ?? ""),
      ),
    ).toBe(true);
    await waitFor(() =>
      expect(
        fetchMock.mock.calls.filter((c) =>
          String(c[0]).startsWith("/api/v1/encounters/e1?"),
        ).length,
      ).toBeGreaterThan(1),
    );
    await waitFor(() =>
      expect(
        screen.getAllByRole("button", { name: "Insert into empty section" })[0],
      ).toBeDisabled(),
    );
    expect(screen.getByLabelText(/Reason for consultation/)).toHaveValue("");
  });

  it("hides the dock and review once the note is signed", async () => {
    setupPage(
      workspace({
        encounter: {
          id: "e1",
          status: "completed",
          encounter_type: "consultation",
          started_at: "2026-08-29T09:00:00Z",
          completed_at: "2026-08-29T10:00:00Z",
          practitioner: "Dr. García",
          facility_name: "Central Hospital",
          own: true,
        },
        note: {
          ...DRAFT_NOTE,
          status: "signed",
          signed_at: "2026-08-29T10:00:00Z",
          signed_by: "Dr. García",
        },
        capabilities: {
          can_document: false,
          can_sign: false,
          can_add_addendum: true,
          can_order_lab: false,
        },
      }),
    );
    await screen.findByText("Alba Demopatient");
    expect(
      screen.queryByRole("button", { name: "Record consultation" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByText("dMind scribe draft")).not.toBeInTheDocument();
  });
});

describe("patient brief and diagnostic history", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  it("renders the brief from record facts", async () => {
    setupPage(workspace({ scribe_draft: null }));
    expect(await screen.findByText("Patient brief")).toBeInTheDocument();
    expect(screen.getByText("Active problems")).toBeInTheDocument();
    expect(
      screen.getAllByText("Type 2 diabetes mellitus").length,
    ).toBeGreaterThan(0);
    expect(
      screen.getAllByText("Metformin 850 mg twice daily").length,
    ).toBeGreaterThan(0);
    expect(screen.getByText("Recent notes")).toBeInTheDocument();
    expect(screen.getByText(/Diabetes review/)).toBeInTheDocument();
    expect(screen.getByText("Recent abnormal results")).toBeInTheDocument();
  });

  it("renders diagnostic trends with objective flags and assistive commentary", async () => {
    const user = userEvent.setup();
    setupPage(workspace({ scribe_draft: null }));
    const summary = await screen.findByText("Diagnostic history");
    await user.click(summary);
    expect(screen.getAllByText("Rising").length).toBeGreaterThan(0);
    expect(screen.getAllByText("6.8 mmol/L").length).toBeGreaterThan(0);
    expect(screen.getAllByText("4.9 mmol/L").length).toBeGreaterThan(0);
    expect(
      screen.getByText(/Potassium is rising; the latest value is above/),
    ).toBeInTheDocument();
    expect(screen.getByText(/trend-rules 0\.1\.0/)).toBeInTheDocument();
    expect(
      screen.getByText("dMind trend commentary (assistive)"),
    ).toBeInTheDocument();
  });
});

function result(
  index: number,
  overrides: Partial<DiagnosticResult> = {},
): DiagnosticResult {
  const day = String(index + 1).padStart(2, "0");
  return {
    id: `o${index}`,
    service_request_id: `sr${index}`,
    value: `${100 + index}`,
    unit: "mg/dL",
    normalized_value: `${100 + index}`,
    comparable: true,
    reference_range: "70-99 mg/dL",
    abnormal: "high",
    critical: false,
    status: "final",
    superseded: false,
    loop_state: "closed",
    effective_at: `2026-08-${day}T09:00:00Z`,
    received_at: `2026-08-${day}T10:00:00Z`,
    ...overrides,
  };
}

function glucoseDiagnostics(results: DiagnosticResult[]): Diagnostics {
  return {
    generated_at: "2026-08-29T09:00:00Z",
    tests: [
      {
        code: "2345-7",
        display: "Glucose [Mass/volume] in Serum",
        unit: "mg/dL",
        reference_range: "70-99 mg/dL",
        results,
        pending: [],
        pending_count: 0,
        latest_value: results.at(-1)?.normalized_value ?? null,
        latest_abnormal: "high",
        direction: "rising",
        result_count: results.length,
        incomparable_count: results.filter((r) => !r.comparable).length,
      },
    ],
    analysis: null,
  };
}

function shownValues(): string[] {
  return screen
    .getAllByRole("row")
    .slice(1)
    .map((row) => row.querySelector("td:nth-child(2) a")?.textContent ?? "");
}

describe("diagnostic history ordering", () => {
  it("previews the three newest results, newest first, and expands coherently", async () => {
    const user = userEvent.setup();
    // Server order: oldest first (o0 … o4).
    const results = [0, 1, 2, 3, 4].map((i) => result(i));
    render(
      <DiagnosticHistory
        lang="en"
        diagnostics={glucoseDiagnostics(results)}
        defaultOpen
      />,
    );
    expect(shownValues()).toEqual(["104 mg/dL", "103 mg/dL", "102 mg/dL"]);
    expect(screen.queryByText("100 mg/dL")).not.toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: /Show more \(2\)/ }));
    expect(shownValues()).toEqual([
      "104 mg/dL",
      "103 mg/dL",
      "102 mg/dL",
      "101 mg/dL",
      "100 mg/dL",
    ]);
    await user.click(screen.getByRole("button", { name: "Show less" }));
    expect(shownValues()).toEqual(["104 mg/dL", "103 mg/dL", "102 mg/dL"]);
  });

  it("shows converted and non-comparable units next to the reported value", () => {
    const results = [
      result(0, { value: "5.6", unit: "mmol/L", normalized_value: "100.89" }),
      result(1, {
        value: "1",
        unit: "g/L",
        normalized_value: null,
        comparable: false,
      }),
      result(2),
    ];
    const diagnostics = glucoseDiagnostics(results);
    diagnostics.tests[0].direction = "mixed_units";
    render(
      <DiagnosticHistory lang="en" diagnostics={diagnostics} defaultOpen />,
    );
    expect(screen.getByText("≈ 100.89 mg/dL")).toBeInTheDocument();
    expect(screen.getByText("not comparable")).toBeInTheDocument();
    expect(screen.getByText("Units not comparable")).toBeInTheDocument();
    // The reported value and unit are never rewritten.
    expect(screen.getByText("5.6 mmol/L")).toBeInTheDocument();
    expect(screen.getByText("1 g/L")).toBeInTheDocument();
  });
});

function LangSwitch() {
  const { setLang } = useSession();
  return (
    <button type="button" onClick={() => setLang("es")}>
      switch-to-es
    </button>
  );
}

describe("workspace language", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    localStorage.clear();
  });

  it("requests commentary in the interface language and re-reads it on change without losing unsaved text", async () => {
    const user = userEvent.setup();
    const ws = workspace({ scribe_draft: null }) as {
      diagnostics: Diagnostics;
    };
    const es = structuredClone(ws);
    es.diagnostics.analysis!.language = "es";
    es.diagnostics.analysis!.statements[0].text =
      "El potasio está en ascenso; el último valor supera el rango de referencia.";
    const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url === "/api/session")
        return Promise.resolve(jsonResponse({ authenticated: true }));
      if (url === "/api/v1/meta/tenant")
        return Promise.resolve(jsonResponse(META));
      if (init?.method === "POST") return Promise.resolve(jsonResponse({}));
      if (url.startsWith("/api/v1/encounters/e1?")) {
        const lang = new URL(url, "http://localhost").searchParams.get("lang");
        return Promise.resolve(jsonResponse(lang === "es" ? es : ws));
      }
      return Promise.resolve(jsonResponse({}));
    });
    vi.stubGlobal("fetch", fetchMock);
    render(
      <SessionProvider>
        <LangSwitch />
        <EncounterPage params={{ id: "e1" }} />
      </SessionProvider>,
    );
    await user.click(await screen.findByText("Diagnostic history"));
    expect(
      screen.getByText(/Potassium is rising; the latest value is above/),
    ).toBeInTheDocument();
    const reads = () =>
      fetchMock.mock.calls
        .map(([input]) => String(input))
        .filter((u) => u.startsWith("/api/v1/encounters/e1?"));
    expect(reads()).toEqual(["/api/v1/encounters/e1?lang=en"]);

    await user.type(screen.getByLabelText(/^Plan/), "Rest and fluids");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();

    await user.click(screen.getByRole("button", { name: "switch-to-es" }));
    await waitFor(() =>
      expect(reads()).toEqual([
        "/api/v1/encounters/e1?lang=en",
        "/api/v1/encounters/e1?lang=es",
      ]),
    );
    expect(
      await screen.findByText(/El potasio está en ascenso/),
    ).toBeInTheDocument();
    // The unsaved local draft survives the language reload.
    expect(screen.getByLabelText(/^Plan/)).toHaveValue("Rest and fluids");
    expect(screen.getByText("Cambios sin guardar")).toBeInTheDocument();
  });
});
