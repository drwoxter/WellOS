// Absolute position of every same-document history entry, recorded in its
// `history.state`. Browsers expose no current index, so `pushState` /
// `replaceState` are wrapped once per document to stamp it; a popstate then
// reveals how far a traversal went and `history.go` can reverse it exactly.
//
// Traversal listeners registered here run from one popstate listener that is
// installed before the App Router's. Browsers run a microtask checkpoint
// between popstate listeners, which is enough for the router to commit the
// new route and unmount the old screen before a listener the screen added
// later ever runs; going through this hook keeps the screen mounted while it
// decides, and `stopImmediatePropagation` keeps the router out of it.

const INDEX_KEY = "__wellosHistoryIndex";

type TraversalListener = (e: PopStateEvent) => void;

let installed = false;
let current: number | null = null;
const listeners: TraversalListener[] = [];

export function readHistoryIndex(state: unknown): number | null {
  if (typeof state !== "object" || state === null) return null;
  const idx = (state as Record<string, unknown>)[INDEX_KEY];
  return typeof idx === "number" && Number.isInteger(idx) ? idx : null;
}

function stamp(state: unknown, idx: number): unknown {
  if (state !== null && typeof state !== "object") return state;
  return { ...(state as Record<string, unknown> | null), [INDEX_KEY]: idx };
}

/**
 * Runs `listener` for every same-document traversal, ahead of the App
 * Router. `first` puts it before listeners registered earlier. Returns the
 * unsubscribe function.
 */
export function onHistoryTraversal(
  listener: TraversalListener,
  first = false,
): () => void {
  installHistoryIndex();
  if (first) listeners.unshift(listener);
  else listeners.push(listener);
  return () => {
    const i = listeners.indexOf(listener);
    if (i >= 0) listeners.splice(i, 1);
  };
}

/**
 * Wraps `History.prototype` so the App Router's own history writes are
 * stamped too, whichever order the wrappers were installed in.
 */
export function installHistoryIndex(): void {
  if (installed || typeof window === "undefined") return;
  installed = true;
  const proto = History.prototype;
  const nativePush = proto.pushState;
  const nativeReplace = proto.replaceState;

  proto.pushState = function pushState(
    this: History,
    data: unknown,
    unused: string,
    url?: string | URL | null,
  ) {
    if (current !== null) current += 1;
    return nativePush.call(
      this,
      current === null ? data : stamp(data, current),
      unused,
      url,
    );
  };
  proto.replaceState = function replaceState(
    this: History,
    data: unknown,
    unused: string,
    url?: string | URL | null,
  ) {
    return nativeReplace.call(
      this,
      current === null ? data : stamp(data, current),
      unused,
      url,
    );
  };
  window.addEventListener(
    "popstate",
    (e: PopStateEvent) => {
      current = readHistoryIndex(e.state);
      for (const listener of [...listeners]) listener(e);
    },
    true,
  );

  current = readHistoryIndex(window.history.state) ?? 0;
  nativeReplace.call(
    window.history,
    stamp(window.history.state, current),
    "",
    window.location.href,
  );
}
