"use client";

import Link from "next/link";
import { useState } from "react";
import { AppShell } from "../../chrome";
import { t, type Lang } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import {
  CATALOG_KINDS,
  NO_SCHEDULING_CAPABILITIES,
  catalogKindLabel,
  catalogName,
  loadCatalog,
  type CatalogEntry,
  type CatalogHistoryEntry,
} from "@/lib/access";
import {
  ConfirmBox,
  MessageLine,
  PanelState,
  ReasonField,
  StatusBadge,
  useAction,
  useLoader,
  opsFetch,
} from "../shared";

type Facility = { id: string; name: string };

type EntryForm = {
  code: string;
  parent_id: string;
  name_en: string;
  name_es: string;
  synonyms: string;
  external_codings: string;
  effective_from: string;
  effective_to: string;
  facility_ids: string[];
  change_reason: string;
};

const EMPTY: EntryForm = {
  code: "",
  parent_id: "",
  name_en: "",
  name_es: "",
  synonyms: "",
  external_codings: "",
  effective_from: "",
  effective_to: "",
  facility_ids: [],
  change_reason: "",
};

function toForm(e: CatalogEntry & { facility_ids?: string[] }): EntryForm {
  return {
    code: e.code,
    parent_id: e.parent_id ?? "",
    name_en: e.name_en,
    name_es: e.name_es,
    synonyms: e.synonyms.join(", "),
    external_codings:
      e.external_codings && Object.keys(e.external_codings as object).length > 0
        ? JSON.stringify(e.external_codings)
        : "",
    effective_from: e.effective_from ?? "",
    effective_to: e.effective_to ?? "",
    facility_ids: e.facility_ids ?? [],
    change_reason: "",
  };
}

function parseCodings(raw: string): unknown | undefined {
  const s = raw.trim();
  if (!s) return undefined;
  return JSON.parse(s);
}

