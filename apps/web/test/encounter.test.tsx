import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import EncounterPage from "@/app/encounters/[id]/page";
import { SessionProvider } from "@/lib/session";

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => "/encounters/e1",
}));

const META = {
  tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
  user: {
    username: "dr.garcia",
    display_name: "Dr. García",
    roles: ["physician"],
  },
  facilities: [
    {
      id: "f1",
      name: "Central Hospital",
      accessible: true,
      can_register: false,
      can_act_clinically: true,
    },
  ],
};

type WorkspaceOverrides = Record<string, unknown>;

function workspace(overrides: WorkspaceOverrides = {}) {
  return {
    encounter: {
      id: "e1",
      status: "in_progress",
      encounter_type: "consultation",
      started_at: "2026-08-29T09:00:00Z",
      completed_at: null,
      practitioner: "Dr. García",
      facility_name: "Central Hospital",
      own: true,
    },
    patient: {
      id: "p1",
      family_name: "Demopatient",
      given_name: "Alba",
      birth_date: "1990-03-03",
      sex: "female",
      identifier: "SYN-0001",
    },
    allergies: [{ substance: "Penicillin", criticality: "high" }],
    medications: [],
    alerts: [],
    note: null,
    addenda: [],
    vitals: [],
    previous_vitals: [],
    diagnoses: [],
    service_requests: [],
    ai_draft: null,
    capabilities: {
      can_document: true,
      can_sign: true,
      can_add_addendum: false,
      can_order_lab: true,
    },
    ...overrides,
  };
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function apiError(status: number, code: string, message: string): Response {
  return jsonResponse({ error: { code, message } }, status);
}

/** Stub fetch: GET workspace returns `ws`; POST handlers are per-path. */
function setup(
  ws: unknown,
  posts: Record<string, (body: unknown) => Response> = {},
) {
  const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (url === "/api/session")
      return Promise.resolve(jsonResponse({ authenticated: true }));
    if (url === "/api/v1/meta/tenant")
      return Promise.resolve(jsonResponse(META));
    if (init?.method === "POST") {
      const handler = posts[url];
      if (handler) {
        const body = init.body ? JSON.parse(String(init.body)) : null;
        return Promise.resolve(handler(body));
      }
      return Promise.resolve(jsonResponse({}));
    }
    if (url === "/api/v1/encounters/e1")
      return Promise.resolve(jsonResponse(ws));
    return Promise.resolve(jsonResponse({}));
  });
  vi.stubGlobal("fetch", fetchMock);
  render(
    <SessionProvider>
      <EncounterPage params={{ id: "e1" }} />
    </SessionProvider>,
  );
  return fetchMock;
}

