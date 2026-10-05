"use client";

import Link from "next/link";
import { usePathname, useRouter } from "next/navigation";
import { useEffect, useRef, useState, type ReactNode } from "react";
import { t, type TKey } from "@/lib/i18n";
import { useSession, type TenantMeta } from "@/lib/session";
import { hasDiagnosticsWorkspaceAccess } from "@/lib/diagnostics";
import { canReadWorklist, canSearchPatients } from "@/lib/clinical";
import { canReadVisits } from "@/lib/visits";
import { canReadRisk } from "@/lib/risk";
import { hasSchedulingConsoleAccess } from "@/lib/access";
import { confirmLeaveUnsaved } from "@/lib/unsaved-guard";
import { Icon } from "@/components/ui/icons";
import { DmindPresence, type DmindState } from "@/components/ui/dmind";
import { Drawer, ToastProvider } from "@/components/ui/overlay";
import { EmptyState, ListRow, Pill } from "@/components/ui/primitives";
import { DmindContextProvider, useDmindContext } from "@/lib/dmind-context";

export type ShellRole = "patient" | "clinical" | "operational";

type IconComponent = (p: { className?: string }) => ReactNode;

export type NavLink = {
  href: string;
  label: string;
  icon: IconComponent;
  /** Additional path prefixes that highlight this link. */
  also?: string[];
  dmind?: boolean;
};

export type NavGroup = { label: string | null; links: NavLink[] };

const PATIENT_ROLES = new Set(["patient", "patient_representative"]);
const OPERATIONAL_ROLES = new Set([
  "registration_staff",
  "transport_coordinator",
  "scheduler",
  "access_coordinator",
]);
const CLINICAL_ROLES = new Set([
  "physician",
  "nurse",
  "laboratory_professional",
  "radiologist",
  "pathologist",
  "pharmacist",
]);

/** Which shell a user gets, derived from server-reported roles and capabilities. */
export function shellRoleFor(meta: TenantMeta | null): ShellRole {
  const roles = meta?.user.roles ?? [];
  const hasClinical =
    roles.some((r) => CLINICAL_ROLES.has(r)) ||
    canSearchPatients(roles) ||
    canReadWorklist(roles) ||
    canReadRisk(roles);
  const hasOperational =
    roles.some((r) => OPERATIONAL_ROLES.has(r)) ||
    canReadVisits(roles) ||
    hasSchedulingConsoleAccess(meta?.scheduling_capabilities);
  const selfService =
    roles.some((r) => PATIENT_ROLES.has(r)) ||
    Boolean(meta?.scheduling_capabilities?.self_service) ||
    Boolean(meta?.diagnostics_capabilities?.self_service);
  if (selfService && !hasClinical && !hasOperational) return "patient";
  if (hasOperational && !hasClinical) return "operational";
  return "clinical";
}

