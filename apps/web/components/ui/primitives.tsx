"use client";

import type { ReactNode } from "react";
import { Icon } from "./icons";

export type Tone = "neutral" | "ok" | "warn" | "critical" | "dmind" | "teal";

function cx(...parts: Array<string | false | null | undefined>): string {
  return parts.filter(Boolean).join(" ");
}

export { cx };

/* ---------- Card ---------- */

export function Card({
  children,
  className,
  tone,
  interactive,
  compact,
  as: Tag = "section",
  ...rest
}: {
  children: ReactNode;
  className?: string;
  tone?: Tone;
  interactive?: boolean;
  compact?: boolean;
  as?: "section" | "div" | "article" | "aside";
} & Omit<React.HTMLAttributes<HTMLElement>, "className" | "children">) {
  return (
    <Tag
      className={cx(
        "card",
        tone && tone !== "neutral" && `tone-${tone}`,
        interactive && "interactive",
        compact && "compact",
        className,
      )}
      {...rest}
    >
      {children}
    </Tag>
  );
}

export function CardHead({
  title,
  eyebrow,
  actions,
  id,
  level = 2,
}: {
  title: ReactNode;
  eyebrow?: ReactNode;
  actions?: ReactNode;
  id?: string;
  level?: 2 | 3;
}) {
  const H = level === 2 ? "h2" : "h3";
  return (
    <div className="card-head">
      <div className="grow">
        {eyebrow ? <p className="eyebrow">{eyebrow}</p> : null}
        <H id={id}>{title}</H>
      </div>
      {actions ? <div className="actions">{actions}</div> : null}
    </div>
  );
}

/* ---------- Stat tile ---------- */

export function StatTile({
  value,
  label,
  tone = "neutral",
  href,
}: {
  value: ReactNode;
  label: ReactNode;
  tone?: Tone;
  href?: string;
}) {
  const body = (
    <>
      <span className="num">{value}</span>
      <span className="label">{label}</span>
    </>
  );
  if (href) {
    return (
      <a
        className={cx("stat-tile", tone !== "neutral" && tone)}
        href={href}
        style={{ textDecoration: "none" }}
      >
        {body}
      </a>
    );
  }
  return (
    <div className={cx("stat-tile", tone !== "neutral" && tone)}>{body}</div>
  );
}

/* ---------- Pill: tone + text + icon, never colour alone ---------- */

const TONE_ICON: Record<
  Tone,
  ((p: { className?: string }) => ReactNode) | null
> = {
  neutral: null,
  ok: Icon.Check,
  warn: Icon.Alert,
  critical: Icon.Alert,
  dmind: Icon.Sparkle,
  teal: null,
};

export function Pill({
  tone = "neutral",
  children,
  outline,
  icon = true,
  className,
}: {
  tone?: Tone;
  children: ReactNode;
  outline?: boolean;
  icon?: boolean;
  className?: string;
}) {
  const I = icon ? TONE_ICON[tone] : null;
  return (
    <span className={cx("pill", tone, outline && "outline", className)}>
      {I ? <I /> : null}
      {children}
    </span>
  );
}

/* ---------- Chips ---------- */

export function ChipGroup({
  label,
  options,
  value,
  onChange,
  disabled,
}: {
  label: string;
  options: { value: string; label: string }[];
  value: string[];
  onChange: (next: string[]) => void;
  disabled?: boolean;
}) {
  return (
    <div className="chips" role="group" aria-label={label}>
      {options.map((o) => {
        const on = value.includes(o.value);
        return (
          <button
            key={o.value}
            type="button"
            className="chip"
            aria-pressed={on}
            disabled={disabled}
            onClick={() =>
              onChange(
                on ? value.filter((v) => v !== o.value) : [...value, o.value],
              )
            }
          >
            {on ? <Icon.Check /> : null}
            {o.label}
          </button>
        );
      })}
    </div>
  );
}

/* ---------- Skeleton ---------- */

export function Skeleton({
  lines = 3,
  title,
  block,
  label,
}: {
  lines?: number;
  title?: boolean;
  block?: boolean;
  label: string;
}) {
  return (
    <div
      className="skeleton-stack"
      role="status"
      aria-live="polite"
      aria-busy="true"
    >
      <span className="sr-only">{label}</span>
      {title ? <div className="skeleton title" aria-hidden="true" /> : null}
      {block ? <div className="skeleton block" aria-hidden="true" /> : null}
      {Array.from({ length: lines }).map((_, i) => (
        <div
          key={i}
          className="skeleton text"
          aria-hidden="true"
          style={{ width: `${92 - (i % 3) * 14}%` }}
        />
      ))}
    </div>
  );
}