function EntryEditor({
  lang,
  kind,
  entry,
  siblings,
  facilities,
  onSaved,
  onCancel,
}: {
  lang: Lang;
  kind: string;
  entry: (CatalogEntry & { facility_ids?: string[] }) | null;
  siblings: CatalogEntry[];
  facilities: Facility[];
  onSaved: () => void;
  onCancel: () => void;
}) {
  const [form, setForm] = useState<EntryForm>(entry ? toForm(entry) : EMPTY);
  const { busy, message, run } = useAction(lang);
  const set = (patch: Partial<EntryForm>) =>
    setForm((f) => ({ ...f, ...patch }));
  const codePattern = /^[a-z0-9][a-z0-9_.-]{1,63}$/;
  const codeOk = entry ? true : codePattern.test(form.code.trim());
  const valid =
    codeOk && form.name_en.trim().length > 0 && form.name_es.trim().length > 0;

  async function save(e: React.FormEvent) {
    e.preventDefault();
    const ok = await run(
      async () => {
        let codings: unknown | undefined;
        try {
          codings = parseCodings(form.external_codings);
        } catch {
          throw new Error(t(lang, "invalidJson"));
        }
        const synonyms = form.synonyms
          .split(",")
          .map((s) => s.trim())
          .filter(Boolean);
        if (entry) {
          await opsFetch(`/api/v1/catalog/${entry.id}`, {
            method: "POST",
            body: JSON.stringify({
              version: entry.version,
              parent_id: form.parent_id || null,
              clear_parent: !form.parent_id && !!entry.parent_id,
              name_en: form.name_en.trim(),
              name_es: form.name_es.trim(),
              synonyms,
              external_codings: codings,
              effective_from: form.effective_from || null,
              effective_to: form.effective_to || null,
              clear_effective_to: !form.effective_to && !!entry.effective_to,
              facility_ids: form.facility_ids,
              change_reason: form.change_reason.trim() || null,
            }),
          });
        } else {
          await opsFetch("/api/v1/catalog", {
            method: "POST",
            body: JSON.stringify({
              kind,
              code: form.code.trim(),
              parent_id: form.parent_id || null,
              name_en: form.name_en.trim(),
              name_es: form.name_es.trim(),
              synonyms,
              external_codings: codings,
              effective_from: form.effective_from || null,
              effective_to: form.effective_to || null,
              facility_ids: form.facility_ids,
              change_reason: form.change_reason.trim() || null,
            }),
          });
        }
      },
      entry ? "catalogUpdated" : "catalogCreated",
    );
    if (ok) onSaved();
  }

  return (
    <form
      className="card nested"
      onSubmit={(e) => void save(e)}
      aria-labelledby="entry-editor-h"
      data-testid="catalog-editor"
    >
      <h4 id="entry-editor-h">
        {entry ? t(lang, "editEntry") : t(lang, "addEntry")} ·{" "}
        {catalogKindLabel(lang, kind)}
      </h4>
      <div className="grid-2">
        <div>
          <label htmlFor="ce-code">{t(lang, "code")}</label>
          <input
            id="ce-code"
            value={form.code}
            onChange={(e) => set({ code: e.target.value })}
            disabled={!!entry}
            required={!entry}
            pattern="^[a-z0-9][a-z0-9_.\-]{1,63}$"
            aria-describedby="ce-code-help"
            autoComplete="off"
          />
          <p id="ce-code-help" className="muted">
            {t(lang, "codeHelp")}
          </p>
        </div>
        <div>
          <label htmlFor="ce-parent">{t(lang, "parentEntry")}</label>
          <select
            id="ce-parent"
            value={form.parent_id}
            onChange={(e) => set({ parent_id: e.target.value })}
          >
            <option value="">{t(lang, "noParent")}</option>
            {siblings
              .filter((s) => s.id !== entry?.id)
              .map((s) => (
                <option key={s.id} value={s.id}>
                  {catalogName(lang, s)} ({s.code})
                </option>
              ))}
          </select>
        </div>
        <div>
          <label htmlFor="ce-en">{t(lang, "nameEn")}</label>
          <input
            id="ce-en"
            value={form.name_en}
            onChange={(e) => set({ name_en: e.target.value })}
            required
            maxLength={200}
          />
        </div>
        <div>
          <label htmlFor="ce-es">{t(lang, "nameEs")}</label>
          <input
            id="ce-es"
            value={form.name_es}
            onChange={(e) => set({ name_es: e.target.value })}
            required
            maxLength={200}
          />
        </div>
        <div>
          <label htmlFor="ce-syn">{t(lang, "synonyms")}</label>
          <input
            id="ce-syn"
            value={form.synonyms}
            onChange={(e) => set({ synonyms: e.target.value })}
            aria-describedby="ce-syn-help"
          />
          <p id="ce-syn-help" className="muted">
            {t(lang, "commaSeparated")}
          </p>
        </div>
        <div>
          <label htmlFor="ce-ext">{t(lang, "externalCodings")}</label>
          <input
            id="ce-ext"
            value={form.external_codings}
            onChange={(e) => set({ external_codings: e.target.value })}
            placeholder='{"snomed": "394579002"}'
            aria-describedby="ce-ext-help"
          />
          <p id="ce-ext-help" className="muted">
            {t(lang, "externalCodingsHelp")}
          </p>
        </div>
        <div>
          <label htmlFor="ce-from">{t(lang, "effectiveFrom")}</label>
          <input
            id="ce-from"
            type="date"
            value={form.effective_from}
            onChange={(e) => set({ effective_from: e.target.value })}
          />
        </div>
        <div>
          <label htmlFor="ce-to">{t(lang, "effectiveTo")}</label>
          <input
            id="ce-to"
            type="date"
            value={form.effective_to}
            onChange={(e) => set({ effective_to: e.target.value })}
          />
        </div>
      </div>
      {facilities.length > 0 ? (
        <fieldset>
          <legend>{t(lang, "facilityAvailability")}</legend>
          <p className="muted">{t(lang, "facilityAvailabilityHelp")}</p>
          <div className="chips">
            {facilities.map((f) => (
              <label key={f.id} className="chip">
                <input
                  type="checkbox"
                  checked={form.facility_ids.includes(f.id)}
                  onChange={(e) =>
                    set({
                      facility_ids: e.target.checked
                        ? [...form.facility_ids, f.id]
                        : form.facility_ids.filter((x) => x !== f.id),
                    })
                  }
                />{" "}
                {f.name}
              </label>
            ))}
          </div>
        </fieldset>
      ) : null}
      <ReasonField
        id="ce-reason"
        lang={lang}
        value={form.change_reason}
        onChange={(v) => set({ change_reason: v })}
        label={t(lang, "changeReason")}
      />
      <MessageLine message={message} />
      <div className="actions">
        <button type="submit" disabled={busy || !valid}>
          {entry ? t(lang, "saveChanges") : t(lang, "createEntry")}
        </button>
        <button type="button" className="secondary" onClick={onCancel}>
          {t(lang, "cancel")}
        </button>
      </div>
    </form>
  );
}

