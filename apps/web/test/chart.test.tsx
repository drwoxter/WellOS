import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import PatientPage from "@/app/patients/[id]/page";
import { SessionProvider } from "@/lib/session";

const push = vi.fn();
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push, replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => "/patients/p1",
}));

const CHART = {
  patient: {
    id: "p1",
    facility_id: "facility-b",
    family_name: "Demopatient",
    given_name: "Carlos",
    birth_date: "1980-01-01",
    sex: "male",
    identifier: "SYN-0003",
  },
  allergies: [],
  medications: [],
  conditions: [],
  observations: [],
  service_requests: [],
  encounters: [],
  consents: [],
  alerts: [],
  vitals: [],
};

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

const CLINICAL_FACILITY = [
  {
    id: "facility-b",
    name: "Annex Clinic",
    accessible: true,
    can_register: false,
    can_act_clinically: true,
  },
];

function encounter(overrides: Record<string, unknown>) {
  return {
    id: "e-consult",
    status: "in_progress",
    encounter_type: "consultation",
    started_at: "2026-08-28T09:00:00Z",
    completed_at: null,
    practitioner: "Dr. García",
    own: true,
    note_status: "draft",
    addenda_count: 0,
    ...overrides,
  };
}

const VISIT = {
  id: "v1",
  status: "scheduled",
  arrival_kind: "scheduled",
  service: "general_medicine",
  reason: "Annual review",
  scheduled_at: "2026-08-29T10:30:00Z",
  arrived_at: null,
  ready_at: null,
  consultation_started_at: null,
  wait_minutes: null,
  priority: null,
  handoff_summary: null,
  encounter_id: null,
  version: 2,
  updated_at: "2026-08-29T08:00:00Z",
  facility: { id: "facility-b", name: "Annex Clinic" },
  patient: {
    id: "p1",
    family_name: "Demopatient",
    given_name: "Carlos",
    identifier: "SYN-0003",
    age_years: 46,
    alert_count: 0,
    allergy_count: 0,
  },
  assignment: null,
  open_alerts: 0,
  capabilities: {
    can_arrive: false,
    can_cancel: false,
    can_no_show: false,
    can_triage: false,
    can_assign: false,
    can_start_consultation: false,
    can_resume_consultation: false,
    assigned_to_other: false,
  },
};

function setup(
  facilities: unknown[],
  chart: unknown = CHART,
  roles: string[] = ["physician"],
) {
  const posts: { url: string; body: unknown }[] = [];
  let chartLoads = 0;
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url === "/api/session")
        return Promise.resolve(jsonResponse({ authenticated: true }));
      if (url === "/api/v1/patients/p1") {
        chartLoads += 1;
        return Promise.resolve(jsonResponse(chart));
      }
      if (url === "/api/v1/meta/tenant")
        return Promise.resolve(
          jsonResponse({
            tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
            user: {
              username: "dr.garcia",
              display_name: "Dr. García",
              roles,
            },
            facilities,
          }),
        );
      if (init?.method === "POST") {
        posts.push({
          url,
          body: init.body ? JSON.parse(String(init.body)) : null,
        });
        if (url.endsWith("/start-consultation"))
          return Promise.resolve(jsonResponse({ encounter_id: "enc-visit" }));
        return Promise.resolve(jsonResponse({ id: "created" }));
      }
      return Promise.resolve(jsonResponse({}));
    }),
  );
  render(
    <SessionProvider>
      <PatientPage params={{ id: "p1" }} />
    </SessionProvider>,
  );
  return { posts, chartLoads: () => chartLoads };
}

