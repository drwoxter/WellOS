import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import {
  availableTransitions,
  hasDiagnosticsWorkspaceAccess,
  parseValue,
  specimenEventsFor,
  transitionNeedsReason,
  type Orderable,
  type SafetyEvaluation,
  type SafetyFinding,
} from "@/lib/diagnostics";
import type { AiCapabilities } from "@/lib/capabilities";
import {
  canConfirm,
  compositionFor,
  OrderComposer,
} from "@/app/diagnostics/composer";

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => "/encounters/e1",
}));

const ENC = "e1";

function orderable(over: Partial<Orderable> = {}): Orderable {
  return {
    id: "ord-k",
    code: "potassium_serum",
    name: "Potassium, serum",
    name_en: "Potassium, serum",
    name_es: "Potasio sérico",
    synonyms: ["K+"],
    external_codings: null,
    category_code: "laboratory",
    modality_code: null,
    result_type: "quantity",
    components: [],
    panel_member_codes: [],
    specimen: { type_code: "serum", container_code: null, fasting_hours: null },
    preparation: null,
    preparation_en: null,
    preparation_es: null,
    scheduling_service_code: null,
    required_resource_types: [],
    fulfilment_modes: ["immediate", "scheduled"],
    safety_rules: [],
    duplicate_window_days: 2,
    redundant_with_codes: [],
    requires_specimen: true,
    expects_imaging_study: false,
    active: true,
    version: 1,
    facility_ids: ["f1"],
    ...over,
  };
}

function finding(over: Partial<SafetyFinding> = {}): SafetyFinding {
  return {
    id: "w1",
    kind: "duplicate",
    severity: "warning",
    orderable_id: "ord-k",
    orderable_code: "potassium_serum",
    text: "Potassium already resulted 1 day ago",
    text_en: "Potassium already resulted 1 day ago",
    text_es: "Potasio ya resultado hace 1 día",
    evidence: ["observation:o1"],
    answerable: false,
    ...over,
  };
}

function evaluation(findings: SafetyFinding[]): SafetyEvaluation {
  const warnings = findings.filter((f) => f.severity === "warning").length;
  const hardStops = findings.length - warnings;
  return {
    id: "ev-1",
    engine_version: "diagnostic-safety.v1",
    input_hash: "abc",
    evaluated_at: "2026-10-04T10:00:00Z",
    performing_facility_id: "f1",
    candidates: [
      {
        orderable_id: "ord-k",
        code: "potassium_serum",
        name: "Potassium, serum",
        fulfilment_mode: "immediate",
        priority: "routine",
        requested_window_start: null,
        requested_window_end: null,
        requires_appointment: false,
        scheduling_service_code: null,
        needs_specimen: true,
        preparation: null,
      },
    ],
    findings,
    warnings,
    hard_stops: hardStops,
    requires_acknowledgement: warnings > 0,
    requires_override: hardStops > 0,
  };
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

type Call = { url: string; method: string; body: unknown };

function stubApi(opts: {
  findings: SafetyFinding[];
  confirm?: (n: number) => Response;
}) {
  const calls: Call[] = [];
  let confirms = 0;
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      const body = init?.body ? JSON.parse(String(init.body)) : null;
      calls.push({ url, method, body });
      if (url.startsWith("/api/v1/diagnostics/catalog?")) {
        return Promise.resolve(jsonResponse({ items: [orderable()] }));
      }
      if (url === `/api/v1/encounters/${ENC}/diagnostic-orders/preflight`) {
        return Promise.resolve(
          jsonResponse({ evaluation: evaluation(opts.findings) }),
        );
      }
      if (url === `/api/v1/encounters/${ENC}/diagnostic-orders`) {
        confirms += 1;
        if (opts.confirm) return Promise.resolve(opts.confirm(confirms));
        return Promise.resolve(
          jsonResponse({
            group: {
              id: "g1",
              priority: "routine",
              clinical_indication: "Weakness",
              clinical_question: null,
              safety_evaluation_id: "ev-1",
              suggestion_artifact_id: null,
              created_at: "2026-10-04T10:01:00Z",
              orders: [
                {
                  id: "o-1",
                  display: "Potassium, serum",
                  order_status: "placed",
                  fulfilment_mode: "immediate",
                  access_request_id: null,
                  schedule_conflict: null,
                },
              ],
            },
          }),
        );
      }
      return Promise.resolve(jsonResponse({ error: "unexpected" }, 500));
    }),
  );
  return calls;
}

const AI_READY: AiCapabilities = {
  model: {
    state: "ready",
    provider: "fake",
    model: "fake-dmind",
    reason: null,
    external: false,
    synthetic: true,
  },
  transcription: {
    state: "disabled",
    provider: "disabled",
    model: null,
    reason: null,
    external: false,
    synthetic: false,
  },
  structured_note: {
    state: "disabled",
    provider: "disabled",
    model: null,
    reason: null,
    external: false,
    synthetic: false,
  },
  transcription_languages: [],
};

