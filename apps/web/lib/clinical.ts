import type { Lang, TKey } from "./i18n";
import { t } from "./i18n";

export type LoopState =
  "ordered" | "received" | "reviewed" | "notified" | "closed";

export const LOOP_STATES: LoopState[] = [
  "ordered",
  "received",
  "reviewed",
  "notified",
  "closed",
];

const stateKey: Record<LoopState, TKey> = {
  ordered: "stateOrdered",
  received: "stateReceived",
  reviewed: "stateReviewed",
  notified: "stateNotified",
  closed: "stateClosed",
};

const shortStateKey: Record<LoopState, TKey> = {
  ordered: "shortOrdered",
  received: "shortReceived",
  reviewed: "shortReviewed",
  notified: "shortNotified",
  closed: "shortClosed",
};

export function loopStateLabel(lang: Lang, state: string): string {
  return isLoopState(state) ? t(lang, stateKey[state]) : state;
}

export function loopStateShortLabel(lang: Lang, state: string): string {
  return isLoopState(state) ? t(lang, shortStateKey[state]) : state;
}

export function isLoopState(state: string): state is LoopState {
  return (LOOP_STATES as string[]).includes(state);
}

export function loopStateIndex(state: string): number {
  return isLoopState(state) ? LOOP_STATES.indexOf(state) : -1;
}

export function formatDate(lang: Lang, iso: string): string {
  // Date-only values (e.g. birth dates) carry no timezone; parse them as
  // local calendar dates so they never shift by a day.
  const dateOnly = /^(\d{4})-(\d{2})-(\d{2})$/.exec(iso);
  const date = dateOnly
    ? new Date(
        Number(dateOnly[1]),
        Number(dateOnly[2]) - 1,
        Number(dateOnly[3]),
      )
    : new Date(iso);
  return date.toLocaleDateString(lang === "es" ? "es" : "en", {
    year: "numeric",
    month: "short",
    day: "numeric",
  });
}

export function formatDateTime(lang: Lang, iso: string): string {
  return new Date(iso).toLocaleString(lang === "es" ? "es" : "en", {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

export function patientName(p: {
  family_name: string;
  given_name: string;
}): string {
  return `${p.given_name} ${p.family_name}`;
}

// Display-only capability hints mirroring the server's role policy. They
// decide which controls are rendered; the backend remains the sole
// authorization boundary for every request.
const WORKLIST_ROLES = [
  "physician",
  "nurse",
  "laboratory_professional",
  "pharmacist",
  "clinical_administrator",
];
const PATIENT_SEARCH_ROLES = [
  "registration_staff",
  "physician",
  "nurse",
  "pharmacist",
  "clinical_administrator",
];

export function canReadWorklist(roles: string[]): boolean {
  return roles.some((r) => WORKLIST_ROLES.includes(r));
}

export function canSearchPatients(roles: string[]): boolean {
  return roles.some((r) => PATIENT_SEARCH_ROLES.includes(r));
}

type FacilityCapability = {
  accessible: boolean;
  can_register: boolean;
  can_act_clinically: boolean;
};

// Registration and clinical capabilities are facility-specific: the server
// derives them per facility from the caller's role assignments, so controls
// only appear for facilities where the role actually applies.
export function registrableFacilities<F extends FacilityCapability>(
  facilities: F[],
): F[] {
  return facilities.filter((f) => f.can_register);
}

export function canRegisterPatients(facilities: FacilityCapability[]): boolean {
  return facilities.some((f) => f.can_register);
}

export function canActClinically(facilities: FacilityCapability[]): boolean {
  return facilities.some((f) => f.can_act_clinically);
}

export function canActClinicallyAt(
  facilities: (FacilityCapability & { id: string })[],
  facilityId: string,
): boolean {
  return facilities.some((f) => f.id === facilityId && f.can_act_clinically);
}

/** Common orderable laboratory tests covered by the deterministic rules. */
export const LAB_TESTS: { code_loinc: string; display: string }[] = [
  { code_loinc: "2823-3", display: "Potassium [Moles/volume] in Serum" },
  { code_loinc: "2345-7", display: "Glucose [Mass/volume] in Serum" },
];
