import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { render } from "@testing-library/react";
import { attachMotion, MotionLayer, tiltFor } from "@/components/ui/motion";

const mediaListeners = new Map<string, Set<() => void>>();

function mockMedia(matching: Set<string>) {
  mediaListeners.clear();
  Object.defineProperty(window, "matchMedia", {
    configurable: true,
    writable: true,
    value: (q: string) => ({
      get matches() {
        return matching.has(q);
      },
      media: q,
      addEventListener: (_: string, fn: () => void) => {
        if (!mediaListeners.has(q)) mediaListeners.set(q, new Set());
        mediaListeners.get(q)!.add(fn);
      },
      removeEventListener: (_: string, fn: () => void) => {
        mediaListeners.get(q)?.delete(fn);
      },
    }),
  });
  return {
    set(q: string, on: boolean) {
      if (on) matching.add(q);
      else matching.delete(q);
      mediaListeners.get(q)?.forEach((fn) => fn());
    },
  };
}

/* jsdom has no PointerEvent; the driver only reads MouseEvent fields. */
function pointer(type: string, init: MouseEventInit = {}): MouseEvent {
  return new MouseEvent(type, { bubbles: true, ...init });
}

function box(el: HTMLElement, w = 200, h = 100) {
  el.getBoundingClientRect = () =>
    ({
      left: 0,
      top: 0,
      width: w,
      height: h,
      right: w,
      bottom: h,
      x: 0,
      y: 0,
    }) as DOMRect;
}

describe("tiltFor", () => {
  it("is flat at the centre and bounded at the edges", () => {
    expect(tiltFor(100, 50, 200, 100)).toEqual({
      rx: "0.00deg",
      ry: "0.00deg",
      mx: "50.0%",
      my: "50.0%",
    });
    const corner = tiltFor(200, 0, 200, 100);
    expect(corner.ry).toBe("7.00deg");
    expect(corner.rx).toBe("7.00deg");
    expect(tiltFor(500, -50, 200, 100).ry).toBe("7.00deg");
  });
  it("degrades safely for a zero-sized box", () => {
    expect(tiltFor(10, 10, 0, 0).rx).toBe("0deg");
  });
});

