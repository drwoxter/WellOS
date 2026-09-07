import { describe, expect, it } from "vitest";
import {
  LOOP_STATES,
  canActClinically,
  canReadWorklist,
  canRegisterPatients,
  canSearchPatients,
  formatBloodPressure,
  formatDate,
  isLoopState,
  loopStateLabel,
  loopStateShortLabel,
  patientName,
  registrableFacilities,
} from "@/lib/clinical";
import { isSchedulableAt } from "@/lib/visits";

describe("clinical helpers", () => {
  it("shows partial blood-pressure readings instead of dropping them", () => {
    expect(formatBloodPressure("120", "80")).toBe("120/80");
    expect(formatBloodPressure("120", null)).toBe("120/—");
    expect(formatBloodPressure(null, "80")).toBe("—/80");
    expect(formatBloodPressure(null, null)).toBeNull();
  });

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

  it("mirrors the server's appointment window", () => {
    const now = new Date("2026-08-29T10:00:00Z");
    const at = (deltaMs: number) =>
      new Date(now.getTime() + deltaMs).toISOString();
    const hour = 60 * 60 * 1000;
    const day = 24 * hour;
    expect(isSchedulableAt(at(-30 * 60 * 1000), now)).toBe(true);
    expect(isSchedulableAt(at(-2 * hour), now)).toBe(false);
    expect(isSchedulableAt(at(364 * day), now)).toBe(true);
    expect(isSchedulableAt(at(366 * day), now)).toBe(false);
  });
});
