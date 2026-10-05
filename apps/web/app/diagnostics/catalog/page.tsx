"use client";

import Link from "next/link";
import { useState } from "react";
import { AppShell } from "../../chrome";
import { t, type Lang } from "@/lib/i18n";
import { useSession } from "@/lib/session";
import { formatDateTime } from "@/lib/clinical";
import { loadCatalog, type CatalogHistoryEntry } from "@/lib/access";
import {
  FULFILMENT_MODES,
  NO_DIAGNOSTICS_CAPABILITIES,
  RESULT_TYPES,
  categoryLabel,
  fulfilmentModeLabel,
  searchCatalog,
  type FulfilmentMode,
  type Orderable,
  type ResultType,
} from "@/lib/diagnostics";
import {
  ConfirmBox,
  MessageLine,
  PanelState,
  ReasonField,
  StatusBadge,
  opsFetch,
  useAction,
  useLoader,
} from "../../scheduling/shared";

type Facility = { id: string; name: string };

const CATEGORIES = [
  "laboratory",
  "imaging",
  "cardiology",
  "respiratory",
  "pathology",
  "procedure",
  "dental",
] as const;

type ComponentRow = {
  code: string;
  display: string;
  result_type: ResultType;
  unit: string;
  reference_range: string;
};

type Form = {
  code: string;
  name_en: string;
  name_es: string;
  synonyms: string;
  loinc: string;
  category_code: string;
  modality_code: string;
  result_type: ResultType;
  components: ComponentRow[];
  panel_member_codes: string;
  specimen_type: string;
  container: string;
  fasting_hours: string;
  preparation_en: string;
  preparation_es: string;
  scheduling_service_code: string;
  required_resource_types: string;
  fulfilment_modes: FulfilmentMode[];
  duplicate_window_days: string;
  redundant_with_codes: string;
  safety_rules: string;
  expects_imaging_study: boolean;
  facility_ids: string[];
  change_reason: string;
};

const EMPTY: Form = {
  code: "",
  name_en: "",
  name_es: "",
  synonyms: "",
  loinc: "",
  category_code: "laboratory",
  modality_code: "laboratory",
  result_type: "quantity",
  components: [],
  panel_member_codes: "",
  specimen_type: "",
  container: "",
  fasting_hours: "",
  preparation_en: "",
  preparation_es: "",
  scheduling_service_code: "",
  required_resource_types: "",
  fulfilment_modes: ["scheduled"],
  duplicate_window_days: "7",
  redundant_with_codes: "",
  safety_rules: "",
  expects_imaging_study: false,
  facility_ids: [],
  change_reason: "",
};

