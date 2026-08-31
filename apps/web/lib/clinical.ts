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
  return new Date(iso).toLocaleDateString(lang === "es" ? "es" : "en", {
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

/** Common orderable laboratory tests covered by the deterministic rules. */
export const LAB_TESTS: { code_loinc: string; display: string }[] = [
  { code_loinc: "2823-3", display: "Potassium [Moles/volume] in Serum" },
  { code_loinc: "2345-7", display: "Glucose [Mass/volume] in Serum" },
];