function HistoryList({ lang, entryId }: { lang: Lang; entryId: string }) {
  const state = useLoader(
    () =>
      opsFetch<{ items: CatalogHistoryEntry[] }>(
        `/api/v1/catalog/${entryId}/history`,
      ),
    entryId,
  );
  return (
    <PanelState
      lang={lang}
      state={state}
      isEmpty={(d) => d.items.length === 0}
      emptyKey="noHistory"
    >
      {(d) => (
        <ol className="stack" data-testid="catalog-history">
          {d.items.map((h) => (
            <li key={h.version}>
              v{h.version} · {formatDateTime(lang, h.recorded_at)} ·{" "}
              {h.changed_by}
              {h.change_reason ? ` — ${h.change_reason}` : ""}
              <div className="muted">
                {h.snapshot.name_en} / {h.snapshot.name_es}
                {h.snapshot.active === false ? ` · ${t(lang, "inactive")}` : ""}
              </div>
            </li>
          ))}
        </ol>
      )}
    </PanelState>
  );
}

function CatalogAdmin() {
  const { lang, meta } = useSession();
  const caps = meta?.scheduling_capabilities ?? NO_SCHEDULING_CAPABILITIES;
  const facilities = meta?.facilities ?? [];
  const [kind, setKind] = useState<string>(CATALOG_KINDS[0]);
  const [q, setQ] = useState("");
  const [includeInactive, setIncludeInactive] = useState(false);
  const [editing, setEditing] = useState<{
    entry: (CatalogEntry & { facility_ids?: string[] }) | null;
  } | null>(null);
  const [historyFor, setHistoryFor] = useState<string | null>(null);
  const [deactivating, setDeactivating] = useState<{
    entry: CatalogEntry;
    reason: string;
  } | null>(null);
  const { busy, message, run } = useAction(lang);
  const state = useLoader(
    () =>
      loadCatalog({
        kind,
        q: q.trim() || undefined,
        include_inactive: includeInactive || undefined,
        limit: 200,
      }),
    `${kind}:${q}:${includeInactive}`,
    caps.can_manage_catalog,
  );

  if (meta && !caps.can_manage_catalog) {
    return (
      <div className="card">
        <p role="alert" className="error">
          {t(lang, "unauthorizedPanel")}
        </p>
        <Link href="/scheduling">{t(lang, "backToScheduling")}</Link>
      </div>
    );
  }

  async function openEdit(e: CatalogEntry) {
    const full = await opsFetch<CatalogEntry & { facility_ids: string[] }>(
      `/api/v1/catalog/${e.id}`,
    );
    setEditing({ entry: full });
  }

  async function deactivate() {
    if (!deactivating) return;
    const ok = await run(async () => {
      await opsFetch(`/api/v1/catalog/${deactivating.entry.id}/deactivate`, {
        method: "POST",
        body: JSON.stringify({
          version: deactivating.entry.version,
          change_reason: deactivating.reason.trim(),
        }),
      });
      setDeactivating(null);
    }, "catalogDeactivated");
    if (ok) state.reload();
  }

  async function reactivate(e: CatalogEntry) {
    const ok = await run(async () => {
      await opsFetch(`/api/v1/catalog/${e.id}`, {
        method: "POST",
        body: JSON.stringify({
          version: e.version,
          active: true,
          change_reason: t(lang, "reactivated"),
        }),
      });
    }, "catalogUpdated");
    if (ok) state.reload();
  }

  return (
    <>
      <div className="row-head">
        <div>
          <h2 style={{ marginTop: 0 }}>{t(lang, "catalogAdmin")}</h2>
          <p className="muted">{t(lang, "catalogAdminIntro")}</p>
        </div>
        <Link href="/scheduling" className="button secondary">
          {t(lang, "backToScheduling")}
        </Link>
      </div>
      <p className="advisory" role="note">
        {t(lang, "catalogNoAuthzNotice")}
      </p>
      <div className="tabs" role="tablist" aria-label={t(lang, "catalogKind")}>
        {CATALOG_KINDS.map((k) => (
          <button
            key={k}
            type="button"
            role="tab"
            id={`ctab-${k}`}
            aria-selected={kind === k}
            aria-controls="catalog-panel"
            tabIndex={kind === k ? 0 : -1}
            onClick={() => {
              setKind(k);
              setEditing(null);
              setHistoryFor(null);
            }}
            onKeyDown={(e) => {
              const idx = CATALOG_KINDS.indexOf(k);
              if (e.key === "ArrowRight" || e.key === "ArrowLeft") {
                e.preventDefault();
                const n =
                  (idx +
                    (e.key === "ArrowRight" ? 1 : -1) +
                    CATALOG_KINDS.length) %
                  CATALOG_KINDS.length;
                setKind(CATALOG_KINDS[n]);
                document.getElementById(`ctab-${CATALOG_KINDS[n]}`)?.focus();
              }
            }}
          >
            {catalogKindLabel(lang, k)}
          </button>
        ))}
      </div>
      <div
        id="catalog-panel"
        role="tabpanel"
        aria-labelledby={`ctab-${kind}`}
        className="card"
      >
        <div className="filters">
          <div>
            <label htmlFor="cat-q">{t(lang, "search")}</label>
            <input
              id="cat-q"
              type="search"
              value={q}
              onChange={(e) => setQ(e.target.value)}
              placeholder={t(lang, "searchCatalogPlaceholder")}
            />
          </div>
          <label className="chip">
            <input
              type="checkbox"
              checked={includeInactive}
              onChange={(e) => setIncludeInactive(e.target.checked)}
            />{" "}
            {t(lang, "includeInactive")}
          </label>
          <button
            type="button"
            data-testid="add-entry"
            onClick={() => setEditing({ entry: null })}
          >
            {t(lang, "addEntry")}
          </button>
        </div>
        <MessageLine message={message} />
        {editing ? (
          <EntryEditor
            key={editing.entry?.id ?? "new"}
            lang={lang}
            kind={kind}
            entry={editing.entry}
            siblings={state.data?.items ?? []}
            facilities={facilities}
            onSaved={() => {
              setEditing(null);
              state.reload();
            }}
            onCancel={() => setEditing(null)}
          />
        ) : null}
        <PanelState
          lang={lang}
          state={state}
          isEmpty={(d) => d.items.length === 0}
          emptyKey="noCatalogEntries"
        >
          {(d) => (
            <div className="table-wrap">
              <table data-testid="catalog-table">
                <thead>
                  <tr>
                    <th scope="col">{t(lang, "code")}</th>
                    <th scope="col">{t(lang, "nameEn")}</th>
                    <th scope="col">{t(lang, "nameEs")}</th>
                    <th scope="col">{t(lang, "parentEntry")}</th>
                    <th scope="col">{t(lang, "status")}</th>
                    <th scope="col">{t(lang, "actions")}</th>
                  </tr>
                </thead>
                <tbody>
                  {d.items.map((e) => (
                    <tr key={e.id}>
                      <td>
                        <code>{e.code}</code>
                      </td>
                      <td>{e.name_en}</td>
                      <td>{e.name_es}</td>
                      <td>
                        {e.parent_id
                          ? (d.items.find((p) => p.id === e.parent_id)?.code ??
                            "…")
                          : "—"}
                      </td>
                      <td>
                        <StatusBadge
                          label={
                            e.active ? t(lang, "active") : t(lang, "inactive")
                          }
                          tone={e.active ? "ok" : "neutral"}
                        />
                        <span className="muted"> v{e.version}</span>
                      </td>
                      <td>
                        <div className="actions compact">
                          <button
                            type="button"
                            className="secondary"
                            disabled={busy}
                            onClick={() => void openEdit(e)}
                          >
                            {t(lang, "edit")}
                          </button>
                          {e.active ? (
                            <button
                              type="button"
                              className="secondary"
                              disabled={busy}
                              onClick={() =>
                                setDeactivating({ entry: e, reason: "" })
                              }
                            >
                              {t(lang, "deactivate")}
                            </button>
                          ) : (
                            <button
                              type="button"
                              className="secondary"
                              disabled={busy}
                              onClick={() => void reactivate(e)}
                            >
                              {t(lang, "reactivate")}
                            </button>
                          )}
                          <button
                            type="button"
                            className="secondary"
                            aria-expanded={historyFor === e.id}
                            onClick={() =>
                              setHistoryFor(historyFor === e.id ? null : e.id)
                            }
                          >
                            {t(lang, "history")}
                          </button>
                        </div>
                        {deactivating?.entry.id === e.id ? (
                          <ConfirmBox
                            lang={lang}
                            title={t(lang, "deactivate")}
                            busy={busy}
                            disabled={deactivating.reason.trim().length < 3}
                            onConfirm={() => void deactivate()}
                            onCancel={() => setDeactivating(null)}
                          >
                            <p className="muted">{t(lang, "deactivateHelp")}</p>
                            <ReasonField
                              id={`deact-${e.id}`}
                              lang={lang}
                              value={deactivating.reason}
                              onChange={(v) =>
                                setDeactivating({ entry: e, reason: v })
                              }
                              required
                              label={t(lang, "changeReason")}
                            />
                          </ConfirmBox>
                        ) : null}
                        {historyFor === e.id ? (
                          <HistoryList lang={lang} entryId={e.id} />
                        ) : null}
                      </td>
                    </tr>
                  ))}
                </tbody>
              </table>
            </div>
          )}
        </PanelState>
      </div>
    </>
  );
}

export default function CatalogPage() {
  return (
    <AppShell>
      <CatalogAdmin />
    </AppShell>
  );
}
