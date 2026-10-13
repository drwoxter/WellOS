import { afterEach, describe, expect, it, vi } from "vitest";
import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import OperationsPage from "@/app/operations/page";
import FlowPage from "@/app/operations/flow/page";
import WorkforcePage from "@/app/operations/workforce/page";
import TheatresPage from "@/app/operations/theatres/page";
import MyCarePage from "@/app/my/care/page";
import CoordinationPage from "@/app/episodes/[id]/coordination/page";
import { SessionProvider } from "@/lib/session";
import { hasOperationsAccess } from "@/lib/care-ops";
import { STALE_AFTER_MINUTES, statusTone } from "@/app/operations/shared";

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => "/operations",
  useParams: () => ({ id: "ep1" }),
}));

const CAPS = {
  can_read: true,
  can_manage_flow: true,
  can_manage_theatre: true,
  can_manage_workforce: true,
  can_review_ai: true,
  can_audit: false,
  self_service: false,
};

function meta(caps: Partial<typeof CAPS> = {}, roles = ["operations_manager"]) {
  return {
    tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
    user: { username: "ops.lopez", display_name: "Ana López", roles },
    facilities: [
      {
        id: "f1",
        name: "Hospital Norte",
        time_zone: "Europe/Madrid",
        accessible: true,
      },
    ],
    environment: { name: "development", synthetic_data: true },
    ai_capabilities: { risk: true },
    care_ops_capabilities: { ...CAPS, ...caps },
  };
}

const NOW = new Date().toISOString();
const SNAPSHOT = {
  facility_id: "f1",
  generated_at: NOW,
  emergency: {
    awaiting_triage: 3,
    awaiting_clinician: 2,
    in_consultation: 4,
    by_priority: [
      {
        priority: "urgent",
        status: "ready_for_consultation",
        count: 1,
        avg_wait_minutes: 69,
        max_wait_minutes: 69,
      },
    ],
  },
  placement_queue: { pending: 2, max_wait_minutes: 95 },
  capacity: {
    total: 40,
    occupied: 36,
    available: 4,
    occupancy_ratio: 0.9,
    units: [
      {
        unit_id: "u1",
        name_en: "Surgical ward",
        name_es: "Planta quirúrgica",
        total: 20,
        occupied: 19,
        available: 1,
        cleaning: 0,
      },
    ],
  },
  episodes: { active: 12 },
  discharge: {
    barriers: [{ category: "transport", count: 1 }],
    overdue_tasks: 1,
  },
  transport: { open: 2, delayed: 1 },
  alerts: [
    { priority: "urgent", count: 3 },
    { priority: "routine", count: 2 },
  ],
  escalations: [
    { kind: "boarding", status: "armed", count: 1, next_due_at: NOW },
  ],
  theatre: {
    cases_next_24h: 3,
    ready: 1,
    blocking_conflicts: 1,
    missing_prerequisites: 1,
  },
  workforce: {
    shifts_next_24h: 2,
    gaps: [
      {
        shift_id: "sh1",
        label: "Surgical ward — night",
        starts_at: NOW,
        ends_at: NOW,
        unit_location_id: "u1",
        gaps: [
          {
            profession_code: "nurse",
            capability_codes: [],
            specialty_codes: [],
            required: 1,
            present: 0,
            on_call: 1,
            shortfall: 1,
          },
        ],
      },
    ],
  },
  forecasts: [
    {
      id: "fc1",
      facility_id: "f1",
      forecast_version: "care-operations-forecast.v1",
      horizon: "7d",
      generated_at: NOW,
      horizon_start: NOW,
      horizon_end: NOW,
      status: "insufficient_history",
      output: {},
      explanation_artifact_id: null,
    },
  ],
};

const BRIEF = {
  snapshot_id: "s1",
  snapshot: SNAPSHOT,
  brief: {
    mode: "dmind",
    artifact_id: "art1",
    status: "awaiting_review",
    output: {
      schema_version: "care-flow-brief.v1",
      summary: "Surgical ward is near capacity; one boarding escalation.",
      pressures: [
        {
          area: "surgical ward",
          explanation: "19 of 20 positions occupied",
          cited_sources: ["snapshot:capacity"],
        },
      ],
      human_decisions: [
        { text: "Decide the isolation placement", cited_sources: [] },
      ],
      cited_sources: ["snapshot:capacity"],
      confidence: "medium",
      limitations: ["Advisory only"],
      missing_information: [],
    },
    synthetic: true,
    reused: false,
    reason: null,
    input_hash: "h",
  },
};

