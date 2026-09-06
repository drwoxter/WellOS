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

function setup(
  roles: string[],
  options: { openConsultationId?: string | null } = {},
) {
  const encounterCalls: string[] = [];
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
    return Promise.resolve(jsonResponse({}));
  });
  vi.stubGlobal("fetch", fetchMock);
  render(
    <SessionProvider>
      <DashboardPage />
    </SessionProvider>,
  );
  return { encounterCalls, fetchMock };
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
      "drafts",
      "results",
      "attention",
      "tasks",
      "ai",
    ]);
    expect(stored.hidden).toEqual(["drafts"]);
    expect(stored.density).toBe("compact");
    expect(JSON.stringify(stored)).not.toMatch(/Demopatient|SYN-0001|cough/);
    await user.click(
      screen.getByRole("button", { name: "Restore role defaults" }),
    );
    expect(await screen.findByText(/Follow-up of cough/)).toBeInTheDocument();
    expect(
      JSON.parse(window.localStorage.getItem(COCKPIT_STORAGE_ITEM) ?? "{}"),
    ).toMatchObject({ hidden: [], density: "expanded" });
  });

  it("starts from a stored layout and ignores malformed storage", async () => {
    window.localStorage.setItem(COCKPIT_STORAGE_ITEM, "{not json");
    setup(["physician"]);
    expect(await screen.findByText("Draft consultations")).toBeInTheDocument();
  });
});
