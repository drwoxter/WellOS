import { beforeEach, describe, expect, it, vi } from "vitest";
import { useState } from "react";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import SchedulingPage from "@/app/scheduling/page";
import { CapacityPanel } from "@/app/scheduling/panels";
import { sortQueue, summarizeQueue } from "@/app/access/page";
import { monthGrid } from "@/app/scheduling/agenda";
import {
  CatalogCombobox,
  Worklist,
  WorklistRow,
} from "@/app/scheduling/shared";
import { SessionProvider } from "@/lib/session";
import {
  NO_SCHEDULING_CAPABILITIES,
  type CatalogEntry,
  type SchedulingCapabilities,
} from "@/lib/access";
import type { VisitItem } from "@/lib/visits";

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => "/scheduling",
}));

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function visit(over: Partial<VisitItem>): VisitItem {
  return {
    id: "v",
    status: "waiting_triage",
    arrival_kind: "scheduled",
    service: "general_medicine",
    reason: null,
    scheduled_at: null,
    arrived_at: "2026-10-04T08:00:00Z",
    ready_at: null,
    consultation_started_at: null,
    wait_minutes: 0,
    priority: null,
    handoff_summary: null,
    encounter_id: null,
    version: 1,
    updated_at: "2026-10-04T08:00:00Z",
    facility: { id: "f1", name: "Central" },
    patient: {
      id: "p",
      family_name: "Demopatient",
      given_name: "A",
      identifier: "SYN-1",
      age_years: 40,
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
    ...over,
  } as VisitItem;
}

describe("access queue ordering", () => {
  it("orders by triage priority, then by longest wait, then stable", () => {
    const items = [
      visit({ id: "a", priority: "standard", wait_minutes: 10 }),
      visit({ id: "b", priority: null, wait_minutes: 95 }),
      visit({ id: "c", priority: "immediate", wait_minutes: 2 }),
      visit({ id: "d", priority: "standard", wait_minutes: 40 }),
      visit({ id: "e", priority: "urgent", wait_minutes: null }),
      visit({ id: "f", priority: null, wait_minutes: 95 }),
    ];
    expect(sortQueue(items).map((v) => v.id)).toEqual([
      "c",
      "e",
      "d",
      "a",
      "b",
      "f",
    ]);
  });

  it("summarises arrival contexts and the longest wait", () => {
    const s = summarizeQueue([
      visit({ arrival_kind: "scheduled", wait_minutes: 12 }),
      visit({ arrival_kind: "walk_in", priority: "urgent", wait_minutes: 70 }),
      visit({ arrival_kind: "urgent", wait_minutes: null }),
      visit({ arrival_kind: "remote", priority: "immediate", wait_minutes: 5 }),
    ]);
    expect(s).toEqual({
      total: 4,
      urgent: 3,
      scheduled: 1,
      walkIn: 1,
      longest: 70,
    });
  });
});

describe("agenda month grid", () => {
  it("covers the whole month in Monday-start weeks", () => {
    const g = monthGrid("2026-10-14");
    expect(g.days[0]).toBe("2026-09-28");
    expect(g.days[g.days.length - 1]).toBe("2026-11-01");
    expect(g.days.length % 7).toBe(0);
    expect(g.days).toContain("2026-10-01");
    expect(g.days).toContain("2026-10-31");
  });

  it("does not pad a month that already starts on Monday", () => {
    const g = monthGrid("2026-06-10");
    expect(g.days[0]).toBe("2026-06-01");
    expect(g.days[g.days.length - 1]).toBe("2026-07-05");
  });
});

describe("catalog combobox", () => {
  const entries: CatalogEntry[] = [
    {
      id: "1",
      kind: "clinical_service",
      code: "general_medicine_consultation",
      parent_id: null,
      name_en: "General medicine consultation",
      name_es: "Consulta de medicina general",
      synonyms: ["GP"],
      external_codings: null,
      config: null,
      active: true,
      version: 1,
      created_at: "",
      updated_at: "",
    } as unknown as CatalogEntry,
    {
      id: "2",
      kind: "clinical_service",
      code: "cardiology_consultation",
      parent_id: null,
      name_en: "Cardiology consultation",
      name_es: "Consulta de cardiología",
      synonyms: [],
      external_codings: null,
      config: null,
      active: true,
      version: 1,
      created_at: "",
      updated_at: "",
    } as unknown as CatalogEntry,
  ];

  it("filters by typed text and reports the catalog code", async () => {
    const user = userEvent.setup();
    const onChange = vi.fn();
    render(
      <CatalogCombobox
        id="svc"
        lang="en"
        label="Service"
        entries={entries}
        value=""
        onChange={onChange}
      />,
    );
    const input = screen.getByRole("combobox", { name: "Service" });
    await user.type(input, "cardio");
    const options = within(screen.getByRole("listbox")).getAllByRole("option");
    expect(options).toHaveLength(1);
    expect(options[0]).toHaveTextContent("Cardiology consultation");
    await user.keyboard("{Enter}");
    expect(onChange).toHaveBeenCalledWith("cardiology_consultation");
  });

  it("keeps the typed text when a selection is edited over", async () => {
    const user = userEvent.setup();
    function Stateful() {
      const [code, setCode] = useState("");
      return (
        <CatalogCombobox
          id="svc"
          lang="en"
          label="Service"
          entries={entries}
          value={code}
          onChange={setCode}
        />
      );
    }
    render(<Stateful />);
    const input = screen.getByRole("combobox", { name: "Service" });
    await user.type(input, "cardio");
    await user.keyboard("{Enter}");
    expect(input).toHaveValue("Cardiology consultation");
    await user.keyboard("{Backspace}");
    expect(input).toHaveValue("Cardiology consultatio");
    await user.keyboard("{Enter}");
    expect(input).toHaveValue("Cardiology consultation");
  });

  it("matches synonyms and shows Spanish names", async () => {
    const user = userEvent.setup();
    render(
      <CatalogCombobox
        id="svc"
        lang="es"
        label="Servicio"
        entries={entries}
        value="general_medicine_consultation"
        onChange={() => {}}
      />,
    );
    const input = screen.getByRole("combobox", { name: "Servicio" });
    expect(input).toHaveValue("Consulta de medicina general");
    await user.clear(input);
    await user.type(input, "GP");
    expect(
      within(screen.getByRole("listbox")).getAllByRole("option"),
    ).toHaveLength(1);
  });
});

function meta(caps: SchedulingCapabilities) {
  return {
    tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
    user: { username: "u", display_name: "Test User", roles: [] },
    facilities: [
      { id: "f1", name: "Central Hospital", accessible: true },
      { id: "f2", name: "Annex Clinic", accessible: true },
    ],
    scheduling_capabilities: caps,
    environment: { synthetic_data: true },
  };
}

function setupConsole(caps: SchedulingCapabilities) {
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/session")
        return Promise.resolve(jsonResponse({ authenticated: true }));
      if (url === "/api/v1/meta/tenant")
        return Promise.resolve(jsonResponse(meta(caps)));
      return Promise.resolve(
        jsonResponse({ items: [], entries: [], next_cursor: null }),
      );
    }),
  );
  render(
    <SessionProvider>
      <SchedulingPage />
    </SessionProvider>,
  );
}

