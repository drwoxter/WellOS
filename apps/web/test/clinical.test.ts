import { describe, expect, it } from "vitest";
import {
  LOOP_STATES,
  isLoopState,
  loopStateLabel,
  loopStateShortLabel,
  patientName,
} from "@/lib/clinical";

describe("clinical helpers", () => {
  it("recognises every workflow state", () => {
    for (const s of LOOP_STATES) expect(isLoopState(s)).toBe(true);
    expect(isLoopState("unknown")).toBe(false);
  });

  it("labels every workflow state in both languages", () => {
    for (const lang of ["en", "es"] as const) {
      for (const s of LOOP_STATES) {
        expect(loopStateLabel(lang, s)).not.toBe("");
        expect(loopStateShortLabel(lang, s)).not.toBe("");
      }
    }
  });

  it("falls back to the raw value for unknown states", () => {
    expect(loopStateLabel("en", "mystery")).toBe("mystery");
  });

  it("formats patient names as given family", () => {
    expect(patientName({ given_name: "Ana", family_name: "Demopatient" })).toBe(
      "Ana Demopatient",
    );
  });
});
