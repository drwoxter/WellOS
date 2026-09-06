import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import PatientPage from "@/app/patients/[id]/page";
import { SessionProvider } from "@/lib/session";

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), prefetch: vi.fn() }),
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

function setup(facilities: unknown[], chart: unknown = CHART) {
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/session")
        return Promise.resolve(jsonResponse({ authenticated: true }));
      if (url === "/api/v1/patients/p1")
        return Promise.resolve(jsonResponse(chart));
      if (url === "/api/v1/meta/tenant")
        return Promise.resolve(
          jsonResponse({
            tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
            user: {
              username: "dr.garcia",
              display_name: "Dr. García",
              roles: ["physician"],
            },
            facilities,
          }),
        );
      return Promise.resolve(jsonResponse({}));
    }),
  );
  render(
    <SessionProvider>
      <PatientPage params={{ id: "p1" }} />
    </SessionProvider>,
  );
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
});
