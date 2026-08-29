"use client";

import { useState } from "react";
import { useRouter } from "next/navigation";
import { AppHeader } from "./chrome";
import { t } from "@/lib/i18n";
import { apiFetch, useSession } from "@/lib/session";

export default function SignInPage() {
  const { lang, setToken } = useSession();
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
      await apiFetch(token, "/api/v1/meta/tenant");
      setToken(token);
      router.push("/worklist");
    } catch (err) {
      setError(err instanceof Error ? err.message : t(lang, "error"));
    } finally {
      setBusy(false);
    }
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