const REQUEST = {
  id: "pr1",
  facility_id: "f1",
  episode_id: "ep1",
  patient_id: "pa",
  kind: "admission",
  status: "candidates_ready",
  priority: "urgent",
  requirements: { isolation_codes: ["airborne"] },
  requested_from: new Date(Date.now() - 50 * 60_000).toISOString(),
  matcher_version: "care-placement.v1",
  facts_hash: "fh",
  ranking_artifact_id: null,
  version: 2,
  created_at: NOW,
};

const MATCH = {
  matcher_version: "care-placement.v1",
  facts_hash: "fh",
  feasible: [
    {
      location_id: "bed-iso-1",
      unit_id: "u1",
      label: "Isolation room 1 · bed A",
      feasible: true,
      rank: 1,
      score: 92,
      factors: [
        {
          code: "isolation_match",
          value: "airborne",
          weight: 40,
          points: 40,
          detail: "negative-pressure room",
        },
      ],
      reasons: [],
      rejection_codes: [],
    },
  ],
  rejected: [
    {
      location_id: "bed-7",
      unit_id: "u1",
      label: "Surgical ward · bed 7",
      feasible: false,
      rank: null,
      score: null,
      factors: [],
      reasons: ["bed is in a shared bay"],
      rejection_codes: ["isolation_not_supported"],
    },
  ],
  rejected_by_code: { isolation_not_supported: 1 },
};

const CANDIDATE_ROWS = [...MATCH.feasible, ...MATCH.rejected].map((c, i) => ({
  id: `cand${i}`,
  location_id: c.location_id,
  generation: 1,
  rank: c.rank,
  feasible: c.feasible,
  score: c.score,
  score_factors: c.factors,
  rejection_codes: c.rejection_codes,
  facts_hash: MATCH.facts_hash,
  label: c.label,
  unit_id: c.unit_id,
}));
const REQUEST_DETAIL = {
  request: REQUEST,
  candidates: CANDIDATE_ROWS,
  placements: [],
  history: [],
};

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

type Route = (url: string, init?: RequestInit) => unknown | undefined;

function setup(ui: React.ReactElement, m: unknown, route: Route) {
  const calls: { url: string; init?: RequestInit }[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      calls.push({ url, init });
      if (url === "/api/session")
        return Promise.resolve(jsonResponse({ authenticated: true }));
      if (url === "/api/v1/meta/tenant")
        return Promise.resolve(jsonResponse(m));
      const hit = route(url, init);
      if (hit === undefined)
        return Promise.resolve(jsonResponse({ error: "not_found" }, 404));
      if (hit instanceof Response || hit instanceof Promise)
        return Promise.resolve(hit);
      return Promise.resolve(jsonResponse(hit));
    }),
  );
  render(<SessionProvider>{ui}</SessionProvider>);
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("care operations shared helpers", () => {
  it("maps statuses to non-colour-only tones and gates navigation on capabilities", () => {
    expect(statusTone("placed")).toBe("ok");
    expect(statusTone("held")).toBe("warn");
    expect(statusTone("insufficient_history")).toBe("critical");
    expect(hasOperationsAccess(undefined)).toBe(false);
    expect(
      hasOperationsAccess({
        ...CAPS,
        can_read: false,
        can_manage_flow: false,
        can_manage_theatre: false,
        can_manage_workforce: false,
      }),
    ).toBe(false);
    expect(
      hasOperationsAccess({
        ...CAPS,
        can_read: false,
        can_manage_flow: false,
        can_manage_theatre: true,
        can_manage_workforce: false,
      }),
    ).toBe(true);
    expect(STALE_AFTER_MINUTES).toBe(10);
  });
});

