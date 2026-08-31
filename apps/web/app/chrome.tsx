"use client";

import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { t } from "@/lib/i18n";
import { useSession } from "@/lib/session";

export function AppHeader({ subtitle }: { subtitle?: string }) {
  const { lang, setLang, theme, setTheme, authenticated, signOut } =
    useSession();
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
      {authenticated ? (
        <button
          className="primary"
          onClick={() => {
            void signOut().then(() => router.push("/"));
          }}
        >
          {t(lang, "signOut")}
        </button>
      ) : null}
    </header>
  );
}

function NavLinks() {
  const { lang } = useSession();
  const pathname = usePathname();
  const links = [
    { href: "/dashboard", label: t(lang, "navHome") },
    { href: "/patients", label: t(lang, "navPatients") },
    { href: "/results", label: t(lang, "navResults") },
  ];
  return (
    <>
      {links.map((l) => {
        const current =
          pathname === l.href || pathname.startsWith(`${l.href}/`);
        return (
          <Link
            key={l.href}
            className="navlink"
            href={l.href}
            aria-current={current ? "page" : undefined}
          >
            {l.label}
          </Link>
        );
      })}
    </>
  );
}

/**
 * Authenticated application shell: sidebar navigation on desktop, compact
 * top navigation on mobile, with user/facility context, language and theme
 * controls, and sign-out. Redirects to sign-in when no session exists.
 */
export function AppShell({ children }: { children: React.ReactNode }) {
  const { lang, setLang, theme, setTheme, authenticated, meta, signOut } =
    useSession();
  const router = useRouter();

  if (authenticated === null) {
    return (
      <main>
        <p className="muted" role="status">
          {t(lang, "loading")}
        </p>
      </main>
    );
  }
  if (!authenticated) {
    return (
      <main>
        <div className="card">
          <p>{t(lang, "unauthenticated")}</p>
          <p>
            <Link className="navlink" href="/">
              {t(lang, "signIn")}
            </Link>
          </p>
        </div>
      </main>
    );
  }

  const accessible = meta?.facilities.filter((f) => f.accessible) ?? [];
  const facilityLabel =
    accessible.length === 0
      ? null
      : meta && accessible.length === meta.facilities.length
        ? t(lang, "allFacilities")
        : accessible.map((f) => f.name).join(" · ");

  return (
    <div className="shell">
      <a className="skip-link" href="#main-content">
        {t(lang, "skipToContent")}
      </a>
      <aside className="sidebar">
        <p className="brand">{t(lang, "appName")}</p>
        <nav aria-label={t(lang, "menu")}>
          <NavLinks />
        </nav>
        <div className="spacer" />
        <div className="shell-context">
          {meta ? (
            <>
              <p className="who" style={{ margin: 0 }}>
                {meta.user.display_name}
              </p>
              {facilityLabel ? (
                <p className="muted" style={{ margin: 0 }}>
                  {t(lang, "facility")}: {facilityLabel}
                </p>
              ) : null}
            </>
          ) : (
            <p className="muted" style={{ margin: 0 }}>
              {t(lang, "loading")}
            </p>
          )}
        </div>
      </aside>
      <div className="shell-main">
        <nav className="mobile-nav" aria-label={t(lang, "menu")}>
          <span className="brand">{t(lang, "appName")}</span>
          <NavLinks />
        </nav>
        <div className="topbar">
          <span className="facility">
            {meta ? (
              <>
                {meta.user.display_name}
                {facilityLabel ? ` — ${facilityLabel}` : ""}
              </>
            ) : (
              t(lang, "loading")
            )}
          </span>
          <label style={{ margin: 0, fontWeight: 400 }}>
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
          <button
            className="secondary"
            onClick={() => {
              void signOut().then(() => router.push("/"));
            }}
          >
            {t(lang, "signOut")}
          </button>
        </div>
        <main id="main-content">{children}</main>
      </div>
    </div>
  );
}
