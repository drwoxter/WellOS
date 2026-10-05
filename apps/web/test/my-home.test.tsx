import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import PatientHomePage from "@/app/my/page";
import { SessionProvider } from "@/lib/session";
import { homeForRoles } from "@/lib/dev-users";
import { greetingKey, referenceBand, trendPoints } from "@/lib/home";

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => "/my",
}));

const META = {
  tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
  user: {
    username: "rep.ortiz",
    display_name: "Marta Ortiz",
    roles: ["patient_representative"],
  },
  facilities: [],
  environment: { name: "development", synthetic_data: true },
  scheduling_capabilities: {
    can_read: false,
    can_manage: false,
    can_manage_catalog: false,
    can_manage_resources: false,
    can_manage_waitlist: false,
    can_coordinate_transport: false,
    can_review_capacity: false,
    can_manage_grants: false,
    self_service: true,
  },
  diagnostics_capabilities: {
    can_read: false,
    can_order: false,
    can_override_safety: false,
    can_fulfil: false,
    can_handle_specimens: false,
    can_write_reports: false,
    can_review: false,
    can_release: false,
    can_manage_catalog: false,
    self_service: true,
  },
};

const ME = {
  user_id: "u1",
  display_name: "Marta Ortiz",
  patients: [
    {
      grant_id: "g1",
      patient_id: "pa",
      relationship: "parent",
      patient: { given_name: "Lucía", family_name: "Ortiz" },
      expires_at: null,
    },
    {
      grant_id: "g2",
      patient_id: "pb",
      relationship: "parent",
      patient: { given_name: "Mateo", family_name: "Ortiz" },
      expires_at: null,
    },
  ],
};

const soon = new Date(Date.now() + 36 * 3600 * 1000).toISOString();
const APPT = {
  id: "a1",
  facility_id: "f1",
  facility_name: "Norte",
  patient_id: "pa",
  service_code: "echo",
  service: {
    code: "echo",
    name_en: "Echocardiogram",
    name_es: "Ecocardiograma",
    preparation_en: "Arrive 10 minutes early.",
    preparation_es: "Llegue 10 minutos antes.",
  },
  modality_code: "in_person",
  status: "confirmed",
  starts_at: soon,
  ends_at: soon,
  time_zone: "Europe/Madrid",
  reason: null,
  access_request_id: null,
  offer_id: null,
  matcher_run_id: null,
  candidate_id: null,
  score: null,
  primary_resource_id: null,
  resources: [],
  visit_id: null,
  confirmation_required: true,
  patient_confirmed_at: null,
  confirmation_due_at: soon,
  booked_via: "representative",
  override_reason: null,
  rescheduled_from: null,
  rescheduled_to: null,
  cancellation_reason: null,
  cancellation_note: null,
  cancelled_at: null,
  fulfilled_at: null,
  no_show_at: null,
  version: 3,
  created_at: "2026-10-01T09:00:00Z",
  updated_at: "2026-10-01T09:00:00Z",
};

const RELEASED = {
  id: "r1",
  service_request_id: "sr1",
  patient_id: "pa",
  order_display: "Potassium, serum",
  category_code: "laboratory",
  modality_code: null,
  status: "final",
  version: 1,
  criticality: "normal",
  conclusion: null,
  issued_at: "2026-10-02T09:00:00Z",
  effective_at: "2026-10-02T08:00:00Z",
  released_at: new Date(Date.now() - 24 * 3600 * 1000).toISOString(),
  notified: true,
  explanation_en: "Your potassium is within the expected range.",
  explanation_es: "Su potasio está dentro del rango esperado.",
  documents: [],
};

const HOME_A = {
  generated_at: new Date().toISOString(),
  patient_id: "pa",
  relationship: "parent",
  patient: { given_name: "Lucía", family_name: "Ortiz" },
  next_action: { kind: "confirm_attendance", appointment_id: "a1" },
  counts: {
    upcoming_appointments: 1,
    to_confirm: 1,
    open_requests: 0,
    options_ready: 0,
    unread_notifications: 1,
    new_results: 1,
    under_review: 1,
    pending_orders: 0,
  },
  appointments: {
    upcoming: [APPT],
    recent: [
      {
        ...APPT,
        id: "a0",
        status: "fulfilled",
        starts_at: "2026-09-30T09:00:00Z",
        ends_at: "2026-09-30T09:30:00Z",
        confirmation_required: false,
      },
    ],
  },
  requests: [],
  notifications: [
    {
      id: "n1",
      patient_id: "pa",
      kind: "appointment_reminder",
      appointment_id: "a1",
      offer_id: null,
      payload: {},
      subject: "Reminder: echocardiogram",
      body: "See you soon.",
      language: "en",
      status: "delivered",
      delivered_at: "2026-10-03T09:00:00Z",
      read_at: null,
    },
  ],
  diagnostics: {
    patient_id: "pa",
    released: [RELEASED],
    under_review: [{ service_request_id: "sr2", order_display: "Lipid panel" }],
    pending: [],
  },
  trends: [
    {
      code: "2823-3",
      unit: "mmol/L",
      display: "Potassium, serum",
      points: [
        {
          value: "4.6",
          unit: "mmol/L",
          reference_range: "3.5-5.1 mmol/L",
          interpretation: "normal",
          effective_at: "2026-06-01T08:00:00Z",
          report_id: "r0",
        },
        {
          value: "4.2",
          unit: "mmol/L",
          reference_range: "3.5-5.1 mmol/L",
          interpretation: "normal",
          effective_at: "2026-10-02T08:00:00Z",
          report_id: "r1",
        },
      ],
    },
  ],
};

