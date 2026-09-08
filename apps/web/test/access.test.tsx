import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import AccessPage from "@/app/access/page";
import { SessionProvider } from "@/lib/session";

const push = vi.fn();
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push, replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => "/access",
}));

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function meta(roles: string[], facilities = 1) {
  return {
    tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
    user: { username: "u", display_name: "Test User", roles },
    facilities: Array.from({ length: facilities }, (_, i) => ({
      id: `f${i + 1}`,
      name: i === 0 ? "Central Hospital" : "Annex Clinic",
      accessible: true,
      can_register: roles.includes("registration_staff"),
      can_act_clinically: roles.includes("physician"),
    })),
  };
}

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
    arrival_kind: "scheduled",
    service: "general_medicine",
    reason: "Diabetes follow-up",
    scheduled_at: "2026-08-29T10:30:00Z",
    arrived_at: null,
    ready_at: null,
    consultation_started_at: null,
    wait_minutes: null,
    priority: null,
    handoff_summary: null,
    encounter_id: null,
    version: 4,
    updated_at: "2026-08-29T08:10:00Z",
    facility: { id: "f1", name: "Central Hospital" },
    patient: {
      id: "p1",
      family_name: "Demopatient",
      given_name: "Carlos",
      identifier: "SYN-0003",
      age_years: 64,
      alert_count: 0,
      allergy_count: 0,
    },
    assignment: null,
    open_alerts: 0,
    capabilities: { ...CAPS_NONE, ...caps },
    ...extra,
  };
}

type Handler = (
  url: string,
  init: RequestInit | undefined,
) => Response | undefined;

function setup(
  roles: string[],
  items: ReturnType<typeof visit>[],
  options: { facilities?: number; handler?: Handler; lang?: "en" | "es" } = {},
) {
  if (options.lang) localStorage.setItem("wellos.lang", options.lang);
  const posts: { url: string; body: unknown }[] = [];
  const loads: string[] = [];
  const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    const custom = options.handler?.(url, init);
    if (custom) return Promise.resolve(custom);
    if (url === "/api/session")
      return Promise.resolve(jsonResponse({ authenticated: true }));
    if (url === "/api/v1/meta/tenant")
      return Promise.resolve(jsonResponse(meta(roles, options.facilities)));
    if (url.startsWith("/api/v1/visits?")) {
      loads.push(url);
      return Promise.resolve(jsonResponse({ items }));
    }
    if (url === "/api/v1/alerts")
      return Promise.resolve(jsonResponse({ items: [] }));
    if (url.startsWith("/api/v1/patients?query="))
      return Promise.resolve(
        jsonResponse({
          patients: [
            {
              id: "p9",
              family_name: "Demopatient",
              given_name: "Marta",
              identifier: "SYN-0004",
              birth_date: "1988-11-21",
              can_open_chart: false,
              can_start_encounter: false,
              open_consultation_id: null,
            },
          ],
        }),
      );
    if (init?.method === "POST") {
      posts.push({ url, body: JSON.parse(String(init.body ?? "null")) });
      return Promise.resolve(jsonResponse({ ok: true }));
    }
    return Promise.resolve(jsonResponse({}));
  });
  vi.stubGlobal("fetch", fetchMock);
  render(
    <SessionProvider>
      <AccessPage />
    </SessionProvider>,
  );
  return { posts, loads };
}

