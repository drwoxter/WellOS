import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import RiskPage from "@/app/risk/page";
import Patient360Page from "@/app/patients/[id]/360/page";
import { CockpitRisk } from "@/app/risk/cockpit-risk";
import { RiskSummaryPanel } from "@/app/risk/risk-view";
import { SessionProvider } from "@/lib/session";
import {
  AI_READY,
  DISABLED,
  DEGRADED,
  capabilities,
} from "./fixtures/capabilities";
import { routeParams } from "./fixtures/params";
import {
  canReadRisk,
  evidenceHref,
  levelGlyph,
  levelKey,
  sortDomains,
} from "@/lib/risk";
import type {
  RiskAssessment,
  RiskSection,
  RiskSummary,
  Worklist,
  WorklistItem,
} from "@/lib/risk";

const push = vi.fn();
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push, replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => "/risk",
  useParams: () => ({ id: "p1" }),
}));

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function meta(roles: string[]) {
  return {
    tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
    user: { username: "dr.garcia", display_name: "Dr. García", roles },
    facilities: [
      {
        id: "f1",
        name: "Central Hospital",
        accessible: true,
        can_register: false,
        can_act_clinically: roles.includes("physician"),
      },
    ],
    ai_capabilities: AI_READY,
  };
}

const NOW = "2026-09-14T08:00:00Z";
const REVIEW = { status: "unreviewed" as const };

const CRITICAL_DOMAIN = {
  domain: "diagnostic_result" as const,
  level: "critical" as const,
  factors: [
    {
      code: "critical_result_unreviewed",
      level: "critical" as const,
      detail: "Potassium 6.9 mmol/L",
      evidence: [
        {
          record_type: "observation",
          record_id: "obs-1",
          label: "Potassium 6.9 mmol/L",
          observed_at: NOW,
        },
      ],
      detected_at: NOW,
    },
  ],
  missing_data: [],
  stale_data: [],
  trend: "worsening" as const,
  detected_at: NOW,
  calculated_at: NOW,
  rules_version: "risk-rules.v1",
  review: REVIEW,
};

const LOW_DOMAIN = (domain: RiskAssessment["domains"][number]["domain"]) => ({
  domain,
  level: "low" as const,
  factors: [],
  missing_data: [],
  stale_data: [],
  trend: "stable" as const,
  detected_at: null,
  calculated_at: NOW,
  rules_version: "risk-rules.v1",
  review: REVIEW,
});

const ASSESSMENT: RiskAssessment = {
  id: "as-1",
  rules_version: "risk-rules.v1",
  calculated_at: NOW,
  calculated_by: null,
  trigger: "seed",
  overall_level: "critical",
  safety_floor: "critical",
  trend: "worsening",
  domains: [
    CRITICAL_DOMAIN,
    LOW_DOMAIN("acute_safety"),
    LOW_DOMAIN("chronic_complexity"),
    LOW_DOMAIN("medication_allergy_safety"),
    {
      ...LOW_DOMAIN("preventive_care"),
      level: "moderate",
      factors: [
        {
          code: "hba1c_overdue",
          level: "moderate",
          detail: null,
          evidence: [],
          detected_at: NOW,
        },
      ],
      missing_data: [
        {
          code: "lipid_missing",
          record_type: "observation",
          record_id: null,
          observed_at: null,
          max_age_days: 365,
        },
      ],
    },
    LOW_DOMAIN("access_utilization"),
    LOW_DOMAIN("care_coordination"),
  ],
  review: REVIEW,
};

const SUMMARY: RiskSummary = {
  id: "sum-1",
  status: "approved",
  template: "risk-summary@1.0.0",
  model: "dmind-fake",
  model_version: "1.0.0",
  prompt_version: "risk-summary-prompt.v1",
  route: "offline",
  generated_at: NOW,
  reviewer: "Dr. García",
  reviewed_at: NOW,
  review_decision: "approve",
  review_note: null,
  confirmed_tasks: 0,
  output: {
    schema_version: "risk-summary.v1",
    ai_generated: true,
    rules_version: "risk-rules.v1",
    overall_level: "critical",
    safety_floor: "critical",
    raised_to_floor: false,
    domains: [
      {
        domain: "diagnostic_result",
        level: "critical",
        summary: "A critical potassium result is awaiting review.",
        reasons: ["critical_result_unreviewed"],
        cited_sources: ["observation:obs-1"],
      },
    ],
    missing_information: ["No lipid screening on record"],
    contradictions: [],
    follow_up_suggestions: [
      {
        category: "review",
        domain: "diagnostic_result",
        text: "Review the critical potassium result.",
        requires_confirmation: true,
      },
    ],
    limitations: ["Does not diagnose or prescribe."],
    cited_sources: ["observation:obs-1"],
    confidence: "high",
  },
};

