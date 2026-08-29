"use client";

import { useRouter } from "next/navigation";
import { t } from "@/lib/i18n";
import { useSession } from "@/lib/session";

export function AppHeader({ subtitle }: { subtitle?: string }) {
  const { lang, setLang, theme, setTheme, token, setToken } = useSession();
  const router = useRouter();
  return (
    <header className="app">
      <h1>
        {t(lang, "appName")}
        {subtitle ? ` — ${subtitle}` : ""}
      </h1>
      <label style={{ margin: 0, fontWeight: 400 }}>
        <span className="sr-only">{t(lang, "language")}</span>
        <select
          aria-label={t(lang, "language")}
          value={lang}
          onChange={(e) => setLang(e.target.value as "en" | "es")}
        >
          <option value="en">English</option>
          <option value="es">Español</option>
        </select>
      </label>
      <label style={{ margin: 0, fontWeight: 400 }}>
        <select
          aria-label={t(lang, "theme")}
          value={theme}
          onChange={(e) => setTheme(e.target.value as "north" | "south")}
        >
          <option value="north">Norte</option>
          <option value="south">Sur</option>
        </select>
      </label>
      {token ? (
        <button
          className="primary"
          onClick={() => {
            setToken(null);
            router.push("/");
          }}
        >
          {t(lang, "signOut")}
        </button>
      ) : null}
    </header>
  );
}