function list(raw: string): string[] {
  return raw
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

function toForm(o: Orderable): Form {
  const codings = Array.isArray(o.external_codings)
    ? (o.external_codings as { system?: string; code?: string }[])
    : [];
  const loinc =
    codings.find((c) => c.system === "http://loinc.org")?.code ?? "";
  return {
    code: o.code,
    name_en: o.name_en,
    name_es: o.name_es,
    synonyms: o.synonyms.join(", "),
    loinc,
    category_code: o.category_code,
    modality_code: o.modality_code ?? "",
    result_type: o.result_type,
    components: o.components.map((c) => ({
      code: c.code,
      display: c.display,
      result_type: c.result_type,
      unit: c.unit ?? "",
      reference_range: c.reference_range ?? "",
    })),
    panel_member_codes: o.panel_member_codes.join(", "),
    specimen_type: o.specimen?.type_code ?? "",
    container: o.specimen?.container_code ?? "",
    fasting_hours:
      o.specimen?.fasting_hours != null ? String(o.specimen.fasting_hours) : "",
    preparation_en: o.preparation_en ?? "",
    preparation_es: o.preparation_es ?? "",
    scheduling_service_code: o.scheduling_service_code ?? "",
    required_resource_types: o.required_resource_types.join(", "),
    fulfilment_modes: o.fulfilment_modes,
    duplicate_window_days: String(o.duplicate_window_days),
    redundant_with_codes: o.redundant_with_codes.join(", "),
    safety_rules:
      o.safety_rules.length > 0 ? JSON.stringify(o.safety_rules, null, 2) : "",
    expects_imaging_study: o.expects_imaging_study,
    facility_ids: o.facility_ids,
    change_reason: "",
  };
}

/** Builds the `OrderableConfig` payload; throws on malformed JSON rules. */
export function configFrom(form: Form): Record<string, unknown> {
  const fasting = form.fasting_hours.trim();
  const rules = form.safety_rules.trim() ? JSON.parse(form.safety_rules) : [];
  if (!Array.isArray(rules)) throw new Error("safety_rules must be an array");
  return {
    category_code: form.category_code,
    ...(form.modality_code.trim()
      ? { modality_code: form.modality_code.trim() }
      : {}),
    result_type: form.result_type,
    components: form.components.map((c) => ({
      code: c.code.trim(),
      display: c.display.trim(),
      result_type: c.result_type,
      ...(c.unit.trim() ? { unit: c.unit.trim() } : {}),
      ...(c.reference_range.trim()
        ? { reference_range: c.reference_range.trim() }
        : {}),
    })),
    panel_member_codes: list(form.panel_member_codes),
    ...(form.specimen_type.trim()
      ? {
          specimen: {
            type_code: form.specimen_type.trim(),
            ...(form.container.trim()
              ? { container_code: form.container.trim() }
              : {}),
            ...(fasting ? { fasting_hours: Number(fasting) } : {}),
          },
        }
      : {}),
    ...(form.preparation_en.trim()
      ? { preparation_en: form.preparation_en.trim() }
      : {}),
    ...(form.preparation_es.trim()
      ? { preparation_es: form.preparation_es.trim() }
      : {}),
    ...(form.scheduling_service_code.trim()
      ? { scheduling_service_code: form.scheduling_service_code.trim() }
      : {}),
    required_resource_types: list(form.required_resource_types),
    fulfilment_modes: form.fulfilment_modes,
    duplicate_window_days: Number(form.duplicate_window_days || "0"),
    redundant_with_codes: list(form.redundant_with_codes),
    safety_rules: rules,
    expects_imaging_study: form.expects_imaging_study,
  };
}

function OrderableEditor({
  lang,
  entry,
  facilities,
  onSaved,
  onCancel,
}: {
  lang: Lang;
  entry: Orderable | null;
  facilities: Facility[];
  onSaved: () => void;
  onCancel: () => void;
}) {
  const [form, setForm] = useState<Form>(entry ? toForm(entry) : EMPTY);
  const { busy, message, run } = useAction(lang);
  const services = useLoader(
    () => loadCatalog({ kind: "clinical_service", limit: 200 }),
    "clinical-services",
  );
  const serviceOptions = services.data?.items ?? [];
  const set = (patch: Partial<Form>) => setForm((f) => ({ ...f, ...patch }));
  const codeOk = entry
    ? true
    : /^[a-z0-9][a-z0-9_.-]{1,63}$/.test(form.code.trim());
  const needsService = form.fulfilment_modes.includes("scheduled");
  const valid =
    codeOk &&
    form.name_en.trim().length > 0 &&
    form.name_es.trim().length > 0 &&
    form.fulfilment_modes.length > 0 &&
    (!needsService || form.scheduling_service_code.trim().length > 0) &&
    form.components.every((c) => c.code.trim() && c.display.trim());

  async function save(e: React.FormEvent) {
    e.preventDefault();
    const ok = await run(
      async () => {
        let config: Record<string, unknown>;
        try {
          config = configFrom(form);
        } catch {
          throw new Error(t(lang, "invalidJson"));
        }
        const external_codings = form.loinc.trim()
          ? [{ system: "http://loinc.org", code: form.loinc.trim() }]
          : [];
        const common = {
          name_en: form.name_en.trim(),
          name_es: form.name_es.trim(),
          synonyms: list(form.synonyms),
          external_codings,
          config,
          facility_ids: form.facility_ids,
          change_reason: form.change_reason.trim() || null,
        };
        if (entry) {
          await opsFetch(`/api/v1/catalog/${entry.id}`, {
            method: "POST",
            body: JSON.stringify({ version: entry.version, ...common }),
          });
        } else {
          await opsFetch("/api/v1/catalog", {
            method: "POST",
            body: JSON.stringify({
              kind: "diagnostic_orderable",
              code: form.code.trim(),
              ...common,
            }),
          });
        }
      },
      entry ? "catalogUpdated" : "catalogCreated",
    );
    if (ok) onSaved();
  }

  const updateComponent = (i: number, patch: Partial<ComponentRow>) =>
    set({
      components: form.components.map((c, j) =>
        j === i ? { ...c, ...patch } : c,
      ),
    });

  return (
    <form
      className="card nested"
      onSubmit={(e) => void save(e)}
      aria-labelledby="dx-cat-editor-h"
      data-testid="dx-catalog-editor"
    >
      <h4 id="dx-cat-editor-h">
        {entry ? t(lang, "editEntry") : t(lang, "dxAddOrderable")}
      </h4>
      <div className="grid-2">
        <label>
          {t(lang, "code")} *
          <input
            value={form.code}
            onChange={(e) => set({ code: e.target.value })}
            disabled={!!entry}
            required
            aria-invalid={!codeOk}
            data-testid="dx-cat-code"
          />
        </label>
        <label>
          LOINC
          <input
            value={form.loinc}
            onChange={(e) => set({ loinc: e.target.value })}
          />
        </label>
        <label>
          {t(lang, "nameEn")} *
          <input
            value={form.name_en}
            onChange={(e) => set({ name_en: e.target.value })}
            required
            data-testid="dx-cat-name-en"
          />
        </label>
        <label>
          {t(lang, "nameEs")} *
          <input
            value={form.name_es}
            onChange={(e) => set({ name_es: e.target.value })}
            required
            data-testid="dx-cat-name-es"
          />
        </label>
        <label>
          {t(lang, "synonyms")}
          <input
            value={form.synonyms}
            onChange={(e) => set({ synonyms: e.target.value })}
          />
        </label>
        <label>
          {t(lang, "dxCategory")}
          <select
            value={form.category_code}
            onChange={(e) => set({ category_code: e.target.value })}
          >
            {CATEGORIES.map((c) => (
              <option key={c} value={c}>
                {categoryLabel(lang, c)}
              </option>
            ))}
          </select>
        </label>
        <label>
          {t(lang, "modality")}
          <input
            value={form.modality_code}
            onChange={(e) => set({ modality_code: e.target.value })}
          />
        </label>
        <label>
          {t(lang, "dxResultType")}
          <select
            value={form.result_type}
            onChange={(e) => set({ result_type: e.target.value as ResultType })}
          >
            {RESULT_TYPES.map((r) => (
              <option key={r} value={r}>
                {r}
              </option>
            ))}
          </select>
        </label>
      </div>

      <fieldset>
        <legend>{t(lang, "dxComponents")}</legend>
        {form.components.map((c, i) => (
          <div
            className="grid-2 dx-component-edit"
            key={i}
            data-testid="dx-cat-component"
          >
            <input
              aria-label={t(lang, "dxComponentCode")}
              placeholder={t(lang, "dxComponentCode")}
              value={c.code}
              onChange={(e) => updateComponent(i, { code: e.target.value })}
            />
            <input
              aria-label={t(lang, "dxComponentDisplay")}
              placeholder={t(lang, "dxComponentDisplay")}
              value={c.display}
              onChange={(e) => updateComponent(i, { display: e.target.value })}
            />
            <select
              aria-label={t(lang, "dxResultType")}
              value={c.result_type}
              onChange={(e) =>
                updateComponent(i, {
                  result_type: e.target.value as ResultType,
                })
              }
            >
              {RESULT_TYPES.map((r) => (
                <option key={r} value={r}>
                  {r}
                </option>
              ))}
            </select>
            <input
              aria-label={t(lang, "dxUnit")}
              placeholder={t(lang, "dxUnit")}
              value={c.unit}
              onChange={(e) => updateComponent(i, { unit: e.target.value })}
            />
            <input
              aria-label={t(lang, "referenceRange")}
              placeholder={t(lang, "referenceRange")}
              value={c.reference_range}
              onChange={(e) =>
                updateComponent(i, { reference_range: e.target.value })
              }
            />
            <button
              type="button"
              className="tertiary"
              aria-label={t(lang, "remove")}
              onClick={() =>
                set({ components: form.components.filter((_, j) => j !== i) })
              }
            >
              ×
            </button>
          </div>
        ))}
        <button
          type="button"
          className="tertiary"
          data-testid="dx-cat-add-component"
          onClick={() =>
            set({
              components: [
                ...form.components,
                {
                  code: "",
                  display: "",
                  result_type: form.result_type,
                  unit: "",
                  reference_range: "",
                },
              ],
            })
          }
        >
          {t(lang, "dxAddComponent")}
        </button>
      </fieldset>

      <div className="grid-2">
        <label>
          {t(lang, "dxPanelMembers")}
          <input
            value={form.panel_member_codes}
            onChange={(e) => set({ panel_member_codes: e.target.value })}
            placeholder="code_a, code_b"
          />
        </label>
        <label>
          {t(lang, "dxRedundantWith")}
          <input
            value={form.redundant_with_codes}
            onChange={(e) => set({ redundant_with_codes: e.target.value })}
          />
        </label>
        <label>
          {t(lang, "dxSpecimenType")}
          <input
            value={form.specimen_type}
            onChange={(e) => set({ specimen_type: e.target.value })}
            placeholder="blood_venous"
          />
        </label>
        <label>
          {t(lang, "dxContainer")}
          <input
            value={form.container}
            onChange={(e) => set({ container: e.target.value })}
          />
        </label>
        <label>
          {t(lang, "dxFastingHours")}
          <input
            type="number"
            min={0}
            value={form.fasting_hours}
            onChange={(e) => set({ fasting_hours: e.target.value })}
          />
        </label>
        <label>
          {t(lang, "dxDuplicateWindowDays")}
          <input
            type="number"
            min={0}
            value={form.duplicate_window_days}
            onChange={(e) => set({ duplicate_window_days: e.target.value })}
          />
        </label>
        <label>
          {t(lang, "dxPreparationEn")}
          <textarea
            rows={2}
            value={form.preparation_en}
            onChange={(e) => set({ preparation_en: e.target.value })}
          />
        </label>
        <label>
          {t(lang, "dxPreparationEs")}
          <textarea
            rows={2}
            value={form.preparation_es}
            onChange={(e) => set({ preparation_es: e.target.value })}
          />
        </label>
        <label>
          {t(lang, "dxSchedulingService")}
          {needsService ? " *" : ""}
          {serviceOptions.length > 0 ? (
            <select
              data-testid="dx-cat-service"
              value={form.scheduling_service_code}
              onChange={(e) => set({ scheduling_service_code: e.target.value })}
            >
              <option value="">—</option>
              {serviceOptions.map((svc) => (
                <option key={svc.code} value={svc.code}>
                  {svc.code} · {lang === "es" ? svc.name_es : svc.name_en}
                </option>
              ))}
            </select>
          ) : (
            <input
              data-testid="dx-cat-service"
              value={form.scheduling_service_code}
              onChange={(e) => set({ scheduling_service_code: e.target.value })}
            />
          )}
        </label>
        <label>
          {t(lang, "dxRequiredResources")}
          <input
            value={form.required_resource_types}
            onChange={(e) => set({ required_resource_types: e.target.value })}
          />
        </label>
      </div>
      <fieldset>
        <legend>{t(lang, "dxFulfilmentModes")} *</legend>
        <div className="chip-row">
          {FULFILMENT_MODES.map((m) => (
            <label key={m} className="check-option">
              <input
                type="checkbox"
                checked={form.fulfilment_modes.includes(m)}
                onChange={(e) =>
                  set({
                    fulfilment_modes: e.target.checked
                      ? [...form.fulfilment_modes, m]
                      : form.fulfilment_modes.filter((x) => x !== m),
                  })
                }
              />
              {fulfilmentModeLabel(lang, m)}
            </label>
          ))}
        </div>
      </fieldset>
      <label className="check-option">
        <input
          type="checkbox"
          checked={form.expects_imaging_study}
          onChange={(e) => set({ expects_imaging_study: e.target.checked })}
        />
        {t(lang, "dxExpectsImaging")}
      </label>
      <label>
        {t(lang, "dxSafetyRulesJson")}
        <textarea
          rows={4}
          value={form.safety_rules}
          onChange={(e) => set({ safety_rules: e.target.value })}
          placeholder='[{"id":"contrast_allergy","kind":"fact","fact_key":"allergy:contrast","severity":"hard_stop","text_en":"…","text_es":"…"}]'
          data-testid="dx-cat-rules"
        />
      </label>
      <fieldset>
        <legend>{t(lang, "facilities")}</legend>
        <div className="chip-row">
          {facilities.map((f) => (
            <label key={f.id} className="check-option">
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
              />
              {f.name}
            </label>
          ))}
        </div>
      </fieldset>
      <ReasonField
        id="dx-cat-reason"
        lang={lang}
        value={form.change_reason}
        onChange={(v) => set({ change_reason: v })}
        label={t(lang, "changeReason")}
      />
      <div className="visit-actions">
        <button
          type="submit"
          className="primary"
          disabled={busy || !valid}
          data-testid="dx-cat-save"
        >
          {t(lang, "save")}
        </button>
        <button type="button" className="tertiary" onClick={onCancel}>
          {t(lang, "cancel")}
        </button>
      </div>
      <MessageLine message={message} />
    </form>
  );
}

function History({ lang, id }: { lang: Lang; id: string }) {
  const state = useLoader(
    () =>
      opsFetch<{ items: CatalogHistoryEntry[] }>(
        `/api/v1/catalog/${id}/history`,
      ),
    id,
  );
  return (
    <PanelState
      lang={lang}
      state={state}
      emptyKey="noHistory"
      isEmpty={(d) => d.items.length === 0}
    >
      {(d) => (
        <ol className="timeline" data-testid="dx-cat-history">
          {d.items.map((h) => (
            <li key={h.version}>
              v{h.version} · {formatDateTime(lang, h.recorded_at)}
              {h.changed_by ? ` · ${h.changed_by}` : ""}
              {h.change_reason ? ` · ${h.change_reason}` : ""}
            </li>
          ))}
        </ol>
      )}
    </PanelState>
  );
}

function CatalogAdmin() {
  const { lang, meta } = useSession();
  const caps = meta?.diagnostics_capabilities ?? NO_DIAGNOSTICS_CAPABILITIES;
  const facilities = meta?.facilities ?? [];
  const [q, setQ] = useState("");
  const [includeInactive, setIncludeInactive] = useState(false);
  const [editing, setEditing] = useState<{ entry: Orderable | null } | null>(
    null,
  );
  const [historyFor, setHistoryFor] = useState<string | null>(null);
  const [deactivating, setDeactivating] = useState<{
    entry: Orderable;
    reason: string;
  } | null>(null);
  const { busy, message, run } = useAction(lang);
  const [tick, setTick] = useState(0);
  const state = useLoader(
    () =>
      searchCatalog({
        q: q.trim() || undefined,
        include_inactive: includeInactive || undefined,
        limit: 200,
      }),
    `${q}:${includeInactive}:${tick}`,
    caps.can_manage_catalog,
  );
  const reload = () => setTick((n) => n + 1);

  if (meta && !caps.can_manage_catalog) {
    return (
      <div className="card">
        <p role="alert" className="error">
          {t(lang, "unauthorizedPanel")}
        </p>
        <Link href="/diagnostics">{t(lang, "navDiagnostics")}</Link>
      </div>
    );
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
    if (ok) reload();
  }

  return (
    <div className="dx-catalog" data-testid="dx-catalog">
      <header className="page-head">
        <div>
          <h2 style={{ marginBottom: 4 }}>{t(lang, "dxCatalogAdmin")}</h2>
          <p className="muted">{t(lang, "dxCatalogIntro")}</p>
        </div>
        <button
          type="button"
          className="primary"
          onClick={() => setEditing({ entry: null })}
          data-testid="dx-cat-add"
        >
          {t(lang, "dxAddOrderable")}
        </button>
      </header>
      <div className="filters grid-2">
        <label>
          {t(lang, "search")}
          <input
            value={q}
            onChange={(e) => setQ(e.target.value)}
            data-testid="dx-cat-search"
          />
        </label>
        <label className="check-option">
          <input
            type="checkbox"
            checked={includeInactive}
            onChange={(e) => setIncludeInactive(e.target.checked)}
          />
          {t(lang, "includeInactive")}
        </label>
      </div>
      {editing ? (
        <OrderableEditor
          lang={lang}
          entry={editing.entry}
          facilities={facilities}
          onSaved={() => {
            setEditing(null);
            reload();
          }}
          onCancel={() => setEditing(null)}
        />
      ) : null}
      {deactivating ? (
        <ConfirmBox
          lang={lang}
          title={`${t(lang, "deactivate")} · ${deactivating.entry.name}`}
          onConfirm={() => void deactivate()}
          onCancel={() => setDeactivating(null)}
          busy={busy}
          disabled={deactivating.reason.trim().length < 3}
        >
          <ReasonField
            id="dx-cat-deactivate-reason"
            lang={lang}
            value={deactivating.reason}
            onChange={(v) => setDeactivating({ ...deactivating, reason: v })}
            required
            label={t(lang, "changeReason")}
          />
        </ConfirmBox>
      ) : null}
      <MessageLine message={message} />
      <PanelState
        lang={lang}
        state={state}
        emptyKey="dxNoOrderables"
        isEmpty={(d) => d.items.length === 0}
      >
        {(d) => (
          <ul className="row-list" data-testid="dx-cat-rows">
            {d.items.map((o) => (
              <li
                key={o.id}
                className="row-card"
                data-testid="dx-cat-row"
                data-code={o.code}
              >
                <div className="row-main">
                  <div className="row-head">
                    <strong>{o.name}</strong>
                    <code>{o.code}</code>
                    <StatusBadge
                      label={categoryLabel(lang, o.category_code)}
                      tone="neutral"
                    />
                    {!o.active ? (
                      <StatusBadge label={t(lang, "inactive")} tone="warn" />
                    ) : null}
                    <span className="muted small">v{o.version}</span>
                  </div>
                  <p className="muted">
                    {o.fulfilment_modes
                      .map((m) => fulfilmentModeLabel(lang, m))
                      .join(", ")}
                    {o.specimen ? ` · ${o.specimen.type_code}` : ""}
                    {o.components.length > 0
                      ? ` · ${o.components.length} ${t(lang, "dxComponents").toLowerCase()}`
                      : ""}
                    {o.safety_rules.length > 0
                      ? ` · ${o.safety_rules.length} ${t(lang, "dxSafetyRules").toLowerCase()}`
                      : ""}
                  </p>
                  <div className="visit-actions">
                    <button
                      type="button"
                      className="secondary"
                      onClick={() => setEditing({ entry: o })}
                      data-testid="dx-cat-edit"
                    >
                      {t(lang, "edit")}
                    </button>
                    <button
                      type="button"
                      className="tertiary"
                      onClick={() =>
                        setHistoryFor(historyFor === o.id ? null : o.id)
                      }
                      aria-expanded={historyFor === o.id}
                      data-testid="dx-cat-history-toggle"
                    >
                      {t(lang, "history")}
                    </button>
                    {o.active ? (
                      <button
                        type="button"
                        className="tertiary"
                        onClick={() =>
                          setDeactivating({ entry: o, reason: "" })
                        }
                        data-testid="dx-cat-deactivate"
                      >
                        {t(lang, "deactivate")}
                      </button>
                    ) : null}
                  </div>
                  {historyFor === o.id ? (
                    <History lang={lang} id={o.id} />
                  ) : null}
                </div>
              </li>
            ))}
          </ul>
        )}
      </PanelState>
    </div>
  );
}

export default function DiagnosticCatalogPage() {
  return (
    <AppShell>
      <CatalogAdmin />
    </AppShell>
  );
}