describe("/operations command centre", () => {
  it("renders live tiles, unit capacity text, forecast honesty and a reviewable dMind brief", async () => {
    let reviewed: unknown = null;
    const calls = setup(<OperationsPage />, meta(), (url, init) => {
      if (url.startsWith("/api/v1/operations/snapshot?")) return SNAPSHOT;
      if (url === "/api/v1/operations/snapshot/brief") return BRIEF;
      if (url === "/api/v1/operations/artifacts/art1/review") {
        reviewed = JSON.parse(String(init?.body));
        return { artifact_id: "art1", status: "approved" };
      }
      return undefined;
    });
    const tiles = await screen.findByTestId("ops-tiles");
    expect(within(tiles).getByText("3")).toBeInTheDocument(); // awaiting triage
    expect(within(tiles).getByText("5")).toBeInTheDocument(); // unresolved alerts (3 + 2)
    expect(screen.getByText(/transport: 1/)).toBeInTheDocument();
    expect(screen.getByText(/boarding: 1/)).toBeInTheDocument();
    expect(screen.getByText(/90\s?%/)).toBeInTheDocument();
    expect(screen.getByTestId("ops-priority")).toHaveTextContent(
      "urgent ready for consultation · 1 · 69 min",
    );
    expect(screen.getByText(/Surgical ward/)).toBeInTheDocument();
    // Textual bar summary rather than colour alone.
    expect(
      screen.getByRole("img", { name: /Surgical ward/ }),
    ).toBeInTheDocument();
    expect(screen.getByTestId("ops-forecast")).toHaveTextContent(
      /insufficient/i,
    );
    expect(screen.getByTestId("ops-freshness")).toHaveAttribute(
      "role",
      "status",
    );

    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: /brief/i }));
    const brief = await screen.findByTestId("ops-brief-outcome");
    expect(brief).toHaveTextContent("Surgical ward is near capacity");
    expect(brief).toHaveTextContent("Decide the isolation placement");
    expect(brief).toHaveAttribute("data-artifact-status", "awaiting_review");
    await user.click(
      within(brief).getByRole("button", { name: /accept as reviewed/i }),
    );
    await waitFor(() =>
      expect(brief).toHaveAttribute("data-artifact-status", "approved"),
    );
    expect(reviewed).toEqual({ decision: "approved" });
    expect(
      calls.some((c) => c.url === "/api/v1/operations/snapshot/brief"),
    ).toBe(true);
  });

  it("flags a stale snapshot as an alert and hides review controls without care_ops.review_ai", async () => {
    const stale = {
      ...SNAPSHOT,
      generated_at: new Date(Date.now() - 30 * 60_000).toISOString(),
    };
    setup(<OperationsPage />, meta({ can_review_ai: false }), (url) => {
      if (url.startsWith("/api/v1/operations/snapshot?")) return stale;
      if (url === "/api/v1/operations/snapshot/brief") return BRIEF;
      return undefined;
    });
    const fresh = await screen.findByTestId("ops-freshness");
    expect(fresh).toHaveAttribute("role", "alert");
    const user = userEvent.setup();
    await user.click(screen.getByRole("button", { name: /brief/i }));
    const brief = await screen.findByTestId("ops-brief-outcome");
    expect(
      within(brief).queryByRole("button", { name: /accept as reviewed/i }),
    ).toBeNull();
  });

  it("shows a permission-denied state when the API refuses the snapshot", async () => {
    setup(<OperationsPage />, meta(), (url) => {
      if (url.startsWith("/api/v1/operations/snapshot?"))
        return jsonResponse({ error: "forbidden", message: "forbidden" }, 403);
      return undefined;
    });
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /permission|forbidden|not allowed/i,
    );
  });
});