function section(overrides: Partial<RiskSection> = {}): RiskSection {
  return {
    rules_version: "risk-rules.v1",
    current: ASSESSMENT,
    history: [
      {
        id: "as-1",
        calculated_at: NOW,
        overall_level: "critical",
        trend: "worsening",
        rules_version: "risk-rules.v1",
        trigger: "seed",
        is_current: true,
        domain_levels: { diagnostic_result: "critical" },
      },
    ],
    summary: null,
    follow_up_owner: null,
    professionals: [{ id: "u-nurse", display_name: "Nurse Ana Kim" }],
    capabilities: {
      can_recalculate: true,
      can_acknowledge: true,
      can_review: true,
      can_assign: true,
    },
    ...overrides,
  };
}

function item(overrides: Partial<WorklistItem> = {}): WorklistItem {
  return {
    assessment_id: "as-1",
    patient: {
      id: "p1",
      family_name: "Riskdemo",
      given_name: "Teresa",
      identifier: "SYN-0103",
      birth_date: "1958-03-04",
      facility_id: "f1",
      facility: "Central Hospital",
    },
    overall_level: "critical",
    focus_level: "critical",
    focus_domain: "overall",
    trend: "worsening",
    domain_levels: { diagnostic_result: "critical", preventive_care: "low" },
    explained: [
      {
        domain: "diagnostic_result",
        level: "critical",
        trend: "worsening",
        detected_at: NOW,
        factors: CRITICAL_DOMAIN.factors,
        missing_data: [],
        stale_data: [],
      },
    ],
    calculated_at: NOW,
    rules_version: "risk-rules.v1",
    service: "general_medicine",
    owner: null,
    treating_professional: "Dr. García",
    review: REVIEW,
    capabilities: {
      can_acknowledge: true,
      can_assign: true,
      can_review: true,
      can_open_360: true,
    },
    ...overrides,
  };
}

function worklist(items: WorklistItem[]): Worklist {
  return {
    items,
    services: ["general_medicine"],
    professionals: [{ id: "u-nurse", display_name: "Nurse Ana Kim" }],
    domains: [
      "acute_safety",
      "chronic_complexity",
      "medication_allergy_safety",
      "diagnostic_result",
      "preventive_care",
      "access_utilization",
      "care_coordination",
    ],
    rules_version: "risk-rules.v1",
    limit: 200,
  };
}

