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
    can_open_detail: true,
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
    can_open_detail: true,
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
        // Mirror the API's keyset cursor: `<index>` of the last row of the
        // previous page (the real API encodes the ordering tuple).
        const cursor = params.get("cursor");
        const start = cursor
          ? items.findIndex((item) => item.id === cursor) + 1
          : 0;
        const page = items.slice(start, start + 200);
        const hasMore = start + 200 < items.length;
        return Promise.resolve(
          jsonResponse({
            items: page,
            has_more: hasMore,
            next_cursor: hasMore ? page[page.length - 1]?.id : null,
          }),
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

  it("ignores a stale response after filters change", async () => {
    let resolveSlow: ((r: Response) => void) | null = null;
    vi.stubGlobal(
      "fetch",
      vi.fn((input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/session")
          return Promise.resolve(jsonResponse({ authenticated: true }));
        if (url === "/api/v1/meta/tenant")
          return Promise.resolve(
            jsonResponse({
              tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
              user: {
                username: "dr.garcia",
                display_name: "Dr. García",
                roles: ["physician"],
              },
              facilities: [],
            }),
          );
        if (url.startsWith("/api/v1/worklist")) {
          const params = new URL(url, "http://localhost").searchParams;
          if (params.get("critical") === "true") {
            // The critical-only request stays pending until after the user
            // has already switched back to the unfiltered view.
            return new Promise<Response>((resolve) => {
              resolveSlow = resolve;
            });
          }
          return Promise.resolve(
            jsonResponse({ items: ITEMS, has_more: false }),
          );
        }
        return Promise.resolve(jsonResponse({}));
      }),
    );
    render(
      <SessionProvider>
        <ResultsPage />
      </SessionProvider>,
    );
    await screen.findAllByText("Marta Demopatient");
    // Switch to critical-only (request hangs), then reset filters (resolves
    // immediately with all items).
    await userEvent.selectOptions(
      screen.getByLabelText("Criticality"),
      "critical",
    );
    await waitFor(() => expect(resolveSlow).not.toBeNull());
    await userEvent.click(
      screen.getByRole("button", { name: "Reset filters" }),
    );
    await screen.findAllByText("Marta Demopatient");
    // The stale critical-only response arrives late; it must not replace
    // the current unfiltered view.
    resolveSlow!(jsonResponse({ items: [ITEMS[0]], has_more: false }));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(screen.getAllByText("Marta Demopatient").length).toBeGreaterThan(0);
  });

  it("clears stale rows and shows loading while a filter change is in flight", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn((input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/session")
          return Promise.resolve(jsonResponse({ authenticated: true }));
        if (url === "/api/v1/meta/tenant")
          return Promise.resolve(
            jsonResponse({
              tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
              user: {
                username: "dr.garcia",
                display_name: "Dr. García",
                roles: ["physician"],
              },
              facilities: [],
            }),
          );
        if (url.startsWith("/api/v1/worklist")) {
          const params = new URL(url, "http://localhost").searchParams;
          if (params.get("critical") === "true") {
            // The critical-only request never resolves within the test.
            return new Promise<Response>(() => {});
          }
          return Promise.resolve(
            jsonResponse({ items: ITEMS, has_more: false }),
          );
        }
        return Promise.resolve(jsonResponse({}));
      }),
    );
    render(
      <SessionProvider>
        <ResultsPage />
      </SessionProvider>,
    );
    await screen.findAllByText("Marta Demopatient");
    await userEvent.selectOptions(
      screen.getByLabelText("Criticality"),
      "critical",
    );
    // The previous unfiltered rows must not be shown under the new filter.
    await waitFor(() =>
      expect(screen.queryByText("Marta Demopatient")).not.toBeInTheDocument(),
    );
    expect(screen.getByRole("status")).toHaveTextContent("Loading");
    // Filter controls stay available while the replacement page loads.
    expect(
      screen.getByRole("button", { name: "Reset filters" }),
    ).toBeInTheDocument();
  });

  it("hides the result link when the server denies detail access", async () => {
    setup([
      { ...ITEMS[0], can_open_detail: false },
      { ...ITEMS[1], can_open_detail: true },
    ]);
    await screen.findAllByText("Carlos Demopatient");
    // One item allows detail (desktop + mobile render), one does not.
    expect(screen.getAllByRole("link", { name: "Open result" })).toHaveLength(
      2,
    );
  });

  it("re-enables pagination when filters change during a hung load-more", async () => {
    let resolveSlow: ((r: Response) => void) | null = null;
    const page = (start: number, count: number, total: number) => ({
      items: Array.from({ length: count }, (_, i) => ({
        ...ITEMS[1],
        id: `44444444-4444-4444-4444-${String(start + i).padStart(12, "0")}`,
        patient: {
          family_name: "Bulk",
          given_name: `Row${start + i}`,
          identifier: `SYN-${start + i}`,
        },
      })),
      has_more: start + count < total,
      next_cursor: start + count < total ? String(start + count) : null,
    });
    vi.stubGlobal(
      "fetch",
      vi.fn((input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/session")
          return Promise.resolve(jsonResponse({ authenticated: true }));
        if (url === "/api/v1/meta/tenant")
          return Promise.resolve(
            jsonResponse({
              tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
              user: {
                username: "dr.garcia",
                display_name: "Dr. García",
                roles: ["physician"],
              },
              facilities: [],
            }),
          );
        if (url.startsWith("/api/v1/worklist")) {
          const params = new URL(url, "http://localhost").searchParams;
          const cursor = params.get("cursor");
          if (cursor && params.get("critical") !== "true") {
            // The unfiltered load-more request hangs until after the user
            // switches filters.
            return new Promise<Response>((resolve) => {
              resolveSlow = resolve;
            });
          }
          const start = cursor ? Number(cursor) : 0;
          return Promise.resolve(jsonResponse(page(start, 200, 400)));
        }
        return Promise.resolve(jsonResponse({}));
      }),
    );
    render(
      <SessionProvider>
        <ResultsPage />
      </SessionProvider>,
    );
    const button = await screen.findByRole("button", {
      name: "Load more results",
    });
    await userEvent.click(button);
    await waitFor(() => expect(resolveSlow).not.toBeNull());
    // Switching filters starts a fresh base load; the stale pagination
    // request must not keep the new view's load-more disabled.
    await userEvent.selectOptions(
      screen.getByLabelText("Criticality"),
      "critical",
    );
    const newButton = await screen.findByRole("button", {
      name: "Load more results",
    });
    await waitFor(() => expect(newButton).toBeEnabled());
    resolveSlow!(jsonResponse(page(200, 200, 400)));
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(
      screen.getByRole("button", { name: "Load more results" }),
    ).toBeEnabled();
  });

  it("shows an empty state when there are no open results", async () => {
    setup([]);
    expect(
      await screen.findByText("No open results. All loops are closed."),
    ).toBeInTheDocument();
  });
});
