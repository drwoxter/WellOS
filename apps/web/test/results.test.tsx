import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import ResultsPage from "@/app/results/page";
import { SessionProvider } from "@/lib/session";

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => "/results",
}));

const ITEMS = [
  {
    id: "11111111-1111-1111-1111-111111111111",
    display: "Potassium [Moles/volume] in Serum",
    code_loinc: "2823-3",
    loop_state: "received",
    has_open_alert: true,
    created_at: "2026-08-28T10:00:00Z",
    patient: {
      family_name: "Demopatient",
      given_name: "Carlos",
      identifier: "SYN-0003",
    },
  },
  {
    id: "22222222-2222-2222-2222-222222222222",
    display: "Glucose [Mass/volume] in Serum",
    code_loinc: "2345-7",
    loop_state: "reviewed",
    has_open_alert: false,
    created_at: "2026-08-28T08:00:00Z",
    patient: {
      family_name: "Demopatient",
      given_name: "Marta",
      identifier: "SYN-0004",
    },
  },
];

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function setup(worklist: unknown = { items: ITEMS }) {
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/session")
        return Promise.resolve(jsonResponse({ authenticated: true }));
      if (url === "/api/v1/worklist")
        return Promise.resolve(jsonResponse(worklist));
      if (url === "/api/v1/meta/tenant")
        return Promise.resolve(
          jsonResponse({
            tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
            user: {
              username: "dr.garcia",
              display_name: "Dr. García",
              roles: ["physician"],
            },
            facilities: [{ id: "f", name: "Central Hospital", accessible: true }],
          }),
        );
      return Promise.resolve(jsonResponse({}));
    }),
  );
  render(
    <SessionProvider>
      <ResultsPage />
    </SessionProvider>,
  );
}

describe("results worklist", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  it("lists open results with criticality and status badges", async () => {
    setup();
    expect(
      (await screen.findAllByText("Carlos Demopatient")).length,
    ).toBeGreaterThan(0);
    expect(screen.getAllByText("Marta Demopatient").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Critical").length).toBeGreaterThan(0);
    expect(screen.getAllByText("Routine").length).toBeGreaterThan(0);
  });

  it("filters to critical results only and resets filters", async () => {
    setup();
    await screen.findAllByText("Carlos Demopatient");
    await userEvent.selectOptions(
      screen.getByLabelText("Criticality"),
      "critical",
    );
    expect(screen.queryAllByText("Marta Demopatient")).toHaveLength(0);
    expect(screen.getAllByText("Carlos Demopatient").length).toBeGreaterThan(
      0,
    );
    await userEvent.click(
      screen.getByRole("button", { name: "Reset filters" }),
    );
    expect(
      (await screen.findAllByText("Marta Demopatient")).length,
    ).toBeGreaterThan(0);
  });

  it("filters by patient search and shows a no-match state", async () => {
    setup();
    await screen.findAllByText("Carlos Demopatient");
    await userEvent.type(
      screen.getByLabelText("Filter by patient name or identifier"),
      "Marta",
    );
    expect(screen.queryAllByText("Carlos Demopatient")).toHaveLength(0);
    await userEvent.type(
      screen.getByLabelText("Filter by patient name or identifier"),
      "nobody-matches",
    );
    expect(
      await screen.findByText("No results match the current filters."),
    ).toBeInTheDocument();
  });

  it("shows an empty state when there are no open results", async () => {
    setup({ items: [] });
    expect(
      await screen.findByText("No open results. All loops are closed."),
    ).toBeInTheDocument();
  });
});
