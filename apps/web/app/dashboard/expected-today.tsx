"use client";

import Link from "next/link";
import { t } from "@/lib/i18n";
import type { Lang, TKey } from "@/lib/i18n";
import { formatDateTime, patientName } from "@/lib/clinical";
import { visitStatusLabel } from "@/lib/visits";
import type { VisitItem } from "@/lib/visits";
import { EmptyState, Pill } from "@/components/ui/primitives";
import type { Tone } from "@/components/ui/primitives";
import { Icon } from "@/components/ui/icons";

const ARRIVAL_TONE: Record<string, Tone> = {
  urgent: "critical",
  walk_in: "warn",
  scheduled: "neutral",
  remote: "teal",
};
const ARRIVAL_SHORT: Record<string, TKey> = {
  urgent: "arrivalUrgentShort",
  walk_in: "arrivalWalkInShort",
  scheduled: "arrivalScheduledShort",
  remote: "arrivalRemoteShort",
};
const EXPECTED_STATUSES: readonly string[] = [
  "in_consultation",
  "ready_for_consultation",
  "triage_in_progress",
  "arrived",
  "scheduled",
];

/** Everyone expected today, urgent and ready first, with the arrival kind
 *  spelled out so scheduled, urgent and walk-in patients are never confused. */
export function ExpectedToday({
  lang,
  visits,
  limit,
}: {
  lang: Lang;
  visits: VisitItem[];
  limit: number;
}) {
  const rows = visits
    .filter((v) => EXPECTED_STATUSES.includes(v.status))
    .sort((a, b) => {
      const urgency = (v: VisitItem) =>
        (v.arrival_kind === "urgent" ? 0 : 2) +
        (v.status === "ready_for_consultation" ? 0 : 1);
      const ua = urgency(a);
      const ub = urgency(b);
      if (ua !== ub) return ua - ub;
      return (a.scheduled_at ?? a.arrived_at ?? "").localeCompare(
        b.scheduled_at ?? b.arrived_at ?? "",
      );
    });
  return (
    <section className="card expected-today" aria-labelledby="expected-h">
      <div className="card-head">
        <h2 id="expected-h">
          <Icon.Calendar /> {t(lang, "expectedToday")}{" "}
          {rows.length > 0 ? (
            <span className="badge neutral">{rows.length}</span>
          ) : null}
        </h2>
        <Link href="/access" className="navlink">
          {t(lang, "openAccessBoard")}
        </Link>
      </div>
      {rows.length === 0 ? (
        <EmptyState inline title={t(lang, "noExpectedToday")} />
      ) : (
        <ul className="expected-list">
          {rows.slice(0, limit).map((v) => (
            <li key={v.id} className={`expected-row kind-${v.arrival_kind}`}>
              <span className="expected-when">
                {v.scheduled_at
                  ? formatDateTime(lang, v.scheduled_at).split(" ").pop()
                  : v.wait_minutes !== null
                    ? `${v.wait_minutes} min`
                    : "—"}
              </span>
              <span className="expected-who">
                <strong>{patientName(v.patient)}</strong>
                <span className="muted">
                  {v.reason ?? v.service} · {visitStatusLabel(lang, v.status)}
                </span>
              </span>
              <Pill
                tone={ARRIVAL_TONE[v.arrival_kind] ?? "neutral"}
                icon={v.arrival_kind === "urgent"}
              >
                {t(
                  lang,
                  ARRIVAL_SHORT[v.arrival_kind] ?? "arrivalScheduledShort",
                )}
              </Pill>
              {v.encounter_id && v.capabilities.can_resume_consultation ? (
                <Link
                  className="navlink"
                  href={`/encounters/${v.encounter_id}`}
                >
                  {t(lang, "resumeConsultation")}
                </Link>
              ) : null}
            </li>
          ))}
        </ul>
      )}
    </section>
  );
}
