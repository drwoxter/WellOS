import { describe, expect, it } from "vitest";
import {
  LOOP_STATES,
  canActClinically,
  canReadWorklist,
  canRegisterPatients,
  canSearchPatients,
  formatDate,
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

  it("formats date-only values without a timezone shift", () => {
    expect(formatDate("en", "1962-07-08")).toBe("Jul 8, 1962");
  });

  it("maps demo roles to display capabilities", () => {
    expect(canReadWorklist(["physician"])).toBe(true);
    expect(canReadWorklist(["nurse"])).toBe(true);
    expect(canReadWorklist(["registration_staff"])).toBe(false);
    expect(canReadWorklist(["privacy_officer"])).toBe(false);
    expect(canSearchPatients(["registration_staff"])).toBe(true);
    expect(canSearchPatients(["privacy_officer"])).toBe(false);
    expect(canRegisterPatients(["registration_staff"])).toBe(true);
    expect(canRegisterPatients(["physician"])).toBe(false);
    expect(canActClinically(["physician"])).toBe(true);
    expect(canActClinically(["nurse"])).toBe(false);
  });
});