describe("/operations/flow placement", () => {
  it("separates deterministic feasible and rejected candidates and sends a human hold with the facts hash", async () => {
    let held: unknown = null;
    setup(<FlowPage />, meta(), (url, init) => {
      if (url.startsWith("/api/v1/operations/placement-requests?"))
        return { requests: [REQUEST] };
      if (url.startsWith("/api/v1/operations/episodes?"))
        return { episodes: [] };
      if (url.startsWith("/api/v1/operations/escalations?"))
        return { escalations: [] };
      if (url === "/api/v1/operations/placement-requests/pr1")
        return REQUEST_DETAIL;
      if (url === "/api/v1/operations/placement-requests/pr1/hold") {
        held = JSON.parse(String(init?.body));
        return {
          request: { ...REQUEST, status: "held", version: 3 },
          placement: {},
        };
      }
      return undefined;
    });
    const user = userEvent.setup();
    const queue = await screen.findByTestId("flow-queue");
    await user.click(within(queue).getByRole("button"));
    const feasible = await screen.findByTestId("flow-feasible");
    expect(within(feasible).getAllByTestId("flow-candidate")).toHaveLength(1);
    expect(feasible).toHaveTextContent("Isolation room 1");
    expect(screen.getByText(/1 rejected candidates/)).toBeInTheDocument();
    expect(screen.getByText("isolation_not_supported")).toBeInTheDocument();
    const hold = screen.getByRole("button", { name: /^Hold/ });
    expect(hold).toBeDisabled();
    await user.click(within(feasible).getByRole("radio"));
    expect(hold).toBeEnabled();
    await user.click(hold);
    await waitFor(() => expect(held).not.toBeNull());
    expect(held).toEqual({
      location_id: "bed-iso-1",
      expected_version: 2,
      facts_hash: "fh",
    });
  });

  it("surfaces a stale 409 conflict without pretending the placement happened", async () => {
    setup(<FlowPage />, meta(), (url) => {
      if (url.startsWith("/api/v1/operations/placement-requests?"))
        return { requests: [REQUEST] };
      if (url.startsWith("/api/v1/operations/episodes?"))
        return { episodes: [] };
      if (url.startsWith("/api/v1/operations/escalations?"))
        return { escalations: [] };
      if (url === "/api/v1/operations/placement-requests/pr1")
        return REQUEST_DETAIL;
      if (url === "/api/v1/operations/placement-requests/pr1/confirm")
        return jsonResponse(
          {
            error: {
              code: "stale_version",
              message: "placement request changed",
            },
          },
          409,
        );
      return undefined;
    });
    const user = userEvent.setup();
    await user.click(
      within(await screen.findByTestId("flow-queue")).getByRole("button"),
    );
    await user.click(
      within(await screen.findByTestId("flow-feasible")).getByRole("radio"),
    );
    await user.click(screen.getByRole("button", { name: /^Confirm/ }));
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /changed|stale|conflict/i,
    );
    expect(screen.getByTestId("flow-request")).toHaveAttribute(
      "data-status",
      "candidates_ready",
    );
  });
});

describe("/operations/workforce and /operations/theatres", () => {
  it("renders capability-specific staffing gaps as an accessible table", async () => {
    const shift = {
      id: "sh1",
      facility_id: "f1",
      unit_location_id: "u1",
      label: "Night",
      local_date: "2026-10-04",
      starts_at: NOW,
      ends_at: NOW,
      status: "planned",
      version: 1,
    };
    setup(<WorkforcePage />, meta(), (url) => {
      if (url.startsWith("/api/v1/operations/workforce/shifts?"))
        return {
          shifts: [
            {
              shift,
              requirements: [],
              reallocation_candidates: [],
              coverage: {
                covered: false,
                engine_version: "workforce-coverage.v1",
                facts_hash: "sha256:wf",
                gaps: [],
              },
            },
          ],
        };
      if (url === "/api/v1/operations/workforce/shifts/sh1")
        return {
          shift,
          requirements: [],
          assignments: [],
          coverage: {
            covered: false,
            engine_version: "workforce-coverage.v1",
            facts_hash: "sha256:wf",
            gaps: [
              {
                profession_code: "nurse",
                capability_codes: ["paediatric"],
                specialty_codes: [],
                required: 2,
                present: 1,
                on_call: 0,
                shortfall: 1,
              },
            ],
          },
          reallocation_candidates: [],
        };
      return undefined;
    });
    const user = userEvent.setup();
    await user.click(
      within(await screen.findByTestId("wf-shifts")).getByRole("button"),
    );
    const table = await screen.findByTestId("wf-gaps");
    expect(
      within(table).getByRole("rowheader", { name: /nurse/ }),
    ).toBeInTheDocument();
    expect(table).toHaveTextContent("paediatric");
    expect(within(table).getAllByRole("columnheader")).toHaveLength(5);
  });

  it("shows deterministic theatre readiness with blocking conflicts", async () => {
    const c = {
      id: "tc1",
      facility_id: "f1",
      episode_id: null,
      patient_id: "pd",
      procedure_code: "hernia-repair",
      responsible_service_code: "surgery",
      status: "planned",
      priority: "routine",
      expected_duration_minutes: 90,
      required_equipment_codes: ["laparoscope"],
      prerequisites: {},
      session_id: null,
      theatre_location_id: null,
      scheduled_start: NOW,
      scheduled_end: NOW,
      version: 1,
    };
    setup(<TheatresPage />, meta(), (url) => {
      if (url.startsWith("/api/v1/operations/theatre/cases?"))
        return { cases: [{ case: c, readiness: null }] };
      if (url === "/api/v1/operations/theatre/cases/tc1")
        return {
          case: c,
          readiness: {
            engine_version: "theatre-conflicts.v1",
            feasible: false,
            conflicts: [
              {
                code: "missing_prerequisite",
                blocking: true,
                detail: "Anaesthetic review not recorded",
              },
            ],
          },
        };
      return undefined;
    });
    const user = userEvent.setup();
    await user.click(
      within(await screen.findByTestId("theatre-cases")).getByRole("button"),
    );
    const r = await screen.findByTestId("theatre-readiness");
    expect(r).toHaveTextContent(/not ready/i);
    expect(r).toHaveTextContent("theatre-conflicts.v1");
    expect(r).toHaveTextContent("Anaesthetic review not recorded");
  });
});

