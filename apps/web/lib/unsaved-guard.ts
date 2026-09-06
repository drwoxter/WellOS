"use client";

import { useRouter } from "next/navigation";
import { useEffect, useRef } from "react";

type AppRouter = ReturnType<typeof useRouter>;
type NavigateMethod = "push" | "replace";

// History entries owned by a guard are marked in `history.state`: the
// screen's own entry is position 0 and the duplicate sitting on top of it
// is position 1, so a popstate reveals which way the user travelled.
type GuardMark = { id: string; pos: 0 | 1 };
const MARK_KEY = "__wellosUnsavedGuard";

function readMark(state: unknown, id: string): GuardMark | null {
  if (typeof state !== "object" || state === null) return null;
  const mark = (state as Record<string, unknown>)[MARK_KEY];
  if (typeof mark !== "object" || mark === null) return null;
  const { id: markId, pos } = mark as Record<string, unknown>;
  return markId === id && (pos === 0 || pos === 1) ? { id, pos } : null;
}

function withMark(state: unknown, mark: GuardMark): Record<string, unknown> {
  const base =
    typeof state === "object" && state !== null
      ? (state as Record<string, unknown>)
      : {};
  return { ...base, [MARK_KEY]: mark };
}

// popstate events produced by a deactivating guard dropping its own history
// entry; a guard activated in the meantime must re-sync rather than treat
// them as user navigation.
const cleanupPops = new WeakSet<Event>();

function dropTrapEntry() {
  const settle = (e: PopStateEvent) => {
    cleanupPops.add(e);
    window.removeEventListener("popstate", settle);
  };
  window.addEventListener("popstate", settle);
  window.history.back();
}

/**
 * Confirms before leaving the current screen while `active`.
 *
 * Covered exits:
 * - `Link` clicks and programmatic `router.push` / `router.replace`: the
 *   shared App Router instance from `useRouter()` is what `next/link` also
 *   navigates through, so wrapping its methods intercepts both.
 * - Browser Back: while active, a duplicate history entry for the current
 *   URL sits on top of the stack, so the first Back lands on the same screen
 *   (nothing unmounts) and can be confirmed — continuing backward — or
 *   undone. Traversals that stay on this screen (Forward onto a duplicate
 *   entry) never ask and never stand the guard down.
 * - Reload, tab close and other full unloads: `beforeunload`.
 *
 * Accepting a navigation stands the guard down for that navigation only, so
 * a single confirmation is ever shown. Deactivating (saved / signed /
 * cancelled) or unmounting restores the router methods, removes listeners and
 * drops the duplicate history entry. A multi-entry jump (history menu) is
 * confirmed but cannot be undone once declined.
 */
export function useUnsavedChangesGuard(active: boolean, message: string) {
  const router = useRouter();
  const messageRef = useRef(message);
  messageRef.current = message;
  const guardId = useRef<string | null>(null);
  if (guardId.current === null) {
    guardId.current = `${Date.now().toString(36)}-${Math.random()
      .toString(36)
      .slice(2)}`;
  }

  useEffect(() => {
    if (!active) return;
    const id = guardId.current ?? "";
    const trappedHref = window.location.href;
    let trapped = false;
    let pos: 0 | 1 = 0;
    let leaving = false;
    let afterPop: (() => void) | null = null;

    const confirmLeave = () => leaving || window.confirm(messageRef.current);

    const armTrap = () => {
      window.history.pushState(
        withMark(window.history.state, { id, pos: 1 }),
        "",
        window.location.href,
      );
      trapped = true;
      pos = 1;
    };

    const onPopState = (e: PopStateEvent) => {
      if (afterPop) {
        const run = afterPop;
        afterPop = null;
        run();
        return;
      }
      const mark = readMark(e.state, id);
      if (cleanupPops.has(e)) {
        // The previous guard's duplicate entry was just dropped from under
        // this one; rebuild it so Back is caught again.
        if (mark?.pos === 0 && trapped) armTrap();
        return;
      }
      if (!trapped) return;
      if (mark) {
        if (mark.pos < pos) {
          // Back onto the screen's own entry: still here, ask now.
          pos = 0;
          trapped = false;
          if (confirmLeave()) {
            leaving = true;
            window.history.back();
          } else {
            armTrap();
          }
        } else {
          // Forward onto the duplicate entry: nothing left the screen.
          pos = mark.pos;
        }
        return;
      }
      if (window.location.href === trappedHref) {
        // A stale duplicate of this screen; still here, keep guarding.
        return;
      }
      // Jumped several entries at once: the browser has already moved.
      trapped = false;
      leaving = confirmLeave();
    };

    const onBeforeUnload = (e: BeforeUnloadEvent) => {
      if (leaving) return;
      e.preventDefault();
      e.returnValue = "";
    };

    const original: Pick<AppRouter, NavigateMethod> = {
      push: router.push,
      replace: router.replace,
    };
    const guarded =
      (method: NavigateMethod): AppRouter[NavigateMethod] =>
      (href, options) => {
        if (!confirmLeave()) return;
        leaving = true;
        const go = () => original[method].call(router, href, options);
        if (trapped && pos === 1) {
          // Drop the duplicate entry first so the destination does not sit
          // on top of two entries for this screen.
          trapped = false;
          afterPop = go;
          window.history.back();
        } else {
          go();
        }
      };

    if (readMark(window.history.state, id)?.pos === 1) {
      // Re-activated on our own duplicate entry (for example typing again
      // right after a save, before the previous guard's cleanup settled).
      trapped = true;
      pos = 1;
    } else {
      window.history.replaceState(
        withMark(window.history.state, { id, pos: 0 }),
        "",
        window.location.href,
      );
      armTrap();
    }
    window.addEventListener("popstate", onPopState);
    window.addEventListener("beforeunload", onBeforeUnload);
    router.push = guarded("push");
    router.replace = guarded("replace");

    return () => {
      window.removeEventListener("popstate", onPopState);
      window.removeEventListener("beforeunload", onBeforeUnload);
      router.push = original.push;
      router.replace = original.replace;
      if (
        trapped &&
        pos === 1 &&
        !leaving &&
        window.location.href === trappedHref
      ) {
        trapped = false;
        dropTrapEntry();
      }
    };
  }, [active, router]);
}