function renderComposer(
  over: { canOverride?: boolean; ai?: AiCapabilities } = {},
) {
  render(
    <OrderComposer
      encounterId={ENC}
      lang="en"
      facilities={[{ id: "f1", name: "Central", can_act_clinically: true }]}
      defaultFacilityId="f1"
      canOverride={over.canOverride ?? false}
      aiCapabilities={over.ai ?? AI_READY}
    />,
  );
}

async function selectPotassium(user: ReturnType<typeof userEvent.setup>) {
  await user.type(screen.getByTestId("dx-search"), "potas");
  await user.click(await screen.findByRole("button", { name: "Add" }));
  expect(screen.getByTestId("dx-items")).toHaveTextContent("Potassium, serum");
}

describe("diagnostics helpers", () => {
  it("gates confirmation on acknowledgement, override permission and indication", () => {
    const warn = finding();
    const stop = finding({
      id: "h1",
      kind: "contraindication",
      severity: "hard_stop",
    });
    expect(canConfirm(null, new Set(), "", true, "x")).toBe(false);
    expect(canConfirm(evaluation([warn]), new Set(), "", true, "x")).toBe(
      false,
    );
    expect(canConfirm(evaluation([warn]), new Set(["w1"]), "", true, "")).toBe(
      false,
    );
    expect(canConfirm(evaluation([warn]), new Set(["w1"]), "", true, "x")).toBe(
      true,
    );
    expect(
      canConfirm(
        evaluation([stop]),
        new Set(),
        "a long enough reason",
        false,
        "x",
      ),
    ).toBe(false);
    expect(canConfirm(evaluation([stop]), new Set(), "short", true, "x")).toBe(
      false,
    );
    expect(
      canConfirm(
        evaluation([stop]),
        new Set(),
        "contrast allergy documented, premedicated",
        true,
        "x",
      ),
    ).toBe(true);
  });

  it("builds the exact server composition from the selection", () => {
    const body = compositionFor(
      [{ orderable: orderable(), fulfilment_mode: "immediate", priority: "" }],
      { q1: true },
      "f1",
      "urgent",
      "es",
    );
    expect(body).toEqual({
      items: [{ orderable_id: "ord-k", fulfilment_mode: "immediate" }],
      answers: { q1: true },
      performing_facility_id: "f1",
      priority: "urgent",
      lang: "es",
    });
  });

  it("exposes only lawful order transitions and reason requirements", () => {
    expect(availableTransitions("on_hold")).toEqual([
      "resume",
      "cancel",
      "enter_in_error",
    ]);
    expect(availableTransitions("completed")).toEqual([]);
    expect(availableTransitions("placed")).not.toContain("start");
    expect(transitionNeedsReason("cancel")).toBe(true);
    expect(transitionNeedsReason("accept")).toBe(false);
    expect(specimenEventsFor("collected")).not.toContain("collected");
  });

  it("parses typed component values strictly", () => {
    expect(parseValue("quantity", "6.9", "mmol/L")).toEqual({
      type: "quantity",
      value: "6.9",
      unit: "mmol/L",
    });
    expect(parseValue("quantity", "six", "mmol/L")).toBeNull();
    expect(parseValue("boolean", "maybe", "")).toBeNull();
    expect(
      parseValue("coded", "http://snomed.info/sct|123|Normal", ""),
    ).toEqual({
      type: "coded",
      system: "http://snomed.info/sct",
      code: "123",
      display: "Normal",
    });
  });

  it("grants the workspace only to readers or reviewers", () => {
    expect(hasDiagnosticsWorkspaceAccess(undefined)).toBe(false);
    expect(
      hasDiagnosticsWorkspaceAccess({
        can_read: false,
        can_order: false,
        can_override_safety: false,
        can_fulfil: false,
        can_handle_specimens: true,
        can_write_reports: false,
        can_review: false,
        can_release: false,
        can_manage_catalog: false,
        self_service: false,
      }),
    ).toBe(false);
  });
});

