"use client";

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useId,
  useRef,
  useState,
  type ReactNode,
} from "react";
import { Icon } from "./icons";
import { cx } from "./primitives";

const FOCUSABLE =
  'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

/** Traps Tab inside `ref`, restores focus on unmount, closes on Escape. */
function useDialogBehaviour(
  ref: React.RefObject<HTMLElement | null>,
  open: boolean,
  onClose: () => void,
) {
  useEffect(() => {
    if (!open) return;
    const previous = document.activeElement as HTMLElement | null;
    const root = ref.current;
    const first =
      root?.querySelector<HTMLElement>("[data-autofocus]") ??
      root?.querySelector<HTMLElement>(FOCUSABLE);
    first?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onClose();
        return;
      }
      if (e.key !== "Tab" || !root) return;
      const items = Array.from(root.querySelectorAll<HTMLElement>(FOCUSABLE));
      if (items.length === 0) return;
      const firstEl = items[0];
      const lastEl = items[items.length - 1];
      if (e.shiftKey && document.activeElement === firstEl) {
        e.preventDefault();
        lastEl.focus();
      } else if (!e.shiftKey && document.activeElement === lastEl) {
        e.preventDefault();
        firstEl.focus();
      }
    };
    document.addEventListener("keydown", onKey);
    const prevOverflow = document.body.style.overflow;
    document.body.style.overflow = "hidden";
    return () => {
      document.removeEventListener("keydown", onKey);
      document.body.style.overflow = prevOverflow;
      previous?.focus?.();
    };
  }, [open, onClose, ref]);
}

/* ---------- Drawer ---------- */

export function Drawer({
  open,
  onClose,
  title,
  children,
  footer,
  closeLabel,
  tone,
  head,
}: {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  children: ReactNode;
  footer?: ReactNode;
  closeLabel: string;
  tone?: "dmind";
  head?: ReactNode;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const titleId = useId();
  useDialogBehaviour(ref, open, onClose);
  if (!open) return null;
  return (
    <>
      <div className="drawer-backdrop" onClick={onClose} aria-hidden="true" />
      <div
        ref={ref}
        className={cx("drawer", tone)}
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
      >
        <div className="drawer-head">
          {head}
          <h2 id={titleId}>{title}</h2>
          <button
            type="button"
            className="ghost icon-button"
            onClick={onClose}
            aria-label={closeLabel}
          >
            <Icon.Close />
          </button>
        </div>
        <div className="drawer-body">{children}</div>
        {footer ? <div className="drawer-foot">{footer}</div> : null}
      </div>
    </>
  );
}

/* ---------- Modal ---------- */

export function Modal({
  open,
  onClose,
  title,
  children,
  actions,
  describedBy,
}: {
  open: boolean;
  onClose: () => void;
  title: ReactNode;
  children: ReactNode;
  actions?: ReactNode;
  describedBy?: string;
}) {
  const ref = useRef<HTMLDivElement>(null);
  const titleId = useId();
  useDialogBehaviour(ref, open, onClose);
  if (!open) return null;
  return (
    <div className="modal-backdrop" onClick={onClose}>
      <div
        ref={ref}
        className="modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={describedBy}
        onClick={(e) => e.stopPropagation()}
      >
        <h2 id={titleId}>{title}</h2>
        {children}
        {actions ? <div className="actions">{actions}</div> : null}
      </div>
    </div>
  );
}

/* ---------- Popover (click/keyboard, not hover-only) ---------- */

export function Popover({
  trigger,
  children,
  label,
  align,
}: {
  trigger: (props: {
    "aria-expanded": boolean;
    "aria-haspopup": "dialog";
    onClick: () => void;
  }) => ReactNode;
  children: ReactNode;
  label: string;
  align?: "end";
}) {
  const [open, setOpen] = useState(false);
  const ref = useRef<HTMLDivElement>(null);
  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (!ref.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") setOpen(false);
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);
  return (
    <div className="popover-anchor" ref={ref}>
      {trigger({
        "aria-expanded": open,
        "aria-haspopup": "dialog",
        onClick: () => setOpen((o) => !o),
      })}
      {open ? (
        <div
          className={cx("popover", align === "end" && "align-end")}
          role="dialog"
          aria-label={label}
        >
          {children}
        </div>
      ) : null}
    </div>
  );
}

/** Tooltip shown on hover *and* focus; content is also exposed via aria-describedby. */
export function Tooltip({
  text,
  children,
}: {
  text: string;
  children: ReactNode;
}) {
  const [show, setShow] = useState(false);
  const id = useId();
  return (
    <span
      className="popover-anchor"
      onMouseEnter={() => setShow(true)}
      onMouseLeave={() => setShow(false)}
      onFocus={() => setShow(true)}
      onBlur={() => setShow(false)}
      aria-describedby={id}
    >
      {children}
      <span id={id} role="tooltip" className={show ? "tooltip" : "sr-only"}>
        {text}
      </span>
    </span>
  );
}

/* ---------- Toasts ---------- */

type Toast = {
  id: number;
  text: string;
  tone: "ok" | "error" | "warn" | "neutral";
};

const ToastCtx = createContext<{
  push: (text: string, tone?: Toast["tone"]) => void;
} | null>(null);

export function ToastProvider({
  children,
  dismissLabel,
}: {
  children: ReactNode;
  dismissLabel: string;
}) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const seq = useRef(0);
  const push = useCallback((text: string, tone: Toast["tone"] = "neutral") => {
    const id = ++seq.current;
    setToasts((t) => [...t, { id, text, tone }]);
    window.setTimeout(
      () => setToasts((t) => t.filter((x) => x.id !== id)),
      6000,
    );
  }, []);
  return (
    <ToastCtx.Provider value={{ push }}>
      {children}
      <div className="toast-region" aria-live="polite" aria-atomic="false">
        {toasts.map((t) => (
          <div
            key={t.id}
            className={cx("toast", t.tone !== "neutral" && t.tone)}
            role="status"
          >
            <span>{t.text}</span>
            <button
              type="button"
              aria-label={dismissLabel}
              onClick={() =>
                setToasts((all) => all.filter((x) => x.id !== t.id))
              }
            >
              <Icon.Close />
            </button>
          </div>
        ))}
      </div>
    </ToastCtx.Provider>
  );
}

export function useToast() {
  const ctx = useContext(ToastCtx);
  return ctx ?? { push: () => undefined };
}