describe("attachMotion", () => {
  let detach: (() => void) | null = null;
  beforeEach(() => {
    vi.useFakeTimers();
  });
  afterEach(() => {
    detach?.();
    detach = null;
    vi.useRealTimers();
    document.body.innerHTML = "";
  });

  it("tilts data-tilt surfaces toward the pointer and clears on leave", () => {
    mockMedia(new Set());
    const el = document.createElement("a");
    el.setAttribute("data-tilt", "");
    box(el);
    document.body.appendChild(el);
    detach = attachMotion(document);
    el.dispatchEvent(
      pointer("pointermove", {
        clientX: 150,
        clientY: 25,
      }),
    );
    expect(el.classList.contains("is-tilting")).toBe(true);
    expect(el.style.getPropertyValue("--ry")).toBe("3.50deg");
    expect(el.style.getPropertyValue("--rx")).toBe("3.50deg");
    expect(el.style.getPropertyValue("--mx")).toBe("75.0%");
    el.dispatchEvent(pointer("pointerout"));
    expect(el.classList.contains("is-tilting")).toBe(false);
    expect(el.style.getPropertyValue("--rx")).toBe("");
  });

  it("stays inert under prefers-reduced-motion", () => {
    mockMedia(new Set(["(prefers-reduced-motion: reduce)"]));
    const el = document.createElement("button");
    el.setAttribute("data-tilt", "");
    box(el);
    document.body.appendChild(el);
    detach = attachMotion(document);
    el.dispatchEvent(
      pointer("pointermove", {
        clientX: 10,
        clientY: 10,
      }),
    );
    el.dispatchEvent(
      pointer("pointerdown", {
        clientX: 10,
        clientY: 10,
      }),
    );
    expect(el.classList.contains("is-tilting")).toBe(false);
    expect(el.classList.contains("is-pressed")).toBe(false);
  });

  it("ripples from the press point and never on disabled buttons", () => {
    mockMedia(new Set());
    const btn = document.createElement("button");
    const off = document.createElement("button");
    off.disabled = true;
    box(btn);
    box(off);
    document.body.append(btn, off);
    detach = attachMotion(document);
    btn.dispatchEvent(
      pointer("pointerdown", {
        clientX: 40,
        clientY: 20,
      }),
    );
    expect(btn.classList.contains("is-pressed")).toBe(true);
    expect(btn.style.getPropertyValue("--px")).toBe("40.0px");
    expect(btn.style.getPropertyValue("--py")).toBe("20.0px");
    vi.advanceTimersByTime(700);
    expect(btn.classList.contains("is-pressed")).toBe(false);
    off.dispatchEvent(
      pointer("pointerdown", {
        clientX: 1,
        clientY: 1,
      }),
    );
    expect(off.classList.contains("is-pressed")).toBe(false);
  });

  it("MotionLayer renders nothing and detaches on unmount", () => {
    mockMedia(new Set());
    const { container, unmount } = render(<MotionLayer />);
    expect(container).toBeEmptyDOMElement();
    const el = document.createElement("button");
    box(el);
    document.body.appendChild(el);
    unmount();
    el.dispatchEvent(
      pointer("pointerdown", {
        clientX: 1,
        clientY: 1,
      }),
    );
    expect(el.classList.contains("is-pressed")).toBe(false);
  });
  it("never tilts a disabled button and drops the tilt when it becomes disabled", () => {
    mockMedia(new Set());
    const el = document.createElement("button");
    el.setAttribute("data-tilt", "");
    box(el);
    document.body.appendChild(el);
    detach = attachMotion(document);
    el.disabled = true;
    el.dispatchEvent(pointer("pointermove", { clientX: 10, clientY: 10 }));
    expect(el.classList.contains("is-tilting")).toBe(false);
    expect(el.style.getPropertyValue("--rx")).toBe("");

    el.disabled = false;
    el.dispatchEvent(pointer("pointermove", { clientX: 10, clientY: 10 }));
    expect(el.classList.contains("is-tilting")).toBe(true);
    el.disabled = true;
    el.dispatchEvent(pointer("pointermove", { clientX: 20, clientY: 10 }));
    expect(el.classList.contains("is-tilting")).toBe(false);
  });

  it("clears a live tilt when reduced motion or a coarse pointer switches on", () => {
    const media = mockMedia(new Set());
    const el = document.createElement("div");
    el.setAttribute("data-tilt", "");
    box(el);
    document.body.appendChild(el);
    detach = attachMotion(document);
    el.dispatchEvent(pointer("pointermove", { clientX: 10, clientY: 10 }));
    expect(el.classList.contains("is-tilting")).toBe(true);

    media.set("(prefers-reduced-motion: reduce)", true);
    expect(el.classList.contains("is-tilting")).toBe(false);
    expect(el.style.getPropertyValue("--mx")).toBe("");
    media.set("(prefers-reduced-motion: reduce)", false);
    expect(el.classList.contains("is-tilting")).toBe(false);

    el.dispatchEvent(pointer("pointermove", { clientX: 10, clientY: 10 }));
    expect(el.classList.contains("is-tilting")).toBe(true);
    media.set("(pointer: coarse)", true);
    expect(el.classList.contains("is-tilting")).toBe(false);

    // A move that arrives while decoration is off also clears stale state.
    media.set("(pointer: coarse)", false);
    el.dispatchEvent(pointer("pointermove", { clientX: 10, clientY: 10 }));
    expect(el.classList.contains("is-tilting")).toBe(true);
    media.set("(prefers-reduced-motion: reduce)", true);
    el.dispatchEvent(pointer("pointermove", { clientX: 12, clientY: 10 }));
    expect(el.classList.contains("is-tilting")).toBe(false);
  });
});