const HOME_B = {
  ...HOME_A,
  patient_id: "pb",
  patient: { given_name: "Mateo", family_name: "Ortiz" },
  next_action: { kind: "none" },
  counts: {
    upcoming_appointments: 0,
    to_confirm: 0,
    open_requests: 0,
    options_ready: 0,
    unread_notifications: 0,
    new_results: 0,
    under_review: 0,
    pending_orders: 0,
  },
  appointments: { upcoming: [], recent: [] },
  notifications: [],
  diagnostics: {
    patient_id: "pb",
    released: [],
    under_review: [],
    pending: [],
  },
  trends: [],
};

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function setup(meta: unknown = META, opts: { deferB?: boolean } = {}) {
  const calls: { url: string; init?: RequestInit }[] = [];
  let confirmedA = false;
  let releaseB: (() => void) | null = null;
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      calls.push({ url, init });
      if (url === "/api/session")
        return Promise.resolve(jsonResponse({ authenticated: true }));
      if (url === "/api/v1/meta/tenant")
        return Promise.resolve(jsonResponse(meta));
      if (url === "/api/v1/me") return Promise.resolve(jsonResponse(ME));
      if (url === "/api/v1/me/home?patient_id=pa") {
        if (!confirmedA) return Promise.resolve(jsonResponse(HOME_A));
        return Promise.resolve(
          jsonResponse({
            ...HOME_A,
            next_action: { kind: "new_results", count: 1 },
            counts: { ...HOME_A.counts, to_confirm: 0 },
            appointments: {
              upcoming: [
                { ...APPT, patient_confirmed_at: new Date().toISOString() },
              ],
              recent: [],
            },
          }),
        );
      }
      if (url === "/api/v1/me/home?patient_id=pb") {
        if (!opts.deferB) return Promise.resolve(jsonResponse(HOME_B));
        return new Promise<Response>((resolve) => {
          releaseB = () => resolve(jsonResponse(HOME_B));
        });
      }
      if (url === "/api/v1/me/appointments/a1/confirm") {
        confirmedA = true;
        return Promise.resolve(jsonResponse({ ...APPT, version: 4 }));
      }
      return Promise.resolve(jsonResponse({ error: "not_found" }, 404));
    }),
  );
  render(
    <SessionProvider>
      <PatientHomePage />
    </SessionProvider>,
  );
  return Object.assign(calls, { releaseB: () => releaseB?.() });
}

