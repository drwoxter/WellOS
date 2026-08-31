import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import PatientsPage from "@/app/patients/page";
import { SessionProvider } from "@/lib/session";

const push = vi.fn();
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push, replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => "/patients",
}));

const PATIENT = {
  id: "44444444-4444-4444-4444-444444444444",
  family_name: "Fresh",
  given_name: "Encounterless",
  birth_date: "1982-03-04",
  sex: "male",
  identifier: "SYN-9999",
};

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function setup(options: {
  clinician: boolean;
  register: boolean;
  canOpenChart?: boolean;
  canStartEncounter?: boolean;
}) {
  const encounterCalls: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url === "/api/session")
        return Promise.resolve(jsonResponse({ authenticated: true }));
      if (url.startsWith("/api/v1/patients?query="))
        return Promise.resolve(
          jsonResponse({
            patients: [
              {
                ...PATIENT,
                can_open_chart: options.canOpenChart ?? true,
                can_start_encounter:
                  options.canStartEncounter ?? options.clinician,
              },
            ],
          }),
        );
      if (url === "/api/v1/encounters" && init?.method === "POST") {
        encounterCalls.push(init.body as string);
        return Promise.resolve(jsonResponse({ id: "enc-1" }));
      }
      if (url === "/api/v1/meta/tenant")
        return Promise.resolve(
          jsonResponse({
            tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
            user: {
              username: options.clinician ? "dr.garcia" : "reg.rivera",
              display_name: options.clinician ? "Dr. García" : "Rosa Rivera",
              roles: [options.clinician ? "physician" : "registration_staff"],
            },
            facilities: [
              {
                id: "f",
                name: "Central Hospital",
                accessible: true,
                can_register: options.register,
                can_act_clinically: options.clinician,
              },
            ],
          }),
        );
      return Promise.resolve(jsonResponse({}));
    }),
  );
  render(
    <SessionProvider>
      <PatientsPage />
    </SessionProvider>,
  );
  return encounterCalls;
}

async function searchFor(term: string) {
  const input = await screen.findByLabelText("Search patients");
  await userEvent.type(input, term);
  await userEvent.click(screen.getByRole("button", { name: "Search" }));
}

describe("patient directory", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    push.mockClear();
  });

  it("lets a physician start an encounter for a patient with no prior encounter", async () => {
    const encounterCalls = setup({ clinician: true, register: false });
    await searchFor("Fresh");
    await screen.findByText("Encounterless Fresh");
    await userEvent.click(
      screen.getByRole("button", { name: "Start encounter" }),
    );
    await waitFor(() =>
      expect(push).toHaveBeenCalledWith(`/patients/${PATIENT.id}`),
    );
    expect(encounterCalls).toHaveLength(1);
    expect(encounterCalls[0]).toContain(PATIENT.id);
    // No registration form for clinicians.
    expect(
      screen.queryByText("Register a new patient"),
    ).not.toBeInTheDocument();
  });

  it("hides the chart link for a physician without a care relationship", async () => {
    setup({ clinician: true, register: false, canOpenChart: false });
    await searchFor("Fresh");
    await screen.findByText("Encounterless Fresh");
    // A newly registered, encounterless patient is not readable by the
    // physician yet: only the start-encounter route is offered.
    expect(screen.queryByText("Open chart")).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Start encounter" }),
    ).toBeInTheDocument();
  });

  it("keeps the chart link for roles that can read within facility scope", async () => {
    setup({ clinician: false, register: true, canOpenChart: true });
    await searchFor("Fresh");
    await screen.findByText("Encounterless Fresh");
    expect(screen.getByText("Open chart")).toBeInTheDocument();
  });

  it("hides the encounter action for patients at facilities without clinical rights", async () => {
    // A clinician at facility A searching a patient at facility B (visible
    // through a search-only role) must not be offered an encounter start
    // that the backend would reject.
    setup({ clinician: true, register: false, canStartEncounter: false });
    await searchFor("Fresh");
    await screen.findByText("Encounterless Fresh");
    expect(
      screen.queryByRole("button", { name: "Start encounter" }),
    ).not.toBeInTheDocument();
  });

  it("shows registration but no encounter action for registration staff", async () => {
    setup({ clinician: false, register: true });
    await searchFor("Fresh");
    await screen.findByText("Encounterless Fresh");
    expect(
      screen.queryByRole("button", { name: "Start encounter" }),
    ).not.toBeInTheDocument();
    expect(screen.getByText("Register a new patient")).toBeInTheDocument();
  });
});
