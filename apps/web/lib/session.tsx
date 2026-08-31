"use client";

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
} from "react";
import type { Lang } from "./i18n";

export type Theme = "north" | "south";

export type Facility = {
  id: string;
  name: string;
  accessible: boolean;
  can_register: boolean;
  can_act_clinically: boolean;
};

export type TenantMeta = {
  tenant: { id: string; name: string; cell: string };
  user: { username: string; display_name: string; roles: string[] };
  facilities: Facility[];
};

type Session = {
  /** null = unknown (loading), otherwise whether a server session exists. */
  authenticated: boolean | null;
  /** Trusted tenant/user/facility context; null until loaded. */
  meta: TenantMeta | null;
  /** True when loading the tenant context failed; retry via reloadMeta. */
  metaError: boolean;
  reloadMeta: () => void;
  lang: Lang;
  theme: Theme;
  signIn: (token: string) => Promise<void>;
  signOut: () => Promise<void>;
  setLang: (l: Lang) => void;
  setTheme: (t: Theme) => void;
};

const Ctx = createContext<Session | null>(null);

// Notified by apiFetch when the API reports the session is gone (401), so
// the shell can drop straight to the sign-in state instead of surfacing
// generic errors on every screen. Ordinary 403s and other errors are not
// session events and never trigger this.
let onSessionExpired: (() => void) | null = null;

export function SessionProvider({ children }: { children: React.ReactNode }) {
  const [authenticated, setAuthenticated] = useState<boolean | null>(null);
  const [meta, setMeta] = useState<TenantMeta | null>(null);
  const [metaError, setMetaError] = useState(false);
  const [lang, setLangState] = useState<Lang>("en");
  const [theme, setThemeState] = useState<Theme>("north");

  // Generation counter for metadata loads: a response is applied only when
  // it belongs to the latest request, so a stale response from a previous
  // session (or an older overlapping retry) can never overwrite the current
  // session's context.
  const metaGeneration = useRef(0);

  const reloadMeta = useCallback(() => {
    const generation = ++metaGeneration.current;
    setMetaError(false);
    apiFetch<TenantMeta>("/api/v1/meta/tenant")
      .then((m) => {
        if (metaGeneration.current === generation) setMeta(m);
      })
      .catch(() => {
        if (metaGeneration.current === generation) setMetaError(true);
      });
  }, []);

  useEffect(() => {
    if (!authenticated) {
      metaGeneration.current += 1;
      setMeta(null);
      setMetaError(false);
      return;
    }
    reloadMeta();
  }, [authenticated, reloadMeta]);

  useEffect(() => {
    fetch("/api/session", { cache: "no-store" })
      .then((r) => r.json())
      .then((d: { authenticated: boolean }) =>
        setAuthenticated(d.authenticated),
      )
      .catch(() => setAuthenticated(false));
    const l = localStorage.getItem("wellos.lang");
    if (l === "en" || l === "es") setLangState(l);
    const th = localStorage.getItem("wellos.theme");
    if (th === "north" || th === "south") setThemeState(th);
  }, []);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    document.documentElement.lang = lang;
  }, [theme, lang]);

  const signIn = useCallback(async (token: string) => {
    const res = await fetch("/api/session", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ token }),
    });
    if (!res.ok) {
      let message = `HTTP ${res.status}`;
      try {
        const body = (await res.json()) as {
          error?: { message?: string };
        } | null;
        message = body?.error?.message ?? message;
      } catch {
        // keep default message
      }
      throw new Error(message);
    }
    setAuthenticated(true);
  }, []);

  const signOut = useCallback(async () => {
    await fetch("/api/session", { method: "DELETE" });
    setAuthenticated(false);
  }, []);

  useEffect(() => {
    onSessionExpired = () => setAuthenticated(false);
    return () => {
      onSessionExpired = null;
    };
  }, []);

  const setLang = useCallback((l: Lang) => {
    localStorage.setItem("wellos.lang", l);
    setLangState(l);
  }, []);
  const setTheme = useCallback((th: Theme) => {
    localStorage.setItem("wellos.theme", th);
    setThemeState(th);
  }, []);

  const value = useMemo(
    () => ({
      authenticated,
      meta,
      metaError,
      reloadMeta,
      lang,
      theme,
      signIn,
      signOut,
      setLang,
      setTheme,
    }),
    [
      authenticated,
      meta,
      metaError,
      reloadMeta,
      lang,
      theme,
      signIn,
      signOut,
      setLang,
      setTheme,
    ],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useSession(): Session {
  const s = useContext(Ctx);
  if (!s) throw new Error("useSession must be used within SessionProvider");
  return s;
}

/** Read the double-submit CSRF token from its JavaScript-readable cookie. */
function csrfToken(): string | undefined {
  const match = document.cookie
    .split("; ")
    .find((c) => c.startsWith("wellos_csrf="));
  return match?.slice("wellos_csrf=".length);
}

/** API failure carrying the bounded machine-readable error code. */
export class ApiRequestError extends Error {
  readonly status: number;
  readonly code: string | undefined;

  constructor(message: string, status: number, code?: string) {
    super(message);
    this.name = "ApiRequestError";
    this.status = status;
    this.code = code;
  }
}

/**
 * Call the API through the same-origin BFF proxy. The opaque session
 * identifier lives in an HttpOnly cookie (no access token ever reaches
 * browser JavaScript); state-changing requests echo the CSRF token in the
 * x-csrf-token header, which the API checks against the session record.
 */
export async function apiFetch<T>(
  path: string,
  init?: RequestInit,
): Promise<T> {
  const method = init?.method?.toUpperCase() ?? "GET";
  const csrf = method === "GET" || method === "HEAD" ? undefined : csrfToken();
  const res = await fetch(path, {
    ...init,
    headers: {
      "Content-Type": "application/json",
      ...(csrf ? { "x-csrf-token": csrf } : {}),
      ...(init?.headers ?? {}),
    },
  });
  if (!res.ok) {
    if (res.status === 401) onSessionExpired?.();
    let message = `HTTP ${res.status}`;
    let code: string | undefined;
    try {
      const body = await res.json();
      message = body?.error?.message ?? message;
      code = body?.error?.code;
    } catch {
      // keep default message
    }
    throw new ApiRequestError(message, res.status, code);
  }
  return (await res.json()) as T;
}
