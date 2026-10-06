"use client";

import { useId, type ReactNode } from "react";
import { cx, type Tone } from "./primitives";

/* All charts are inline SVG with a textual summary (`summary`) so the
   information never depends on the graphic alone. Criticality is encoded
   with shape/text as well as colour. */

export type TrendPoint = {
  x: number; // epoch ms
  y: number;
  label: string; // formatted date
  flag?: "normal" | "abnormal" | "critical";
};

export function TrendChart({
  points,
  band,
  unit,
  title,
  summary,
  height = 120,
}: {
  points: TrendPoint[];
  band?: { low: number | null; high: number | null } | null;
  unit?: string | null;
  title: string;
  summary: string;
  height?: number;
}) {
  const gid = useId().replace(/:/g, "");
  const w = 320;
  const pad = { l: 34, r: 10, t: 10, b: 22 };
  if (points.length === 0) return null;
  const xs = points.map((p) => p.x);
  const ys = points.map((p) => p.y);
  const minX = Math.min(...xs);
  const maxX = Math.max(...xs);
  let minY = Math.min(...ys, band?.low ?? Infinity);
  let maxY = Math.max(...ys, band?.high ?? -Infinity);
  if (minY === maxY) {
    minY -= 1;
    maxY += 1;
  }
  const spanY = maxY - minY;
  minY -= spanY * 0.12;
  maxY += spanY * 0.12;
  const sx = (x: number) =>
    maxX === minX
      ? pad.l + (w - pad.l - pad.r) / 2
      : pad.l + ((x - minX) / (maxX - minX)) * (w - pad.l - pad.r);
  const sy = (y: number) =>
    pad.t + (1 - (y - minY) / (maxY - minY)) * (height - pad.t - pad.b);
  const path = points
    .map(
      (p, i) => `${i ? "L" : "M"}${sx(p.x).toFixed(1)},${sy(p.y).toFixed(1)}`,
    )
    .join(" ");
  const area = `${path} L${sx(points[points.length - 1].x).toFixed(1)},${(height - pad.b).toFixed(1)} L${sx(points[0].x).toFixed(1)},${(height - pad.b).toFixed(1)} Z`;
  const ticks = [minY + spanY * 0.12, maxY - spanY * 0.12];
  return (
    <figure className="chart" aria-label={title}>
      <svg
        viewBox={`0 0 ${w} ${height}`}
        role="img"
        aria-describedby={`${gid}-sum`}
      >
        <title>{title}</title>
        <defs>
          <linearGradient id={`${gid}-fill`} x1="0" x2="0" y1="0" y2="1">
            <stop offset="0%" stopColor="var(--primary)" stopOpacity="0.22" />
            <stop offset="100%" stopColor="var(--primary)" stopOpacity="0" />
          </linearGradient>
        </defs>
        {band && band.low !== null && band.high !== null ? (
          <rect
            className="chart-band"
            x={pad.l}
            width={w - pad.l - pad.r}
            y={sy(band.high)}
            height={Math.max(sy(band.low) - sy(band.high), 1)}
            rx={3}
          />
        ) : null}
        {ticks.map((tv) => (
          <g key={tv}>
            <line
              className="chart-grid"
              x1={pad.l}
              x2={w - pad.r}
              y1={sy(tv)}
              y2={sy(tv)}
            />
            <text
              className="chart-axis"
              x={pad.l - 4}
              y={sy(tv) + 3}
              textAnchor="end"
            >
              {Number.isInteger(tv) ? tv : tv.toFixed(1)}
            </text>
          </g>
        ))}
        <path d={area} fill={`url(#${gid}-fill)`} />
        <path className="chart-line" d={path} />
        {points.map((p, i) => (
          <g key={i}>
            {p.flag === "critical" ? (
              <path
                className="chart-dot critical"
                d={`M${sx(p.x)},${sy(p.y) - 6} L${sx(p.x) + 6},${sy(p.y) + 5} L${sx(p.x) - 6},${sy(p.y) + 5} Z`}
              />
            ) : p.flag === "abnormal" ? (
              <rect
                className="chart-dot abnormal"
                x={sx(p.x) - 4}
                y={sy(p.y) - 4}
                width={8}
                height={8}
                transform={`rotate(45 ${sx(p.x)} ${sy(p.y)})`}
              />
            ) : (
              <circle className="chart-dot" cx={sx(p.x)} cy={sy(p.y)} r={3.5} />
            )}
          </g>
        ))}
        <text className="chart-axis" x={pad.l} y={height - 6}>
          {points[0].label}
        </text>
        {points.length > 1 ? (
          <text
            className="chart-axis"
            x={w - pad.r}
            y={height - 6}
            textAnchor="end"
          >
            {points[points.length - 1].label}
          </text>
        ) : null}
      </svg>
      <figcaption id={`${gid}-sum`} className="chart-summary">
        {summary}
        {unit ? ` (${unit})` : ""}
      </figcaption>
    </figure>
  );
}

