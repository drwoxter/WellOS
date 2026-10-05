"use client";

import type { ReactNode } from "react";
import { t, type Lang } from "@/lib/i18n";
import { Icon } from "./icons";
import { cx } from "./primitives";
import { Pill } from "./primitives";

export type DmindState =
  "idle" | "listening" | "processing" | "ready" | "attention" | "offline";

const STATE_KEY = {
  idle: "dmindStateIdle",
  listening: "dmindStateListening",
  processing: "dmindStateProcessing",
  ready: "dmindStateReady",
  attention: "dmindStateAttention",
  offline: "dmindStateOffline",
} as const;

/**
 * dMind's visual presence: a CSS/SVG orb whose state is always announced in
 * text as well. "ready" means a suggestion is available for professional
 * review — never that an analysis is validated or final.
 */
export function DmindPresence({
  state,
  lang,
  size,
  label,
  sub,
  hideLabel,
  className,
}: {
  state: DmindState;
  lang: Lang;
  size?: "lg" | "xl";
  label?: ReactNode;
  sub?: ReactNode;
  hideLabel?: boolean;
  className?: string;
}) {
  const text = t(lang, STATE_KEY[state]);
  return (
    <span
      className={cx("dmind-presence", state, size, className)}
      data-state={state}
      role="status"
      aria-live={state === "ready" || state === "attention" ? "polite" : "off"}
    >
      <span className="dmind-orb" aria-hidden="true">
        {state === "offline" ? (
          <Icon.Close />
        ) : state === "ready" ? (
          <Icon.Check />
        ) : (
          <Icon.Sparkle />
        )}
      </span>
      <span className={hideLabel ? "sr-only" : "dmind-label"}>
        <span>{label ?? text}</span>
        {!hideLabel && sub ? <span className="dmind-sub">{sub}</span> : null}
        {hideLabel ? ` — ${text}` : null}
      </span>
    </span>
  );
}

export type ArtifactMeta = {
  status?: string | null;
  autonomy_level?: string | null;
  model?: string | null;
  prompt_version?: string | null;
  created_at?: string | null;
  synthetic?: boolean | null;
};

/**
 * Container for a real dMind artifact. Shows status, provenance and the
 * mandatory "requires professional validation" line; the caller renders the
 * artifact body (rationale, draft, summary).
 */
export function DmindPanel({
  lang,
  title,
  state,
  meta,
  children,
  limitations,
  actions,
  className,
}: {
  lang: Lang;
  title: ReactNode;
  state: DmindState;
  meta?: ArtifactMeta | null;
  children: ReactNode;
  limitations?: ReactNode;
  actions?: ReactNode;
  className?: string;
}) {
  return (
    <section
      className={cx("dmind-panel", className)}
      aria-label={typeof title === "string" ? title : undefined}
    >
      <div className="dmind-panel-head">
        <DmindPresence
          state={state}
          lang={lang}
          label={title}
          sub={t(lang, STATE_KEY[state])}
        />
        <div className="dmind-meta">
          {meta?.status ? (
            <Pill tone="dmind" icon={false}>
              {meta.status}
            </Pill>
          ) : null}
          {meta?.autonomy_level ? (
            <Pill tone="neutral" icon={false}>
              {meta.autonomy_level}
            </Pill>
          ) : null}
          {meta?.model ? (
            <Pill tone="neutral" icon={false} outline>
              {meta.model}
              {meta.prompt_version ? ` · ${meta.prompt_version}` : ""}
            </Pill>
          ) : null}
          {meta?.synthetic ? (
            <Pill tone="warn" icon={false}>
              {t(lang, "dmindSynthetic")}
            </Pill>
          ) : null}
        </div>
      </div>
      {children}
      {actions ? (
        <div className="actions" style={{ marginTop: "0.6rem" }}>
          {actions}
        </div>
      ) : null}
      <p className="dmind-limits">
        <strong>{t(lang, "dmindAssistive")}</strong>{" "}
        {t(lang, "dmindRequiresReview")}
        {limitations ? <> {limitations}</> : null}
      </p>
    </section>
  );
}
