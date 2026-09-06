// Browser audio capture behind a small interface so the consultation
// recorder can be exercised without a microphone. Audio never leaves the
// recorder except as the finished container blob handed to the caller; it is
// never written to storage of any kind.

export type RecorderErrorKind =
  "unsupported" | "permission_denied" | "no_device" | "failed";

export class RecorderError extends Error {
  kind: RecorderErrorKind;
  constructor(kind: RecorderErrorKind, message?: string) {
    super(message ?? kind);
    this.kind = kind;
  }
}

export type Recording = {
  blob: Blob;
  mimeType: string;
  /** Captured time excluding pauses. */
  durationMs: number;
};

export interface AudioRecorder {
  /** Requests the microphone and starts capturing. */
  start(): Promise<void>;
  pause(): void;
  resume(): void;
  /** Stops capture and releases the microphone. */
  stop(): Promise<Recording>;
  /** Stops capture, releases the microphone and drops everything captured. */
  discard(): void;
  /** Captured time so far, excluding pauses. */
  elapsedMs(): number;
}

export type RecorderFactory = () => AudioRecorder;

const PREFERRED_TYPES = [
  "audio/webm;codecs=opus",
  "audio/webm",
  "audio/ogg;codecs=opus",
  "audio/ogg",
  "audio/mp4",
];

export function isRecordingSupported(): boolean {
  return (
    typeof navigator !== "undefined" &&
    typeof navigator.mediaDevices?.getUserMedia === "function" &&
    typeof MediaRecorder !== "undefined"
  );
}

function pickMimeType(): string | undefined {
  if (typeof MediaRecorder.isTypeSupported !== "function") return undefined;
  return PREFERRED_TYPES.find((t) => MediaRecorder.isTypeSupported(t));
}

/** Wall-clock stopwatch that excludes paused intervals. */
export class Stopwatch {
  private accumulated = 0;
  private runningSince: number | null = null;
  constructor(private readonly now: () => number = () => Date.now()) {}
  start() {
    if (this.runningSince === null) this.runningSince = this.now();
  }
  pause() {
    if (this.runningSince !== null) {
      this.accumulated += this.now() - this.runningSince;
      this.runningSince = null;
    }
  }
  elapsed(): number {
    return (
      this.accumulated +
      (this.runningSince === null ? 0 : this.now() - this.runningSince)
    );
  }
}

class MediaStreamRecorder implements AudioRecorder {
  private stream: MediaStream | null = null;
  private recorder: MediaRecorder | null = null;
  private chunks: BlobPart[] = [];
  private readonly clock = new Stopwatch();

  async start(): Promise<void> {
    if (!isRecordingSupported()) throw new RecorderError("unsupported");
    try {
      this.stream = await navigator.mediaDevices.getUserMedia({ audio: true });
    } catch (err) {
      const name = err instanceof Error ? err.name : "";
      if (name === "NotAllowedError" || name === "SecurityError") {
        throw new RecorderError("permission_denied");
      }
      if (name === "NotFoundError" || name === "OverconstrainedError") {
        throw new RecorderError("no_device");
      }
      throw new RecorderError("failed", name || undefined);
    }
    const mimeType = pickMimeType();
    try {
      this.recorder = new MediaRecorder(
        this.stream,
        mimeType ? { mimeType } : undefined,
      );
    } catch (err) {
      this.release();
      throw new RecorderError(
        "failed",
        err instanceof Error ? err.name : undefined,
      );
    }
    this.chunks = [];
    this.recorder.ondataavailable = (e: BlobEvent) => {
      if (e.data && e.data.size > 0) this.chunks.push(e.data);
    };
    try {
      this.recorder.start(1000);
    } catch (err) {
      this.release();
      throw new RecorderError(
        "failed",
        err instanceof Error ? err.name : undefined,
      );
    }
    this.clock.start();
  }

  pause(): void {
    if (this.recorder?.state === "recording") {
      this.recorder.pause();
      this.clock.pause();
    }
  }

  resume(): void {
    if (this.recorder?.state === "paused") {
      this.recorder.resume();
      this.clock.start();
    }
  }

  stop(): Promise<Recording> {
    const recorder = this.recorder;
    if (!recorder) return Promise.reject(new RecorderError("failed"));
    this.clock.pause();
    const durationMs = Math.round(this.clock.elapsed());
    return new Promise((resolve, reject) => {
      recorder.onstop = () => {
        const mimeType = recorder.mimeType || "audio/webm";
        const blob = new Blob(this.chunks, { type: mimeType });
        this.chunks = [];
        this.release();
        resolve({ blob, mimeType, durationMs });
      };
      recorder.onerror = () => {
        this.release();
        reject(new RecorderError("failed"));
      };
      if (recorder.state === "inactive") {
        recorder.onstop(new Event("stop"));
      } else {
        recorder.stop();
      }
    });
  }

  discard(): void {
    this.chunks = [];
    const recorder = this.recorder;
    if (recorder && recorder.state !== "inactive") {
      recorder.ondataavailable = null;
      recorder.onstop = null;
      recorder.stop();
    }
    this.release();
  }

  elapsedMs(): number {
    return this.clock.elapsed();
  }

  private release() {
    this.stream?.getTracks().forEach((track) => track.stop());
    this.stream = null;
    this.recorder = null;
  }
}

export const createMediaStreamRecorder: RecorderFactory = () =>
  new MediaStreamRecorder();

export function formatTimecode(ms: number): string {
  const total = Math.max(0, Math.floor(ms / 1000));
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${String(m).padStart(2, "0")}:${String(s).padStart(2, "0")}`;
}

export function blobToBase64(blob: Blob): Promise<string> {
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onerror = () => reject(reader.error ?? new Error("read failed"));
    reader.onload = () => {
      const url = String(reader.result);
      resolve(url.slice(url.indexOf(",") + 1));
    };
    reader.readAsDataURL(blob);
  });
}
