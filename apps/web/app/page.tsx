"use client";

import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { AppHeader } from "./chrome";
import { t } from "@/lib/i18n";
import type { TKey } from "@/lib/i18n";
import { useSession } from "@/lib/session";

const DEMO_USERS: {
  username: string;
  roleKey: TKey;
  descriptionKey: TKey;
  home: string;
}[] = [
  {
    username: "dr.garcia",
    roleKey: "roleClinician",
    descriptionKey: "demoGarcia",
    home: "/dashboard",
  },
  {
    username: "nurse.kim",
    roleKey: "roleNurse",
    descriptionKey: "demoNurse",
    home: "/dashboard",
  },
  {
    username: "reg.rivera",
    roleKey: "roleRegistration",
    descriptionKey: "demoRegistration",
    home: "/patients",
  },
  {
    username: "privacy.wolf",
    roleKey: "rolePrivacy",
    descriptionKey: "demoPrivacy",
    home: "/dashboard",
  },
];

export default function SignInPage() {
  const { lang, authenticated, signIn: sessionSignIn } = useSession();
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState<string | null>(null);
  const router = useRouter();

  // Redirect only visitors who arrived already authenticated; a role-card
  // sign-in navigates to its own role-appropriate home instead.
  useEffect(() => {
    if (authenticated && busy === null) router.replace("/dashboard");
  }, [authenticated, busy, router]);

  async function signInAs(username: string, home: string) {
    setBusy(username);
    setError(null);
    try {
      await sessionSignIn(`dev-${username}`);
      router.push(home);
    } catch (err) {
      setError(err instanceof Error ? err.message : t(lang, "error"));
      setBusy(null);
    }
  }

  const devAuth = process.env.NEXT_PUBLIC_WELLOS_DEV_AUTH === "true";

  if (!devAuth) {
    return (
      <>
        <AppHeader />
        <main>
          <div className="card">
            <h2>{t(lang, "signIn")}</h2>
            <p className="muted">{t(lang, "oidcSignInHelp")}</p>
            <p>
              <a className="primary" href="/api/auth/oidc/login" role="button">
                {t(lang, "oidcSignInButton")}
              </a>
            </p>
          </div>
        </main>
      </>
    );
  }

  return (
    <>
      <AppHeader />
      <main>
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
          <div className="role-cards">
            {DEMO_USERS.map((u) => (
              <button
                key={u.username}
                className="role-card"
                disabled={busy !== null}
                onClick={() => void signInAs(u.username, u.home)}
              >
                <span className="role">{t(lang, u.roleKey)}</span>
                <span>{t(lang, u.descriptionKey)}</span>
                <span className="muted">
                  {busy === u.username
                    ? t(lang, "loading")
                    : `${t(lang, "signInAs")} ${u.username}`}
                </span>
              </button>
            ))}
          </div>
        </div>
      </main>
    </>
  );
}