describe("encounter documentation workspace", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  it("shows the safety header, allergy badge and editable note for a draft", async () => {
    setup(workspace());
    expect(await screen.findByText("Alba Demopatient")).toBeInTheDocument();
    expect(screen.getByText("Penicillin")).toBeInTheDocument();
    expect(screen.getByText("In progress")).toBeInTheDocument();
    expect(
      screen.getByLabelText(/Reason for consultation/),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Save draft" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Sign and complete" }),
    ).toBeInTheDocument();
  });

  it("marks unsaved changes and saves the draft", async () => {
    const user = userEvent.setup();
    setup(workspace(), {
      "/api/v1/encounters/e1/note": () =>
        jsonResponse({ id: "n1", status: "draft", version: 1 }),
    });
    const reason = await screen.findByLabelText(/Reason for consultation/);
    await user.type(reason, "Chest pain");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Save draft" }));
    expect(await screen.findByText("Draft saved.")).toBeInTheDocument();
    expect(screen.queryByText("Unsaved changes")).not.toBeInTheDocument();
  });

  it("surfaces a version conflict instead of overwriting", async () => {
    const user = userEvent.setup();
    setup(
      workspace({
        note: {
          id: "n1",
          status: "draft",
          version: 3,
          reason_for_encounter: "Cough",
          history_present_illness: null,
          medical_history: null,
          review_of_systems: null,
          physical_exam: null,
          assessment: null,
          plan: null,
          follow_up: null,
          author: "Dr. García",
          updated_at: "2026-08-29T09:10:00Z",
          signed_at: null,
          signed_by: null,
        },
      }),
      {
        "/api/v1/encounters/e1/note": () =>
          apiError(409, "version_conflict", "reload before saving"),
      },
    );
    const reason = await screen.findByLabelText(/Reason for consultation/);
    await user.type(reason, " and fever");
    await user.click(screen.getByRole("button", { name: "Save draft" }));
    expect(
      await screen.findByText(/changed by someone else/i),
    ).toBeInTheDocument();
  });

  it("requires confirmation before signing", async () => {
    const user = userEvent.setup();
    const signCalls: unknown[] = [];
    setup(
      workspace({
        note: {
          id: "n1",
          status: "draft",
          version: 2,
          reason_for_encounter: "Review",
          history_present_illness: null,
          medical_history: null,
          review_of_systems: null,
          physical_exam: null,
          assessment: "Stable",
          plan: null,
          follow_up: null,
          author: "Dr. García",
          updated_at: "2026-08-29T09:10:00Z",
          signed_at: null,
          signed_by: null,
        },
      }),
      {
        "/api/v1/encounters/e1/sign": (body) => {
          signCalls.push(body);
          return jsonResponse({
            id: "n1",
            status: "signed",
            encounter_status: "completed",
          });
        },
      },
    );
    await user.click(
      await screen.findByRole("button", { name: "Sign and complete" }),
    );
    expect(signCalls).toHaveLength(0);
    expect(screen.getByText(/signed note is permanent/i)).toBeInTheDocument();
    await user.click(screen.getByRole("button", { name: "Confirm" }));
    await waitFor(() => expect(signCalls).toHaveLength(1));
    expect(signCalls[0]).toEqual({ version: 2 });
  });

  it("renders a signed note read-only with distinguished addenda", async () => {
    setup(
      workspace({
        encounter: {
          id: "e1",
          status: "completed",
          encounter_type: "consultation",
          started_at: "2026-08-29T09:00:00Z",
          completed_at: "2026-08-29T10:00:00Z",
          practitioner: "Dr. García",
          facility_name: "Central Hospital",
          own: true,
        },
        note: {
          id: "n1",
          status: "signed",
          version: 4,
          reason_for_encounter: "Follow-up",
          history_present_illness: null,
          medical_history: null,
          review_of_systems: null,
          physical_exam: null,
          assessment: "Improving",
          plan: "Continue treatment",
          follow_up: null,
          author: "Dr. García",
          updated_at: "2026-08-29T10:00:00Z",
          signed_at: "2026-08-29T10:00:00Z",
          signed_by: "Dr. García",
        },
        addenda: [
          {
            body: "Correction: onset was two weeks ago.",
            author: "Dr. García",
            created_at: "2026-08-30T08:00:00Z",
          },
        ],
        capabilities: {
          can_document: false,
          can_sign: false,
          can_add_addendum: true,
          can_order_lab: false,
        },
      }),
    );
    expect(await screen.findByText("Clinical summary")).toBeInTheDocument();
    expect(screen.getByText("Signed")).toBeInTheDocument();
    expect(
      screen.getByText("Correction: onset was two weeks ago."),
    ).toBeInTheDocument();
    expect(screen.getByText("Addendum")).toBeInTheDocument();
    // No editable note sections on a signed record.
    expect(
      screen.queryByRole("button", { name: "Save draft" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Add addendum" }),
    ).toBeInTheDocument();
  });

  it("asks for explicit confirmation for unusual vital values", async () => {
    const user = userEvent.setup();
    let confirmed: boolean | null = null;
    setup(workspace(), {
      "/api/v1/encounters/e1/vitals": (body) => {
        const b = body as { confirm_unusual: boolean };
        confirmed = b.confirm_unusual;
        if (!b.confirm_unusual) {
          return apiError(
            422,
            "unusual_values",
            "values outside the usual range require confirmation: heart_rate_bpm",
          );
        }
        return jsonResponse({ id: "v1", bmi: null });
      },
    });
    await user.click(
      await screen.findByRole("button", { name: "Record vital signs" }),
    );
    await user.type(screen.getByLabelText(/Heart rate/), "190");
    const submit = screen
      .getAllByRole("button", { name: "Record vital signs" })
      .find((b) => b.getAttribute("type") === "submit");
    expect(submit).toBeDefined();
    await user.click(submit as HTMLElement);
    expect(
      await screen.findByRole("button", { name: "Confirm values and save" }),
    ).toBeInTheDocument();
    await user.click(
      screen.getByRole("button", { name: "Confirm values and save" }),
    );
    await waitFor(() => expect(confirmed).toBe(true));
    expect(
      await screen.findByText("Vital signs recorded."),
    ).toBeInTheDocument();
  });

  it("shows the dMind draft as assistive with explicit accept/reject", async () => {
    setup(
      workspace({
        ai_draft: {
          id: "a1",
          status: "awaiting_review",
          output: {
            summary: "Deterministic assistive summary.",
            limitations: [],
            cited_sources: [],
          },
          limitations: ["Incomplete documentation sections: plan"],
          citations: ["encounter_note:n1:assessment"],
          model: "dmind-fake",
          model_version: "0.1.0",
          generated_at: "2026-08-29T09:30:00Z",
          review_decision: null,
        },
      }),
    );
    expect(
      await screen.findByText("Deterministic assistive summary."),
    ).toBeInTheDocument();
    expect(
      screen.getByText(/Assistive draft — requires your review/),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Accept and copy into assessment" }),
    ).toBeInTheDocument();
    expect(
      screen.getByRole("button", { name: "Reject draft" }),
    ).toBeInTheDocument();
  });

  it("keeps dirty local edits when a refresh returns a newer note version", async () => {
    const user = userEvent.setup();
    const baseNote = {
      id: "n1",
      status: "draft",
      version: 3,
      reason_for_encounter: "Cough",
      history_present_illness: null,
      medical_history: null,
      review_of_systems: null,
      physical_exam: null,
      assessment: null,
      plan: null,
      follow_up: null,
      author: "Dr. García",
      updated_at: "2026-08-29T09:10:00Z",
      signed_at: null,
      signed_by: null,
    };
    let currentWs = workspace({ note: baseNote });
    const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url === "/api/session")
        return Promise.resolve(jsonResponse({ authenticated: true }));
      if (url === "/api/v1/meta/tenant")
        return Promise.resolve(jsonResponse(META));
      if (init?.method === "POST" && url === "/api/v1/encounters/e1/vitals") {
        // Simulate the note advancing on the server before the refresh.
        currentWs = workspace({
          note: {
            ...baseNote,
            version: 4,
            reason_for_encounter: "Rewritten elsewhere",
          },
        });
        return Promise.resolve(jsonResponse({ id: "v1", bmi: null }));
      }
      if (url === "/api/v1/encounters/e1")
        return Promise.resolve(jsonResponse(currentWs));
      return Promise.resolve(jsonResponse({}));
    });
    vi.stubGlobal("fetch", fetchMock);
    render(
      <SessionProvider>
        <EncounterPage params={{ id: "e1" }} />
      </SessionProvider>,
    );

    const reason = await screen.findByLabelText(/Reason for consultation/);
    await user.type(reason, " and fever");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();

    // Recording vitals triggers a workspace refresh with the newer note.
    await user.click(
      await screen.findByRole("button", { name: "Record vital signs" }),
    );
    await user.type(screen.getByLabelText(/Heart rate/), "72");
    const submit = screen
      .getAllByRole("button", { name: "Record vital signs" })
      .find((b) => b.getAttribute("type") === "submit");
    await user.click(submit as HTMLElement);

    // Local dirty edits are preserved and the version drift is surfaced as a
    // conflict instead of silently adopting the newer server note.
    expect(
      await screen.findByText(/changed by someone else/i),
    ).toBeInTheDocument();
    expect(screen.getByLabelText(/Reason for consultation/)).toHaveValue(
      "Cough and fever",
    );
  });

  it("asks for confirmation before internal navigation with unsaved edits", async () => {
    const user = userEvent.setup();
    const confirmMock = vi.fn(() => false);
    vi.stubGlobal("confirm", confirmMock);
    setup(workspace());
    const reason = await screen.findByLabelText(/Reason for consultation/);
    await user.type(reason, "Chest pain");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();

    const anchor = document.createElement("a");
    anchor.href = "/patients/p1";
    anchor.textContent = "Patient chart";
    document.body.appendChild(anchor);
    const cancelled = !fireEvent.click(anchor);
    expect(confirmMock).toHaveBeenCalledWith(
      expect.stringMatching(/unsaved documentation/i),
    );
    expect(cancelled).toBe(true);
    expect(screen.getByLabelText(/Reason for consultation/)).toHaveValue(
      "Chest pain",
    );
    anchor.remove();
  });

  it("shows an unauthorized state for out-of-scope encounters", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn((input: RequestInfo | URL) => {
        const url = String(input);
        if (url === "/api/session")
          return Promise.resolve(jsonResponse({ authenticated: true }));
        if (url === "/api/v1/meta/tenant")
          return Promise.resolve(jsonResponse(META));
        return Promise.resolve(
          jsonResponse(
            { error: { code: "not_found", message: "HTTP 404" } },
            404,
          ),
        );
      }),
    );
    render(
      <SessionProvider>
        <EncounterPage params={{ id: "e1" }} />
      </SessionProvider>,
    );
    expect(
      await screen.findByText(/do not have permission/i),
    ).toBeInTheDocument();
  });
});