describe("/operations/theatres scheduling", () => {
  it("lets a theatre coordinator schedule a case and refreshes readiness", async () => {
    const c = {
      id: "tc2",
      facility_id: "f1",
      episode_id: null,
      patient_id: "pd",
      procedure_code: "appendicectomy",
      responsible_service_code: "surgery",
      status: "ready_for_scheduling",
      priority: "urgent",
      expected_duration_minutes: 60,
      required_equipment_codes: [],
      prerequisites: {},
      session_id: null,
      theatre_location_id: null,
      scheduled_start: null,
      scheduled_end: null,
      version: 2,
    };
    let scheduled = false;
    const calls = setup(
      <TheatresPage />,
      meta({ can_manage_theatre: true }, ["theatre_coordinator"]),
      (url, init) => {
        if (url.startsWith("/api/v1/operations/theatre/cases?"))
          return {
            cases: [
              {
                case: scheduled ? { ...c, status: "scheduled", version: 3 } : c,
                readiness: null,
              },
            ],
          };
        if (url.startsWith("/api/v1/operations/locations?"))
          return {
            locations: [
              {
                id: "th1",
                facility_id: "f1",
                level: "position",
                type_code: "theatre",
                code: "OR-1",
                name_en: "Theatre 1",
                name_es: "Quirófano 1",
                operational_status: "open",
                position_state: "available",
                active: true,
                version: 4,
              },
              {
                id: "bed1",
                facility_id: "f1",
                level: "position",
                type_code: "bed",
                code: "B-1",
                name_en: "Bed 1",
                name_es: "Cama 1",
                operational_status: "open",
                position_state: "available",
                active: true,
                version: 1,
              },
            ],
          };
        if (url === "/api/v1/operations/theatre/cases/tc2/schedule") {
          scheduled = true;
          return {
            case: {
              ...c,
              status: "scheduled",
              theatre_location_id: "th1",
              scheduled_start: NOW,
              version: 3,
            },
            readiness: {
              engine_version: "theatre-conflicts.v1",
              feasible: true,
              conflicts: [],
            },
          };
        }
        if (url === "/api/v1/operations/theatre/cases/tc2")
          return scheduled
            ? {
                case: {
                  ...c,
                  status: "scheduled",
                  theatre_location_id: "th1",
                  scheduled_start: NOW,
                  version: 3,
                },
                readiness: {
                  engine_version: "theatre-conflicts.v1",
                  feasible: true,
                  conflicts: [],
                },
              }
            : { case: c, readiness: null };
        return undefined;
      },
    );
    const user = userEvent.setup();
    await user.click(
      within(await screen.findByTestId("theatre-cases")).getByRole("button"),
    );
    const form = await screen.findByTestId("theatre-schedule");
    const room = within(form).getByRole("combobox", { name: /theatre/i });
    expect(within(room).getAllByRole("option")).toHaveLength(2);
    await user.selectOptions(room, "th1");
    fireEvent.change(within(form).getByLabelText(/scheduled start/i), {
      target: { value: "2026-10-20T09:30" },
    });
    await user.click(within(form).getByRole("button", { name: /^schedule$/i }));
    await waitFor(() =>
      expect(screen.getByTestId("theatre-readiness")).toHaveTextContent(
        /^ready|ready theatre-conflicts/i,
      ),
    );
    const post = calls.find((x) =>
      x.url.endsWith("/theatre/cases/tc2/schedule"),
    );
    expect(post).toBeDefined();
    const body = JSON.parse(String(post?.init?.body));
    expect(body).toMatchObject({
      expected_version: 2,
      theatre_location_id: "th1",
      expected_theatre_version: 4,
    });
    expect(new Date(body.scheduled_start).getTime()).toBe(
      new Date("2026-10-20T09:30").getTime(),
    );
    expect(screen.getByText(/case scheduled/i)).toBeInTheDocument();
  });

  it("hides the scheduling form from read-only operations users", async () => {
    const c = {
      id: "tc3",
      facility_id: "f1",
      episode_id: null,
      patient_id: "pd",
      procedure_code: "hernia-repair",
      responsible_service_code: "surgery",
      status: "ready_for_scheduling",
      priority: "routine",
      expected_duration_minutes: 60,
      required_equipment_codes: [],
      prerequisites: {},
      session_id: null,
      theatre_location_id: null,
      scheduled_start: null,
      scheduled_end: null,
      version: 1,
    };
    const calls = setup(
      <TheatresPage />,
      meta({ can_manage_theatre: false }),
      (url) => {
        if (url.startsWith("/api/v1/operations/theatre/cases?"))
          return { cases: [{ case: c, readiness: null }] };
        if (url === "/api/v1/operations/theatre/cases/tc3")
          return { case: c, readiness: null };
        return undefined;
      },
    );
    const user = userEvent.setup();
    await user.click(
      within(await screen.findByTestId("theatre-cases")).getByRole("button"),
    );
    await screen.findByTestId("theatre-readiness");
    expect(screen.queryByTestId("theatre-schedule")).toBeNull();
    expect(
      calls.some((x) => x.url.startsWith("/api/v1/operations/locations")),
    ).toBe(false);
  });
});