function patient360(risk: RiskSection | null = section()) {
  return {
    patient: {
      id: "p1",
      facility_id: "f1",
      family_name: "Riskdemo",
      given_name: "Teresa",
      birth_date: "1958-03-04",
      sex: "female",
      identifier: "SYN-0103",
    },
    allergies: [{ substance: "Penicillin", criticality: "high" }],
    medications: [{ name: "Metformin 850 mg", status: "active" }],
    conditions: [
      { code: "E11", display: "Type 2 diabetes mellitus", status: "active" },
      { code: "J45", display: "Asthma", status: "resolved" },
    ],
    service_requests: [],
    encounters: [
      {
        id: "enc-1",
        status: "completed",
        encounter_type: "consultation",
        started_at: "2026-09-01T09:00:00Z",
        completed_at: "2026-09-01T09:30:00Z",
        practitioner: "Dr. García",
        own: true,
        note_status: "signed",
        addenda_count: 0,
      },
    ],
    alerts: [
      { severity: "critical", message: "Critical potassium", created_at: NOW },
    ],
    visit: null,
    brief: {
      open_tasks: [
        {
          id: "task-1",
          description: "Review critical potassium",
          priority: "high",
          status: "open",
          due_at: NOW,
          created_at: NOW,
          service_request_id: "sr-1",
          source: "result_loop",
        },
      ],
      open_requests: [],
      recent_abnormal: [
        {
          id: "obs-1",
          service_request_id: "sr-1",
          code_loinc: "2823-3",
          display: "Potassium",
          value: "6.9",
          unit: "mmol/L",
          reference_range: "3.5-5.1 mmol/L",
          abnormal: "high",
          critical: true,
          effective_at: NOW,
        },
      ],
      recent_notes: [
        {
          encounter_id: "enc-1",
          signed_at: "2026-09-01T09:30:00Z",
          author: "Dr. García",
          reason_for_encounter: "Diabetes follow-up",
          assessment: "Stable glycaemic control.",
        },
      ],
      alerts: [],
      allergies: [],
      conditions: [],
      medications: [],
    },
    diagnostics: { tests: [], generated_at: NOW },
    care_team: [
      {
        id: "ct-1",
        function: "treating_professional",
        source: "consultation",
        since: "2026-09-01T09:00:00Z",
        assignee_user_id: "u-garcia",
        assignee: "Dr. García",
        queue: null,
      },
    ],
    responsible_professional: {
      assignee: "Dr. García",
      since: "2026-09-01T09:00:00Z",
    },
    internal_alerts: [],
    risk,
    capabilities: {
      can_start_consultation: true,
      open_consultation_id: null as string | null,
      can_view_risk: risk !== null,
    },
    generated_at: NOW,
  };
}

type Handler = (
  url: string,
  init: RequestInit | undefined,
) => Response | undefined;

function mockFetch(options: { roles?: string[]; handler?: Handler } = {}) {
  const posts: { url: string; body: unknown }[] = [];
  const gets: string[] = [];
  const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (init?.method === "POST")
      posts.push({ url, body: JSON.parse(String(init.body ?? "null")) });
    else gets.push(url);
    const custom = options.handler?.(url, init);
    if (custom) return Promise.resolve(custom);
    if (url === "/api/session")
      return Promise.resolve(jsonResponse({ authenticated: true }));
    if (url === "/api/v1/meta/tenant")
      return Promise.resolve(
        jsonResponse(meta(options.roles ?? ["physician"])),
      );
    return Promise.resolve(jsonResponse({ ok: true }));
  });
  vi.stubGlobal("fetch", fetchMock);
  return { posts, gets };
}

beforeEach(() => {
  localStorage.clear();
  push.mockReset();
});

describe("risk helpers", () => {
  it("only clinical roles may read risk", () => {
    expect(canReadRisk(["physician"])).toBe(true);
    expect(canReadRisk(["nurse"])).toBe(true);
    expect(canReadRisk(["registration_staff"])).toBe(false);
    expect(canReadRisk([])).toBe(false);
  });

  it("levels have a label and a glyph so colour is never the only cue", () => {
    for (const level of [
      "insufficient_data",
      "low",
      "moderate",
      "high",
      "critical",
    ]) {
      expect(levelGlyph(level)).not.toBe("");
      expect(levelKey(level)).toMatch(/^riskLevel/);
    }
  });

  it("routes evidence to the WellOS source record", () => {
    const ref = (record_type: string, record_id = "x") => ({
      record_type,
      record_id,
      label: "",
      observed_at: null,
    });
    expect(evidenceHref(ref("encounter", "e1"), "p1")).toBe("/encounters/e1");
    expect(evidenceHref(ref("service_request", "s1"), "p1")).toBe(
      "/requests/s1",
    );
    expect(evidenceHref(ref("observation", "o1"), "p1", { o1: "s9" })).toBe(
      "/requests/s9",
    );
    expect(evidenceHref(ref("observation", "o1"), "p1")).toBe(
      "/patients/p1/360#results",
    );
    expect(evidenceHref(ref("alert"), "p1")).toBe("/patients/p1/360#alerts");
    expect(evidenceHref(ref("medication"), "p1")).toBe(
      "/patients/p1/360#medications",
    );
    expect(evidenceHref(ref("unknown"), "p1")).toBeNull();
  });

  it("sorts domains most severe first", () => {
    const sorted = sortDomains([
      { domain: "acute_safety", level: "low" },
      { domain: "care_coordination", level: "critical" },
      { domain: "preventive_care", level: "insufficient_data" },
      { domain: "chronic_complexity", level: "high" },
    ]);
    expect(sorted.map((d) => d.level)).toEqual([
      "critical",
      "high",
      "low",
      "insufficient_data",
    ]);
  });
});

