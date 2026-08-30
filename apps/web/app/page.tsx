"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { AppHeader } from "./chrome";
import { t } from "@/lib/i18n";
import { useSession } from "@/lib/session";

export default function SignInPage() {
  const { lang, signIn: sessionSignIn } = useSession();
  const [value, setValue] = useState("dev-dr.garcia");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const router = useRouter();

  async function signIn(e: React.FormEvent) {
    e.preventDefault();
    setBusy(true);
    setError(null);
    const token = value.startsWith("dev-") ? value : `dev-${value}`;
    try {
      await sessionSignIn(token);
      router.push("/worklist");
    } catch (err) {
      setError(err instanceof Error ? err.message : t(lang, "error"));
    } finally {
      setBusy(false);
    }
  }

  // The token-entry form exists only for explicit local development.
  // Production deployments sign in through the configured OIDC provider.
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
          <h2>{t(lang, "signIn")}</h2>
          <p className="muted">{t(lang, "signInHelp")}</p>
          <form onSubmit={signIn}>
            <label htmlFor="token">{t(lang, "tokenLabel")}</label>
            <input
              id="token"
              value={value}
              onChange={(e) => setValue(e.target.value)}
              autoComplete="off"
            />
            {error ? (
              <p role="alert" className="error">
                {error}
              </p>
            ) : null}
            <p>
              <button className="primary" disabled={busy} type="submit">
                {t(lang, "signIn")}
              </button>
            </p>
          </form>
          <p className="muted">
            dev-dr.garcia · dev-nurse.kim · dev-reg.rivera · dev-privacy.wolf
          </p>
        </div>
      </main>
    </>
  );
}
