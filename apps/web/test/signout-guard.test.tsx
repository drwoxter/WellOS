import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import EncounterPage from "@/app/encounters/[id]/page";
import { AppHeader, AppShell } from "@/app/chrome";
import { SessionProvider } from "@/lib/session";
import { useUnsavedChangesGuard } from "@/lib/unsaved-guard";

// One shared router instance, as the App Router context provides; the
// unsaved guard wraps its methods while active, so assertions use the spy.
const pushSpy = vi.fn();
const router = { push: pushSpy, replace: vi.fn(), prefetch: vi.fn() };
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

const WORKSPACE = {
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
  allergies: [],
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
};

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

/** Renders the encounter workspace inside the authenticated shell (its
 *  top-bar sign-out) with the sign-in header's sign-out alongside, as both
 *  controls share one session. Returns the recorded session revocations. */
function setup(
  revoke: () => Promise<Response> = () =>
    Promise.resolve(jsonResponse({ ok: true })),
) {
  const revocations: string[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      if (url === "/api/session") {
        if (init?.method === "DELETE") {
          revocations.push(url);
          return revoke();
        }
        return Promise.resolve(jsonResponse({ authenticated: true }));
      }
      if (url === "/api/v1/meta/tenant")
        return Promise.resolve(jsonResponse(META));
      if (url === "/api/v1/encounters/e1")
        return Promise.resolve(jsonResponse(WORKSPACE));
      return Promise.resolve(jsonResponse({}));
    }),
  );
  render(
    <SessionProvider>
      <div data-testid="header">
        <AppHeader />
      </div>
      <AppShell>
        <EncounterPage params={{ id: "e1" }} />
      </AppShell>
    </SessionProvider>,
  );
  return revocations;
}

async function dirtyNote() {
  const reason = await screen.findByLabelText(/Reason for consultation/);
  await userEvent.setup().type(reason, "Chest pain");
  expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
  return reason;
}

function topbarSignOut() {
  const topbar = document.querySelector(".topbar");
  if (!(topbar instanceof HTMLElement)) throw new Error("topbar missing");
  return within(topbar).getByRole("button", { name: "Sign out" });
}

function lastPush() {
  return pushSpy.mock.calls.at(-1)?.[0];
}

function headerSignOut() {
  return within(screen.getByTestId("header")).getByRole("button", {
    name: "Sign out",
  });
}

describe("sign-out with unsaved documentation", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    pushSpy.mockClear();
    router.replace.mockClear();
  });

  it("asks before revoking the session from the shell top bar and keeps everything when declined", async () => {
    const confirmMock = vi.fn(() => false);
    vi.stubGlobal("confirm", confirmMock);
    const revocations = setup();
    const reason = await dirtyNote();

    await userEvent.setup().click(topbarSignOut());
    expect(confirmMock).toHaveBeenCalledWith(
      expect.stringMatching(/unsaved documentation/i),
    );
    // Nothing happened: session intact, no navigation, editor untouched.
    expect(revocations).toHaveLength(0);
    expect(pushSpy).not.toHaveBeenCalled();
    expect(reason).toHaveValue("Chest pain");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();
    expect(topbarSignOut()).toBeInTheDocument();

    // Accepting signs out once, then leaves without asking again.
    confirmMock.mockReturnValue(true);
    await userEvent.setup().click(topbarSignOut());
    await waitFor(() => expect(revocations).toHaveLength(1));
    await waitFor(() => expect(lastPush()).toBe("/"));
    expect(confirmMock).toHaveBeenCalledTimes(2);
  });

  it("asks before revoking the session from the header control too", async () => {
    const confirmMock = vi.fn(() => false);
    vi.stubGlobal("confirm", confirmMock);
    const revocations = setup();
    const reason = await dirtyNote();

    await userEvent.setup().click(headerSignOut());
    expect(confirmMock).toHaveBeenCalledTimes(1);
    expect(revocations).toHaveLength(0);
    expect(pushSpy).not.toHaveBeenCalled();
    expect(reason).toHaveValue("Chest pain");
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();

    confirmMock.mockReturnValue(true);
    await userEvent.setup().click(headerSignOut());
    await waitFor(() => expect(revocations).toHaveLength(1));
    await waitFor(() => expect(lastPush()).toBe("/"));
  });

  it("re-arms the guard when an accepted sign-out fails to revoke the session", async () => {
    const confirmMock = vi.fn(() => true);
    vi.stubGlobal("confirm", confirmMock);
    const revocations = setup(() =>
      Promise.reject(new TypeError("network down")),
    );
    await dirtyNote();

    await userEvent.setup().click(topbarSignOut());
    await waitFor(() => expect(revocations).toHaveLength(1));
    expect(pushSpy).not.toHaveBeenCalled();
    expect(screen.getByText("Unsaved changes")).toBeInTheDocument();

    // The guard still asks for the next exit.
    confirmMock.mockReturnValue(false);
    router.push("/patients/p1");
    expect(confirmMock).toHaveBeenCalledTimes(2);
  });

  it("completes an accepted exit even when the screen unmounts before its history entry is dropped", async () => {
    // Sign-out ends the session, which unmounts the workspace while the
    // accepted navigation is still waiting for the guard's duplicate history
    // entry to be popped; the navigation must still happen.
    vi.stubGlobal(
      "confirm",
      vi.fn(() => true),
    );
    function Guard() {
      useUnsavedChangesGuard(true, "unsaved documentation");
      return null;
    }
    const { unmount } = render(<Guard />);
    router.push("/");
    expect(pushSpy).not.toHaveBeenCalled();
    unmount();
    await waitFor(() => expect(lastPush()).toBe("/"));
    expect(pushSpy).toHaveBeenCalledTimes(1);
  });

  it("signs out without asking when the note is clean", async () => {
    const confirmMock = vi.fn(() => false);
    vi.stubGlobal("confirm", confirmMock);
    const revocations = setup();
    await screen.findByLabelText(/Reason for consultation/);

    await userEvent.setup().click(topbarSignOut());
    await waitFor(() => expect(revocations).toHaveLength(1));
    await waitFor(() => expect(lastPush()).toBe("/"));
    expect(confirmMock).not.toHaveBeenCalled();
  });
});
