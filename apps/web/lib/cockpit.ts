// Dashboard cockpit layout: which widgets show, in what order, at what size
// and how dense. The layout is a per-user preference kept on the server
// (tenant/user isolated) — never patients, results or any other clinical
// data. A legacy browser-stored layout is migrated once and then dropped.

import { apiFetch } from "@/lib/session";

export const COCKPIT_WIDGETS = [
  "ready",
  "alerts",
  "triage",
  "access",
  "drafts",
  "attention",
  "results",
  "tasks",
  "ai",
] as const;

export type CockpitWidget = (typeof COCKPIT_WIDGETS)[number];
export type Density = "compact" | "expanded";
export type WidgetSize = "half" | "full";

export type CockpitConfig = {
  order: CockpitWidget[];
  hidden: CockpitWidget[];
  sizes: Partial<Record<CockpitWidget, WidgetSize>>;
  density: Density;
};

/** Widgets fed by the visit/alert API (access board) versus the diagnostic
 *  worklist API; a role only gets the widgets whose data it may read. */
export const VISIT_WIDGETS: readonly CockpitWidget[] = [
  "ready",
  "alerts",
  "triage",
  "access",
];
export const WORKLIST_WIDGETS: readonly CockpitWidget[] = [
  "drafts",
  "attention",
  "results",
  "tasks",
  "ai",
];

/** Legacy browser-only layout; read once for migration, then removed. */
export const COCKPIT_STORAGE_ITEM = "wellos.cockpit.v2";

export const PREFERENCES_PATH = "/api/v1/me/dashboard-preferences";

export function defaultConfig(roles: string[]): CockpitConfig {
  if (roles.includes("physician")) {
    return {
      order: [
        "ready",
        "alerts",
        "drafts",
        "attention",
        "results",
        "tasks",
        "ai",
        "triage",
        "access",
      ],
      hidden: ["triage", "access"],
      sizes: { ready: "full" },
      density: "expanded",
    };
  }
  if (roles.includes("nurse")) {
    return {
      order: [
        "triage",
        "alerts",
        "access",
        "ready",
        "results",
        "tasks",
        "attention",
        "ai",
        "drafts",
      ],
      hidden: ["access", "drafts", "ai"],
      sizes: { triage: "full" },
      density: "compact",
    };
  }
  if (roles.includes("registration_staff")) {
    return {
      order: [...COCKPIT_WIDGETS],
      hidden: [
        "ready",
        "triage",
        "drafts",
        "attention",
        "results",
        "tasks",
        "ai",
      ],
      sizes: { access: "full" },
      density: "expanded",
    };
  }
  if (roles.includes("laboratory_professional")) {
    return {
      order: [
        "results",
        "tasks",
        "attention",
        "ai",
        "drafts",
        "ready",
        "alerts",
        "triage",
        "access",
      ],
      hidden: ["drafts"],
      sizes: { results: "full" },
      density: "compact",
    };
  }
  return {
    order: [...COCKPIT_WIDGETS],
    hidden: ["drafts", "attention", "ai"],
    sizes: {},
    density: "compact",
  };
}

function isWidget(x: unknown): x is CockpitWidget {
  return (
    typeof x === "string" && (COCKPIT_WIDGETS as readonly string[]).includes(x)
  );
}

/** Accepts only a well-formed layout document; anything else yields the
 *  fallback so a tampered or outdated value can never break the dashboard. */
export function parseLayout(
  parsed: unknown,
  fallback: CockpitConfig,
): CockpitConfig {
  if (typeof parsed !== "object" || parsed === null) return fallback;
  const p = parsed as Record<string, unknown>;
  if (!Array.isArray(p.order) || !Array.isArray(p.hidden)) return fallback;
  const order = p.order.filter(isWidget);
  const hidden = p.hidden.filter(isWidget);
  const density: Density = p.density === "compact" ? "compact" : "expanded";
  const sizes: Partial<Record<CockpitWidget, WidgetSize>> = {};
  if (typeof p.sizes === "object" && p.sizes !== null) {
    for (const [w, size] of Object.entries(
      p.sizes as Record<string, unknown>,
    )) {
      if (isWidget(w) && (size === "half" || size === "full")) sizes[w] = size;
    }
  }
  // Every widget appears exactly once; newly introduced widgets are appended.
  const seen = new Set<CockpitWidget>();
  const complete: CockpitWidget[] = [];
  for (const w of [...order, ...COCKPIT_WIDGETS]) {
    if (!seen.has(w)) {
      seen.add(w);
      complete.push(w);
    }
  }
  return {
    order: complete,
    hidden: Array.from(new Set(hidden)),
    sizes,
    density,
  };
}

export function parseConfig(
  raw: string | null,
  fallback: CockpitConfig,
): CockpitConfig {
  if (!raw) return fallback;
  try {
    return parseLayout(JSON.parse(raw), fallback);
  } catch {
    return fallback;
  }
}

/** A valid legacy browser layout, if one exists (migration source only). */
export function readLegacyLocalConfig(
  storage: Pick<Storage, "getItem"> | null,
): CockpitConfig | null {
  if (!storage) return null;
  try {
    const raw = storage.getItem(COCKPIT_STORAGE_ITEM);
    if (!raw) return null;
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return null;
    const p = parsed as Record<string, unknown>;
    if (!Array.isArray(p.order) || !Array.isArray(p.hidden)) return null;
    return parseLayout(parsed, defaultConfig([]));
  } catch {
    return null;
  }
}

