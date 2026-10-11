"use client";

import { useEffect, useState } from "react";

export const REDUCED_MOTION_QUERY = "(prefers-reduced-motion: reduce)";
export const COARSE_POINTER_QUERY = "(pointer: coarse)";

const MAX_TILT_DEG = 7;
const PRESS_CLASS = "is-pressed";
const TILT_CLASS = "is-tilting";
const RIPPLE_MS = 650;

export type TiltVars = {
  rx: string;
  ry: string;
  mx: string;
  my: string;
};

/** Rotation and highlight position for a pointer at (x, y) inside a box. */
export function tiltFor(
  x: number,
  y: number,
  width: number,
  height: number,
  maxDeg = MAX_TILT_DEG,
): TiltVars {
  if (width <= 0 || height <= 0) {
    return { rx: "0deg", ry: "0deg", mx: "50%", my: "50%" };
  }
  const px = Math.min(1, Math.max(0, x / width));
  const py = Math.min(1, Math.max(0, y / height));
  const ry = (px - 0.5) * 2 * maxDeg;
  const rx = (0.5 - py) * 2 * maxDeg;
  return {
    rx: `${rx.toFixed(2)}deg`,
    ry: `${ry.toFixed(2)}deg`,
    mx: `${(px * 100).toFixed(1)}%`,
    my: `${(py * 100).toFixed(1)}%`,
  };
}

function matches(query: string): boolean {
  return typeof window !== "undefined" &&
    typeof window.matchMedia === "function"
    ? window.matchMedia(query).matches
    : false;
}

/** True when the user prefers reduced motion; tracks changes live. */
export function usePrefersReducedMotion(): boolean {
  const [reduced, setReduced] = useState(false);
  useEffect(() => {
    if (typeof window.matchMedia !== "function") return;
    const mq = window.matchMedia(REDUCED_MOTION_QUERY);
    const update = () => setReduced(mq.matches);
    update();
    mq.addEventListener("change", update);
    return () => mq.removeEventListener("change", update);
  }, []);
  return reduced;
}

function applyTilt(el: HTMLElement, e: PointerEvent): void {
  const r = el.getBoundingClientRect();
  const v = tiltFor(e.clientX - r.left, e.clientY - r.top, r.width, r.height);
  el.style.setProperty("--rx", v.rx);
  el.style.setProperty("--ry", v.ry);
  el.style.setProperty("--mx", v.mx);
  el.style.setProperty("--my", v.my);
  el.classList.add(TILT_CLASS);
}

function clearTilt(el: HTMLElement): void {
  el.classList.remove(TILT_CLASS);
  el.style.removeProperty("--rx");
  el.style.removeProperty("--ry");
  el.style.removeProperty("--mx");
  el.style.removeProperty("--my");
}

/**
 * Document-level motion driver. Elements opt in with `data-tilt` (pointer-
 * tracking 3D tilt + specular highlight) and every button, `.button`,
 * `.role-card` and `.priority-tile` gets a ripple from the press point. Pure
 * presentation: nothing here changes focus, activation or semantics, and it
 * stays inert under prefers-reduced-motion or on coarse pointers.
 */
export function attachMotion(root: Document = document): () => void {
  let active: HTMLElement | null = null;
  const timers = new WeakMap<HTMLElement, number>();

  const decorative = () =>
    !matches(REDUCED_MOTION_QUERY) && !matches(COARSE_POINTER_QUERY);

  const onMove = (e: PointerEvent) => {
    if (!decorative()) return;
    const target = e.target instanceof Element ? e.target : null;
    const el = target?.closest<HTMLElement>("[data-tilt]") ?? null;
    if (active && active !== el) clearTilt(active);
    active = el;
    if (el) applyTilt(el, e);
  };
  const onLeave = (e: PointerEvent) => {
    const target = e.target instanceof Element ? e.target : null;
    const el = target?.closest<HTMLElement>("[data-tilt]") ?? null;
    if (el && (!e.relatedTarget || !el.contains(e.relatedTarget as Node))) {
      clearTilt(el);
      if (active === el) active = null;
    }
  };
  const onDown = (e: PointerEvent) => {
    if (matches(REDUCED_MOTION_QUERY)) return;
    const target = e.target instanceof Element ? e.target : null;
    const el =
      target?.closest<HTMLElement>(
        "button, .button, .role-card, .priority-tile",
      ) ?? null;
    if (!el || (el instanceof HTMLButtonElement && el.disabled)) return;
    const r = el.getBoundingClientRect();
    el.style.setProperty("--px", `${(e.clientX - r.left).toFixed(1)}px`);
    el.style.setProperty("--py", `${(e.clientY - r.top).toFixed(1)}px`);
    el.classList.remove(PRESS_CLASS);
    // Restart the animation even on rapid repeated presses.
    void el.offsetWidth;
    el.classList.add(PRESS_CLASS);
    const prev = timers.get(el);
    if (prev) window.clearTimeout(prev);
    timers.set(
      el,
      window.setTimeout(() => el.classList.remove(PRESS_CLASS), RIPPLE_MS),
    );
  };

  root.addEventListener("pointermove", onMove, { passive: true });
  root.addEventListener("pointerout", onLeave, { passive: true });
  root.addEventListener("pointerdown", onDown, { passive: true });
  return () => {
    root.removeEventListener("pointermove", onMove);
    root.removeEventListener("pointerout", onLeave);
    root.removeEventListener("pointerdown", onDown);
    if (active) clearTilt(active);
  };
}

/** Mounts the document-level motion driver once per tree. Renders nothing. */
export function MotionLayer() {
  useEffect(() => attachMotion(document), []);
  return null;
}
