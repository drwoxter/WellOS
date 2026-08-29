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
  token: string | null;
  lang: Lang;
  theme: Theme;
  setToken: (t: string | null) => void;
  setLang: (l: Lang) => void;
  setTheme: (t: Theme) => void;
};

const Ctx = createContext<Session | null>(null);

export function SessionProvider({ children }: { children: React.ReactNode }) {
  const [token, setTokenState] = useState<string | null>(null);
  const [lang, setLangState] = useState<Lang>("en");
  const [theme, setThemeState] = useState<Theme>("north");

  useEffect(() => {
    setTokenState(sessionStorage.getItem("wellos.token"));
    const l = localStorage.getItem("wellos.lang");
    if (l === "en" || l === "es") setLangState(l);
    const th = localStorage.getItem("wellos.theme");
    if (th === "north" || th === "south") setThemeState(th);
  }, []);

  useEffect(() => {
    document.documentElement.dataset.theme = theme;
    document.documentElement.lang = lang;
  }, [theme, lang]);

  const setToken = useCallback((t: string | null) => {
    if (t) sessionStorage.setItem("wellos.token", t);
    else sessionStorage.removeItem("wellos.token");
    setTokenState(t);
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
    () => ({ token, lang, theme, setToken, setLang, setTheme }),
    [token, lang, theme, setToken, setLang, setTheme],
  );
  return <Ctx.Provider value={value}>{children}</Ctx.Provider>;
}

export function useSession(): Session {
  const s = useContext(Ctx);
  if (!s) throw new Error("useSession must be used within SessionProvider");
  return s;
}

export async function apiFetch<T>(
  token: string,
  path: string,
  init?: RequestInit,
): Promise<T> {
  const res = await fetch(path, {
    ...init,
    headers: {
      Authorization: `Bearer ${token}`,
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