/** Capability-driven navigation, grouped per shell role. */
export function navGroupsFor(
  lang: "en" | "es",
  meta: TenantMeta | null,
  role: ShellRole,
): NavGroup[] {
  const roles = meta?.user.roles ?? [];
  const sched = meta?.scheduling_capabilities;
  const dx = meta?.diagnostics_capabilities;
  const L = (k: TKey) => t(lang, k);

  if (role === "patient") {
    return [
      {
        label: null,
        links: [
          { href: "/my", label: L("navHome"), icon: Icon.Home },
          ...(sched?.self_service
            ? [
                {
                  href: "/my/appointments",
                  label: L("navMyAppointments"),
                  icon: Icon.Calendar,
                },
              ]
            : []),
          ...(dx?.self_service
            ? [
                {
                  href: "/my/diagnostics",
                  label: L("navHealth"),
                  icon: Icon.Heart,
                },
              ]
            : []),
        ],
      },
    ];
  }

  const care: NavLink[] = [
    {
      href: "/dashboard",
      label: L(role === "operational" ? "navHome" : "navToday"),
      icon: Icon.Home,
    },
    ...(canSearchPatients(roles)
      ? [
          {
            href: "/patients",
            label: L("navPatients"),
            icon: Icon.Users,
            also: ["/encounters"],
          },
        ]
      : []),
    ...(hasDiagnosticsWorkspaceAccess(dx)
      ? [{ href: "/diagnostics", label: L("navDiagnostics"), icon: Icon.Flask }]
      : []),
    ...(canReadWorklist(roles)
      ? [
          {
            href: "/results",
            label: L("navResults"),
            icon: Icon.Report,
            also: ["/requests"],
          },
        ]
      : []),
    ...(canReadRisk(roles)
      ? [{ href: "/risk", label: L("navRisk"), icon: Icon.Pulse }]
      : []),
  ];
  const ops: NavLink[] = [
    ...(canReadVisits(roles)
      ? [
          {
            href: "/access",
            label: L("navAccess"),
            icon: Icon.Door,
            also: ["/visits"],
          },
        ]
      : []),
    ...(hasSchedulingConsoleAccess(sched)
      ? [{ href: "/scheduling", label: L("navAgenda"), icon: Icon.Calendar }]
      : []),
    ...(sched?.can_manage_resources
      ? [
          {
            href: "/scheduling/resources",
            label: L("resourceAdmin"),
            icon: Icon.Layers,
          },
        ]
      : []),
    ...(sched?.can_manage_catalog
      ? [
          {
            href: "/scheduling/catalog",
            label: L("catalogAdmin"),
            icon: Icon.Grid,
          },
        ]
      : []),
  ];
  const me: NavLink[] = [
    ...(sched?.self_service
      ? [
          {
            href: "/my/appointments",
            label: L("navMyAppointments"),
            icon: Icon.Calendar,
          },
        ]
      : []),
    ...(dx?.self_service
      ? [
          {
            href: "/my/diagnostics",
            label: L("navMyDiagnostics"),
            icon: Icon.Heart,
          },
        ]
      : []),
  ];
  const groups: NavGroup[] =
    role === "operational"
      ? [
          {
            label: L("uiNavGroupOperations"),
            links: ops.length ? ops : care.slice(0, 1),
          },
          {
            label: L("uiNavGroupCare"),
            links: ops.length ? care : care.slice(1),
          },
        ]
      : [
          { label: L("uiNavGroupCare"), links: care },
          { label: L("uiNavGroupOperations"), links: ops },
        ];
  if (me.length) groups.push({ label: L("uiNavGroupMe"), links: me });
  return groups.filter((g) => g.links.length > 0);
}

function isCurrent(pathname: string, l: NavLink): boolean {
  const matches = (p: string) =>
    p === "/"
      ? pathname === "/"
      : pathname === p || pathname.startsWith(`${p}/`);
  if (matches(l.href)) {
    // Prefer the most specific link (e.g. /scheduling/catalog over /scheduling).
    return true;
  }
  return (l.also ?? []).some(matches);
}

function currentLink(pathname: string, groups: NavGroup[]): NavLink | null {
  const all = groups.flatMap((g) => g.links);
  const exact = all.filter((l) => isCurrent(pathname, l));
  if (exact.length === 0) return null;
  return exact.sort((a, b) => b.href.length - a.href.length)[0];
}

/** Sign-out confirmed against unsaved documentation before the session is
 *  revoked; declining leaves session and editor untouched. */
function useGuardedSignOut() {
  const { signOut } = useSession();
  const router = useRouter();
  return () => {
    const stay = confirmLeaveUnsaved();
    if (!stay) return;
    void signOut().then(
      () => router.push("/"),
      () => stay(),
    );
  };
}

function initials(name: string): string {
  const parts = name
    .replace(/\([^)]*\)/g, " ")
    .trim()
    .split(/\s+/)
    .filter((p) => /^\p{L}/u.test(p));
  if (parts.length === 0) return "?";
  const first = parts[0][0] ?? "";
  const last = parts.length > 1 ? (parts[parts.length - 1][0] ?? "") : "";
  return (first + last).toUpperCase();
}

export function BrandMark() {
  return (
    <span className="brand-mark" aria-hidden="true">
      <svg
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2.2"
        strokeLinecap="round"
        strokeLinejoin="round"
      >
        <path d="M3 12h4l2-6 4 12 2-6h6" />
      </svg>
    </span>
  );
}

/**
 * Account menu: language, appearance and sign-out live here instead of as
 * top-level controls. Keyboard: Escape closes, focus returns to the trigger.
 */
