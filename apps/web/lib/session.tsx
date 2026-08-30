"use client";

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
} from "react";
import type { Lang } from "./i18n";

export type Theme = "north" | "south";

type Session = {
  /** null = unknown (loading), otherwise whether a server session exists. */
  authenticated: boolean | null;
  lang: Lang;
  theme: Theme;
  signIn: (token: string) => Promise<void>;
  signOut: () => Promise<void>;
  setLang: (l: Lang) => void;
  setTheme: (t: Theme) => void;
};

const Ctx = createContext<Session | null>(null);

export function SessionProvider({ children }: { children: React.ReactNode }) {
  const [authenticated, setAuthenticated] = useState<boolean | null>(null);
  const [lang, setLangState] = useState<Lang>("en");
  const [theme, setThemeState] = useState<Theme>("north");

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

  const setLang = useCallback((l: Lang) => {
    localStorage.setItem("wellos.lang", l);
    setLangState(l);
  }, []);
  const setTheme = useCallback((th: Theme) => {
    localStorage.setItem("wellos.theme", th);
    setThemeState(th);
  }, []);

  const value = useMemo(
    () => ({ authenticated, lang, theme, signIn, signOut, setLang, setTheme }),
    [authenticated, lang, theme, signIn, signOut, setLang, setTheme],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useSession(): Session {
  const s = useContext(Ctx);
  if (!s) throw new Error("useSession must be used within SessionProvider");
  return s;
}

/**
 * Call the API through the same-origin BFF proxy. The bearer token lives in
 * an HttpOnly cookie, so it is never readable from browser JavaScript.
 */
export async function apiFetch<T>(
  path: string,
  init?: RequestInit,
): Promise<T> {
  const res = await fetch(path, {
    ...init,
    headers: {
      "Content-Type": "application/json",
      ...(init?.headers ?? {}),
    },
  });
  if (!res.ok) {
    let message = `HTTP ${res.status}`;
    try {
      const body = await res.json();
      message = body?.error?.message ?? message;
    } catch {
      // keep default message
    }
    throw new Error(message);
  }
  return (await res.json()) as T;
}