describe("risk worklist", () => {
  const worklistCards = () =>
    Array.from(document.querySelectorAll<HTMLElement>("ul.risk-worklist > li"));

  function renderWorklist(
    options: { roles?: string[]; handler?: Handler } = {},
  ) {
    const mocks = mockFetch(options);
    render(
      <SessionProvider>
        <RiskPage />
      </SessionProvider>,
    );
    return mocks;
  }

  it("shows the loading state and then items in the server's order", async () => {
    const items = [
      item(),
      item({
        assessment_id: "as-2",
        overall_level: "high",
        focus_level: "high",
        patient: {
          ...item().patient,
          id: "p2",
          given_name: "Hugo",
          identifier: "SYN-0102",
        },
      }),
    ];
    renderWorklist({
      handler: (url) =>
        url.startsWith("/api/v1/risk/worklist")
          ? jsonResponse(worklist(items))
          : undefined,
    });
    expect(await screen.findByRole("status")).toHaveTextContent("Loading");
    await screen.findByText("Teresa Riskdemo");
    const cards = worklistCards();
    expect(cards).toHaveLength(2);
    expect(cards[0]).toHaveTextContent("Teresa Riskdemo");
    expect(cards[0]).toHaveAttribute("data-level", "critical");
    expect(cards[0]).toHaveTextContent(
      "A critical result has not been reviewed yet",
    );
    expect(
      within(cards[0]).getByRole("link", { name: "Open Patient 360" }),
    ).toHaveAttribute("href", "/patients/p1/360");
    expect(cards[1]).toHaveTextContent("Hugo Riskdemo");
    expect(screen.getAllByText(/risk-rules\.v1/).length).toBeGreaterThan(0);
  });

  it("technical evidence is collapsed by default and links to the source record", async () => {
    renderWorklist({
      handler: (url) =>
        url.startsWith("/api/v1/risk/worklist")
          ? jsonResponse(worklist([item()]))
          : undefined,
    });
    await screen.findByText("Teresa Riskdemo");
    const card = worklistCards()[0];
    const details = card.querySelector("details.risk-technical");
    expect(details).not.toBeNull();
    expect(details).not.toHaveAttribute("open");
    await userEvent.click(within(card).getAllByText("Technical evidence")[0]);
    expect(
      within(card).getByRole("link", { name: /Potassium 6\.9 mmol\/L/ }),
    ).toHaveAttribute("href", "/patients/p1/360#results");
    expect(within(card).getByText(/Rules version/)).toBeInTheDocument();
  });

  it("filters are sent to the server and reset restores the full list", async () => {
    const { gets } = renderWorklist({
      handler: (url) =>
        url.startsWith("/api/v1/risk/worklist")
          ? jsonResponse(
              url.includes("trend=worsening")
                ? worklist([item()])
                : worklist([]),
            )
          : undefined,
    });
    expect(
      await screen.findByText(/No patient currently has an elevated risk/),
    ).toBeInTheDocument();
    await userEvent.selectOptions(screen.getByLabelText("Trend"), "worsening");
    expect(await screen.findByText("Teresa Riskdemo")).toBeInTheDocument();
    await userEvent.selectOptions(
      screen.getByLabelText("Domain"),
      "diagnostic_result",
    );
    await waitFor(() =>
      expect(
        gets.some(
          (u) =>
            u.includes("domain=diagnostic_result") &&
            u.includes("trend=worsening"),
        ),
      ).toBe(true),
    );
    await userEvent.click(
      screen.getByRole("button", { name: "Reset filters" }),
    );
    await waitFor(() =>
      expect(gets[gets.length - 1]).toBe("/api/v1/risk/worklist"),
    );
  });

  it("acknowledge posts the displayed assessment and reloads", async () => {
    let review: WorklistItem["review"] = { status: "unreviewed" };
    const { posts } = renderWorklist({
      handler: (url, init) => {
        if (url.startsWith("/api/v1/risk/worklist"))
          return jsonResponse(worklist([item({ review })]));
        if (url.endsWith("/risk/acknowledge") && init?.method === "POST") {
          review = { status: "acknowledged", reviewer: "Dr. García" };
          return jsonResponse({ ok: true });
        }
        return undefined;
      },
    });
    await userEvent.type(
      await screen.findByLabelText("Review note (optional)"),
      "Seen",
    );
    await userEvent.click(screen.getByRole("button", { name: "Acknowledge" }));
    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0]).toEqual({
      url: "/api/v1/patients/p1/risk/acknowledge",
      body: { assessment_id: "as-1", domain: "overall", note: "Seen" },
    });
    expect(await screen.findByText("Risk acknowledged.")).toBeInTheDocument();
    await waitFor(() =>
      expect(
        screen.queryByRole("button", { name: "Acknowledge" }),
      ).not.toBeInTheDocument(),
    );
    expect(screen.getByText("Acknowledged")).toBeInTheDocument();
  });

  it("assign requires choosing a professional and posts the assignee", async () => {
    const { posts } = renderWorklist({
      handler: (url) =>
        url.startsWith("/api/v1/risk/worklist")
          ? jsonResponse(worklist([item()]))
          : undefined,
    });
    await userEvent.click(
      await screen.findByRole("button", { name: "Assign follow-up" }),
    );
    const confirm = screen.getByRole("button", { name: "Confirm assignment" });
    expect(confirm).toBeDisabled();
    await userEvent.selectOptions(
      screen.getByLabelText("Assign to"),
      "u-nurse",
    );
    await userEvent.click(confirm);
    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0]).toEqual({
      url: "/api/v1/patients/p1/risk/assign",
      body: { assignee_user_id: "u-nurse" },
    });
  });

  it("explains insufficient data instead of showing it as low risk", async () => {
    renderWorklist({
      handler: (url) =>
        url.startsWith("/api/v1/risk/worklist")
          ? jsonResponse(
              worklist([
                item({
                  overall_level: "insufficient_data",
                  focus_level: "insufficient_data",
                  trend: "unknown",
                  domain_levels: {},
                  explained: [],
                }),
              ]),
            )
          : undefined,
    });
    await screen.findByText("Teresa Riskdemo");
    const card = worklistCards()[0];
    expect(card).toHaveAttribute("data-level", "insufficient_data");
    expect(card).toHaveTextContent("Insufficient data");
    expect(card).toHaveTextContent(/the record is too sparse to assess/);
    expect(card).not.toHaveTextContent(/\bLow\b/);
  });

  it("shows a recoverable error state", async () => {
    let fail = true;
    renderWorklist({
      handler: (url) => {
        if (!url.startsWith("/api/v1/risk/worklist")) return undefined;
        if (fail) {
          fail = false;
          return jsonResponse(
            { error: { code: "internal_error", message: "boom" } },
            500,
          );
        }
        return jsonResponse(worklist([item()]));
      },
    });
    expect(await screen.findByRole("alert")).toHaveTextContent("boom");
    await userEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText("Teresa Riskdemo")).toBeInTheDocument();
  });

  it("is not offered to roles without risk permissions", async () => {
    const { gets } = renderWorklist({ roles: ["registration_staff"] });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "You do not have permission to view this information.",
    );
    expect(gets.some((u) => u.startsWith("/api/v1/risk"))).toBe(false);
  });

  it("renders in Spanish", async () => {
    localStorage.setItem("wellos.lang", "es");
    renderWorklist({
      handler: (url) =>
        url.startsWith("/api/v1/risk/worklist")
          ? jsonResponse(worklist([item()]))
          : undefined,
    });
    expect(
      (
        await screen.findAllByRole("heading", {
          name: "Lista de trabajo de riesgo",
        })
      ).length,
    ).toBeGreaterThan(0);
    expect(await screen.findByText("Abrir Paciente 360")).toBeInTheDocument();
    expect(screen.getAllByText("Crítico").length).toBeGreaterThan(0);
  });
});