export function UserMenu({ compact }: { compact?: boolean }) {
  const { lang, setLang, theme, setTheme, authenticated, meta } = useSession();
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);
  const buttonRef = useRef<HTMLButtonElement>(null);
  const signOut = useGuardedSignOut();

  useEffect(() => {
    if (!open) return;
    const onDoc = (e: MouseEvent) => {
      if (!rootRef.current?.contains(e.target as Node)) setOpen(false);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        setOpen(false);
        buttonRef.current?.focus();
      }
    };
    document.addEventListener("mousedown", onDoc);
    document.addEventListener("keydown", onKey);
    return () => {
      document.removeEventListener("mousedown", onDoc);
      document.removeEventListener("keydown", onKey);
    };
  }, [open]);

  const name = meta?.user?.display_name ?? "";
  return (
    <div className="user-menu" ref={rootRef}>
      <button
        ref={buttonRef}
        type="button"
        className="user-menu-button"
        aria-haspopup="dialog"
        aria-expanded={open}
        aria-label={t(lang, "uiOpenAccountMenu")}
        onClick={() => setOpen((o) => !o)}
      >
        <span className="avatar" aria-hidden="true">
          {name ? initials(name) : <Icon.User />}
        </span>
        {!compact && name ? <span className="user-name">{name}</span> : null}
        <Icon.ChevronDown aria-hidden="true" />
      </button>
      {open ? (
        <div
          className="user-menu-panel"
          role="dialog"
          aria-label={t(lang, "uiAccount")}
        >
          {meta ? (
            <div className="menu-head">
              <span className="avatar" aria-hidden="true">
                {initials(name)}
              </span>
              <div>
                <div className="who">{name}</div>
                <div className="roles">
                  {meta.user.username} · {meta.tenant.name}
                </div>
              </div>
            </div>
          ) : null}
          <label>
            <span className="row" style={{ gap: "0.4rem" }}>
              <Icon.Globe aria-hidden="true" /> {t(lang, "language")}
            </span>
            <select
              value={lang}
              onChange={(e) => setLang(e.target.value as "en" | "es")}
            >
              <option value="en">English</option>
              <option value="es">Español</option>
            </select>
          </label>
          <label>
            <span className="row" style={{ gap: "0.4rem" }}>
              <Icon.Palette aria-hidden="true" /> {t(lang, "uiAppearance")}
            </span>
            <select
              value={theme}
              onChange={(e) => setTheme(e.target.value as "north" | "south")}
            >
              <option value="north">{t(lang, "uiAppearanceNorth")}</option>
              <option value="south">{t(lang, "uiAppearanceSouth")}</option>
            </select>
          </label>
          {authenticated ? (
            <div className="menu-footer">
              <button type="button" className="secondary" onClick={signOut}>
                <Icon.SignOut aria-hidden="true" /> {t(lang, "signOut")}
              </button>
            </div>
          ) : null}
        </div>
      ) : null}
    </div>
  );
}

/** Header for unauthenticated pages (sign-in). */
export function AppHeader({ subtitle }: { subtitle?: string }) {
  const { lang } = useSession();
  return (
    <header className="app topbar" style={{ position: "static" }}>
      <Link
        href="/"
        className="brand"
        style={{
          color: "inherit",
          textDecoration: "none",
          display: "inline-flex",
          alignItems: "center",
          gap: "0.6rem",
          fontWeight: 700,
        }}
      >
        <BrandMark />
        <span>
          {t(lang, "appName")}
          {subtitle ? ` — ${subtitle}` : ""}
        </span>
      </Link>
      <span className="topbar-spacer" />
      <UserMenu compact />
    </header>
  );
}

function NavItem({
  link,
  pathname,
  onNavigate,
}: {
  link: NavLink;
  pathname: string;
  onNavigate?: () => void;
}) {
  const current = isCurrent(pathname, link);
  const I = link.icon;
  return (
    <Link
      className={`navlink${link.dmind ? " dmind" : ""}`}
      href={link.href}
      aria-current={current ? "page" : undefined}
      onClick={onNavigate}
    >
      <I />
      <span className="navlink-label">{link.label}</span>
    </Link>
  );
}

function useRailCollapsed(): [boolean, (v: boolean) => void] {
  const [collapsed, setCollapsed] = useState(false);
  useEffect(() => {
    try {
      setCollapsed(window.localStorage.getItem("wellos.rail") === "collapsed");
    } catch {
      // ignore storage failures
    }
  }, []);
  return [
    collapsed,
    (v) => {
      setCollapsed(v);
      try {
        window.localStorage.setItem(
          "wellos.rail",
          v ? "collapsed" : "expanded",
        );
      } catch {
        // ignore storage failures
      }
    },
  ];
}

function dmindStateFor(
  meta: TenantMeta | null,
  pageState: DmindState | undefined,
): DmindState {
  const model = meta?.ai_capabilities?.model;
  if (
    !model ||
    model.state === "disabled" ||
    model.state === "invalid_configuration"
  ) {
    return "offline";
  }
  return pageState ?? "idle";
}

