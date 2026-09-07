import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import TriagePage from "@/app/visits/[id]/triage/page";
import { SessionProvider } from "@/lib/session";

const push = vi.fn();
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push, replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => "/visits/v1/triage",
  useParams: () => ({ id: "v1" }),
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
    user: { username: "nurse.kim", display_name: "Nurse Ana Kim", roles },
    facilities: [
      {
        id: "f1",
        name: "Central Hospital",
        accessible: true,
        can_register: false,
        can_act_clinically: roles.includes("physician"),
      },
    ],
  };
}

const CAPS_NONE = {
  can_arrive: false,
  can_cancel: false,
  can_no_show: false,
  can_triage: false,
  can_assign: false,
  can_start_consultation: false,
  can_resume_consultation: false,
  assigned_to_other: false,
};

const TRIAGE = {
  id: "tr1",
  version: 2,
  reason: "Shortness of breath and fever",
  concerns: ["fever", "shortness_of_breath"],
  onset: "Since yesterday",
  red_flags: [],
  note: null,
  vital_signs_id: "vs1",
  priority: null,
  safety_floor: "urgent",
  safety_rules: [{ rule: "vitals:spo2_below_94", priority: "urgent" }],
  rules_version: "triage-safety@1.0.0",
  requested_service: "general_medicine",
  ai_artifact_id: null,
  ai_decision: null,
  completed_at: null,
  updated_at: "2026-08-29T08:15:00Z",
  author_name: "Nurse Ana Kim",
};

const PROPOSAL = {
  id: "art1",
  status: "awaiting_review",
  model: "dmind-fake",
  model_version: "1.0.0",
  output: {
    proposed_priority: "urgent",
    safety_floor: "urgent",
    raised_to_floor: false,
    proposed_service: "general_medicine",
    important_facts: ["SpO₂ 93 %"],
    missing_information: [],
    contradictions: [],
    handoff_summary: "Dyspnoea with fever, SpO₂ 93 %, urgent floor.",
    rationale: ["Oxygen saturation below 94 %"],
    confidence: "medium",
    limitations: ["Deterministic rules only"],
    cited_sources: ["triage.concerns", "vitals.spo2_percent"],
  },
  limitations: ["Deterministic rules only"],
  citations: ["triage.concerns", "vitals.spo2_percent"],
  triage_version: 2,
  review_decision: null,
  review_detail: null,
  reviewed_at: null,
  generated_at: "2026-08-29T08:16:00Z",
};

function detail(overrides: Record<string, unknown> = {}) {
  return {
    id: "v1",
    status: "triage_in_progress",
    arrival_kind: "walk_in",
    service: "general_medicine",
    reason: "Shortness of breath and fever",
    scheduled_at: null,
    arrived_at: "2026-08-29T08:00:00Z",
    ready_at: null,
    consultation_started_at: null,
    wait_minutes: 20,
    priority: null,
    handoff_summary: null,
    encounter_id: null,
    version: 5,
    updated_at: "2026-08-29T08:15:00Z",
    facility: { id: "f1", name: "Central Hospital" },
    patient: {
      id: "p7",
      family_name: "Demopatient",
      given_name: "Diego",
      identifier: "SYN-0007",
      age_years: 55,
      alert_count: 0,
      allergy_count: 1,
    },
    assignment: null,
    open_alerts: 0,
    capabilities: { ...CAPS_NONE, can_triage: true, can_assign: true },
    patient_safety: {
      birth_date: "1970-10-02",
      sex: "male",
      allergies: [{ substance: "Penicillin", criticality: "high" }],
      alerts: [],
      conditions: [{ display: "Asthma", status: "active" }],
      medications: [],
    },
    triage: TRIAGE,
    vitals: [
      {
        id: "vs1",
        visit_id: "v1",
        encounter_id: null,
        systolic_mmhg: "128",
        diastolic_mmhg: "82",
        heart_rate_bpm: "98",
        respiratory_rate_bpm: "22",
        temperature_c: "38.2",
        spo2_percent: "93",
        weight_kg: null,
        height_cm: null,
        bmi: null,
        recorded_at: "2026-08-29T08:10:00Z",
      },
    ],
    proposal: null,
    professionals: [{ id: "u-garcia", display_name: "Dr. Gabriel García" }],
    queues: [
      { id: "q-gm", code: "general_medicine", name: "General medicine" },
    ],
    rules_version: "triage-safety@1.0.0",
    ...overrides,
  };
}