describe("Patient 360", () => {
  function render360(
    data: ReturnType<typeof patient360>,
    options: { roles?: string[]; handler?: Handler } = {},
  ) {
    const mocks = mockFetch({
      ...options,
      handler: (url, init) =>
        options.handler?.(url, init) ??
        (url.startsWith("/api/v1/patients/p1/360")
          ? jsonResponse(data)
          : undefined),
    });
    render(
      <SessionProvider>
        <Patient360Page params={routeParams({ id: "p1" })} />
      </SessionProvider>,
    );
    return mocks;
  }

  it("shows the essential sections without navigation", async () => {
    render360(patient360());
    expect(
      await screen.findByRole("heading", { name: "Teresa Riskdemo" }),
    ).toBeInTheDocument();
    for (const name of [
      "Unresolved safety issues",
      "Care team",
      "Current risk",
      "Conditions and history",
      "Allergies and medication safety",
      "Recent encounters and notes",
      "Pending tasks, tests and follow-ups",
      "Diagnostic-result trends",
      "Preventive-care gaps",
      "Risk evolution",
    ]) {
      expect(
        screen.getByRole("region", { name: new RegExp(`^${name}`) }),
      ).toBeInTheDocument();
    }
    expect(screen.getByText("Critical potassium")).toBeInTheDocument();
    expect(screen.getAllByText("Dr. García").length).toBeGreaterThan(0);
    expect(screen.getByText("Type 2 diabetes mellitus")).toBeInTheDocument();
    const meds = screen.getByRole("region", {
      name: /^Allergies and medication safety/,
    });
    expect(within(meds).getByText("Penicillin")).toBeInTheDocument();
    expect(within(meds).getByText("Metformin 850 mg")).toBeInTheDocument();
    expect(screen.getByText("Stable glycaemic control.")).toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "Review critical potassium" }),
    ).toHaveAttribute("href", "/requests/sr-1");
    expect(screen.getByRole("link", { name: "Potassium" })).toHaveAttribute(
      "href",
      "/requests/sr-1",
    );
    const gaps = screen.getByRole("region", { name: /^Preventive-care gaps/ });
    expect(
      within(gaps).getByText("No HbA1c within the expected interval"),
    ).toBeInTheDocument();
    expect(within(gaps).getByText(/Lipid screening/)).toBeInTheDocument();
    // Risk: overall level with glyph + label, deterministic floor, domains.
    const risk = screen.getByRole("region", { name: /^Current risk/ });
    expect(within(risk).getAllByText("Critical").length).toBeGreaterThan(0);
    expect(within(risk).getByText(/Deterministic floor/)).toBeInTheDocument();
    expect(within(risk).getAllByText(/Worsening/).length).toBeGreaterThan(0);
    expect(
      screen.getByRole("heading", { name: "dMind risk summary" }),
    ).toBeInTheDocument();
    expect(screen.getByText("AI-generated")).toBeInTheDocument();
  });

  it("starts a consultation directly from the header", async () => {
    const { posts } = render360(patient360(), {
      handler: (url, init) =>
        url === "/api/v1/encounters" && init?.method === "POST"
          ? jsonResponse({ id: "enc-new" })
          : undefined,
    });
    await userEvent.click(
      await screen.findByRole("button", { name: "Start consultation" }),
    );
    await waitFor(() =>
      expect(push).toHaveBeenCalledWith("/encounters/enc-new"),
    );
    expect(posts[0]).toEqual({
      url: "/api/v1/encounters",
      body: { patient_id: "p1" },
    });
  });

  it("offers to resume an open consultation instead of starting another", async () => {
    const data = patient360();
    data.capabilities.open_consultation_id = "enc-open";
    render360(data);
    expect(
      await screen.findByRole("link", { name: "Resume consultation" }),
    ).toHaveAttribute("href", "/encounters/enc-open");
    expect(
      screen.queryByRole("button", { name: "Start consultation" }),
    ).not.toBeInTheDocument();
  });

  it("explains when no assessment exists and when risk is not visible", async () => {
    render360(patient360(section({ current: null, history: [] })));
    expect(
      (
        await screen.findAllByText(
          "No risk assessment has been calculated yet.",
        )
      ).length,
    ).toBeGreaterThan(0);
    expect(
      screen.getByRole("button", { name: "Recalculate risk" }),
    ).toBeInTheDocument();
  });

  it("hides risk for users without risk permission but keeps the record", async () => {
    render360(patient360(null), { roles: ["registration_staff"] });
    expect(
      await screen.findByRole("heading", { name: "Teresa Riskdemo" }),
    ).toBeInTheDocument();
    expect(
      screen.getAllByText(
        "You are not authorized to view risk for this patient.",
      ).length,
    ).toBeGreaterThan(0);
    expect(
      screen.queryByRole("heading", { name: "dMind risk summary" }),
    ).not.toBeInTheDocument();
  });

  it("recalculates on request and reloads", async () => {
    const { posts, gets } = render360(patient360());
    await userEvent.click(
      await screen.findByRole("button", { name: "Recalculate risk" }),
    );
    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0].url).toBe("/api/v1/patients/p1/risk/recalculate");
    expect(await screen.findByText("Risk recalculated.")).toBeInTheDocument();
    await waitFor(() =>
      expect(
        gets.filter((u) => u.startsWith("/api/v1/patients/p1/360")).length,
      ).toBe(2),
    );
  });

  it("shows unauthorized for a 403", async () => {
    render360(patient360(), {
      handler: (url) =>
        url.startsWith("/api/v1/patients/p1/360")
          ? jsonResponse({ error: { code: "forbidden", message: "no" } }, 403)
          : undefined,
    });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "You do not have permission to view this information.",
    );
  });
});

