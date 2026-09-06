"use client";

import { useRouter } from "next/navigation";
import { useEffect, useRef } from "react";

type AppRouter = ReturnType<typeof useRouter>;
type NavigateMethod = "push" | "replace";

// popstate events produced by a deactivating guard dropping its own history
// entry; a guard activated in the meantime must not read them as Back.
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
 * - Browser Back / Forward: while active, a duplicate history entry for the
 *   current URL sits on top of the stack, so the first Back lands on the same
 *   screen (nothing unmounts) and can be confirmed or undone.
 * - Reload, tab close and other full unloads: `beforeunload`.
 *
 * Accepting a navigation stands the guard down for that navigation only, so
 * a single confirmation is ever shown. Deactivating (saved / signed /
 * cancelled) or unmounting restores the router methods, removes listeners and
 * drops the duplicate history entry.
 */
export function useUnsavedChangesGuard(active: boolean, message: string) {
  const router = useRouter();
  const messageRef = useRef(message);
  messageRef.current = message;

  useEffect(() => {
    if (!active) return;
    const trappedHref = window.location.href;
    let trapped = false;
    let leaving = false;
    let afterPop: (() => void) | null = null;

    const confirmLeave = () => leaving || window.confirm(messageRef.current);

    const armTrap = () => {
      window.history.pushState(window.history.state, "", window.location.href);
      trapped = true;
    };

    const onPopState = (e: PopStateEvent) => {
      if (cleanupPops.has(e)) return;
      if (afterPop) {
        const run = afterPop;
        afterPop = null;
        run();
        return;
      }
      if (!trapped) return;
      trapped = false;
      if (confirmLeave()) {
        leaving = true;
        window.history.back();
      } else {
        armTrap();
      }
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
        if (trapped) {
          // Drop the duplicate entry first so the destination does not sit
          // on top of two entries for this screen.
          trapped = false;
          afterPop = go;
          window.history.back();
        } else {
          go();
        }
      };

    armTrap();
    window.addEventListener("popstate", onPopState);
    window.addEventListener("beforeunload", onBeforeUnload);
    router.push = guarded("push");
    router.replace = guarded("replace");

    return () => {
      window.removeEventListener("popstate", onPopState);
      window.removeEventListener("beforeunload", onBeforeUnload);
      router.push = original.push;
      router.replace = original.replace;
      if (trapped && !leaving && window.location.href === trappedHref) {
        trapped = false;
        dropTrapEntry();
      }
    };
  }, [active, router]);
}