export type BarDatum = {
  label: string;
  value: number;
  tone?: Tone | "neutral";
};

export function BarChart({
  data,
  title,
  summary,
  height = 110,
}: {
  data: BarDatum[];
  title: string;
  summary: string;
  height?: number;
}) {
  const gid = useId().replace(/:/g, "");
  if (data.length === 0) return null;
  const w = 320;
  const pad = { l: 6, r: 6, t: 14, b: 22 };
  const max = Math.max(...data.map((d) => d.value), 1);
  const bw = (w - pad.l - pad.r) / data.length;
  return (
    <figure className="chart" aria-label={title}>
      <svg
        viewBox={`0 0 ${w} ${height}`}
        role="img"
        aria-describedby={`${gid}-sum`}
      >
        <title>{title}</title>
        {data.map((d, i) => {
          const h = (d.value / max) * (height - pad.t - pad.b);
          const x = pad.l + i * bw + bw * 0.15;
          const y = height - pad.b - h;
          return (
            <g key={d.label}>
              <rect
                className={cx("bar", d.tone)}
                x={x}
                y={y}
                width={bw * 0.7}
                height={Math.max(h, d.value > 0 ? 2 : 0)}
                rx={4}
                style={{
                  transformBox: "fill-box",
                  animationDelay: `${i * 40}ms`,
                }}
              />
              <text
                className="chart-axis"
                x={x + bw * 0.35}
                y={y - 4}
                textAnchor="middle"
              >
                {d.value}
              </text>
              <text
                className="chart-axis"
                x={x + bw * 0.35}
                y={height - 6}
                textAnchor="middle"
              >
                {d.label}
              </text>
            </g>
          );
        })}
      </svg>
      <figcaption id={`${gid}-sum`} className="chart-summary">
        {summary}
      </figcaption>
    </figure>
  );
}

/* Heatmap: rows × cols with 0..1 intensity, textual value in each cell. */
export type HeatCell = {
  value: number | null; // 0..1 load; null = closed/no data
  text: string; // short label shown in cell
  title: string; // full accessible description
  closed?: boolean;
};

