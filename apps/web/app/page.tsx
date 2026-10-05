"use client";

import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { AppHeader, BrandMark } from "./chrome";
import { Icon } from "@/components/ui/icons";
import { t } from "@/lib/i18n";
import {
  fetchAuthProviders,
  fetchDevUsers,
  homeForRoles,
  roleLabels,
  type AuthProviders,
  type DevUser,
} from "@/lib/dev-users";
import { useSession } from "@/lib/session";

/**
 * Sign-in. Each method is rendered only when the server confirms it exists:
 * the identity-provider flow when OIDC is configured, and synthetic
 * development identities only when the server reports development
 * authentication (a `dev-fixtures` build in `WELLOS_ENV=development|test`
 * with `WELLOS_DEV_AUTH=true`). The browser bundle contains no usernames and
 * no client-side switch can enable them.
 */
export default function SignInPage() {
  const {
    lang,
    authenticated,
    meta,
    metaError,
    signIn: sessionSignIn,
  } = useSession();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const [providers, setProviders] = useState<AuthProviders | null>(null);
  const [providersError, setProvidersError] = useState(false);
  const [devUsers, setDevUsers] = useState<DevUser[] | null>(null);
  const [devUsersError, setDevUsersError] = useState(false);
  const router = useRouter();

  useEffect(() => {
    let cancelled = false;
    fetchAuthProviders()
      .then(async (p) => {
        if (cancelled) return;
        setProviders(p);
        if (!p.development) return;
        try {
          const res = await fetchDevUsers();
          if (!cancelled) setDevUsers(res ? res.users : null);
        } catch {
          if (!cancelled) setDevUsersError(true);
        }
      })
      .catch(() => {
        if (!cancelled) setProvidersError(true);
      });
    return () => {
      cancelled = true;
    };
  }, []);

  // Redirect only visitors who arrived already authenticated, to their
  // role-appropriate home once the workspace context is known.
  useEffect(() => {
    if (!authenticated || busy !== null) return;
    if (meta) router.replace(homeForRoles(meta.user.roles));
    else if (metaError) router.replace("/dashboard");
  }, [authenticated, busy, meta, metaError, router]);

  async function signInAs(user: DevUser) {
    setBusy(user.username);
    setError(null);
    try {
      await sessionSignIn(`dev-${user.username}`);
      router.push(homeForRoles(user.roles));
    } catch (err) {
      setError(err instanceof Error ? err.message : t(lang, "error"));
      setBusy(null);
    }
  }

  return (
    <>
      <AppHeader />
      <main className="signin">
        <section className="hero signin-hero" aria-labelledby="signin-tagline">
          <p className="eyebrow">{t(lang, "appName")}</p>
          <h1
            id="signin-tagline"
            style={{ display: "flex", alignItems: "center", gap: "0.6rem" }}
          >
            <BrandMark /> {t(lang, "signInTagline")}
          </h1>
          <p className="hero-sub">{t(lang, "signInIntro")}</p>
          <ul className="signin-pillars">
            <li>
              <Icon.Shield aria-hidden="true" /> {t(lang, "signInPillar1")}
            </li>
            <li>
              <Icon.Sparkle aria-hidden="true" /> {t(lang, "signInPillar2")}
            </li>
            <li>
              <Icon.Eye aria-hidden="true" /> {t(lang, "signInPillar3")}
            </li>
          </ul>
        </section>
        <div className="card">
          <h2>{t(lang, "signIn")}</h2>
          {providersError ? (
            <p role="alert" className="error">
              {t(lang, "signInMethodsUnavailable")}
            </p>
          ) : providers === null ? (
            <p className="muted" role="status">
              {t(lang, "loading")}
            </p>
          ) : providers.oidc ? (
            <>
              <p className="muted">{t(lang, "oidcSignInHelp")}</p>
              <p>
                <a
                  className="button primary"
                  href="/api/auth/oidc/login"
                  role="button"
                >
                  {t(lang, "oidcSignInButton")}
                </a>
              </p>
            </>
          ) : providers.development ? (
            <p className="muted">{t(lang, "oidcNotConfiguredDev")}</p>
          ) : (
            <p role="alert" className="error">
              {t(lang, "noSignInMethod")}
            </p>
          )}
        </div>
        {devUsersError ? (
          <p role="status" className="muted">
            {t(lang, "devLoginUnavailable")}
          </p>
        ) : null}
        {providers?.development && devUsers ? (
          <div className="card">
            <h2>
              {t(lang, "devLoginTitle")}{" "}
              <span className="dev-badge">{t(lang, "devLoginBadge")}</span>
            </h2>
            <p className="muted">{t(lang, "devLoginHelp")}</p>
            {error ? (
              <p role="alert" className="error">
                {error}
              </p>
            ) : null}
            {devUsers.length === 0 ? (
              <p className="muted">{t(lang, "devLoginEmpty")}</p>
            ) : (
              <div className="role-cards">
                {devUsers.map((u) => (
                  <button
                    key={u.username}
                    className="role-card"
                    disabled={busy !== null}
                    onClick={() => void signInAs(u)}
                  >
                    <span className="role">{roleLabels(lang, u.roles)}</span>
                    <span>{u.display_name}</span>
                    <span className="muted">
                      {t(lang, "devLoginSynthetic")}: {u.tenant_name}
                    </span>
                    <span className="muted">
                      {busy === u.username
                        ? t(lang, "loading")
                        : `${t(lang, "signInAs")} ${u.username}`}
                    </span>
                  </button>
                ))}
              </div>
            )}
          </div>
        ) : null}
      </main>
    </>
  );
}