describe("/my/care patient view", () => {
  it("shows only released, patient-visible care information", async () => {
    const m = {
      ...meta(
        {
          can_read: false,
          can_manage_flow: false,
          can_manage_theatre: false,
          can_manage_workforce: false,
          can_review_ai: false,
          self_service: true,
        },
        ["patient_representative"],
      ),
      user: {
        username: "rep.alba",
        display_name: "Rep Alba",
        roles: ["patient_representative"],
      },
    };
    setup(<MyCarePage />, m, (url) => {
      if (url === "/api/v1/me")
        return {
          user_id: "u1",
          display_name: "Rep Alba",
          patients: [
            {
              grant_id: "g1",
              patient_id: "pa",
              relationship: "self",
              patient: { given_name: "Alba", family_name: "Núñez" },
              expires_at: null,
            },
          ],
        };
      if (url.startsWith("/api/v1/me/care"))
        return {
          patient_id: "pa",
          generated_at: NOW,
          episodes: [
            {
              episode_id: "ep1",
              episode_type_code: "inpatient",
              status: "discharge_preparation",
              started_at: NOW,
              expected_end_at: null,
              facility_id: "f1",
              location: {
                unit_en: "Surgical ward",
                unit_es: "Planta quirúrgica",
                position_en: "Bed 7",
                position_es: "Cama 7",
              },
              next_step: {
                id: "t1",
                status: "in_progress",
                text_en: "Transport home is being arranged",
                text_es: "Se está organizando el transporte",
                due_at: null,
              },
              pending_tasks: [
                {
                  id: "t1",
                  status: "in_progress",
                  due_at: null,
                  text_en: "Transport home is being arranged",
                  text_es: "Se está organizando el transporte",
                  discharge_preparation: true,
                },
              ],
              procedures: [],
            },
          ],
          appointments: [],
          transport: [
            {
              id: "tr1",
              status: "requested",
              pickup_window_start: null,
              pickup_window_end: null,
            },
          ],
          notifications: [
            {
              id: "n1",
              kind: "booking_confirmation",
              status: "delivered",
              created_at: NOW,
              delivered_at: NOW,
            },
          ],
          contact: {
            facility_id: "f1",
            facility_name: "Hospital Norte",
            time_zone: "Europe/Madrid",
          },
        };
      return undefined;
    });
    const view = await screen.findByTestId("mycare-episodes");
    expect(view).toHaveTextContent("Hospital Norte");
    expect(view).toHaveTextContent("Bed 7");
    expect(view).toHaveTextContent("Transport home is being arranged");
    expect(view).toHaveTextContent(/discharge/i);
    expect(screen.getByTestId("mycare-notifications")).not.toHaveTextContent(
      "booking_confirmation",
    );
    // Operational discharge preparation is never shown as medical clearance.
    expect(view).toHaveTextContent(/not medical clearance/i);
    expect(view).not.toHaveTextContent(/medically cleared|discharged now/i);
    expect(view).not.toHaveTextContent(/barrier_category|staff only/i);
  });
});

