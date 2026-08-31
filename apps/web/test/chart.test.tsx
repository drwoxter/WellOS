import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
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
};

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function setup(facilities: unknown[]) {
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/session")
        return Promise.resolve(jsonResponse({ authenticated: true }));
      if (url === "/api/v1/patients/p1")
        return Promise.resolve(jsonResponse(CHART));
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
      await screen.findByRole("button", { name: "Start encounter" }),
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
      screen.queryByRole("button", { name: "Start encounter" }),
    ).not.toBeInTheDocument();
  });
});