describe("OrderComposer", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  it("requires every warning to be acknowledged before confirming and binds the evaluation", async () => {
    const calls = stubApi({ findings: [finding()] });
    const user = userEvent.setup();
    renderComposer();
    await selectPotassium(user);
    await user.type(screen.getByTestId("dx-indication"), "Weakness");
    await user.click(screen.getByTestId("dx-preflight"));
    const safety = await screen.findByTestId("dx-safety");
    expect(safety).toHaveTextContent("Potassium already resulted");
    const confirm = screen.getByTestId("dx-confirm");
    expect(confirm).toBeDisabled();
    await user.click(screen.getByRole("checkbox"));
    expect(confirm).toBeEnabled();
    await user.click(confirm);
    await screen.findByTestId("dx-placed");
    const post = calls.find(
      (c) =>
        c.url === `/api/v1/encounters/${ENC}/diagnostic-orders` &&
        c.method === "POST",
    );
    expect(post?.body).toMatchObject({
      safety_evaluation_id: "ev-1",
      acknowledged_ids: ["w1"],
      clinical_indication: "Weakness",
      performing_facility_id: "f1",
      items: [{ orderable_id: "ord-k", fulfilment_mode: "immediate" }],
    });
    expect(
      typeof (post?.body as { idempotency_key: string }).idempotency_key,
    ).toBe("string");
  });

  it("blocks hard stops without override permission and needs a reason with it", async () => {
    const stop = finding({
      id: "h1",
      kind: "contraindication",
      severity: "hard_stop",
      text: "Contrast allergy",
    });
    stubApi({ findings: [stop] });
    const user = userEvent.setup();
    renderComposer({ canOverride: false });
    await selectPotassium(user);
    await user.type(screen.getByTestId("dx-indication"), "Weakness");
    await user.click(screen.getByTestId("dx-preflight"));
    await screen.findByTestId("dx-safety");
    expect(screen.getByRole("alert")).toHaveTextContent("Contrast allergy");
    expect(screen.queryByTestId("dx-override-reason")).not.toBeInTheDocument();
    expect(screen.getByTestId("dx-confirm")).toBeDisabled();
  });

  it("enables an authorised override only with a substantive reason", async () => {
    const stop = finding({
      id: "h1",
      kind: "contraindication",
      severity: "hard_stop",
    });
    const calls = stubApi({ findings: [stop] });
    const user = userEvent.setup();
    renderComposer({ canOverride: true });
    await selectPotassium(user);
    await user.type(screen.getByTestId("dx-indication"), "Weakness");
    await user.click(screen.getByTestId("dx-preflight"));
    await screen.findByTestId("dx-safety");
    const reason = screen.getByTestId("dx-override-reason");
    await user.type(reason, "short");
    expect(screen.getByTestId("dx-confirm")).toBeDisabled();
    await user.type(reason, " but clinically justified");
    expect(screen.getByTestId("dx-confirm")).toBeEnabled();
    await user.click(screen.getByTestId("dx-confirm"));
    await screen.findByTestId("dx-placed");
    const post = calls.find(
      (c) => c.method === "POST" && c.url.endsWith("/diagnostic-orders"),
    );
    expect((post?.body as { override_reason: string }).override_reason).toBe(
      "short but clinically justified",
    );
  });

  it("reuses the idempotency key on retry and drops a stale evaluation on 409", async () => {
    const calls = stubApi({
      findings: [],
      confirm: (n) =>
        n === 1
          ? jsonResponse({ error: "upstream" }, 502)
          : jsonResponse({ error: "safety_evaluation_stale" }, 409),
    });
    const user = userEvent.setup();
    renderComposer();
    await selectPotassium(user);
    await user.type(screen.getByTestId("dx-indication"), "Weakness");
    await user.click(screen.getByTestId("dx-preflight"));
    await screen.findByTestId("dx-safety");
    await user.click(screen.getByTestId("dx-confirm"));
    await waitFor(() => expect(screen.getByRole("alert")).toBeInTheDocument());
    expect(screen.getByTestId("dx-safety")).toBeInTheDocument();
    await user.click(screen.getByTestId("dx-confirm"));
    await waitFor(() =>
      expect(screen.getByRole("alert")).toHaveTextContent(/no longer current/i),
    );
    expect(screen.queryByTestId("dx-safety")).not.toBeInTheDocument();
    const posts = calls.filter(
      (c) => c.method === "POST" && c.url.endsWith("/diagnostic-orders"),
    );
    expect(posts).toHaveLength(2);
    const keys = posts.map(
      (p) => (p.body as { idempotency_key: string }).idempotency_key,
    );
    expect(keys[0]).toBe(keys[1]);
  });

  it("invalidates the evaluation when the selection changes", async () => {
    stubApi({ findings: [] });
    const user = userEvent.setup();
    renderComposer();
    await selectPotassium(user);
    await user.type(screen.getByTestId("dx-indication"), "Weakness");
    await user.click(screen.getByTestId("dx-preflight"));
    await screen.findByTestId("dx-safety");
    await user.click(screen.getByRole("button", { name: /Remove: Potassium/ }));
    expect(screen.queryByTestId("dx-safety")).not.toBeInTheDocument();
    expect(screen.getByTestId("dx-preflight")).toBeDisabled();
  });

  it("keeps dMind suggestions unavailable when the model is disabled", () => {
    stubApi({ findings: [] });
    renderComposer({
      ai: {
        ...AI_READY,
        model: { ...AI_READY.model, state: "disabled", synthetic: false },
      },
    });
    expect(screen.getByTestId("dx-ask-dmind")).toBeDisabled();
    expect(screen.getByRole("status")).toBeInTheDocument();
  });
});