describe("/episodes/[id]/coordination tasks", () => {
  function detail(taskStatus: string, version: number) {
    return {
      episode: {
        id: "ep1",
        facility_id: "f1",
        patient_id: "p1",
        episode_type_code: "inpatient",
        status: "discharge_preparation",
        responsible_service_code: "internal_medicine",
        current_placement_id: null,
        started_at: NOW,
        expected_end_at: null,
        ended_at: null,
        priority: "routine",
        barriers: [],
        version: 3,
        created_at: NOW,
        updated_at: NOW,
      },
      links: [],
      placements: [],
      placement_requests: [],
      tasks: [
        {
          id: "t1",
          episode_id: "ep1",
          patient_id: "p1",
          title_en: "Arrange transport home",
          title_es: "Organizar transporte a casa",
          kind: "transport",
          status: taskStatus,
          profession_code: null,
          capability_codes: [],
          due_at: null,
          escalate_at: null,
          barrier_category_code: null,
          patient_visible: true,
          version,
          depends_on: [],
        },
      ],
      handoffs: [],
      escalations: [],
      theatre_cases: [],
      history: [],
      ai_artifacts: [],
    };
  }

  it("keeps task actions disabled until the reload after a transition has landed, so the next action carries the fresh version", async () => {
    let task = { status: "open", version: 1 };
    let releaseReload: (() => void) | null = null;
    let loads = 0;
    const calls = setup(<CoordinationPage />, meta(), (url, init) => {
      if (url === "/api/v1/operations/episodes/ep1") {
        loads += 1;
        const body = detail(task.status, task.version);
        if (loads === 2) {
          return new Promise<Response>((resolve) => {
            releaseReload = () => resolve(jsonResponse(body));
          });
        }
        return body;
      }
      if (url === "/api/v1/operations/tasks/t1/transition") {
        const sent = JSON.parse(String(init?.body)) as {
          transition: string;
          expected_version: number;
        };
        if (sent.expected_version !== task.version) {
          return jsonResponse(
            {
              error: {
                code: "conflict",
                message: "task changed since it was loaded; reload and retry",
              },
            },
            409,
          );
        }
        task = {
          status: sent.transition === "start" ? "in_progress" : "completed",
          version: task.version + 1,
        };
        return { task: detail(task.status, task.version).tasks[0] };
      }
      return undefined;
    });
    const user = userEvent.setup();
    const row = await screen.findByTestId("coord-task");
    await user.click(within(row).getByRole("button", { name: "Start" }));

    await waitFor(() => expect(loads).toBe(2));
    const complete = within(row).getByRole("button", { name: "Complete" });
    expect(complete).toBeDisabled();
    expect(row).toHaveAttribute("data-status", "open");

    releaseReload!();
    await waitFor(() =>
      expect(row).toHaveAttribute("data-status", "in_progress"),
    );
    await waitFor(() =>
      expect(
        within(row).getByRole("button", { name: "Complete" }),
      ).toBeEnabled(),
    );
    await user.click(within(row).getByRole("button", { name: "Complete" }));
    await waitFor(() =>
      expect(row).toHaveAttribute("data-status", "completed"),
    );

    const transitions = calls
      .filter((c) => c.url === "/api/v1/operations/tasks/t1/transition")
      .map(
        (c) =>
          JSON.parse(String(c.init?.body)) as {
            transition: string;
            expected_version: number;
          },
      );
    expect(transitions).toEqual([
      { transition: "start", expected_version: 1 },
      { transition: "complete", expected_version: 2 },
    ]);
    expect(row).not.toHaveTextContent(/changed since it was loaded/i);
  });
});