describe("patient home /my", () => {
  beforeEach(() => {
    localStorage.clear();
    vi.unstubAllGlobals();
  });

  it("is the landing route for patients and representatives", () => {
    expect(homeForRoles(["patient"])).toBe("/my");
    expect(homeForRoles(["patient_representative"])).toBe("/my");
    expect(homeForRoles(["physician"])).toBe("/dashboard");
    expect(homeForRoles(["registration_staff"])).toBe("/access");
  });

  it("shows one real next action and confirms attendance through the self-service API", async () => {
    const calls = setup();
    expect(
      await screen.findByText(
        "Please confirm you will attend your appointment.",
      ),
    ).toBeInTheDocument();
    expect(
      screen.getByText("You are viewing this on behalf of", { exact: false }),
    ).toHaveTextContent("Lucía Ortiz");
    // Released result with the approved explanation; under-review is status only.
    expect(screen.getAllByText("Potassium, serum").length).toBeGreaterThan(0);
    expect(
      screen.getByText("Your potassium is within the expected range."),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/being reviewed by your care team/),
    ).toBeInTheDocument();
    expect(screen.queryByText("Lipid panel")).not.toBeInTheDocument();
    // Trend from two released values with a textual summary.
    expect(
      screen.getByText(/How your values are changing/),
    ).toBeInTheDocument();
    expect(screen.getAllByText(/4\.2 mmol\/L/).length).toBeGreaterThan(0);

    await userEvent
      .setup()
      .click(screen.getByTestId("home-confirm-attendance"));
    await waitFor(() => {
      const c = calls.find(
        (x) => x.url === "/api/v1/me/appointments/a1/confirm",
      );
      expect(c?.init?.method).toBe("POST");
      expect(JSON.parse(String(c?.init?.body))).toEqual({ version: 3 });
    });
    expect(
      await screen.findByText("New results have been released to you."),
    ).toBeInTheDocument();
    expect(screen.getAllByText("Attendance confirmed.").length).toBeGreaterThan(
      0,
    );
  });

  it("switches between dependants without leaking one child's data into the other's home", async () => {
    const calls = setup();
    await screen.findByText("Please confirm you will attend your appointment.");
    const group = screen.getByRole("group", {
      name: "Who are you looking at?",
    });
    await userEvent
      .setup()
      .click(within(group).getByRole("button", { name: /Mateo Ortiz/ }));
    expect(
      await screen.findByText("Nothing needs your attention right now."),
    ).toBeInTheDocument();
    expect(screen.getByText("No upcoming appointments")).toBeInTheDocument();
    expect(screen.queryByText("Echocardiogram")).not.toBeInTheDocument();
    expect(screen.queryByText("Potassium, serum")).not.toBeInTheDocument();
    expect(screen.getByText("No trends yet")).toBeInTheDocument();
    expect(calls.some((c) => c.url === "/api/v1/me/home?patient_id=pb")).toBe(
      true,
    );
    // Only grant-derived patient ids are ever requested.
    for (const c of calls.filter((c) => c.url.startsWith("/api/v1/me/home"))) {
      expect([
        "/api/v1/me/home?patient_id=pa",
        "/api/v1/me/home?patient_id=pb",
      ]).toContain(c.url);
    }
  });

  it("keeps the previous person's home off the page while a switch is loading", async () => {
    const calls = setup(META, { deferB: true });
    await screen.findByText("Please confirm you will attend your appointment.");
    expect(screen.getAllByText("Potassium, serum").length).toBeGreaterThan(0);
    const group = screen.getByRole("group", {
      name: "Who are you looking at?",
    });
    await userEvent
      .setup()
      .click(within(group).getByRole("button", { name: /Mateo Ortiz/ }));
    // Mateo's home has not arrived: nothing of Lucía's remains actionable.
    expect(await screen.findByText("Loading…")).toBeInTheDocument();
    expect(
      screen.queryByText("Please confirm you will attend your appointment."),
    ).not.toBeInTheDocument();
    expect(screen.queryAllByText("Potassium, serum")).toHaveLength(0);
    expect(
      screen.queryByRole("button", { name: /Confirm attendance/ }),
    ).not.toBeInTheDocument();
    calls.releaseB();
    expect(
      await screen.findByText("Nothing needs your attention right now."),
    ).toBeInTheDocument();
  });

  it("orders the timeline by instant, not by the localised date text", async () => {
    setup();
    await screen.findByText("Please confirm you will attend your appointment.");
    const items = within(
      screen.getByRole("list", { name: "Recent activity" }),
    ).getAllByRole("listitem");
    // "Oct" sorts before "Sep" as text; the October release is the newer event.
    expect(items[0]).toHaveTextContent("Potassium, serum");
    expect(items[1]).toHaveTextContent("Echocardiogram");
  });

  it("renders in Spanish", async () => {
    localStorage.setItem("wellos.lang", "es");
    setup();
    expect(
      await screen.findByText("Por favor, confirme que asistirá a su cita."),
    ).toBeInTheDocument();
    expect(screen.getByText("Lo importante hoy")).toBeInTheDocument();
    expect(
      screen.getByText("Su potasio está dentro del rango esperado."),
    ).toBeInTheDocument();
    expect(
      screen.getByText("Llegue 10 minutos antes.", { exact: false }),
    ).toBeInTheDocument();
  });

  it("explains honestly when the account is not enabled for self-service", async () => {
    setup({
      ...META,
      user: {
        username: "dr.garcia",
        display_name: "Dr. García",
        roles: ["physician"],
      },
      scheduling_capabilities: {
        ...META.scheduling_capabilities,
        self_service: false,
      },
      diagnostics_capabilities: {
        ...META.diagnostics_capabilities,
        self_service: false,
      },
    });
    expect(
      await screen.findByText(
        "Your account is not enabled for patient self-service.",
      ),
    ).toBeInTheDocument();
  });
});

describe("home helpers", () => {
  it("parses reference bands and flags trend points", () => {
    expect(referenceBand("3.5-5.1 mmol/L")).toEqual({ low: 3.5, high: 5.1 });
    expect(referenceBand("<5")).toBeNull();
    expect(referenceBand(null)).toBeNull();
    const pts = trendPoints("en", HOME_A.trends[0]);
    expect(pts).toHaveLength(2);
    expect(pts[1].flag).toBe("normal");
    expect(greetingKey(8)).toBe("greetingMorning");
    expect(greetingKey(15)).toBe("greetingAfternoon");
    expect(greetingKey(21)).toBe("greetingEvening");
  });
});