describe("scheduling console navigation", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    localStorage.clear();
  });

  it("groups the eight workspaces by intent in a vertical tablist", async () => {
    setupConsole({
      ...NO_SCHEDULING_CAPABILITIES,
      can_read: true,
      can_manage: true,
      can_manage_waitlist: true,
      can_coordinate_transport: true,
      can_review_capacity: true,
      can_manage_catalog: true,
    });
    await screen.findByRole("tab", { name: "Find the best appointment" });
    const tablist = screen.getByRole("tablist", {
      name: "Scheduling console",
    });
    expect(tablist).toHaveAttribute("aria-orientation", "vertical");
    expect(within(tablist).getAllByRole("tab")).toHaveLength(8);
    const nav = screen.getByRole("navigation", {
      name: "Scheduling workspaces",
    });
    expect(nav).toHaveTextContent("Plan and book");
    expect(nav).toHaveTextContent("Work queues");
    expect(nav).toHaveTextContent("Operations");
    expect(nav).toHaveTextContent("Administration");
    expect(within(nav).getByRole("link", { name: /Catalog/ })).toHaveAttribute(
      "href",
      "/scheduling/catalog",
    );
    expect(
      screen.getByRole("tab", { name: "Find the best appointment" }),
    ).toHaveAttribute("aria-selected", "true");
  });

  it("keyboard order follows the groups and wraps", async () => {
    const user = userEvent.setup();
    setupConsole({
      ...NO_SCHEDULING_CAPABILITIES,
      can_read: true,
      can_manage: true,
      can_review_capacity: true,
    });
    const find = await screen.findByRole("tab", {
      name: "Find the best appointment",
    });
    find.focus();
    await user.keyboard("{ArrowDown}");
    expect(screen.getByRole("tab", { name: "Agenda" })).toHaveFocus();
    await user.keyboard("{End}");
    expect(
      screen.getByRole("tab", { name: "Transport coordination" }),
    ).toHaveAttribute("aria-selected", "true");
    await user.keyboard("{ArrowRight}");
    expect(find).toHaveFocus();
    // The breadcrumb names the active group and workspace.
    expect(screen.getByRole("tabpanel")).toHaveTextContent(
      /Plan and book.*Find the best appointment/,
    );
  });

  it("a single-capability role sees one tab and no group headings", async () => {
    setupConsole({
      ...NO_SCHEDULING_CAPABILITIES,
      can_coordinate_transport: true,
    });
    const tabs = await screen.findAllByRole("tab");
    expect(tabs).toHaveLength(1);
    expect(tabs[0]).toHaveTextContent("Transport coordination");
    const nav = screen.getByRole("navigation", {
      name: "Scheduling workspaces",
    });
    expect(nav).not.toHaveTextContent("Operations");
    expect(screen.queryByTestId("find-cta")).toBeNull();
  });
});

