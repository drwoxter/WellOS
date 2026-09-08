import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import DashboardPage from "@/app/dashboard/page";
import { SessionProvider } from "@/lib/session";
import { COCKPIT_STORAGE_ITEM } from "@/lib/cockpit";

const push = vi.fn();
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push, replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => "/dashboard",
}));

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

const PATIENT = {
  id: "p1",
  family_name: "Demopatient",
  given_name: "Alba",
  identifier: "SYN-0001",
  birth_date: "1990-03-03",
};

function meta(roles: string[]) {
  return {
    tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
    user: { username: "u", display_name: "Test User", roles },
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

const SUMMARY = {
  critical_open: 1,
  awaiting_review: 2,
  awaiting_notification: 0,
  awaiting_closure: 0,
  recently_closed: 3,
};

const COCKPIT = {
  draft_consultations: [
    {
      id: "enc-draft",
      started_at: "2026-08-28T09:00:00Z",
      updated_at: "2026-08-28T09:30:00Z",
      reason: "Follow-up of cough",
      patient: PATIENT,
    },
  ],
  attention: [
    {
      patient: PATIENT,
      open_alerts: 1,
      open_tasks: 2,
      latest_at: "2026-08-28T09:00:00Z",
      open_consultation_id: null,
      can_open_chart: true,
      can_start_encounter: true,
    },
  ],
  pending_tasks: [
    {
      id: "task-1",
      description: "Notify ordering clinician of critical potassium",
      priority: "high",
      status: "open",
      due_at: null,
      created_at: "2026-08-28T09:00:00Z",
      service_request_id: "sr1",
      patient: PATIENT,
      can_open_detail: true,
    },
  ],
  ai_activity: [
    {
      id: "a1",
      artifact_type: "scribe_draft",
      status: "awaiting_review",
      model: "fake-transcribe",
      model_version: "0.1.0",
      generated_at: "2026-08-28T09:10:00Z",
      reviewed_at: null,
      review_decision: null,
      encounter_id: "enc-draft",
      service_request_id: null,
      patient: PATIENT,
      can_open: true,
    },
    {
      id: "a2",
      artifact_type: "result_summary",
      status: "unavailable",
      model: null,
      model_version: null,
      generated_at: null,
      reviewed_at: null,
      review_decision: null,
      encounter_id: null,
      service_request_id: "sr1",
      patient: PATIENT,
      can_open: false,
    },
    {
      id: "a3",
      artifact_type: "encounter_summary",
      status: "superseded",
      model: "dmind-fake",
      model_version: "0.1.0",
      generated_at: "2026-08-27T09:10:00Z",
      reviewed_at: null,
      review_decision: null,
      encounter_id: "enc-old",
      service_request_id: null,
      patient: PATIENT,
      can_open: true,
    },
  ],
  generated_at: "2026-08-29T09:00:00Z",
};

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

function visit(
  id: string,
  status: string,
  caps: Partial<typeof CAPS_NONE> = {},
  extra: Record<string, unknown> = {},
) {
  return {
    id,
    status,
    arrival_kind: "walk_in",
    service: "general_medicine",
    reason: `Reason ${id}`,
    scheduled_at: null,
    arrived_at: "2026-08-29T08:00:00Z",
    ready_at: null,
    consultation_started_at: null,
    wait_minutes: 12,
    priority: "standard",
    handoff_summary: null,
    encounter_id: null,
    version: 3,
    updated_at: "2026-08-29T08:10:00Z",
    facility: { id: "f1", name: "Central Hospital" },
    patient: {
      ...PATIENT,
      age_years: 36,
      alert_count: 0,
      allergy_count: 1,
    },
    assignment: null,
    open_alerts: 0,
    capabilities: { ...CAPS_NONE, ...caps },
    ...extra,
  };
}

const VISITS = [
  visit("v-sched", "scheduled", { can_arrive: true }, { arrived_at: null }),
  visit("v-arrived", "arrived", { can_triage: true }),
  visit("v-triage", "triage_in_progress", { can_triage: true }),
  visit("v-ready", "ready_for_consultation", { can_start_consultation: true }),
  visit(
    "v-open",
    "in_consultation",
    { can_resume_consultation: true },
    { encounter_id: "enc-open" },
  ),
];

const ALERTS = [
  {
    id: "al1",
    visit_id: "v-ready",
    kind: "ready_for_consultation",
    priority: "standard",
    status: "open",
    created_at: "2026-08-29T08:20:00Z",
    acknowledged_at: null,
    acknowledged_by_me: false,
    target: { kind: "professional" },
    visit: {
      status: "ready_for_consultation",
      reason: "Reason v-ready",
      wait_minutes: 12,
      handoff_summary: null,
      encounter_id: null,
      version: 3,
    },
    patient: PATIENT,
  },
];

function setup(
  roles: string[],
  options: { openConsultationId?: string | null } = {},
) {
  const encounterCalls: string[] = [];
  const visitCalls: { url: string; body: string }[] = [];
  let visitLoads = 0;
  const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (url === "/api/session")
      return Promise.resolve(jsonResponse({ authenticated: true }));
    if (url === "/api/v1/meta/tenant")
      return Promise.resolve(jsonResponse(meta(roles)));
    if (url === "/api/v1/worklist/summary")
      return Promise.resolve(jsonResponse(SUMMARY));
    if (url === "/api/v1/worklist")
      return Promise.resolve(jsonResponse({ items: [] }));
    if (url === "/api/v1/dashboard/cockpit")
      return Promise.resolve(jsonResponse(COCKPIT));
    if (url.startsWith("/api/v1/patients?query="))
      return Promise.resolve(
        jsonResponse({
          patients: [
            {
              ...PATIENT,
              can_open_chart: true,
              can_start_encounter: true,
              open_consultation_id: options.openConsultationId ?? null,
            },
          ],
        }),
      );
    if (url === "/api/v1/encounters" && init?.method === "POST") {
      encounterCalls.push(String(init.body));
      return Promise.resolve(jsonResponse({ id: "enc-new" }));
    }
    if (url === "/api/v1/visits?view=access") {
      visitLoads += 1;
      return Promise.resolve(jsonResponse({ items: VISITS }));
    }
    if (url === "/api/v1/alerts")
      return Promise.resolve(jsonResponse({ items: ALERTS }));
    if (url.startsWith("/api/v1/visits/") && init?.method === "POST") {
      visitCalls.push({ url, body: String(init.body ?? "") });
      return Promise.resolve(jsonResponse({ encounter_id: "enc-from-visit" }));
    }
    if (url.startsWith("/api/v1/alerts/") && init?.method === "POST") {
      visitCalls.push({ url, body: "" });
      return Promise.resolve(jsonResponse({ status: "acknowledged" }));
    }
    return Promise.resolve(jsonResponse({}));
  });
  vi.stubGlobal("fetch", fetchMock);
  render(
    <SessionProvider>
      <DashboardPage />
    </SessionProvider>,
  );
  return {
    encounterCalls,
    visitCalls,
    fetchMock,
    visitLoads: () => visitLoads,
  };
}

