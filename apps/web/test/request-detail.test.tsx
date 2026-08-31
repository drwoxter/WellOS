import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen } from "@testing-library/react";
import RequestDetailPage from "@/app/requests/[id]/page";
import { SessionProvider } from "@/lib/session";

const SR_ID = "55555555-5555-5555-5555-555555555555";

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => `/requests/${SR_ID}`,
  useParams: () => ({ id: SR_ID }),
}));

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function detail(
  loopState: string,
  capabilities = { review: true, notify: true, close: true },
) {
  return {
    service_request: {
      id: SR_ID,
      display: "Potassium [Moles/volume] in Serum",
      code_loinc: "2823-3",
      loop_state: loopState,
      version: 2,
      created_at: "2026-08-01T09:00:00Z",
      patient: {
        id: "p1",
        family_name: "Demopatient",
        given_name: "Carlos",
        identifier: "SYN-0001",
      },
    },
    observations: [],
    rule_evaluations: [],
    ai_artifacts: [],
    follow_up_tasks: [],
    alerts: [
      {
        id: "a1",
        severity: "critical",
        message: "Critical potassium 6.9 mmol/L",
        status: "open",
      },
    ],
    data_quality_issues: [],
    notes: [],
    capabilities,
  };
}

function setup(
  loopState: string,
  capabilities = { review: true, notify: true, close: true },
) {
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL) => {
      const url = String(input);
      if (url === "/api/session")
        return Promise.resolve(jsonResponse({ authenticated: true }));
      if (url === `/api/v1/service-requests/${SR_ID}`)
        return Promise.resolve(jsonResponse(detail(loopState, capabilities)));
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
      <RequestDetailPage />
    </SessionProvider>,
  );
}

describe("result detail critical banner", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  it("asks for clinician review while the result awaits review", async () => {
    setup("received");
    expect(
      await screen.findByText(/Critical result — requires clinician review/),
    ).toBeInTheDocument();
  });

  it("shows follow-up wording once the result is reviewed", async () => {
    setup("reviewed");
    expect(
      await screen.findByText(
        /Critical result — reviewed, follow-up in progress/,
      ),
    ).toBeInTheDocument();
    expect(
      screen.queryByText(/Critical result — requires clinician review/),
    ).not.toBeInTheDocument();
  });

  it("hides the transition action when the server denies the capability", async () => {
    setup("received", { review: false, notify: false, close: false });
    await screen.findByText(/Critical result — requires clinician review/);
    expect(
      screen.queryByRole("button", { name: "Mark reviewed" }),
    ).not.toBeInTheDocument();
    expect(screen.queryByText("Next action")).not.toBeInTheDocument();
  });

  it("offers the transition when the server grants the capability", async () => {
    setup("received");
    await screen.findByText(/Critical result — requires clinician review/);
    expect(await screen.findByText("Next action")).toBeInTheDocument();
  });

  it("keeps the follow-up wording after patient notification", async () => {
    setup("notified");
    expect(
      await screen.findByText(
        /Critical result — reviewed, follow-up in progress/,
      ),
    ).toBeInTheDocument();
    expect(
      screen.queryByText(/Critical result — requires clinician review/),
    ).not.toBeInTheDocument();
  });
});