export function Heatmap({
  rows,
  cols,
  cells,
  title,
  summary,
  onPick,
}: {
  rows: string[];
  cols: string[];
  cells: HeatCell[][];
  title: string;
  summary: string;
  onPick?: (r: number, c: number) => void;
}) {
  const level = (v: number | null) =>
    v === null ? "" : v < 0.35 ? "l1" : v < 0.65 ? "l2" : v < 0.9 ? "l3" : "l4";
  return (
    <div>
      <div
        className="heatmap"
        role="table"
        aria-label={title}
        style={{
          gridTemplateColumns: `auto repeat(${cols.length}, minmax(0, 1fr))`,
        }}
      >
        <div role="row" style={{ display: "contents" }}>
          <div role="columnheader" className="hm-label" />
          {cols.map((c) => (
            <div key={c} role="columnheader" className="hm-col-label">
              {c}
            </div>
          ))}
        </div>
        {rows.map((r, ri) => (
          <div role="row" key={r} style={{ display: "contents" }}>
            <div role="rowheader" className="hm-label">
              {r}
            </div>
            {cols.map((c, ci) => {
              const cell = cells[ri]?.[ci];
              if (!cell) return <div key={c} role="cell" className="hm-cell" />;
              const Tag = onPick ? "button" : "div";
              return (
                <Tag
                  key={c}
                  role="cell"
                  type={onPick ? "button" : undefined}
                  className={cx(
                    "hm-cell",
                    cell.closed ? "closed" : level(cell.value),
                  )}
                  title={cell.title}
                  aria-label={cell.title}
                  onClick={onPick ? () => onPick(ri, ci) : undefined}
                  style={
                    onPick
                      ? { border: 0, cursor: "pointer", font: "inherit" }
                      : undefined
                  }
                >
                  {cell.text}
                </Tag>
              );
            })}
          </div>
        ))}
      </div>
      <p className="chart-summary">{summary}</p>
    </div>
  );
}

/* Month/week calendar grid with items per day. */
export type CalendarItem = {
  id: string;
  label: string;
  tone?: Tone | "hold";
  onClick?: () => void;
  title?: string;
};

export function CalendarGrid({
  days,
  headers,
  moreLabel,
  todayKey,
  label,
}: {
  days: {
    key: string;
    day: string;
    items: CalendarItem[];
    closed?: boolean;
    muted?: boolean;
  }[];
  headers: string[];
  moreLabel: (n: number) => string;
  todayKey?: string;
  label: string;
}) {
  const cols = headers.length;
  return (
    <div
      className="cal-grid"
      role="grid"
      aria-label={label}
      style={{ gridTemplateColumns: `repeat(${cols}, minmax(0, 1fr))` }}
    >
      {headers.map((h, i) => (
        <div
          key={h}
          role="columnheader"
          className={cx("cal-head", days[i]?.key === todayKey && "today")}
        >
          {h}
        </div>
      ))}
      {days.map((d) => (
        <div
          key={d.key}
          role="gridcell"
          className={cx(
            "cal-cell",
            d.closed && "closed",
            d.key === todayKey && "today",
          )}
          style={d.muted ? { opacity: 0.55 } : undefined}
        >
          <span className="cal-day">{d.day}</span>
          {d.items.slice(0, 3).map((it) =>
            it.onClick ? (
              <button
                key={it.id}
                type="button"
                className={cx("cal-item", it.tone)}
                onClick={it.onClick}
                title={it.title ?? it.label}
              >
                {it.label}
              </button>
            ) : (
              <span
                key={it.id}
                className={cx("cal-item", it.tone)}
                title={it.title ?? it.label}
              >
                {it.label}
              </span>
            ),
          )}
          {d.items.length > 3 ? (
            <span className="cal-more">{moreLabel(d.items.length - 3)}</span>
          ) : null}
        </div>
      ))}
    </div>
  );
}

export function ChartLegend({
  items,
}: {
  items: { tone: string; label: ReactNode }[];
}) {
  const color: Record<string, string> = {
    ok: "var(--mint-600)",
    warn: "var(--amber-600)",
    critical: "var(--red-600)",
    dmind: "var(--indigo-500)",
    teal: "var(--primary)",
    neutral: "var(--ink-200)",
    hold: "var(--indigo-200)",
  };
  return (
    <ul className="chart-legend">
      {items.map((it, i) => (
        <li key={i}>
          <span
            className="swatch"
            style={{ background: color[it.tone] ?? it.tone }}
            aria-hidden="true"
          />
          {it.label}
        </li>
      ))}
    </ul>
  );
}