describe("patient access board", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    localStorage.clear();
    push.mockClear();
  });

  it("opens on the role's usual board and shows human-readable visits only", async () => {
    const { loads } = setup(
      ["registration_staff"],
      [visit("v1", "scheduled", { can_arrive: true, can_cancel: true })],
    );
    const tab = await screen.findByRole("tab", { name: "Arrivals" });
    expect(tab).toHaveAttribute("aria-selected", "true");
    await waitFor(() => expect(loads[0]).toBe("/api/v1/visits?view=access"));
    const card = await screen.findByRole("listitem", {
      name: "Carlos Demopatient — Scheduled",
    });
    expect(within(card).getByText("Diabetes follow-up")).toBeInTheDocument();
    expect(within(card).getByText(/SYN-0003/)).toBeInTheDocument();
    expect(card.textContent).not.toContain("v1");
    // Registration staff can register arrivals but never triage or consult.
    expect(
      screen.getByRole("heading", { name: "New arrival or appointment" }),
    ).toBeInTheDocument();
    expect(within(card).queryByRole("link", { name: /triage/i })).toBeNull();
    expect(
      within(card).queryByRole("button", { name: "Start consultation" }),
    ).toBeNull();
  });

  it("marks a visit as arrived with its current version and refreshes the list", async () => {
    const user = userEvent.setup();
    const { posts, loads } = setup(
      ["registration_staff"],
      [visit("v1", "scheduled", { can_arrive: true })],
    );
    await user.click(
      await screen.findByRole("button", { name: "Mark arrived" }),
    );
    await waitFor(() =>
      expect(posts).toEqual([
        { url: "/api/v1/visits/v1/arrive", body: { version: 4 } },
      ]),
    );
    await waitFor(() => expect(loads).toHaveLength(2));
  });

  it("asks for confirmation before cancelling and can back out", async () => {
    const user = userEvent.setup();
    const { posts } = setup(
      ["registration_staff"],
      [visit("v1", "scheduled", { can_cancel: true })],
    );
    await user.click(
      await screen.findByRole("button", { name: "Cancel visit" }),
    );
    const box = screen.getByRole("group");
    expect(
      within(box).getByText(
        "Cancel this visit? The patient leaves today's lists.",
      ),
    ).toBeInTheDocument();
    await user.click(within(box).getByRole("button", { name: "Cancel" }));
    expect(screen.queryByRole("group")).toBeNull();
    expect(posts).toEqual([]);
    await user.click(screen.getByRole("button", { name: "Cancel visit" }));
    await user.click(screen.getByRole("button", { name: "Confirm" }));
    await waitFor(() =>
      expect(posts).toEqual([
        { url: "/api/v1/visits/v1/cancel", body: { version: 4 } },
      ]),
    );
  });

  it("explains a version conflict and reloads the current state", async () => {
    const user = userEvent.setup();
    const { loads } = setup(
      ["registration_staff"],
      [visit("v1", "scheduled", { can_arrive: true })],
      {
        handler: (url, init) =>
          url === "/api/v1/visits/v1/arrive" && init?.method === "POST"
            ? jsonResponse(
                { error: { code: "version_conflict", message: "stale" } },
                409,
              )
            : undefined,
      },
    );
    await user.click(
      await screen.findByRole("button", { name: "Mark arrived" }),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /changed in the meantime/i,
    );
    await waitFor(() => expect(loads).toHaveLength(2));
  });

  it("shows a permission-denied state instead of a partial board", async () => {
    setup(["registration_staff"], [], {
      handler: (url) =>
        url.startsWith("/api/v1/visits?")
          ? jsonResponse(
              { error: { code: "forbidden", message: "forbidden" } },
              403,
            )
          : undefined,
    });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "You do not have permission to view this information.",
    );
    expect(screen.queryByRole("tablist")).toBeNull();
  });

  it("tells roles without access or triage that the board is not for them", async () => {
    const { loads } = setup(["laboratory_professional"], []);
    expect(await screen.findByRole("alert")).toHaveTextContent(
      "Your role does not include patient access or triage.",
    );
    expect(loads).toEqual([]);
  });

  it("switches boards with the tabs and reloads the matching view", async () => {
    const user = userEvent.setup();
    const { loads } = setup(
      ["nurse"],
      [
        visit(
          "v2",
          "arrived",
          { can_triage: true },
          { arrival_kind: "walk_in" },
        ),
      ],
    );
    const triageTab = await screen.findByRole("tab", { name: "Triage" });
    expect(triageTab).toHaveAttribute("aria-selected", "true");
    await waitFor(() => expect(loads[0]).toBe("/api/v1/visits?view=triage"));
    expect(screen.getByRole("link", { name: "Open triage" })).toHaveAttribute(
      "href",
      "/visits/v2/triage",
    );
    await user.click(screen.getByRole("tab", { name: "Ready" }));
    await waitFor(() => expect(loads).toContain("/api/v1/visits?view=ready"));
    expect(screen.getByRole("tab", { name: "Ready" })).toHaveAttribute(
      "aria-selected",
      "true",
    );
  });

  it("registers a walk-in from patient search without exposing identifiers", async () => {
    const user = userEvent.setup();
    const { posts } = setup(["registration_staff"], []);
    await screen.findByRole("heading", { name: "New arrival or appointment" });
    await user.type(screen.getByLabelText("Find the patient"), "Marta");
    await user.click(screen.getByRole("button", { name: "Search" }));
    expect(await screen.findByText("Marta Demopatient")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Choose" }));
    expect(
      screen.getByRole("button", { name: "Change patient" }),
    ).toBeInTheDocument();
    await user.click(screen.getByRole("radio", { name: "Walk-in" }));
    await user.type(screen.getByLabelText(/Reason for visit/), "Cough");
    await user.click(screen.getByRole("button", { name: "Register" }));
    await waitFor(() =>
      expect(posts).toEqual([
        {
          url: "/api/v1/visits",
          body: {
            patient_id: "p9",
            arrival_kind: "walk_in",
            service: "general_medicine",
            reason: "Cough",
            scheduled_at: null,
          },
        },
      ]),
    );
    expect(
      screen.getByText(
        "Registered. Marta Demopatient — Walk-in, General medicine.",
      ),
    ).toBeInTheDocument();
  });

  it("filters by facility only when the user works at more than one", async () => {
    const user = userEvent.setup();
    const { loads } = setup(
      ["clinical_administrator"],
      [visit("v1", "scheduled")],
      { facilities: 2 },
    );
    const select = await screen.findByLabelText("Facility");
    const card = await screen.findByRole("listitem", {
      name: "Carlos Demopatient — Scheduled",
    });
    expect(within(card).getByText("Central Hospital")).toBeInTheDocument();
    await user.selectOptions(select, "f2");
    await waitFor(() =>
      expect(loads).toContain("/api/v1/visits?view=access&facility_id=f2"),
    );
  });

  it("renders the board in Spanish", async () => {
    setup(
      ["registration_staff"],
      [visit("v1", "scheduled", { can_arrive: true, can_cancel: true })],
      { lang: "es" },
    );
    expect(
      await screen.findByRole("heading", {
        name: "Nueva llegada o cita",
      }),
    ).toBeInTheDocument();
    expect(screen.getByRole("tab", { name: "Llegadas" })).toBeInTheDocument();
    expect(
      screen.getByRole("listitem", { name: "Carlos Demopatient — Programada" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Marcar llegada" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Cancelar visita" }),
    ).toBeInTheDocument();
  });
});