describe("capacity heatmap", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    localStorage.clear();
  });

  it("renders one cell per forecast day and marks closed days", async () => {
    const user = userEvent.setup();
    const days = Array.from({ length: 7 }, (_, i) => {
      const date = `2026-10-${String(5 + i).padStart(2, "0")}`;
      return {
        date,
        expected_demand: i === 2 ? 9 : 3,
        available_capacity: i === 4 ? 0 : 10,
        gap: 0,
        demand_low: 2,
        demand_high: 4,
        factors: [],
      };
    });
    vi.stubGlobal(
      "fetch",
      vi.fn((input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/session")
          return Promise.resolve(jsonResponse({ authenticated: true }));
        if (url === "/api/v1/meta/tenant")
          return Promise.resolve(
            jsonResponse(
              meta({
                ...NO_SCHEDULING_CAPABILITIES,
                can_review_capacity: true,
              }),
            ),
          );
        if (url.startsWith("/api/v1/capacity/forecasts"))
          return Promise.resolve(
            jsonResponse({
              forecasts: [
                {
                  id: "fc1",
                  facility_id: "f1",
                  service_code: "general_medicine_consultation",
                  forecast_version: "capacity-forecast.v1",
                  horizon_start: "2026-10-05",
                  horizon_end: "2026-10-11",
                  status: "ready",
                  inputs_hash: "h",
                  forecast: {
                    status: "ready",
                    version: "capacity-forecast.v1",
                    days,
                    confidence: 0.7,
                    history_weeks: 8,
                    recommendations: [],
                    pressure_days: ["2026-10-07"],
                  },
                  explanation_artifact_id: null,
                  created_at: "2026-10-04T08:00:00Z",
                },
              ],
            }),
          );
        return Promise.resolve(
          jsonResponse({ items: [], entries: [], next_cursor: null }),
        );
      }),
    );
    render(
      <SessionProvider>
        <CapacityPanel
          lang="en"
          facilityId="f1"
          facilities={[{ id: "f1", name: "Central" } as never]}
        />
      </SessionProvider>,
    );
    await user.click(
      await screen.findByRole("button", { name: "View evidence" }),
    );
    const heat = await screen.findByTestId("capacity-heatmap");
    const table = within(heat).getByRole("table", { name: "Capacity heatmap" });
    const cells = within(table).getAllByRole("cell");
    expect(cells.filter((c) => c.getAttribute("aria-label"))).toHaveLength(7);
    expect(
      within(table).getByRole("cell", { name: /no capacity \(closed\)/ }),
    ).toHaveClass("closed");
    expect(
      within(table).getByRole("cell", { name: /9\.0 expected of 10 capacity/ }),
    ).toHaveClass("l4");
    expect(heat).toHaveTextContent("1 of 7 days under pressure");
    // The accessible evidence table is still there next to the heatmap.
    expect(
      within(screen.getByTestId("forecast-detail")).getByRole("columnheader", {
        name: "Expected demand",
      }),
    ).toBeInTheDocument();
  });
});

