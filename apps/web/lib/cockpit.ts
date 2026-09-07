// Dashboard cockpit layout: which widgets show, in what order and how dense.
// Only this configuration is kept in browser storage — never patients,
// results or any other clinical data.

export const COCKPIT_WIDGETS = [
  "drafts",
  "attention",
  "results",
  "tasks",
  "ai",
] as const;

export type CockpitWidget = (typeof COCKPIT_WIDGETS)[number];
export type Density = "compact" | "expanded";

export type CockpitConfig = {
  order: CockpitWidget[];
  hidden: CockpitWidget[];
  density: Density;
};

export const COCKPIT_STORAGE_ITEM = "wellos.cockpit.v1";

export function defaultConfig(roles: string[]): CockpitConfig {
  const physician = roles.includes("physician");
  if (physician) {
    return {
      order: ["drafts", "attention", "results", "tasks", "ai"],
      hidden: [],
      density: "expanded",
    };
  }
  if (roles.includes("laboratory_professional") || roles.includes("nurse")) {
    return {
      order: ["results", "tasks", "attention", "ai", "drafts"],
      hidden: ["drafts"],
      density: "compact",
    };
  }
  return {
    order: [...COCKPIT_WIDGETS],
    hidden: ["drafts", "attention", "ai"],
    density: "compact",
  };
}

function isWidget(x: unknown): x is CockpitWidget {
  return (
    typeof x === "string" && (COCKPIT_WIDGETS as readonly string[]).includes(x)
  );
}

/** Accepts only a well-formed layout; anything else yields the role default so
 *  a tampered or outdated value can never break the dashboard. */
export function parseConfig(
  raw: string | null,
  fallback: CockpitConfig,
): CockpitConfig {
  if (!raw) return fallback;
  try {
    const parsed: unknown = JSON.parse(raw);
    if (typeof parsed !== "object" || parsed === null) return fallback;
    const p = parsed as Record<string, unknown>;
    if (!Array.isArray(p.order) || !Array.isArray(p.hidden)) return fallback;
    const order = p.order.filter(isWidget);
    const hidden = p.hidden.filter(isWidget);
    const density: Density = p.density === "compact" ? "compact" : "expanded";
    // Every widget appears exactly once; newly introduced widgets are appended.
    const seen = new Set<CockpitWidget>();
    const complete: CockpitWidget[] = [];
    for (const w of [...order, ...COCKPIT_WIDGETS]) {
      if (!seen.has(w)) {
        seen.add(w);
        complete.push(w);
      }
    }
    return { order: complete, hidden: Array.from(new Set(hidden)), density };
  } catch {
    return fallback;
  }
}

export function loadConfig(
  storage: Pick<Storage, "getItem"> | null,
  roles: string[],
): CockpitConfig {
  const fallback = defaultConfig(roles);
  if (!storage) return fallback;
  try {
    return parseConfig(storage.getItem(COCKPIT_STORAGE_ITEM), fallback);
  } catch {
    return fallback;
  }
}

export function saveConfig(
  storage: Pick<Storage, "setItem"> | null,
  config: CockpitConfig,
): void {
  try {
    storage?.setItem(COCKPIT_STORAGE_ITEM, JSON.stringify(config));
  } catch {
    // Storage may be unavailable (private mode, quota); the layout simply
    // does not persist.
  }
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

export function setDensity(
  config: CockpitConfig,
  density: Density,
): CockpitConfig {
  return { ...config, density };
}

export function visibleWidgets(config: CockpitConfig): CockpitWidget[] {
  return config.order.filter((w) => !config.hidden.includes(w));
}