describe("patient chart clinical actions", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  it("shows clinical actions when the user can act at the patient's facility", async () => {
    setup([
      {
        id: "facility-b",
        name: "Annex Clinic",
        accessible: true,
        can_register: false,
        can_act_clinically: true,
      },
    ]);
    expect(
      await screen.findByRole("button", { name: "Start consultation" }),
    ).toBeInTheDocument();
  });

  it("hides clinical actions when the user is clinical only at another facility", async () => {
    setup([
      {
        id: "facility-a",
        name: "Central Hospital",
        accessible: true,
        can_register: false,
        can_act_clinically: true,
      },
      {
        id: "facility-b",
        name: "Annex Clinic",
        accessible: true,
        can_register: true,
        can_act_clinically: false,
      },
    ]);
    expect(
      (await screen.findAllByText("Carlos Demopatient")).length,
    ).toBeGreaterThan(0);
    expect(
      screen.queryByRole("button", { name: "Start consultation" }),
    ).not.toBeInTheDocument();
  });

  it("never offers a historical order-only encounter as a resumable consultation", async () => {
    setup(CLINICAL_FACILITY, {
      ...CHART,
      encounters: [
        encounter({
          id: "e-orders",
          encounter_type: "order_only",
          note_status: null,
        }),
      ],
    });
    expect(
      await screen.findByRole("button", { name: "Start consultation" }),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("link", { name: /Resume consultation/ }),
    ).not.toBeInTheDocument();
    // Its laboratory activity stays visible in the timeline, labelled for
    // what it is, and reachable from the encounters tab.
    expect(
      screen.getByText(/Laboratory orders: Dr\. García/),
    ).toBeInTheDocument();
    await userEvent
      .setup()
      .click(screen.getByRole("tab", { name: "Encounters" }));
    expect(
      screen.queryByRole("link", { name: /Resume consultation/ }),
    ).not.toBeInTheDocument();
    expect(screen.getByRole("link", { name: "Open orders" })).toHaveAttribute(
      "href",
      "/encounters/e-orders",
    );
  });

  it("offers a genuine draft consultation for resumption", async () => {
    setup(CLINICAL_FACILITY, {
      ...CHART,
      encounters: [
        encounter({
          id: "e-orders",
          encounter_type: "order_only",
          note_status: null,
          started_at: "2026-08-29T09:00:00Z",
        }),
        encounter({ id: "e-consult" }),
      ],
    });
    expect(
      await screen.findByRole("link", { name: /Resume consultation/ }),
    ).toHaveAttribute("href", "/encounters/e-consult");
    await userEvent
      .setup()
      .click(screen.getByRole("tab", { name: "Encounters" }));
    const resume = screen.getAllByRole("link", { name: /Resume consultation/ });
    expect(resume).toHaveLength(2);
    for (const link of resume) {
      expect(link).toHaveAttribute("href", "/encounters/e-consult");
    }
    expect(screen.getByRole("link", { name: "Open orders" })).toHaveAttribute(
      "href",
      "/encounters/e-orders",
    );
  });

  it("does not show a visit card when there is no visit and the user cannot register one", async () => {
    setup(CLINICAL_FACILITY, {
      ...CHART,
      visit: null,
      can_manage_visit: false,
    });
    await screen.findByRole("button", { name: "Start consultation" });
    expect(screen.queryByRole("region", { name: "Today's visit" })).toBeNull();
  });

  it("lets registration staff mark a scheduled visit as arrived with its current version", async () => {
    const user = userEvent.setup();
    const registration = [
      { ...CLINICAL_FACILITY[0], can_act_clinically: false },
    ];
    const { posts, chartLoads } = setup(
      registration,
      {
        ...CHART,
        visit: {
          ...VISIT,
          capabilities: { ...VISIT.capabilities, can_arrive: true },
        },
        can_manage_visit: true,
      },
      ["registration_staff"],
    );
    const card = await screen.findByRole("region", { name: "Today's visit" });
    expect(within(card).getByText("Annual review")).toBeInTheDocument();
    expect(within(card).getByText("Scheduled")).toBeInTheDocument();
    // A visit already exists: no second registration form.
    expect(
      within(card).queryByRole("button", { name: "Register arrival" }),
    ).toBeNull();
    expect(
      screen.queryByRole("button", { name: "Start consultation" }),
    ).toBeNull();
    await user.click(
      within(card).getByRole("button", { name: "Mark arrived" }),
    );
    await waitFor(() =>
      expect(posts).toEqual([
        { url: "/api/v1/visits/v1/arrive", body: { version: 2 } },
      ]),
    );
    await waitFor(() => expect(chartLoads()).toBe(2));
  });

  it("registers an arrival for this patient from the chart without a patient search", async () => {
    const user = userEvent.setup();
    const registration = [
      { ...CLINICAL_FACILITY[0], can_act_clinically: false },
    ];
    const { posts, chartLoads } = setup(
      registration,
      { ...CHART, visit: null, can_manage_visit: true },
      ["registration_staff"],
    );
    const card = await screen.findByRole("region", { name: "Today's visit" });
    expect(within(card).getByText("No visit today.")).toBeInTheDocument();
    await user.click(
      within(card).getByRole("button", { name: "Register arrival" }),
    );
    // The patient is fixed: no search box, no change-patient control.
    expect(within(card).queryByLabelText("Find the patient")).toBeNull();
    expect(
      within(card).queryByRole("button", { name: "Change patient" }),
    ).toBeNull();
    expect(within(card).getByText("Carlos Demopatient")).toBeInTheDocument();
    await user.click(within(card).getByRole("radio", { name: "Urgent" }));
    await user.type(
      within(card).getByLabelText(/Reason for visit/),
      "Chest pain since this morning",
    );
    await user.click(within(card).getByRole("button", { name: "Register" }));
    await waitFor(() =>
      expect(posts).toEqual([
        {
          url: "/api/v1/visits",
          body: {
            patient_id: "p1",
            arrival_kind: "urgent",
            service: "general_medicine",
            reason: "Chest pain since this morning",
            scheduled_at: null,
          },
        },
      ]),
    );
    await waitFor(() => expect(chartLoads()).toBe(2));
  });

  it("starts the consultation from the ready visit so it stays linked to the handoff", async () => {
    const user = userEvent.setup();
    const { posts } = setup(CLINICAL_FACILITY, {
      ...CHART,
      visit: {
        ...VISIT,
        status: "ready_for_consultation",
        arrival_kind: "walk_in",
        arrived_at: "2026-08-29T08:00:00Z",
        ready_at: "2026-08-29T08:20:00Z",
        priority: "urgent",
        capabilities: {
          ...VISIT.capabilities,
          can_start_consultation: true,
        },
      },
      can_manage_visit: false,
    });
    const card = await screen.findByRole("region", { name: "Today's visit" });
    expect(within(card).getByText("Urgent")).toBeInTheDocument();
    // Exactly one way to start: through the visit, not a detached encounter.
    expect(
      screen.getAllByRole("button", { name: "Start consultation" }),
    ).toHaveLength(1);
    await user.click(
      within(card).getByRole("button", { name: "Start consultation" }),
    );
    await waitFor(() =>
      expect(push).toHaveBeenCalledWith("/encounters/enc-visit"),
    );
    expect(posts).toEqual([
      { url: "/api/v1/visits/v1/start-consultation", body: { version: 2 } },
    ]);
    // Laboratory ordering stays available for clinicians.
    expect(
      screen.getByRole("button", { name: "Order laboratory test" }),
    ).toBeInTheDocument();
  });
});