function DmindLauncher() {
  const { lang, meta } = useSession();
  const { context } = useDmindContext();
  const [open, setOpen] = useState(false);
  if (!meta?.ai_capabilities) return null;
  const state = dmindStateFor(meta, context?.state);
  const items = context?.items ?? [];
  return (
    <>
      <button
        type="button"
        className="dmind-launcher"
        aria-haspopup="dialog"
        aria-expanded={open}
        onClick={() => setOpen(true)}
      >
        <DmindPresence
          state={state}
          lang={lang}
          label="dMind"
          sub={items.length ? `${items.length}` : undefined}
        />
        <span className="sr-only">{t(lang, "dmindOpenContext")}</span>
      </button>
      <Drawer
        open={open}
        onClose={() => setOpen(false)}
        title={t(lang, "dmindContextTitle")}
        closeLabel={t(lang, "uiClose")}
        tone="dmind"
        head={<DmindPresence state={state} lang={lang} hideLabel />}
      >
        <p className="muted" style={{ marginTop: 0 }}>
          {t(
            lang,
            state === "offline" ? "dmindStateOffline" : "dmindRequiresReview",
          )}
        </p>
        {context?.note ? <p>{context.note}</p> : null}
        {items.length === 0 ? (
          <EmptyState
            title={t(lang, "stateEmptyTitle")}
            description={t(lang, "dmindNoArtifacts")}
            icon={<Icon.Sparkle />}
          />
        ) : (
          <ul className="list-plain">
            {items.map((it) => (
              <ListRow
                key={it.id}
                tone={it.tone ?? "dmind"}
                title={
                  <>
                    {it.href ? (
                      <Link href={it.href}>{it.title}</Link>
                    ) : (
                      it.title
                    )}
                    {it.status ? (
                      <Pill tone="dmind" icon={false}>
                        {it.status}
                      </Pill>
                    ) : null}
                    {it.synthetic ? (
                      <Pill tone="warn" icon={false}>
                        {t(lang, "dmindSynthetic")}
                      </Pill>
                    ) : null}
                  </>
                }
                sub={
                  <>
                    {it.sub}
                    {it.model
                      ? ` · ${it.model}${it.prompt_version ? ` ${it.prompt_version}` : ""}`
                      : ""}
                    {it.autonomy_level ? ` · ${it.autonomy_level}` : ""}
                  </>
                }
              />
            ))}
          </ul>
        )}
      </Drawer>
    </>
  );
}

/**
 * Authenticated application shell. Clinical and operational professionals
 * get a collapsible desktop rail with grouped, capability-driven links;
 * patients and representatives get a calm centred column. On narrow screens
 * every role uses a bottom navigation with large touch targets. Language,
 * appearance and sign-out live in the account menu.
 */