describe("dMind risk summary panel", () => {
  function renderPanel(risk: RiskSection) {
    const mocks = mockFetch();
    const onChanged = vi.fn(async () => undefined);
    render(
      <RiskSummaryPanel
        lang="en"
        patientId="p1"
        risk={risk}
        capabilities={AI_READY}
        onChanged={onChanged}
      />,
    );
    return { ...mocks, onChanged };
  }

  it("generates a summary bound to the displayed assessment", async () => {
    const { posts } = renderPanel(section());
    await userEvent.click(
      screen.getByRole("button", { name: "Generate dMind summary" }),
    );
    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0]).toEqual({
      url: "/api/v1/patients/p1/risk/summary",
      body: { language: "en", assessment_id: "as-1" },
    });
  });

  it("labels AI output and requires approval before any suggestion can become work", async () => {
    renderPanel(
      section({ summary: { ...SUMMARY, status: "awaiting_review" } }),
    );
    expect(screen.getByText("AI-generated")).toBeInTheDocument();
    expect(screen.getByText("Requires confirmation")).toBeInTheDocument();
    expect(
      screen.getByText("A critical potassium result is awaiting review."),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/No lipid screening on record/),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Create follow-up task…" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Approve summary" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Reject summary" }),
    ).toBeInTheDocument();
  });

  it("creates a task only after an explicit confirmation", async () => {
    const { posts, onChanged } = renderPanel(section({ summary: SUMMARY }));
    await userEvent.click(
      screen.getByRole("button", { name: "Create follow-up task…" }),
    );
    expect(posts).toHaveLength(0);
    await userEvent.selectOptions(screen.getByLabelText("Priority"), "urgent");
    await userEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(posts).toHaveLength(0);
    await userEvent.click(
      screen.getByRole("button", { name: "Create follow-up task…" }),
    );
    await userEvent.click(
      screen.getByRole("button", { name: "Confirm and create task" }),
    );
    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0]).toEqual({
      url: "/api/v1/patients/p1/risk/summary/sum-1/confirm",
      body: { suggestion_index: 0, priority: "urgent", due_in_days: 14 },
    });
    expect(
      (await screen.findAllByText("Follow-up task created.")).length,
    ).toBeGreaterThan(0);
    expect(onChanged).toHaveBeenCalled();
  });

  it("never lets an AI level appear below the deterministic floor", () => {
    renderPanel(
      section({
        summary: {
          ...SUMMARY,
          output: { ...SUMMARY.output, raised_to_floor: true },
        },
      }),
    );
    expect(
      screen.getByText(/raised to match the deterministic assessment/),
    ).toBeInTheDocument();
    expect(screen.getByText(/Deterministic floor/)).toHaveTextContent(
      "Critical",
    );
  });

  it("marks a summary for an older assessment as no longer actionable", () => {
    renderPanel(section({ summary: { ...SUMMARY, status: "superseded" } }));
    expect(screen.getByText(/no longer actionable/)).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Create follow-up task…" }),
    ).not.toBeInTheDocument();
  });

  it("keeps an approved summary visible after risk was recalculated", () => {
    renderPanel(
      section({
        summary: {
          ...SUMMARY,
          assessment_id: "as-0",
          for_current_assessment: false,
          confirmed_tasks: 1,
        },
      }),
    );
    expect(screen.getByRole("note")).toHaveTextContent(
      /explains an earlier assessment/,
    );
    expect(
      screen.getByText("A critical potassium result is awaiting review."),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Create follow-up task…" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Generate a new summary" }),
    ).toBeInTheDocument();
  });
});

