"use client";

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useState,
  type ReactNode,
} from "react";
import type { DmindState } from "@/components/ui/dmind";

/** One real dMind artifact surfaced for the current page. */
export type DmindContextItem = {
  id: string;
  title: string;
  sub?: string;
  href?: string;
  status?: string | null;
  autonomy_level?: string | null;
  model?: string | null;
  prompt_version?: string | null;
  created_at?: string | null;
  synthetic?: boolean | null;
  tone?: "ok" | "warn" | "critical" | "dmind" | "neutral";
};

export type DmindPageContext = {
  state: DmindState;
  items: DmindContextItem[];
  /** Short explanation of what dMind is doing (or why it is not) on this page. */
  note?: string | null;
};

type Ctx = {
  context: DmindPageContext | null;
  setContext: (next: DmindPageContext | null) => void;
};

const DmindCtx = createContext<Ctx | null>(null);

export function DmindContextProvider({ children }: { children: ReactNode }) {
  const [context, setContextState] = useState<DmindPageContext | null>(null);
  const setContext = useCallback((next: DmindPageContext | null) => {
    setContextState(next);
  }, []);
  const value = useMemo(() => ({ context, setContext }), [context, setContext]);
  return <DmindCtx.Provider value={value}>{children}</DmindCtx.Provider>;
}

export function useDmindContext(): Ctx {
  return useContext(DmindCtx) ?? { context: null, setContext: () => undefined };
}

/**
 * Pages call this with the real artifacts they loaded; the shell renders the
 * presence orb and context drawer from it. Clears on unmount so the next
 * page never inherits stale suggestions.
 */
export function useDmindPageContext(next: DmindPageContext | null) {
  const { setContext } = useDmindContext();
  const key = JSON.stringify(next);
  useEffect(() => {
    setContext(next);
    return () => setContext(null);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [key, setContext]);
}