export function AppShell({ children }: { children: ReactNode }) {
  const { lang, authenticated, meta, metaError, reloadMeta } = useSession();
  const pathname = usePathname();
  const [collapsed, setCollapsed] = useRailCollapsed();
  const [moreOpen, setMoreOpen] = useState(false);

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
            <Link className="button primary" href="/">
              {t(lang, "signIn")}
            </Link>
          </p>
        </div>
      </main>
    );
  }

  const role = shellRoleFor(meta);
  const groups = navGroupsFor(lang, meta, role);
  const allLinks = groups.flatMap((g) => g.links);
  const current = currentLink(pathname, groups);
  const accessible = meta?.facilities.filter((f) => f.accessible) ?? [];
  const facilityLabel =
    accessible.length === 0
      ? null
      : meta && accessible.length === meta.facilities.length
        ? t(lang, "allFacilities")
        : accessible.map((f) => f.name).join(" · ");
  const primaryMobile = allLinks.slice(0, 4);
  const overflow = allLinks.slice(4);

  return (
    <DmindContextProvider>
      <ToastProvider dismissLabel={t(lang, "uiDismiss")}>
        <div
          className={`shell role-${role}${collapsed ? " rail-collapsed" : ""}`}
        >
          <a className="skip-link" href="#main-content">
            {t(lang, "skipToContent")}
          </a>
          <aside className="sidebar">
            <Link
              href={role === "patient" ? "/my" : "/dashboard"}
              className="brand"
            >
              <BrandMark />
              <span className="brand-text">{t(lang, "appName")}</span>
            </Link>
            <nav aria-label={t(lang, "uiMainNav")}>
              {groups.map((g, gi) => (
                <div key={gi}>
                  {g.label ? (
                    <p className="nav-group-label">{g.label}</p>
                  ) : null}
                  {g.links.map((l) => (
                    <NavItem key={l.href} link={l} pathname={pathname} />
                  ))}
                </div>
              ))}
            </nav>
            <div className="spacer" />
            <div className="shell-context">
              {meta ? (
                <>
                  <p className="who" style={{ margin: 0 }}>
                    {meta.user.display_name}
                  </p>
                  {facilityLabel ? (
                    <p style={{ margin: 0 }}>
                      {t(lang, "facility")}: {facilityLabel}
                    </p>
                  ) : null}
                </>
              ) : metaError ? (
                <p style={{ margin: 0 }}>
                  {t(lang, "contextLoadFailed")}{" "}
                  <button
                    className="linklike"
                    onClick={reloadMeta}
                    style={{ color: "inherit" }}
                  >
                    {t(lang, "retry")}
                  </button>
                </p>
              ) : (
                <p style={{ margin: 0 }}>{t(lang, "loading")}</p>
              )}
            </div>
            <button
              type="button"
              className="ghost rail-toggle"
              aria-pressed={collapsed}
              aria-label={t(lang, collapsed ? "uiExpandNav" : "uiCollapseNav")}
              onClick={() => setCollapsed(!collapsed)}
            >
              {collapsed ? <Icon.Expand /> : <Icon.Collapse />}
              <span className="navlink-label">
                {t(lang, collapsed ? "uiExpandNav" : "uiCollapseNav")}
              </span>
            </button>
          </aside>
          <div className="shell-main">
            <header className="topbar">
              <Link
                href={role === "patient" ? "/my" : "/dashboard"}
                className="brand topbar-brand"
              >
                <BrandMark />
              </Link>
              {current ? (
                <span className="page-title">{current.label}</span>
              ) : (
                <span className="page-title">{t(lang, "appName")}</span>
              )}
              {facilityLabel && role !== "patient" ? (
                <span className="facility">
                  <Icon.Map aria-hidden="true" />
                  {facilityLabel}
                </span>
              ) : null}
              <span className="topbar-spacer" />
              {meta?.environment?.synthetic_data ? (
                <span className="synthetic-chip">
                  {t(lang, "uiSyntheticChip")}
                </span>
              ) : null}
              {metaError ? (
                <button className="linklike" onClick={reloadMeta}>
                  {t(lang, "retry")}
                </button>
              ) : null}
              <UserMenu />
            </header>
            <main id="main-content">{children}</main>
            <nav className="mobile-nav" aria-label={t(lang, "uiMainNav")}>
              {primaryMobile.map((l) => (
                <NavItem key={l.href} link={l} pathname={pathname} />
              ))}
              {overflow.length > 0 ? (
                <button
                  type="button"
                  className="navlink"
                  style={{ background: "none", border: 0, font: "inherit" }}
                  aria-haspopup="dialog"
                  aria-expanded={moreOpen}
                  aria-current={
                    overflow.some((l) => isCurrent(pathname, l))
                      ? "page"
                      : undefined
                  }
                  onClick={() => setMoreOpen(true)}
                >
                  <Icon.Menu />
                  <span className="navlink-label">{t(lang, "uiMore")}</span>
                </button>
              ) : null}
            </nav>
            <Drawer
              open={moreOpen}
              onClose={() => setMoreOpen(false)}
              title={t(lang, "uiMainNav")}
              closeLabel={t(lang, "uiClose")}
            >
              <nav aria-label={t(lang, "uiMore")} className="stack">
                {groups.map((g, gi) => (
                  <div key={gi}>
                    {g.label ? (
                      <p
                        className="nav-group-label"
                        style={{ color: "var(--muted)" }}
                      >
                        {g.label}
                      </p>
                    ) : null}
                    {g.links.map((l) => {
                      const I = l.icon;
                      return (
                        <Link
                          key={l.href}
                          href={l.href}
                          className="navlink"
                          style={{ color: "var(--fg)" }}
                          aria-current={
                            isCurrent(pathname, l) ? "page" : undefined
                          }
                          onClick={() => setMoreOpen(false)}
                        >
                          <I />
                          <span className="navlink-label">{l.label}</span>
                        </Link>
                      );
                    })}
                  </div>
                ))}
              </nav>
            </Drawer>
            <DmindLauncher />
          </div>
        </div>
      </ToastProvider>
    </DmindContextProvider>
  );
}