type Handler = (
  url: string,
  init: RequestInit | undefined,
) => Response | undefined;

function setup(
  d: ReturnType<typeof detail>,
  options: { roles?: string[]; handler?: Handler; lang?: "en" | "es" } = {},
) {
  if (options.lang) localStorage.setItem("wellos.lang", options.lang);
  const posts: { url: string; body: unknown }[] = [];
  let current = d;
  let loads = 0;
  const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (init?.method === "POST")
      posts.push({ url, body: JSON.parse(String(init.body ?? "null")) });
    const custom = options.handler?.(url, init);
    if (custom) return Promise.resolve(custom);
    if (url === "/api/session")
      return Promise.resolve(jsonResponse({ authenticated: true }));
    if (url === "/api/v1/meta/tenant")
      return Promise.resolve(jsonResponse(meta(options.roles ?? ["nurse"])));
    if (
      url === "/api/v1/visits/v1" &&
      (!init?.method || init.method === "GET")
    ) {
      loads += 1;
      return Promise.resolve(jsonResponse(current));
    }
    if (init?.method === "POST") {
      if (url.endsWith("/start-consultation"))
        return Promise.resolve(jsonResponse({ encounter_id: "enc-visit" }));
      if (url.endsWith("/review"))
        return Promise.resolve(
          jsonResponse({
            applied_priority: "urgent",
            applied_service: "general_medicine",
          }),
        );
      if (url.endsWith("/triage"))
        return Promise.resolve(
          jsonResponse({ id: "v1", version: current.version + 1 }),
        );
      return Promise.resolve(jsonResponse({ ok: true }));
    }
    return Promise.resolve(jsonResponse({}));
  });
  vi.stubGlobal("fetch", fetchMock);
  render(
    <SessionProvider>
      <TriagePage />
    </SessionProvider>,
  );
  return {
    posts,
    loads: () => loads,
    setServer: (next: ReturnType<typeof detail>) => {
      current = next;
    },
  };
}

