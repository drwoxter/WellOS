import type { AiCapabilities, CapabilityStatus } from "@/lib/capabilities";

/** A fixture (synthetic) provider that is ready — mirrors `dev-fixtures`. */
export const READY_FAKE: CapabilityStatus = {
  state: "ready",
  provider: "local-fake",
  model: "dmind-fake",
  reason: null,
  external: false,
  synthetic: true,
};

/** A real external provider that is ready. */
export const READY_REAL: CapabilityStatus = {
  state: "ready",
  provider: "openai_compatible",
  model: "gpt-test",
  reason: null,
  external: true,
  synthetic: false,
};

export const DISABLED: CapabilityStatus = {
  state: "disabled",
  provider: "disabled",
  model: null,
  reason: "DMIND_MODEL_PROVIDER=disabled",
  external: false,
  synthetic: false,
};

export const DEGRADED: CapabilityStatus = {
  state: "degraded",
  provider: "openai_compatible",
  model: "gpt-test",
  reason: "2 consecutive provider failure(s)",
  external: true,
  synthetic: false,
};

export const INVALID: CapabilityStatus = {
  state: "invalid_configuration",
  provider: "openai_compatible",
  model: null,
  reason: "endpoint host is not in the allowlist",
  external: true,
  synthetic: false,
};

export function capabilities(
  overrides: Partial<AiCapabilities> = {},
): AiCapabilities {
  return {
    model: READY_FAKE,
    transcription: READY_FAKE,
    structured_note: READY_FAKE,
    transcription_languages: ["en", "es"],
    ...overrides,
  };
}

/** Every AI capability available through the synthetic fixture provider. */
export const AI_READY: AiCapabilities = capabilities();