describe("consultation cockpit risk", () => {
  it("shows the elevated domains and re-reads after a confirmed change", async () => {
    let calculatedAt = NOW;
    const { gets } = mockFetch({
      handler: (url) =>
        url === "/api/v1/patients/p1/risk"
          ? jsonResponse(
              section({
                current: { ...ASSESSMENT, calculated_at: calculatedAt },
              }),
            )
          : undefined,
    });
    const { rerender } = render(
      <CockpitRisk
        lang="en"
        patientId="p1"
        refreshKey="0:0::in_progress"
        capabilities={AI_READY}
      />,
    );
    expect(
      await screen.findByRole("heading", {
        name: "Patient 360 before you start",
      }),
    ).toBeInTheDocument();
    expect(await screen.findByText("Diagnostic results")).toBeInTheDocument();
    expect(screen.getByText("Preventive care")).toBeInTheDocument();
    expect(screen.queryByText("Acute safety")).not.toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: "Open Patient 360" }),
    ).toHaveAttribute("href", "/patients/p1/360");
    expect(
      screen.queryByText("Risk updated after the last confirmed change."),
    ).not.toBeInTheDocument();

    calculatedAt = "2026-09-14T08:05:00Z";
    rerender(
      <CockpitRisk
        lang="en"
        patientId="p1"
        refreshKey="1:0::in_progress"
        capabilities={AI_READY}
      />,
    );
    expect(
      await screen.findByText("Risk updated after the last confirmed change."),
    ).toBeInTheDocument();
    expect(gets.filter((u) => u === "/api/v1/patients/p1/risk")).toHaveLength(
      2,
    );
  });

  it("renders nothing when the clinician may not read risk", async () => {
    mockFetch({
      handler: (url) =>
        url === "/api/v1/patients/p1/risk"
          ? jsonResponse({ error: { code: "forbidden", message: "no" } }, 403)
          : undefined,
    });
    render(
      <CockpitRisk
        lang="en"
        patientId="p1"
        refreshKey="k"
        capabilities={AI_READY}
      />,
    );
    await waitFor(() =>
      expect(
        screen.queryByRole("heading", { name: "Patient 360 before you start" }),
      ).not.toBeInTheDocument(),
    );
  });

  it("can be collapsed and shows a retry on error", async () => {
    let fail = true;
    mockFetch({
      handler: (url) => {
        if (url !== "/api/v1/patients/p1/risk") return undefined;
        if (fail) {
          fail = false;
          return jsonResponse(
            { error: { code: "internal_error", message: "down" } },
            500,
          );
        }
        return jsonResponse(section());
      },
    });
    render(
      <CockpitRisk
        lang="en"
        patientId="p1"
        refreshKey="k"
        capabilities={AI_READY}
      />,
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /Patient 360 is unavailable/,
    );
    await userEvent.click(screen.getByRole("button", { name: "Try again" }));
    expect(await screen.findByText("Diagnostic results")).toBeInTheDocument();
    await userEvent.click(screen.getByRole("button", { name: "Hide" }));
    expect(screen.getByText("Diagnostic results")).not.toBeVisible();
  });

  it("contains a malformed risk payload so the consultation keeps rendering", async () => {
    const consoleError = vi
      .spyOn(console, "error")
      .mockImplementation(() => undefined);
    mockFetch({
      handler: (url) =>
        url === "/api/v1/patients/p1/risk" ? jsonResponse({}) : undefined,
    });
    render(
      <>
        <CockpitRisk
          lang="en"
          patientId="p1"
          refreshKey="k"
          capabilities={AI_READY}
        />
        <p>Consultation note stays here</p>
      </>,
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /Patient 360 is unavailable/,
    );
    expect(screen.getByText("Consultation note stays here")).toBeVisible();
    consoleError.mockRestore();
  });
});