describe("triage workspace", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    localStorage.clear();
    push.mockClear();
  });

  it("shows the patient safety header, previous vitals and the deterministic floor", async () => {
    setup(detail());
    const header = await screen.findByRole("region", { name: "Patient" });
    expect(
      within(header).getByRole("heading", { name: "Diego Demopatient" }),
    ).toBeInTheDocument();
    expect(within(header).getByText("Allergy: Penicillin")).toBeInTheDocument();
    expect(within(header).getByText("Asthma")).toBeInTheDocument();
    expect(within(header).getByText("In triage")).toBeInTheDocument();
    expect(screen.getByText(/Previous vital signs/)).toHaveTextContent(
      /128\/82 mmHg · 98 bpm · 22 \/min · 38\.2 °C · SpO₂ 93 %/,
    );
    const floor = screen.getByRole("region", { name: "Safety floor" });
    expect(within(floor).getAllByText("Urgent")).toHaveLength(2);
    expect(
      within(floor).getByText(/Oxygen saturation below 94%/),
    ).toBeInTheDocument();
    expect(
      within(floor).getByText(/Rules: triage-safety@1\.0\.0/),
    ).toHaveTextContent(/Triaged by Nurse Ana Kim/);
    // Reason is prefilled from the saved assessment.
    expect(
      screen.getByRole("textbox", { name: "Reason for visit" }),
    ).toHaveValue("Shortness of breath and fever");
    expect(screen.getByRole("checkbox", { name: "Fever" })).toBeChecked();
    expect(
      within(screen.getByRole("group", { name: "Red flags" })).getByRole(
        "checkbox",
        { name: "Chest pain" },
      ),
    ).not.toBeChecked();
  });

  it("does not allow a priority below the safety floor", async () => {
    const user = userEvent.setup();
    setup(detail());
    const priority = await screen.findByLabelText("Operational priority");
    expect(
      within(priority).getByRole("option", { name: "Standard" }),
    ).toBeDisabled();
    expect(
      within(priority).getByRole("option", { name: "Non-urgent" }),
    ).toBeDisabled();
    expect(
      within(priority).getByRole("option", { name: "Urgent" }),
    ).toBeEnabled();
    expect(
      within(priority).getByRole("option", { name: "Immediate" }),
    ).toBeEnabled();
    // Completion needs a priority.
    expect(
      screen.getByRole("button", { name: "Complete triage and route" }),
    ).toBeDisabled();
    await user.selectOptions(priority, "urgent");
    expect(
      screen.getByRole("button", { name: "Complete triage and route" }),
    ).toBeEnabled();
  });

  it("saves the assessment with the visit version and refreshes", async () => {
    const user = userEvent.setup();
    const { posts, loads } = setup(detail());
    const save = await screen.findByRole("button", { name: "Save triage" });
    expect(save).toBeDisabled();
    const flags = screen.getByRole("group", { name: "Red flags" });
    await user.click(
      within(flags).getByRole("checkbox", { name: "Chest pain" }),
    );
    await user.type(screen.getByLabelText("Oxygen saturation (%)"), "91");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Ask dMind" })).toBeDisabled();
    await user.click(save);
    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0].url).toBe("/api/v1/visits/v1/triage");
    expect(posts[0].body).toMatchObject({
      version: 5,
      reason: "Shortness of breath and fever",
      concerns: ["fever", "shortness_of_breath"],
      red_flags: ["chest_pain"],
      vitals: { confirm_unusual: false, spo2_percent: "91" },
      priority: null,
    });
    expect(await screen.findByText("Triage saved.")).toBeInTheDocument();
    expect(loads()).toBe(2);
    expect(screen.getByLabelText("Oxygen saturation (%)")).toHaveValue("");
  });

  it("asks to confirm unusual values before saving them", async () => {
    const user = userEvent.setup();
    let attempts = 0;
    const { posts } = setup(detail(), {
      handler: (url, init) => {
        if (url === "/api/v1/visits/v1/triage" && init?.method === "POST") {
          attempts += 1;
          if (attempts === 1)
            return jsonResponse(
              { error: { code: "unusual_values", message: "unusual" } },
              422,
            );
        }
        return undefined;
      },
    });
    await user.type(await screen.findByLabelText("Heart rate (bpm)"), "190");
    await user.click(screen.getByRole("button", { name: "Save triage" }));
    const confirm = await screen.findByRole("button", {
      name: "Confirm values and save",
    });
    expect(screen.getByRole("alert")).toHaveTextContent(
      /outside the usual range/i,
    );
    await user.click(confirm);
    await waitFor(() => expect(posts).toHaveLength(2));
    expect(posts[0].body).toMatchObject({
      vitals: { confirm_unusual: false, heart_rate_bpm: "190" },
    });
    expect(posts[1].body).toMatchObject({
      vitals: { confirm_unusual: true, heart_rate_bpm: "190" },
    });
    expect(await screen.findByText("Triage saved.")).toBeInTheDocument();
  });

  it("reloads the visit on a version conflict and keeps the form usable", async () => {
    const user = userEvent.setup();
    const { loads, setServer } = setup(detail(), {
      handler: (url, init) =>
        url === "/api/v1/visits/v1/triage" && init?.method === "POST"
          ? jsonResponse(
              { error: { code: "version_conflict", message: "stale" } },
              409,
            )
          : undefined,
    });
    await user.type(await screen.findByLabelText("Onset"), " (worse)");
    setServer(detail({ version: 6 }));
    await user.click(screen.getByRole("button", { name: "Save triage" }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /changed in the meantime/i,
    );
    await waitFor(() => expect(loads()).toBe(2));
    expect(screen.getByLabelText("Onset")).toHaveValue("Since yesterday");
  });

  it("reviews a dMind proposal only by explicit decision and adopts it into the form", async () => {
    const user = userEvent.setup();
    const { posts } = setup(detail({ proposal: PROPOSAL }));
    const panel = await screen.findByRole("region", {
      name: "dMind triage suggestion",
    });
    expect(
      within(panel).getByText(
        "Assistive suggestion — your decision is required",
      ),
    ).toBeInTheDocument();
    expect(
      within(panel).getByText("Proposed priority: Urgent"),
    ).toBeInTheDocument();
    expect(
      within(panel).getByText(
        /Facts used: triage\.concerns, vitals\.spo2_percent/,
      ),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Operational priority")).toHaveValue("");
    await user.click(within(panel).getByRole("button", { name: "Accept" }));
    await waitFor(() =>
      expect(posts).toEqual([
        {
          url: "/api/v1/visits/v1/triage/proposal/art1/review",
          body: { version: 5, decision: "accept" },
        },
      ]),
    );
    expect(
      await screen.findByText("Your decision was recorded."),
    ).toBeInTheDocument();
    expect(screen.getByLabelText("Operational priority")).toHaveValue("urgent");
    expect(screen.getByLabelText("Handoff summary")).toHaveValue(
      "Dyspnoea with fever, SpO₂ 93 %, urgent floor.",
    );
  });

  it("marks a proposal stale when the triage moved past the version it cited", async () => {
    setup(
      detail({
        triage: { ...TRIAGE, version: 3 },
        proposal: PROPOSAL,
      }),
    );
    const panel = await screen.findByRole("region", {
      name: "dMind triage suggestion",
    });
    expect(within(panel).getByRole("status")).toHaveTextContent(
      /made from earlier facts/i,
    );
    expect(within(panel).queryByRole("button", { name: "Accept" })).toBeNull();
  });

  it("completes triage with priority, service, handoff and a named professional", async () => {
    const user = userEvent.setup();
    const { posts } = setup(detail());
    await user.selectOptions(
      await screen.findByLabelText("Operational priority"),
      "urgent",
    );
    await user.type(
      screen.getByLabelText("Handoff summary"),
      "Dyspnoea, SpO₂ 93 %.",
    );
    await user.click(screen.getByRole("radio", { name: "Named professional" }));
    await user.selectOptions(
      screen.getByRole("combobox", { name: "Named professional" }),
      "u-garcia",
    );
    await user.click(
      screen.getByRole("button", { name: "Complete triage and route" }),
    );
    // Dirty form: saved first, then completed against the version the save
    // produced for exactly these facts.
    await waitFor(() => expect(posts).toHaveLength(2));
    expect(posts[0].url).toBe("/api/v1/visits/v1/triage");
    expect(posts[0].body).toMatchObject({ version: 5, priority: "urgent" });
    expect(posts[1]).toEqual({
      url: "/api/v1/visits/v1/triage/complete",
      body: {
        version: 6,
        priority: "urgent",
        requested_service: "general_medicine",
        handoff_summary: "Dyspnoea, SpO₂ 93 %.",
        assignee_user_id: "u-garcia",
        queue_id: null,
      },
    });
    expect(
      await screen.findByText(
        "Triage completed. The care team has been alerted.",
      ),
    ).toBeInTheDocument();
  });

  it("completes against the displayed version and surfaces a concurrent change as a conflict for review", async () => {
    const user = userEvent.setup();
    const displayed = detail({
      triage: { ...TRIAGE, priority: "urgent" },
      handoff_summary: "Dyspnoea with fever, urgent.",
    });
    const concurrent = detail({
      version: 6,
      triage: { ...TRIAGE, version: 3, priority: "immediate" },
      handoff_summary: "Deteriorated: SpO₂ 86 %, immediate.",
    });
    const { posts, loads, setServer } = setup(displayed, {
      handler: (url, init) => {
        if (url.endsWith("/triage/complete") && init?.method === "POST") {
          const body = JSON.parse(String(init.body)) as { version: number };
          return body.version === 6
            ? jsonResponse({ ok: true })
            : jsonResponse(
                {
                  error: {
                    code: "stale_version",
                    message: "visit changed",
                  },
                },
                409,
              );
        }
        return undefined;
      },
    });
    expect(await screen.findByLabelText("Operational priority")).toHaveValue(
      "urgent",
    );
    const loadsBefore = loads();
    // Another clinician raises the priority after this workspace loaded.
    setServer(concurrent);
    await user.click(
      screen.getByRole("button", { name: "Complete triage and route" }),
    );
    // Only the completion is sent, bound to the version behind the visible
    // form — never a freshly fetched version paired with the old values.
    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0].url).toBe("/api/v1/visits/v1/triage/complete");
    expect(posts[0].body).toMatchObject({
      version: 5,
      priority: "urgent",
      handoff_summary: "Dyspnoea with fever, urgent.",
    });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /changed in the meantime/i,
    );
    // The workspace reloads with the concurrent facts for review; nothing
    // was completed with the stale values.
    await waitFor(() => expect(loads()).toBeGreaterThan(loadsBefore));
    expect(screen.getByLabelText("Operational priority")).toHaveValue(
      "immediate",
    );
    expect(screen.getByLabelText("Handoff summary")).toHaveValue(
      "Deteriorated: SpO₂ 86 %, immediate.",
    );
    expect(screen.queryByText(/Triage completed/)).toBeNull();
    expect(posts).toHaveLength(1);
  });

  it("keeps the typed handoff summary after saving the triage", async () => {
    const user = userEvent.setup();
    const { posts } = setup(detail());
    await user.type(
      await screen.findByLabelText("Handoff summary"),
      "Dyspnoea, SpO₂ 93 %.",
    );
    await user.click(
      within(screen.getByRole("group", { name: "Red flags" })).getByRole(
        "checkbox",
        { name: "Chest pain" },
      ),
    );
    await user.click(screen.getByRole("button", { name: "Save triage" }));
    await waitFor(() => expect(posts).toHaveLength(1));
    expect(await screen.findByText("Triage saved.")).toBeInTheDocument();
    expect(screen.getByLabelText("Handoff summary")).toHaveValue(
      "Dyspnoea, SpO₂ 93 %.",
    );
  });

  it("shows the routed handoff read-only once ready and lets the assigned physician start", async () => {
    const user = userEvent.setup();
    const { posts } = setup(
      detail({
        status: "ready_for_consultation",
        priority: "urgent",
        handoff_summary: "Dyspnoea, SpO₂ 93 %.",
        assignment: {
          kind: "professional",
          display_name: "Dr. Gabriel García",
        },
        capabilities: { ...CAPS_NONE, can_start_consultation: true },
      }),
      { roles: ["physician"] },
    );
    expect(
      await screen.findByText("Triage is read-only for your role."),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Assigned to Dr. Gabriel García"),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("textbox", { name: "Reason for visit" }),
    ).toBeDisabled();
    expect(screen.queryByRole("button", { name: "Save triage" })).toBeNull();
    await user.click(
      screen.getByRole("button", { name: "Start consultation" }),
    );
    await waitFor(() =>
      expect(push).toHaveBeenCalledWith("/encounters/enc-visit"),
    );
    expect(posts).toEqual([
      { url: "/api/v1/visits/v1/start-consultation", body: { version: 5 } },
    ]);
  });

  it("shows permission-denied and not-found states with a way back", async () => {
    setup(detail(), {
      handler: (url, init) =>
        url === "/api/v1/visits/v1" && !init?.method
          ? jsonResponse(
              { error: { code: "forbidden", message: "forbidden" } },
              403,
            )
          : undefined,
    });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "You do not have permission to view this information.",
    );
    expect(
      screen.getByRole("link", { name: "Back to access" }),
    ).toHaveAttribute("href", "/access");
  });

  it("shows not found for an unknown visit", async () => {
    setup(detail(), {
      handler: (url, init) =>
        url === "/api/v1/visits/v1" && !init?.method
          ? jsonResponse(
              { error: { code: "not_found", message: "not found" } },
              404,
            )
          : undefined,
    });
    expect(await screen.findByRole("alert")).toHaveTextContent("Not found");
  });

  it("renders the workspace in Spanish", async () => {
    setup(detail({ proposal: PROPOSAL }), { lang: "es" });
    expect(
      await screen.findByRole("heading", { name: "Triaje" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("textbox", { name: "Motivo de la visita" }),
    ).toHaveValue("Shortness of breath and fever");
    expect(
      screen.getByRole("region", { name: "Piso de seguridad" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Guardar triaje" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Completar triaje y derivar" }),
    ).toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Aceptar" })).toBeInTheDocument();
    expect(screen.getByText("Alergia: Penicillin")).toBeInTheDocument();
  });
});