describe("Worklist master-detail", () => {
  type Item = { id: string; name: string; v: number };
  const items: Item[] = [
    { id: "a", name: "Alpha", v: 1 },
    { id: "b", name: "Beta", v: 1 },
    { id: "c", name: "Gamma", v: 1 },
  ];
  const ui = (list: Item[]) => (
    <Worklist
      lang="en"
      label="Things"
      items={list}
      keyOf={(i) => i.id}
      renderRow={(i) => <WorklistRow title={i.name} meta={`v${i.v}`} />}
      renderDetail={(i) => (
        <li data-testid="detail">
          {i.name} detail v{i.v}
        </li>
      )}
    />
  );

  it("selects the first item by default, then the clicked one, and keeps it across reloads", async () => {
    const user = userEvent.setup();
    const { rerender } = render(ui(items));
    expect(screen.getByTestId("detail")).toHaveTextContent("Alpha detail");
    const rows = within(
      screen.getByRole("list", { name: "Things" }),
    ).getAllByRole("button");
    expect(rows).toHaveLength(3);
    expect(rows[0]).toHaveAttribute("aria-current", "true");
    await user.click(rows[1]);
    expect(screen.getByTestId("detail")).toHaveTextContent("Beta detail");
    expect(rows[1]).toHaveAttribute("aria-current", "true");
    expect(rows[0]).not.toHaveAttribute("aria-current");
    // Reload with fresh objects: the same record stays selected and shows new data.
    rerender(ui(items.map((i) => ({ ...i, v: 2 }))));
    expect(screen.getByTestId("detail")).toHaveTextContent("Beta detail v2");
    // The selected record disappears: fall back to the first one.
    rerender(ui(items.filter((i) => i.id !== "b")));
    expect(screen.getByTestId("detail")).toHaveTextContent("Alpha detail");
  });

  it("moves focus and the detail together with the arrow keys and wraps", async () => {
    const user = userEvent.setup();
    render(ui(items));
    const rows = within(
      screen.getByRole("list", { name: "Things" }),
    ).getAllByRole("button");
    rows[0].focus();
    await user.keyboard("{ArrowDown}");
    expect(rows[1]).toHaveFocus();
    expect(rows[1]).toHaveAttribute("aria-current", "true");
    expect(screen.getByTestId("detail")).toHaveTextContent("Beta detail");
    await user.keyboard("{End}");
    expect(rows[2]).toHaveFocus();
    expect(screen.getByTestId("detail")).toHaveTextContent("Gamma detail");
    await user.keyboard("{ArrowDown}");
    expect(rows[0]).toHaveFocus();
    expect(screen.getByTestId("detail")).toHaveTextContent("Alpha detail");
    await user.keyboard("{ArrowUp}");
    expect(rows[2]).toHaveFocus();
    await user.keyboard("{Enter}");
    expect(screen.getByTestId("detail")).toHaveTextContent("Gamma detail");
  });

  it("explains an empty selection", () => {
    render(ui([]));
    expect(
      screen.getByText("Select an item to see its details."),
    ).toBeInTheDocument();
  });
});
