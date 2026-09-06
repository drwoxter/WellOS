import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import EncounterPage from "@/app/encounters/[id]/page";
import { SessionProvider } from "@/lib/session";

// One shared router instance, as the App Router context provides: the
// navigation guard wraps its methods and `next/link` navigates through it.
const router = { push: vi.fn(), replace: vi.fn(), prefetch: vi.fn() };
vi.mock("next/navigation", () => ({
  useRouter: () => router,
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

/** Let queued history traversals dispatch their popstate events. */
function settleHistory(): Promise<void> {
  return new Promise((r) => setTimeout(r, 25));
}

function deferred<T>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((r) => {
    resolve = r;
  });
  return { promise, resolve };
}

const DRAFT_NOTE = {
  id: "n1",
  status: "draft",
  version: 1,
  reason_for_encounter: "Cough",
  history_present_illness: null,
  medical_history: null,
  review_of_systems: null,
  physical_exam: null,
  assessment: "Viral illness",
  plan: null,
  follow_up: null,
  author: "Dr. García",
  updated_at: "2026-08-29T09:10:00Z",
  signed_at: null,
  signed_by: null,
};

/** Stub fetch: GET workspace returns `ws` (or whatever `getWorkspace`
 *  yields per call); POST handlers are per-path. */
function setup(
  ws: unknown,
  posts: Record<string, (body: unknown) => Response | Promise<Response>> = {},
  getWorkspace?: () => Response | Promise<Response>,
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
      return Promise.resolve(getWorkspace ? getWorkspace() : jsonResponse(ws));
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
    router.push.mockClear();
    router.replace.mockClear();
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

  it("keeps text typed during a delayed save marked unsaved", async () => {
    const user = userEvent.setup();
    const saveBodies: Record<string, unknown>[] = [];
    const pending = deferred<Response>();
    let calls = 0;
    setup(workspace(), {
      "/api/v1/encounters/e1/note": (body) => {
        saveBodies.push(body as Record<string, unknown>);
        calls += 1;
        return calls === 1
          ? pending.promise
          : jsonResponse({ id: "n1", status: "draft", version: 2 });
      },
    });
    const reason = await screen.findByLabelText(/Reason for consultation/);
    await user.type(reason, "Chest pain");
    await user.click(screen.getByRole("button", { name: "Save draft" }));
    expect(await screen.findAllByText("Saving…")).not.toHaveLength(0);

    // Repeated saves are refused while the first is in flight.
    expect(screen.getByRole("button", { name: "Saving…" })).toBeDisabled();
    expect(saveBodies).toHaveLength(1);

    // The clinician keeps typing while the request is pending.
    await user.type(screen.getByLabelText(/Reason for consultation/), " worse");
    expect(screen.getByLabelText(/Reason for consultation/)).toHaveValue(
      "Chest pain worse",
    );

    pending.resolve(jsonResponse({ id: "n1", status: "draft", version: 1 }));
    expect(
      await screen.findByText(
        "Draft saved. Edits made while saving are not saved yet.",
      ),
    ).toBeInTheDocument();
    expect(saveBodies[0]).toMatchObject({ reason_for_encounter: "Chest pain" });
    // Only the submitted snapshot counts as saved: the newer text remains
    // visible and the note stays dirty.
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
    expect(screen.getByLabelText(/Reason for consultation/)).toHaveValue(
      "Chest pain worse",
    );

    await user.click(screen.getByRole("button", { name: "Save draft" }));
    expect(await screen.findByText("Draft saved.")).toBeInTheDocument();
    expect(saveBodies[1]).toMatchObject({
      reason_for_encounter: "Chest pain worse",
      version: 1,
    });
    expect(screen.queryByText("Unsaved changes")).not.toBeInTheDocument();
  });

  it("freezes editing during a delayed sign and signs exactly the visible draft", async () => {
    const user = userEvent.setup();
    const saveBodies: Record<string, unknown>[] = [];
    const signCalls: unknown[] = [];
    const pendingSave = deferred<Response>();
    setup(workspace({ note: DRAFT_NOTE }), {
      "/api/v1/encounters/e1/note": (body) => {
        saveBodies.push(body as Record<string, unknown>);
        return pendingSave.promise;
      },
      "/api/v1/encounters/e1/sign": (body) => {
        signCalls.push(body);
        return jsonResponse({
          id: "n1",
          status: "signed",
          encounter_status: "completed",
        });
      },
    });
    const plan = await screen.findByLabelText(/^Plan/);
    await user.type(plan, "Rest and fluids");
    await user.click(screen.getByRole("button", { name: "Sign and complete" }));
    await user.click(screen.getByRole("button", { name: "Confirm" }));
    expect(await screen.findAllByText("Signing…")).not.toHaveLength(0);
    expect(saveBodies).toHaveLength(1);
    expect(saveBodies[0]).toMatchObject({
      plan: "Rest and fluids",
      version: 1,
    });

    // Inputs are read-only while the sign finalises: nothing typed now can
    // be lost between the saved snapshot and the signed record.
    const frozenPlan = screen.getByLabelText(/^Plan/);
    expect(frozenPlan).toHaveAttribute("readonly");
    await user.type(frozenPlan, " and review");
    expect(frozenPlan).toHaveValue("Rest and fluids");

    pendingSave.resolve(
      jsonResponse({ id: "n1", status: "draft", version: 2 }),
    );
    await waitFor(() => expect(signCalls).toHaveLength(1));
    expect(signCalls[0]).toEqual({ version: 2 });
    expect(await screen.findByText(/Note signed\./)).toBeInTheDocument();
  });

  it("guards programmatic navigation and browser history while dirty", async () => {
    const user = userEvent.setup();
    const confirmMock = vi.fn(() => false);
    vi.stubGlobal("confirm", confirmMock);
    const originalPush = router.push;
    setup(workspace(), {
      "/api/v1/encounters/e1/note": () =>
        jsonResponse({ id: "n1", status: "draft", version: 1 }),
    });
    const reason = await screen.findByLabelText(/Reason for consultation/);
    await user.type(reason, "Chest pain");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();

    // Declined programmatic navigation: nothing happens.
    router.push("/patients/p1");
    expect(confirmMock).toHaveBeenCalledWith(
      expect.stringMatching(/unsaved documentation/i),
    );
    expect(originalPush).not.toHaveBeenCalled();

    // Declined Back: URL, text and dirty state are untouched.
    const href = window.location.href;
    window.history.back();
    await waitFor(() => expect(confirmMock).toHaveBeenCalledTimes(2));
    expect(window.location.href).toBe(href);
    expect(screen.getByLabelText(/Reason for consultation/)).toHaveValue(
      "Chest pain",
    );
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();

    // Accepted programmatic navigation proceeds exactly once.
    confirmMock.mockReturnValue(true);
    router.push("/patients/p1");
    await waitFor(() =>
      expect(originalPush).toHaveBeenCalledWith("/patients/p1", undefined),
    );
    expect(confirmMock).toHaveBeenCalledTimes(3);
  });

  it("keeps guarding after Forward onto a duplicate entry and only asks when Back leaves the screen", async () => {
    const user = userEvent.setup();
    const confirmMock = vi.fn(() => false);
    vi.stubGlobal("confirm", confirmMock);
    setup(workspace());
    const reason = await screen.findByLabelText(/Reason for consultation/);
    await user.type(reason, "Chest pain");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
    const href = window.location.href;

    // A same-URL entry ahead of the guard's duplicate, as left behind by a
    // guard that re-armed before its predecessor's cleanup settled.
    window.history.pushState(window.history.state, "", window.location.href);
    window.history.back();
    await settleHistory();
    window.history.forward();
    await settleHistory();

    // Forward never left the screen: no question, nothing stood down.
    expect(confirmMock).not.toHaveBeenCalled();
    expect(window.location.href).toBe(href);
    expect(screen.getByLabelText(/Reason for consultation/)).toHaveValue(
      "Chest pain",
    );
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();

    // Back onto the screen's own entry asks; declining keeps everything.
    window.history.back();
    await settleHistory();
    window.history.back();
    await waitFor(() => expect(confirmMock).toHaveBeenCalledTimes(1));
    expect(window.location.href).toBe(href);
    expect(screen.getByLabelText(/Reason for consultation/)).toHaveValue(
      "Chest pain",
    );
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();

    // Accepting continues backward, off the screen, with one question.
    confirmMock.mockReturnValue(true);
    const backSpy = vi.spyOn(window.history, "back");
    window.history.back();
    await waitFor(() => expect(confirmMock).toHaveBeenCalledTimes(2));
    expect(backSpy).toHaveBeenCalledTimes(2);
    backSpy.mockRestore();
  });

  it("returns to the screen when a multi-entry history jump is declined", async () => {
    const user = userEvent.setup();
    const confirmMock = vi.fn(() => false);
    vi.stubGlobal("confirm", confirmMock);
    setup(workspace());
    const reason = await screen.findByLabelText(/Reason for consultation/);
    // Two earlier screens behind the encounter, as a history menu would show.
    window.history.pushState(null, "", "/patients");
    window.history.pushState(null, "", "/patients/p1");
    window.history.pushState(null, "", "/encounters/e1");
    await user.type(reason, "Chest pain");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
    const href = window.location.href;
    expect(href).toContain("/encounters/e1");

    // Jump straight over the duplicate and the screen's own entry.
    window.history.go(-3);
    await waitFor(() => expect(confirmMock).toHaveBeenCalledTimes(1));
    await waitFor(() => expect(window.location.href).toBe(href));
    await settleHistory();
    expect(window.location.href).toBe(href);
    expect(screen.getByLabelText(/Reason for consultation/)).toHaveValue(
      "Chest pain",
    );
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();

    // The guard is still armed: a plain Back asks again.
    window.history.back();
    await waitFor(() => expect(confirmMock).toHaveBeenCalledTimes(2));
    await settleHistory();
    expect(window.location.href).toBe(href);

    // Accepting a jump lets the browser stay where it landed.
    confirmMock.mockReturnValue(true);
    window.history.go(-2);
    await waitFor(() => expect(confirmMock).toHaveBeenCalledTimes(3));
    await settleHistory();
    expect(window.location.href).toContain("/patients/p1");
  });

  it("re-arms Back when the note is edited again right after a save", async () => {
    const user = userEvent.setup();
    const confirmMock = vi.fn(() => false);
    vi.stubGlobal("confirm", confirmMock);
    const pendingSave = deferred<Response>();
    setup(workspace(), {
      "/api/v1/encounters/e1/note": () => pendingSave.promise,
    });
    const reason = await screen.findByLabelText(/Reason for consultation/);
    await user.type(reason, "Chest pain");
    await user.click(screen.getByRole("button", { name: "Save draft" }));

    // Edit in the same task that renders the save result, before the
    // previous guard's cleanup traversal (a queued task) has landed.
    const raced = new Promise<void>((resolve) => {
      const observer = new MutationObserver(() => {
        if (!screen.queryByText("Draft saved.")) return;
        observer.disconnect();
        fireEvent.change(reason, { target: { value: "Chest pain, acute" } });
        resolve();
      });
      observer.observe(document.body, {
        childList: true,
        subtree: true,
        characterData: true,
      });
    });
    pendingSave.resolve(
      jsonResponse({ id: "n1", status: "draft", version: 1 }),
    );
    await raced;
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
    const href = window.location.href;
    await settleHistory();

    // No duplicate is left ahead of the guard, and Back asks exactly once.
    window.history.forward();
    await settleHistory();
    expect(confirmMock).not.toHaveBeenCalled();
    expect(window.location.href).toBe(href);
    window.history.back();
    await waitFor(() => expect(confirmMock).toHaveBeenCalledTimes(1));
    expect(window.location.href).toBe(href);
    expect(screen.getByLabelText(/Reason for consultation/)).toHaveValue(
      "Chest pain, acute",
    );
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
  });

  it("stands the navigation guard down once the draft is saved", async () => {
    const user = userEvent.setup();
    const confirmMock = vi.fn(() => false);
    vi.stubGlobal("confirm", confirmMock);
    const originalPush = router.push;
    setup(workspace(), {
      "/api/v1/encounters/e1/note": () =>
        jsonResponse({ id: "n1", status: "draft", version: 1 }),
    });
    const reason = await screen.findByLabelText(/Reason for consultation/);
    await user.type(reason, "Chest pain");
    await user.click(screen.getByRole("button", { name: "Save draft" }));
    expect(await screen.findByText("Draft saved.")).toBeInTheDocument();
    expect(router.push).toBe(originalPush);
    router.push("/patients/p1");
    expect(confirmMock).not.toHaveBeenCalled();
    expect(originalPush).toHaveBeenCalledWith("/patients/p1");
  });

  it("presents order-only encounters as laboratory contexts, not consultations", async () => {
    setup(
      workspace({
        encounter: {
          id: "e1",
          status: "in_progress",
          encounter_type: "order_only",
          started_at: "2026-08-29T09:00:00Z",
          completed_at: null,
          practitioner: "Dr. García",
          facility_name: "Central Hospital",
          own: true,
        },
        capabilities: {
          can_document: false,
          can_sign: false,
          can_add_addendum: false,
          can_order_lab: true,
        },
      }),
    );
    expect(
      await screen.findByText(/only holds laboratory orders/i),
    ).toBeInTheDocument();
    expect(
      screen.queryByLabelText(/Reason for consultation/),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Sign and complete" }),
    ).not.toBeInTheDocument();
    expect(screen.getAllByText("Laboratory orders").length).toBeGreaterThan(0);
  });

  it("flags a dMind draft generated from an older note version as stale", async () => {
    setup(
      workspace({
        note: { ...DRAFT_NOTE, version: 3 },
        ai_draft: {
          id: "a1",
          status: "superseded",
          output: {
            summary: "Older assistive summary.",
            limitations: [],
            cited_sources: ["encounter_note:n1:v2:assessment"],
          },
          limitations: [],
          citations: ["encounter_note:n1:v2:assessment"],
          model: "dmind-fake",
          model_version: "0.1.0",
          generated_at: "2026-08-29T09:30:00Z",
          review_decision: null,
          note_version: 2,
          stale: true,
        },
      }),
    );
    expect(
      await screen.findByText(/note changed after this draft was generated/i),
    ).toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Accept and copy into assessment" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByText("Older assistive summary."),
    ).not.toBeInTheDocument();
  });

  it("shows the exact note version a current dMind draft was generated from", async () => {
    setup(
      workspace({
        note: { ...DRAFT_NOTE, version: 3 },
        ai_draft: {
          id: "a1",
          status: "awaiting_review",
          output: {
            summary: "Current assistive summary.",
            limitations: [],
            cited_sources: ["encounter_note:n1:v3:assessment"],
          },
          limitations: [],
          citations: ["encounter_note:n1:v3:assessment"],
          model: "dmind-fake",
          model_version: "0.1.0",
          generated_at: "2026-08-29T09:30:00Z",
          review_decision: null,
          note_version: 3,
          stale: false,
        },
      }),
    );
    expect(
      await screen.findByText("Current assistive summary."),
    ).toBeInTheDocument();
    expect(screen.getByText(/Note version 3/)).toBeInTheDocument();
    expect(
      screen.getByText("encounter_note:n1:v3:assessment"),
    ).toBeInTheDocument();
  });

  const AWAITING_DRAFT = {
    id: "a1",
    status: "awaiting_review",
    output: {
      summary: "Accepted assistive summary.",
      limitations: [],
      cited_sources: ["encounter_note:n1:v1:assessment"],
    },
    limitations: [],
    citations: ["encounter_note:n1:v1:assessment"],
    model: "dmind-fake",
    model_version: "0.1.0",
    generated_at: "2026-08-29T09:30:00Z",
    review_decision: null,
    note_version: 1,
    stale: false,
  };

  it("accepts a dMind draft through the atomic endpoint and hydrates the persisted note", async () => {
    const user = userEvent.setup();
    const accepts: Record<string, unknown>[] = [];
    const reviews: unknown[] = [];
    const saves: Record<string, unknown>[] = [];
    const pending = [deferred<Response>(), deferred<Response>()];
    const note = { ...DRAFT_NOTE };
    const draft = { ...AWAITING_DRAFT };
    setup(workspace({ note, ai_draft: draft }), {
      "/api/v1/encounters/e1/ai-draft/accept": (body) => {
        accepts.push(body as Record<string, unknown>);
        return pending[accepts.length - 1].promise;
      },
      "/api/v1/ai-artifacts/a1/review": (body) => {
        reviews.push(body);
        return jsonResponse({ id: "a1", status: "approved" });
      },
      "/api/v1/encounters/e1/note": (body) => {
        saves.push(body as Record<string, unknown>);
        return jsonResponse({ id: "n1", status: "draft", version: 3 });
      },
    });
    const accept = await screen.findByRole("button", {
      name: "Accept and copy into assessment",
    });
    const withSummary = "Viral illness\n\nAccepted assistive summary.";

    // The current draft travels with the artifact so the server persists
    // both in one transaction; nothing is claimed while it is in flight.
    await user.click(accept);
    await waitFor(() => expect(accepts).toHaveLength(1));
    expect(accepts[0]).toMatchObject({
      artifact_id: "a1",
      version: 1,
      reason_for_encounter: "Cough",
      assessment: "Viral illness",
    });
    expect(screen.getAllByText("Saving…").length).toBeGreaterThan(0);
    expect(screen.getByLabelText(/^Assessment/)).toHaveValue("Viral illness");
    expect(screen.queryByText(/Draft accepted/)).not.toBeInTheDocument();

    // A failed transaction leaves the local note exactly as it was: no AI
    // text and no approval.
    pending[0].resolve(
      apiError(409, "version_conflict", "the note was updated by someone else"),
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /changed by someone else/,
    );
    expect(screen.getByLabelText(/^Assessment/)).toHaveValue("Viral illness");
    expect(screen.queryByText(/Draft accepted/)).not.toBeInTheDocument();
    expect(screen.queryByText(/Unsaved changes/)).not.toBeInTheDocument();

    // Success hydrates the persisted assessment and version: the note is
    // saved, not merely edited locally.
    await user.click(accept);
    await waitFor(() => expect(accepts).toHaveLength(2));
    Object.assign(note, { version: 2, assessment: withSummary });
    Object.assign(draft, { status: "approved", review_decision: "approved" });
    pending[1].resolve(
      jsonResponse({
        artifact_id: "a1",
        status: "approved",
        note: {
          id: "n1",
          status: "draft",
          version: 2,
          assessment: withSummary,
        },
      }),
    );
    expect(await screen.findByText(/Draft accepted/)).toBeInTheDocument();
    expect(screen.getByLabelText(/^Assessment/)).toHaveValue(withSummary);
    expect(screen.getByText(/Draft saved\./)).toBeInTheDocument();
    expect(screen.queryByText(/Unsaved changes/)).not.toBeInTheDocument();
    // Approval never goes through the decision-only review endpoint.
    expect(reviews).toHaveLength(0);

    // The next save builds on the version the acceptance produced.
    await user.type(screen.getByLabelText(/^Plan/), "Fluids");
    await user.click(screen.getByRole("button", { name: "Save draft" }));
    await waitFor(() => expect(saves).toHaveLength(1));
    expect(saves[0]).toMatchObject({
      version: 2,
      assessment: withSummary,
      plan: "Fluids",
    });
  });

  it("keeps text typed during a delayed dMind acceptance and marks it unsaved", async () => {
    const user = userEvent.setup();
    const pendingAccept = deferred<Response>();
    const note = { ...DRAFT_NOTE };
    const draft = { ...AWAITING_DRAFT };
    setup(workspace({ note, ai_draft: draft }), {
      "/api/v1/encounters/e1/ai-draft/accept": () => pendingAccept.promise,
    });
    const accept = await screen.findByRole("button", {
      name: "Accept and copy into assessment",
    });
    await user.click(accept);
    expect(await screen.findAllByText("Saving…")).not.toHaveLength(0);

    const assessment = screen.getByLabelText(/^Assessment/);
    await user.type(assessment, " worsening");
    Object.assign(note, {
      version: 2,
      assessment: "Viral illness\n\nAccepted assistive summary.",
    });
    Object.assign(draft, { status: "approved", review_decision: "approved" });
    pendingAccept.resolve(
      jsonResponse({
        artifact_id: "a1",
        status: "approved",
        note: {
          id: "n1",
          status: "draft",
          version: 2,
          assessment: "Viral illness\n\nAccepted assistive summary.",
        },
      }),
    );
    expect(await screen.findByText(/Draft accepted/)).toBeInTheDocument();
    // The newer local text is neither discarded nor reported as saved.
    expect(assessment).toHaveValue(
      "Viral illness worsening\n\nAccepted assistive summary.",
    );
    expect(screen.getByText(/Unsaved changes/)).toBeInTheDocument();
    expect(
      screen.getByText(/Edits made while saving are not saved yet/),
    ).toBeInTheDocument();
  });

  it("blocks dMind acceptance during a delayed sign so no approval can omit its text", async () => {
    const user = userEvent.setup();
    const saveBodies: Record<string, unknown>[] = [];
    const signCalls: unknown[] = [];
    const reviewCalls: unknown[] = [];
    const pendingSign = deferred<Response>();
    setup(workspace({ note: DRAFT_NOTE, ai_draft: AWAITING_DRAFT }), {
      "/api/v1/encounters/e1/note": (body) => {
        saveBodies.push(body as Record<string, unknown>);
        return jsonResponse({ id: "n1", status: "draft", version: 2 });
      },
      "/api/v1/encounters/e1/sign": (body) => {
        signCalls.push(body);
        return pendingSign.promise;
      },
      "/api/v1/encounters/e1/ai-draft/accept": (body) => {
        reviewCalls.push(body);
        return jsonResponse({ artifact_id: "a1", status: "approved" });
      },
    });
    const accept = await screen.findByRole("button", {
      name: "Accept and copy into assessment",
    });
    expect(accept).toBeEnabled();

    // The draft is already saved, so signing goes straight to the sign
    // request and the dMind draft stays current while it is in flight.
    await user.click(screen.getByRole("button", { name: "Sign and complete" }));
    await user.click(screen.getByRole("button", { name: "Confirm" }));
    expect(await screen.findAllByText("Signing…")).not.toHaveLength(0);
    await waitFor(() => expect(signCalls).toEqual([{ version: 1 }]));
    expect(saveBodies).toHaveLength(0);

    // While the sign is in flight every dMind control is disabled and an
    // acceptance attempt neither records a decision nor reports success.
    expect(accept).toBeDisabled();
    expect(screen.getByRole("button", { name: "Reject draft" })).toBeDisabled();
    expect(
      screen.getByRole("button", { name: /Generate draft summary/ }),
    ).toBeDisabled();
    await user.click(accept);
    expect(reviewCalls).toHaveLength(0);
    expect(screen.queryByText(/Draft accepted/)).not.toBeInTheDocument();
    expect(screen.getByLabelText(/^Assessment/)).toHaveValue("Viral illness");

    pendingSign.resolve(
      jsonResponse({
        id: "n1",
        status: "signed",
        encounter_status: "completed",
      }),
    );
    expect(await screen.findByText(/Note signed\./)).toBeInTheDocument();
    // No approval exists for text that never entered the signed note.
    expect(reviewCalls).toHaveLength(0);
  });

  it("retires an awaiting dMind draft as soon as the note it cites is saved", async () => {
    const user = userEvent.setup();
    const note = { ...DRAFT_NOTE };
    const draft = { ...AWAITING_DRAFT };
    const fetchMock = setup(workspace({ note, ai_draft: draft }), {
      "/api/v1/encounters/e1/note": () => {
        // The server supersedes the draft in the same transaction.
        Object.assign(note, { version: 2, plan: "Rest" });
        Object.assign(draft, { status: "superseded", stale: true });
        return jsonResponse({ id: "n1", status: "draft", version: 2 });
      },
    });
    const accept = await screen.findByRole("button", {
      name: "Accept and copy into assessment",
    });
    expect(accept).toBeEnabled();
    const workspaceReads = () =>
      fetchMock.mock.calls.filter(
        ([input, init]) =>
          String(input) === "/api/v1/encounters/e1" && init?.method !== "POST",
      ).length;
    const readsBefore = workspaceReads();

    await user.type(screen.getByLabelText(/^Plan/), "Rest");
    await user.click(screen.getByRole("button", { name: "Save draft" }));
    expect(await screen.findByText("Draft saved.")).toBeInTheDocument();

    // Review controls go away with the save and the workspace is refreshed
    // so the superseded state comes from the server, not a guess.
    expect(
      screen.queryByRole("button", { name: "Accept and copy into assessment" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Reject draft" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText(/The note changed after this draft was generated/),
    ).toBeInTheDocument();
    await waitFor(() => expect(workspaceReads()).toBe(readsBefore + 1));
    expect(screen.getByLabelText(/^Plan/)).toHaveValue("Rest");
    expect(screen.queryByText("Unsaved changes")).not.toBeInTheDocument();
  });

  it("keeps an awaiting dMind draft reviewable when a save changes nothing", async () => {
    const user = userEvent.setup();
    const saves: Record<string, unknown>[] = [];
    setup(workspace({ note: { ...DRAFT_NOTE }, ai_draft: AWAITING_DRAFT }), {
      "/api/v1/encounters/e1/note": (body) => {
        saves.push(body as Record<string, unknown>);
        // Identical sections: the server returns the current version.
        return jsonResponse({ id: "n1", status: "draft", version: 1 });
      },
    });
    expect(
      await screen.findByRole("button", {
        name: "Accept and copy into assessment",
      }),
    ).toBeEnabled();

    await user.click(screen.getByRole("button", { name: "Save draft" }));
    expect(await screen.findByText("Draft saved.")).toBeInTheDocument();
    expect(saves).toHaveLength(1);
    expect(saves[0].version).toBe(1);

    // Nothing the draft summarised changed, so it is still reviewable.
    expect(
      screen.getByRole("button", { name: "Accept and copy into assessment" }),
    ).toBeEnabled();
    expect(
      screen.queryByText(/The note changed after this draft was generated/),
    ).not.toBeInTheDocument();
  });

  it("saves the note before generating a dMind draft so it summarises the visible text", async () => {
    const user = userEvent.setup();
    const posts: string[] = [];
    let savedPlan: unknown = null;
    setup(workspace({ note: { ...DRAFT_NOTE } }), {
      "/api/v1/encounters/e1/note": (body) => {
        posts.push("note");
        savedPlan = (body as { plan?: unknown }).plan;
        return jsonResponse({ id: "n1", status: "draft", version: 2 });
      },
      "/api/v1/encounters/e1/ai-draft": () => {
        posts.push("ai-draft");
        return jsonResponse({ ...AWAITING_DRAFT, note_version: 2 });
      },
    });
    const plan = await screen.findByLabelText(/^Plan/);
    await user.type(plan, "Rest");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();

    await user.click(
      screen.getByRole("button", { name: "Generate draft summary" }),
    );
    await waitFor(() => expect(posts).toEqual(["note", "ai-draft"]));
    expect(savedPlan).toBe("Rest");
    await waitFor(() =>
      expect(screen.queryByText("Unsaved changes")).not.toBeInTheDocument(),
    );
  });

  it("does not generate a dMind draft when the note cannot be saved first", async () => {
    const user = userEvent.setup();
    const posts: string[] = [];
    setup(workspace({ note: { ...DRAFT_NOTE } }), {
      "/api/v1/encounters/e1/note": () => {
        posts.push("note");
        return apiError(409, "version_conflict", "changed elsewhere");
      },
      "/api/v1/encounters/e1/ai-draft": () => {
        posts.push("ai-draft");
        return jsonResponse(AWAITING_DRAFT);
      },
    });
    const plan = await screen.findByLabelText(/^Plan/);
    await user.type(plan, "Rest");
    await user.click(
      screen.getByRole("button", { name: "Generate draft summary" }),
    );
    expect(
      await screen.findByText(/The note must be saved before a draft/),
    ).toBeInTheDocument();
    expect(posts).toEqual(["note"]);
    expect(plan).toHaveValue("Rest");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
  });

  it("does not generate a dMind draft when the note is edited during the save before it", async () => {
    const user = userEvent.setup();
    const posts: string[] = [];
    const pendingSave = deferred<Response>();
    setup(workspace({ note: { ...DRAFT_NOTE } }), {
      "/api/v1/encounters/e1/note": () => {
        posts.push("note");
        return posts.length === 1
          ? pendingSave.promise
          : jsonResponse({ id: "n1", status: "draft", version: 3 });
      },
      "/api/v1/encounters/e1/ai-draft": () => {
        posts.push("ai-draft");
        return jsonResponse({ ...AWAITING_DRAFT, note_version: 3 });
      },
    });
    const plan = await screen.findByLabelText(/^Plan/);
    await user.type(plan, "Rest");
    await user.click(
      screen.getByRole("button", { name: "Generate draft summary" }),
    );
    await waitFor(() => expect(posts).toEqual(["note"]));

    // More text arrives while the prerequisite save is still in flight; it
    // is not in the saved snapshot, so no draft may be generated from it.
    await user.type(plan, " and fluids");
    pendingSave.resolve(
      jsonResponse({ id: "n1", status: "draft", version: 2 }),
    );

    expect(
      await screen.findByText(/The note changed while it was being saved/),
    ).toBeInTheDocument();
    expect(posts).toEqual(["note"]);
    expect(plan).toHaveValue("Rest and fluids");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
    expect(
      screen.getByText(
        "Draft saved. Edits made while saving are not saved yet.",
      ),
    ).toBeInTheDocument();

    // Generating again saves the visible text first and then proceeds.
    await user.click(
      screen.getByRole("button", { name: "Generate draft summary" }),
    );
    await waitFor(() => expect(posts).toEqual(["note", "note", "ai-draft"]));
    expect(screen.queryByText("Unsaved changes")).not.toBeInTheDocument();
  });

  it("withholds dMind acceptance while the note has edits the draft never summarised", async () => {
    const user = userEvent.setup();
    const accepts: unknown[] = [];
    setup(workspace({ note: { ...DRAFT_NOTE }, ai_draft: AWAITING_DRAFT }), {
      "/api/v1/encounters/e1/ai-draft/accept": (body) => {
        accepts.push(body);
        return jsonResponse({ note: { version: 2, assessment: "x" } });
      },
    });
    expect(
      await screen.findByRole("button", {
        name: "Accept and copy into assessment",
      }),
    ).toBeEnabled();

    await user.type(screen.getByLabelText(/^Plan/), "Rest");
    expect(
      screen.queryByRole("button", { name: "Accept and copy into assessment" }),
    ).not.toBeInTheDocument();
    expect(
      screen.getByText(/unsaved edits this draft does not cover/),
    ).toBeInTheDocument();
    expect(accepts).toHaveLength(0);
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
  });

  it("ignores a post-save workspace read that resolves after a later save", async () => {
    const user = userEvent.setup();
    const saves: Record<string, unknown>[] = [];
    // Workspace reads after the initial one are answered by the test.
    const reads: { resolve: (value: Response) => void }[] = [];
    let version = 1;
    setup(
      null,
      {
        "/api/v1/encounters/e1/note": (body) => {
          saves.push(body as Record<string, unknown>);
          version += 1;
          return jsonResponse({ id: "n1", status: "draft", version });
        },
      },
      () => {
        if (reads.length === 0) {
          reads.push({ resolve: () => undefined });
          return jsonResponse(workspace());
        }
        const d = deferred<Response>();
        reads.push(d);
        return d.promise;
      },
    );
    const plan = await screen.findByLabelText(/^Plan/);

    // First save: its follow-up workspace read (reads[1]) is left hanging.
    await user.type(plan, "A");
    await user.click(screen.getByRole("button", { name: "Save draft" }));
    expect(await screen.findByText("Draft saved.")).toBeInTheDocument();
    await waitFor(() => expect(reads).toHaveLength(2));

    // Second save completes, and its own read (reads[2]) answers first.
    await user.type(plan, "B");
    await user.click(screen.getByRole("button", { name: "Save draft" }));
    await waitFor(() => expect(saves).toHaveLength(2));
    expect(saves[1]).toMatchObject({ version: 2, plan: "AB" });
    await waitFor(() => expect(reads).toHaveLength(3));
    reads[2].resolve(
      jsonResponse(
        workspace({ note: { ...DRAFT_NOTE, version: 3, plan: "AB" } }),
      ),
    );
    expect(await screen.findByText("Draft saved.")).toBeInTheDocument();
    expect(plan).toHaveValue("AB");

    // The older read now arrives with the older note: it must not win.
    reads[1].resolve(
      jsonResponse(
        workspace({ note: { ...DRAFT_NOTE, version: 2, plan: "A" } }),
      ),
    );
    await new Promise((r) => setTimeout(r, 20));
    expect(plan).toHaveValue("AB");
    expect(screen.queryByText("Unsaved changes")).not.toBeInTheDocument();

    // The next save still builds on the newest version.
    await user.type(plan, "C");
    await user.click(screen.getByRole("button", { name: "Save draft" }));
    await waitFor(() => expect(saves).toHaveLength(3));
    expect(saves[2]).toMatchObject({ version: 3, plan: "ABC" });
  });

  it("keeps the saved workspace usable when the post-save refresh fails", async () => {
    const user = userEvent.setup();
    let failReads = false;
    setup(
      null,
      {
        "/api/v1/encounters/e1/note": () =>
          jsonResponse({ id: "n1", status: "draft", version: 2 }),
      },
      () =>
        failReads
          ? apiError(503, "unavailable", "backend unavailable")
          : jsonResponse(workspace()),
    );
    const plan = await screen.findByLabelText(/^Plan/);
    failReads = true;
    await user.type(plan, "Rest");
    await user.click(screen.getByRole("button", { name: "Save draft" }));
    expect(await screen.findByText("Draft saved.")).toBeInTheDocument();

    // The editor stays, with the saved text, behind a retryable notice.
    expect(
      await screen.findByText(/could not be refreshed/),
    ).toBeInTheDocument();
    expect(screen.getByLabelText(/^Plan/)).toHaveValue("Rest");
    expect(screen.queryByText("Unsaved changes")).not.toBeInTheDocument();
    expect(screen.getByRole("button", { name: "Save draft" })).toBeEnabled();

    failReads = false;
    await user.click(screen.getByRole("button", { name: "Try again" }));
    await waitFor(() =>
      expect(
        screen.queryByText(/could not be refreshed/),
      ).not.toBeInTheDocument(),
    );
    expect(screen.getByLabelText(/^Plan/)).toBeInTheDocument();
  });

  it("closes the workspace after signing even when the refresh fails", async () => {
    const user = userEvent.setup();
    let failReads = false;
    setup(
      null,
      {
        "/api/v1/encounters/e1/note": () =>
          jsonResponse({ id: "n1", status: "draft", version: 2 }),
        "/api/v1/encounters/e1/sign": () =>
          jsonResponse({
            id: "n1",
            status: "signed",
            encounter_status: "completed",
          }),
      },
      () =>
        failReads
          ? apiError(503, "unavailable", "backend unavailable")
          : jsonResponse(workspace()),
    );
    const plan = await screen.findByLabelText(/^Plan/);
    await user.type(plan, "Rest and fluids");
    failReads = true;
    await user.click(screen.getByRole("button", { name: "Sign and complete" }));
    await user.click(screen.getByRole("button", { name: "Confirm" }));

    // The signed record is shown read-only with the confirmed text; nothing
    // can be edited, saved, signed or cancelled against the closed encounter.
    expect(await screen.findByText("Clinical summary")).toBeInTheDocument();
    expect(screen.getByText("Signed and completed")).toBeInTheDocument();
    expect(screen.getByText("Rest and fluids")).toBeInTheDocument();
    expect(screen.queryByLabelText(/^Plan/)).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Save draft" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Sign and complete" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Cancel consultation" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Record vital signs" }),
    ).not.toBeInTheDocument();
    expect(
      await screen.findByText(/could not be refreshed/),
    ).toBeInTheDocument();
  });

  it("closes the workspace after cancellation even when the refresh fails", async () => {
    const user = userEvent.setup();
    let failReads = false;
    setup(
      null,
      {
        "/api/v1/encounters/e1/cancel": () =>
          jsonResponse({ id: "e1", status: "cancelled" }),
      },
      () =>
        failReads
          ? apiError(503, "unavailable", "backend unavailable")
          : jsonResponse(workspace()),
    );
    await screen.findByLabelText(/^Plan/);
    failReads = true;
    await user.click(
      screen.getByRole("button", { name: "Cancel consultation" }),
    );
    await user.click(screen.getByRole("button", { name: "Confirm" }));

    expect(await screen.findByText("Cancelled")).toBeInTheDocument();
    expect(screen.queryByLabelText(/^Plan/)).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Save draft" }),
    ).not.toBeInTheDocument();
    expect(
      screen.queryByRole("button", { name: "Record vital signs" }),
    ).not.toBeInTheDocument();
    expect(
      await screen.findByText(/could not be refreshed/),
    ).toBeInTheDocument();
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