describe("dashboard cockpit", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    push.mockClear();
    window.localStorage.clear();
  });

  it("offers Start consultation to clinicians and creates/resumes through one call", async () => {
    const user = userEvent.setup();
    const { encounterCalls } = setup(["physician"]);
    const start = await screen.findByRole("region", {
      name: "Start consultation",
    });
    const input = within(start).getByLabelText("Choose a patient");
    await user.type(input, "SYN-0001");
    await user.click(within(start).getByRole("button", { name: "Search" }));
    const row = await within(start).findByText("Alba Demopatient");
    expect(row).toBeInTheDocument();
    await user.click(
      within(start).getByRole("button", { name: "Start consultation" }),
    );
    await waitFor(() =>
      expect(push).toHaveBeenCalledWith("/encounters/enc-new"),
    );
    expect(encounterCalls).toHaveLength(1);
    expect(JSON.parse(encounterCalls[0])).toEqual({
      patient_id: "p1",
      encounter_type: "consultation",
      resume: true,
    });
  });

  it("labels the action Resume when the patient already has an open consultation", async () => {
    const user = userEvent.setup();
    setup(["physician"], { openConsultationId: "enc-open" });
    const start = await screen.findByRole("region", {
      name: "Start consultation",
    });
    await user.type(within(start).getByLabelText("Choose a patient"), "Alba");
    await user.click(within(start).getByRole("button", { name: "Search" }));
    expect(
      await within(start).findByRole("button", { name: "Resume consultation" }),
    ).toBeInTheDocument();
    expect(
      within(start).getByText("Consultation in progress — will resume"),
    ).toBeInTheDocument();
  });

  it("does not show Start consultation to non-clinical roles", async () => {
    setup(["laboratory_professional"]);
    await screen.findByText("Pending tasks");
    expect(
      screen.queryByRole("region", { name: "Start consultation" }),
    ).not.toBeInTheDocument();
  });

  it("gives laboratory professionals the results-first cockpit", async () => {
    setup(["laboratory_professional"]);
    await screen.findByText("Pending tasks");
    const widgets = screen
      .getAllByRole("region")
      .filter((s) => s.classList.contains("widget"))
      .map((s) => s.getAttribute("aria-labelledby"));
    expect(widgets).toEqual(["w-results", "w-tasks", "w-attention", "w-ai"]);
  });

  it("shows physicians their ready patients and alerts and starts from the card", async () => {
    const user = userEvent.setup();
    const { visitCalls, visitLoads } = setup(["physician"]);
    const ready = await screen.findByRole("region", {
      name: /^Ready for consultation/,
    });
    // Only ready/in-consultation visits, no triage or scheduled ones.
    expect(within(ready).getByText("Reason v-ready")).toBeInTheDocument();
    expect(within(ready).getByText("Reason v-open")).toBeInTheDocument();
    expect(within(ready).queryByText("Reason v-arrived")).toBeNull();
    expect(within(ready).queryByText("Reason v-sched")).toBeNull();
    // Resuming an open consultation is plain navigation to its workspace.
    expect(
      within(ready).getByRole("link", { name: "Resume consultation" }),
    ).toHaveAttribute("href", "/encounters/enc-open");
    // Physicians do not get the triage or access widgets by default.
    expect(screen.queryByRole("region", { name: /^Triage queue/ })).toBeNull();
    expect(
      screen.queryByRole("region", {
        name: /^Today's appointments and arrivals/,
      }),
    ).toBeNull();
    // Flow strip counts every state of today's flow.
    const flow = screen.getByRole("region", { name: "Today's flow" });
    expect(
      within(flow).getByText("Appointments").previousSibling?.textContent,
    ).toBe("1");
    expect(within(flow).getByText("Ready").previousSibling?.textContent).toBe(
      "1",
    );
    // Alerts widget with acknowledgement.
    const alerts = screen.getByRole("region", { name: /^Alerts for you/ });
    expect(within(alerts).getByText(/Alba Demopatient/)).toBeInTheDocument();
    await user.click(
      within(alerts).getByRole("button", { name: "Acknowledge" }),
    );
    await waitFor(() =>
      expect(visitCalls.map((c) => c.url)).toContain(
        "/api/v1/alerts/al1/acknowledge",
      ),
    );
    await waitFor(() => expect(visitLoads()).toBe(2));
    // Start consultation from the ready card sends the current version and
    // navigates to the encounter returned by the server.
    await user.click(
      within(ready).getByRole("button", { name: "Start consultation" }),
    );
    await waitFor(() =>
      expect(push).toHaveBeenCalledWith("/encounters/enc-from-visit"),
    );
    const start = visitCalls.find((c) =>
      c.url.endsWith("/visits/v-ready/start-consultation"),
    );
    expect(start).toBeDefined();
    expect(JSON.parse(start!.body)).toEqual({ version: 3 });
  });

  it("gives nurses the triage queue with links into the triage workspace", async () => {
    setup(["nurse"]);
    const triage = await screen.findByRole("region", { name: /^Triage queue/ });
    expect(within(triage).getByText("Reason v-arrived")).toBeInTheDocument();
    expect(within(triage).getByText("Reason v-triage")).toBeInTheDocument();
    expect(within(triage).queryByText("Reason v-ready")).toBeNull();
    expect(
      within(triage).getByRole("link", { name: "Open triage" }),
    ).toHaveAttribute("href", "/visits/v-arrived/triage");
    expect(
      within(triage).getByRole("link", { name: "Continue triage" }),
    ).toHaveAttribute("href", "/visits/v-triage/triage");
    for (const link of screen.getAllByRole("link", {
      name: "Open access board",
    })) {
      expect(link).toHaveAttribute("href", "/access");
    }
  });

  it("shows registration staff only the access widgets", async () => {
    setup(["registration_staff"]);
    const access = await screen.findByRole("region", {
      name: /^Today's appointments and arrivals/,
    });
    expect(within(access).getByText("Reason v-sched")).toBeInTheDocument();
    expect(within(access).getByText("Reason v-arrived")).toBeInTheDocument();
    expect(within(access).queryByText("Reason v-triage")).toBeNull();
    expect(
      within(access).getByRole("button", { name: "Mark arrived" }),
    ).toBeInTheDocument();
    const widgets = screen
      .getAllByRole("region")
      .filter((s) => s.classList.contains("widget"))
      .map((s) => s.getAttribute("aria-labelledby"));
    expect(widgets).toEqual(["w-alerts", "w-access"]);
    expect(screen.queryByText("Pending tasks")).toBeNull();
    expect(
      screen.queryByRole("region", { name: "Start consultation" }),
    ).toBeNull();
  });

  it("renders the role-default widgets with record data", async () => {
    setup(["physician"]);
    expect(await screen.findByText("Draft consultations")).toBeInTheDocument();
    expect(screen.getByText(/Follow-up of cough/)).toBeInTheDocument();
    expect(screen.getByText("Patients needing attention")).toBeInTheDocument();
    expect(
      screen.getByText("Critical and pending results"),
    ).toBeInTheDocument();
    expect(screen.getByText("Pending tasks")).toBeInTheDocument();
    expect(
      screen.getByText("Notify ordering clinician of critical potassium"),
    ).toBeInTheDocument();
    expect(screen.getByText("Recent dMind activity")).toBeInTheDocument();
    expect(screen.getByText("Scribe draft")).toBeInTheDocument();
    expect(screen.getAllByText("Awaiting review").length).toBeGreaterThan(0);
    expect(screen.getByText("Unavailable")).toBeInTheDocument();
    expect(screen.getByText("Superseded")).toBeInTheDocument();
    expect(
      screen.getAllByRole("link", { name: "Resume consultation" }).length,
    ).toBeGreaterThan(0);
  });

  it("hides, reorders and restores widgets, storing only layout locally", async () => {
    const user = userEvent.setup();
    setup(["physician"]);
    await screen.findByText("Draft consultations");
    await user.click(
      screen.getByRole("button", { name: "Customize dashboard" }),
    );
    await user.click(
      screen.getByRole("button", { name: "Hide: Draft consultations" }),
    );
    expect(screen.queryByText("Follow-up of cough")).not.toBeInTheDocument();
    await user.click(
      screen.getByRole("button", {
        name: "Move down: Patients needing attention",
      }),
    );
    const headings = screen
      .getAllByRole("heading", { level: 2 })
      .map((h) => h.textContent)
      .filter((h) =>
        [
          "Patients needing attention",
          "Critical and pending results",
          "Pending tasks",
          "Recent dMind activity",
        ].includes(h ?? ""),
      );
    expect(headings).toEqual([
      "Critical and pending results",
      "Patients needing attention",
      "Pending tasks",
      "Recent dMind activity",
    ]);
    await user.click(screen.getByRole("radio", { name: "Compact" }));
    const stored = JSON.parse(
      window.localStorage.getItem(COCKPIT_STORAGE_ITEM) ?? "null",
    );
    expect(Object.keys(stored).sort()).toEqual(["density", "hidden", "order"]);
    expect(stored.order).toEqual([
      "ready",
      "alerts",
      "drafts",
      "results",
      "attention",
      "tasks",
      "ai",
      "triage",
      "access",
    ]);
    expect(stored.hidden).toEqual(["triage", "access", "drafts"]);
    expect(stored.density).toBe("compact");
    expect(JSON.stringify(stored)).not.toMatch(/Demopatient|SYN-0001|cough/);
    await user.click(
      screen.getByRole("button", { name: "Restore role defaults" }),
    );
    expect(await screen.findByText(/Follow-up of cough/)).toBeInTheDocument();
    expect(
      JSON.parse(window.localStorage.getItem(COCKPIT_STORAGE_ITEM) ?? "{}"),
    ).toMatchObject({ hidden: ["triage", "access"], density: "expanded" });
  });

  it("starts from a stored layout and ignores malformed storage", async () => {
    window.localStorage.setItem(COCKPIT_STORAGE_ITEM, "{not json");
    setup(["physician"]);
    expect(await screen.findByText("Draft consultations")).toBeInTheDocument();
  });
});