/* ---------- Honest states ---------- */

export function EmptyState({
  title,
  description,
  action,
  icon,
  inline,
}: {
  title: ReactNode;
  description?: ReactNode;
  action?: ReactNode;
  icon?: ReactNode;
  inline?: boolean;
}) {
  return (
    <div className={cx("state empty", inline && "inline")} role="status">
      <span className="state-icon" aria-hidden="true">
        {icon ?? <Icon.Info />}
      </span>
      <div>
        <p className="state-title">{title}</p>
        {description ? <p>{description}</p> : null}
        {action ? <div className="actions">{action}</div> : null}
      </div>
    </div>
  );
}

export function ErrorState({
  title,
  description,
  onRetry,
  retryLabel,
  inline,
}: {
  title: ReactNode;
  description?: ReactNode;
  onRetry?: () => void;
  retryLabel: string;
  inline?: boolean;
}) {
  return (
    <div className={cx("state error", inline && "inline")} role="alert">
      <span className="state-icon" aria-hidden="true">
        <Icon.Alert />
      </span>
      <div>
        <p className="state-title">{title}</p>
        {description ? <p>{description}</p> : null}
        {onRetry ? (
          <div className="actions">
            <button type="button" className="secondary sm" onClick={onRetry}>
              {retryLabel}
            </button>
          </div>
        ) : null}
      </div>
    </div>
  );
}

export function OfflineState({
  title,
  description,
  inline,
}: {
  title: ReactNode;
  description?: ReactNode;
  inline?: boolean;
}) {
  return (
    <div className={cx("state offline", inline && "inline")} role="status">
      <span className="state-icon" aria-hidden="true">
        <Icon.Wifi />
      </span>
      <div>
        <p className="state-title">{title}</p>
        {description ? <p>{description}</p> : null}
      </div>
    </div>
  );
}

/* ---------- Accordion ---------- */

export function Accordion({
  summary,
  children,
  defaultOpen,
  icon,
  className,
}: {
  summary: ReactNode;
  children: ReactNode;
  defaultOpen?: boolean;
  icon?: ReactNode;
  className?: string;
}) {
  return (
    <details className={cx("accordion", className)} open={defaultOpen}>
      <summary>
        {icon}
        <span className="grow">{summary}</span>
      </summary>
      <div className="accordion-body">{children}</div>
    </details>
  );
}

/* ---------- Timeline ---------- */

export type TimelineItem = {
  id: string;
  when: ReactNode;
  title: ReactNode;
  sub?: ReactNode;
  tone?: Tone;
  href?: string;
};

export function Timeline({
  items,
  label,
}: {
  items: TimelineItem[];
  label: string;
}) {
  return (
    <ol className="timeline" aria-label={label}>
      {items.map((it) => {
        const I = TONE_ICON[it.tone ?? "neutral"] ?? Icon.Clock;
        return (
          <li
            key={it.id}
            className={cx(it.tone && it.tone !== "neutral" && it.tone)}
          >
            <span className="tl-dot" aria-hidden="true">
              <I />
            </span>
            <div className="tl-when">{it.when}</div>
            <div className="tl-title">
              {it.href ? <a href={it.href}>{it.title}</a> : it.title}
            </div>
            {it.sub ? <div className="tl-sub">{it.sub}</div> : null}
          </li>
        );
      })}
    </ol>
  );
}

/* ---------- List row ---------- */

export function ListRow({
  title,
  sub,
  tone,
  leading,
  actions,
  as: Tag = "li",
  className,
}: {
  title: ReactNode;
  sub?: ReactNode;
  tone?: Tone;
  leading?: ReactNode;
  actions?: ReactNode;
  as?: "li" | "div";
  className?: string;
}) {
  return (
    <Tag
      className={cx("list-row", tone && tone !== "neutral" && tone, className)}
    >
      {leading}
      <div className="row-main">
        <div className="row-title">{title}</div>
        {sub ? <div className="row-sub">{sub}</div> : null}
      </div>
      {actions ? <div className="row-actions">{actions}</div> : null}
    </Tag>
  );
}
