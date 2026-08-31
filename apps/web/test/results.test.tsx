import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
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

function setup(allItems: typeof ITEMS = ITEMS) {
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/session")
        return Promise.resolve(jsonResponse({ authenticated: true }));
      if (url.startsWith("/api/v1/worklist")) {
        // Mirror the API's server-side filtering.
        const params = new URL(url, "http://localhost").searchParams;
        const critical = params.get("critical") === "true";
        const state = params.get("state");
        const q = params.get("query")?.toLowerCase();
        const items = allItems.filter((item) => {
          if (critical && !item.has_open_alert) return false;
          if (state && item.loop_state !== state) return false;
          if (q) {
            const hay =
              `${item.patient.family_name} ${item.patient.given_name} ${item.patient.identifier}`.toLowerCase();
            if (!hay.includes(q)) return false;
          }
          return true;
        });
        const offset = Number(params.get("offset") ?? "0");
        const page = items.slice(offset, offset + 200);
        return Promise.resolve(
          jsonResponse({ items: page, has_more: offset + 200 < items.length }),
        );
      }
      if (url === "/api/v1/meta/tenant")
        return Promise.resolve(
          jsonResponse({
            tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
            user: {
              username: "dr.garcia",
              display_name: "Dr. García",
              roles: ["physician"],
            },
            facilities: [
              {
                id: "f",
                name: "Central Hospital",
                accessible: true,
                can_register: false,
                can_act_clinically: true,
              },
            ],
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
    await waitFor(() =>
      expect(screen.queryAllByText("Marta Demopatient")).toHaveLength(0),
    );
    expect(screen.getAllByText("Carlos Demopatient").length).toBeGreaterThan(0);
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
    await waitFor(
      () => expect(screen.queryAllByText("Carlos Demopatient")).toHaveLength(0),
      { timeout: 2000 },
    );
    await userEvent.type(
      screen.getByLabelText("Filter by patient name or identifier"),
      "nobody-matches",
    );
    expect(
      await screen.findByText(
        "No results match the current filters.",
        {},
        { timeout: 2000 },
      ),
    ).toBeInTheDocument();
  });

  it("loads older results beyond the first page on demand", async () => {
    const many = Array.from({ length: 201 }, (_, i) => ({
      ...ITEMS[1],
      id: `33333333-3333-3333-3333-${String(i).padStart(12, "0")}`,
      patient: {
        family_name: i === 200 ? "Oldest" : "Bulk",
        given_name: `Row${i}`,
        identifier: `SYN-${i}`,
      },
    }));
    setup(many);
    const button = await screen.findByRole("button", {
      name: "Load more results",
    });
    expect(screen.queryAllByText(/Row200 Oldest/)).toHaveLength(0);
    await userEvent.click(button);
    expect(
      (await screen.findAllByText(/Row200 Oldest/)).length,
    ).toBeGreaterThan(0);
    expect(
      screen.queryByRole("button", { name: "Load more results" }),
    ).not.toBeInTheDocument();
  });

  it("shows an empty state when there are no open results", async () => {
    setup([]);
    expect(
      await screen.findByText("No open results. All loops are closed."),
    ).toBeInTheDocument();
  });
});
