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
  registrableFacilities,
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
  });

  it("derives registration and clinical capabilities per facility", () => {
    const facilities = [
      {
        id: "a",
        name: "Central",
        accessible: true,
        can_register: true,
        can_act_clinically: false,
      },
      {
        id: "b",
        name: "Annex",
        accessible: true,
        can_register: false,
        can_act_clinically: false,
      },
    ];
    expect(canRegisterPatients(facilities)).toBe(true);
    expect(registrableFacilities(facilities).map((f) => f.id)).toEqual(["a"]);
    expect(canActClinically(facilities)).toBe(false);
    expect(canRegisterPatients([])).toBe(false);
    expect(canActClinically([])).toBe(false);
  });
});
