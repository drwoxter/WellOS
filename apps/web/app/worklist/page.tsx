"use client";

import Link from "next/link";
import { useEffect, useState } from "react";
import { useRouter } from "next/navigation";
import { AppHeader } from "../chrome";
import { t } from "@/lib/i18n";
import { apiFetch, useSession } from "@/lib/session";

type WorklistItem = {
  id: string;
  display: string;
  code_loinc: string;
  loop_state: string;
  has_open_alert: boolean;
  version: number;
  patient: { family_name: string; given_name: string; identifier: string };
};

export default function WorklistPage() {
  const { token, lang } = useSession();
  const [items, setItems] = useState<WorklistItem[] | null>(null);
  const [error, setError] = useState<string | null>(null);
  const router = useRouter();

  useEffect(() => {
    if (token === null) return;
    apiFetch<{ items: WorklistItem[] }>(token, "/api/v1/worklist")
      .then((d) => setItems(d.items))
      .catch((e) => setError(e instanceof Error ? e.message : String(e)));
  }, [token]);

  useEffect(() => {
    const stored = sessionStorage.getItem("wellos.token");
    if (!stored) router.replace("/");
  }, [router]);

  return (
    <>
      <AppHeader subtitle={t(lang, "worklist")} />
      <main>
        {error ? (
          <p role="alert" className="error">
            {error}
          </p>
        ) : null}
        <div className="card">
          <h2>{t(lang, "worklist")}</h2>
          {items === null ? (
            <p className="muted">{t(lang, "loading")}</p>
          ) : (
            <table>
              <thead>
                <tr>
                  <th scope="col">{t(lang, "patient")}</th>
                  <th scope="col">{t(lang, "identifier")}</th>
                  <th scope="col">LOINC</th>
                  <th scope="col">{t(lang, "state")}</th>
                  <th scope="col">{t(lang, "openAlert")}</th>
                </tr>
              </thead>
              <tbody>
                {items.map((it) => (
                  <tr key={it.id}>
                    <td>
                      <Link href={`/requests/${it.id}`}>
                        {it.patient.family_name}, {it.patient.given_name} —{" "}
                        {it.display}
                      </Link>
                    </td>
                    <td>{it.patient.identifier}</td>
                    <td>{it.code_loinc}</td>
                    <td>
                      <span className="badge">{it.loop_state}</span>
                    </td>
                    <td>
                      {it.has_open_alert ? (
                        <span className="badge critical">{t(lang, "yes")}</span>
                      ) : (
                        <span className="badge ok">{t(lang, "no")}</span>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
        </div>
      </main>
    </>
  );
}