export function clearLegacyLocalConfig(
  storage: Pick<Storage, "removeItem"> | null,
): void {
  try {
    storage?.removeItem(COCKPIT_STORAGE_ITEM);
  } catch {
    // Storage may be unavailable; nothing to clean up then.
  }
}

export type StoredPreferences = {
  layout: unknown;
  version: number;
  updated_at: string | null;
};

export function loadPreferences(): Promise<StoredPreferences> {
  return apiFetch<StoredPreferences>(PREFERENCES_PATH);
}

export function savePreferences(
  layout: CockpitConfig,
  version: number,
): Promise<StoredPreferences> {
  return apiFetch<StoredPreferences>(PREFERENCES_PATH, {
    method: "PUT",
    body: JSON.stringify({ layout, version }),
  });
}

export function sameConfig(a: CockpitConfig, b: CockpitConfig): boolean {
  return JSON.stringify(a) === JSON.stringify(b);
}

export function toggleHidden(
  config: CockpitConfig,
  widget: CockpitWidget,
): CockpitConfig {
  const hidden = config.hidden.includes(widget)
    ? config.hidden.filter((w) => w !== widget)
    : [...config.hidden, widget];
  return { ...config, hidden };
}

export function move(
  config: CockpitConfig,
  widget: CockpitWidget,
  direction: -1 | 1,
): CockpitConfig {
  const idx = config.order.indexOf(widget);
  const target = idx + direction;
  if (idx < 0 || target < 0 || target >= config.order.length) return config;
  const order = [...config.order];
  [order[idx], order[target]] = [order[target], order[idx]];
  return { ...config, order };
}

/** Places `widget` immediately before `before` (or last when `before` is
 *  null), as a drag-and-drop reorder does. */
export function moveBefore(
  config: CockpitConfig,
  widget: CockpitWidget,
  before: CockpitWidget | null,
): CockpitConfig {
  if (widget === before) return config;
  const order = config.order.filter((w) => w !== widget);
  const at = before ? order.indexOf(before) : -1;
  if (at < 0) order.push(widget);
  else order.splice(at, 0, widget);
  return { ...config, order };
}

export function setDensity(
  config: CockpitConfig,
  density: Density,
): CockpitConfig {
  return { ...config, density };
}

export function setSize(
  config: CockpitConfig,
  widget: CockpitWidget,
  size: WidgetSize,
): CockpitConfig {
  return { ...config, sizes: { ...config.sizes, [widget]: size } };
}

export function sizeOf(
  config: CockpitConfig,
  widget: CockpitWidget,
): WidgetSize {
  return config.sizes[widget] ?? "half";
}

/** Widgets the signed-in role can actually populate. */
export function availableWidgets(
  canReadVisits: boolean,
  canReadWorklist: boolean,
): CockpitWidget[] {
  return COCKPIT_WIDGETS.filter((w) =>
    VISIT_WIDGETS.includes(w) ? canReadVisits : canReadWorklist,
  );
}

export function visibleWidgets(
  config: CockpitConfig,
  available: readonly CockpitWidget[] = COCKPIT_WIDGETS,
): CockpitWidget[] {
  return config.order.filter(
    (w) => !config.hidden.includes(w) && available.includes(w),
  );
}

export type Priority = {
  key:
    | "critical"
    | "ready"
    | "triage"
    | "alerts"
    | "overdue"
    | "review"
    | "attention"
    | "drafts";
  count: number;
  href: string;
  tone: "critical" | "warn" | "teal" | "neutral";
};

/** The top priorities for the signed-in professional, highest first; only
 *  real non-zero counts qualify, so an empty list means nothing is pending. */
export function topPriorities(
  input: {
    criticalOpen?: number;
    ready?: number;
    waitingTriage?: number;
    highAlerts?: number;
    overdueTasks?: number;
    awaitingReview?: number;
    attention?: number;
    drafts?: number;
  },
  limit = 3,
): Priority[] {
  const all: Priority[] = [
    {
      key: "critical",
      count: input.criticalOpen ?? 0,
      href: "/results",
      tone: "critical",
    },
    {
      key: "alerts",
      count: input.highAlerts ?? 0,
      href: "#w-alerts",
      tone: "critical",
    },
    { key: "ready", count: input.ready ?? 0, href: "#w-ready", tone: "teal" },
    {
      key: "triage",
      count: input.waitingTriage ?? 0,
      href: "#w-triage",
      tone: "warn",
    },
    {
      key: "overdue",
      count: input.overdueTasks ?? 0,
      href: "#w-tasks",
      tone: "warn",
    },
    {
      key: "review",
      count: input.awaitingReview ?? 0,
      href: "/results",
      tone: "warn",
    },
    {
      key: "attention",
      count: input.attention ?? 0,
      href: "#w-attention",
      tone: "neutral",
    },
    {
      key: "drafts",
      count: input.drafts ?? 0,
      href: "#w-drafts",
      tone: "neutral",
    },
  ];
  return all.filter((p) => p.count > 0).slice(0, limit);
}
