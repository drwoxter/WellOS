import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import { SessionProvider } from "@/lib/session";
import type { ReportDetail, Review } from "@/lib/diagnostics";
import { routeParams } from "./fixtures/params";
import DiagnosticReportPage, {
  approvedExplanation,
  currentSynthesis,
} from "@/app/diagnostics/reports/[id]/page";

const REPORT = "rep-1";

vi.mock("next/navigation", () => ({
  useRouter: () => ({ push: vi.fn(), replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => `/diagnostics/reports/${REPORT}`,
}));

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function review(over: Partial<Review> = {}): Review {
  return {
    id: "rv-1",
    report_version: 2,
    reviewer_id: "u-garcia",
    reviewer_name: "Dr. García",
    clinical_assessment: "Hyperkalaemia confirmed on repeat; ECG unremarkable.",
    disposition: "urgent_follow_up",
    disposition_note: null,
    follow_up_task_ids: [],
    synthesis_artifact_id: null,
    reviewed_at: "2026-10-04T11:00:00Z",
    ...over,
  };
}

function report(over: Partial<ReportDetail> = {}): ReportDetail {
  return {
    id: REPORT,
    service_request_id: "sr-1",
    patient_id: "p1",
    status: "corrected",
    version: 2,
    replaces: "rep-0",
    category_code: "laboratory",
    conclusion: null,
    conclusion_codes: [],
    criticality: "critical",
    criticality_rules: [
      {
        component_ref: "component:seq:1",
        code: "2823-3",
        interpretation: "critical",
        reference_range: "3.5-5.1 mmol/L",
        source: "catalog_component_critical_range",
      },
    ],
    performer_id: null,
    performing_facility_id: "f",
    performing_service_code: null,
    signed_by: "lab.chen",
    signed_at: "2026-10-04T10:30:00Z",
    issued_at: "2026-10-04T10:30:00Z",
    effective_at: null,
    source_system: "fixture-analyser",
    external_report_id: null,
    change_reason: "Analyser re-run after sample-handling review",
    created_at: "2026-10-04T10:30:00Z",
    reviewable: true,
    components: [
      {
        id: "c1",
        code: "2823-3",
        display: "Potassium",
        value: { type: "quantity", value: "6.9", unit: "mmol/L" },
        value_text: "6.9 mmol/L",
        reference_range: "3.5-5.1",
        interpretation: "critical",
        effective_at: null,
        received_at: null,
        source_system: null,
        status: "final",
        superseded: false,
        amends: "c0",
      },
    ],
    replaced_by: null,
    reviews: [],
    release_decisions: [],
    synthesis: [],
    explanations: [],
    documents: [],
    imaging_studies: [],
    order: {
      id: "sr-1",
      display: "Electrolytes panel",
      order_status: "completed",
      fulfilment_mode: "immediate",
      priority: "urgent",
    } as ReportDetail["order"],
    patient: { id: "p1", given_name: "Carlos", family_name: "Demopatient" },
    ...over,
  };
}

type Call = { url: string; method: string; body: unknown };

function setup(
  detail: ReportDetail,
  caps: Partial<Record<string, boolean>> = {},
): Call[] {
  const calls: Call[] = [];
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
      const url = String(input);
      const method = init?.method ?? "GET";
      calls.push({
        url,
        method,
        body: init?.body ? JSON.parse(String(init.body)) : null,
      });
      if (url === "/api/session")
        return Promise.resolve(jsonResponse({ authenticated: true }));
      if (url === `/api/v1/diagnostics/reports/${REPORT}`)
        return Promise.resolve(jsonResponse(detail));
      if (url === `/api/v1/diagnostics/reports/${REPORT}/review`)
        return Promise.resolve(jsonResponse({ review: review() }));
      if (url === `/api/v1/diagnostics/reports/${REPORT}/release`)
        return Promise.resolve(jsonResponse({ decision: {} }));
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
            diagnostics_capabilities: {
              can_read: true,
              can_order: true,
              can_override_safety: true,
              can_fulfil: false,
              can_handle_specimens: false,
              can_write_reports: false,
              can_review: true,
              can_release: true,
              can_manage_catalog: false,
              self_service: false,
              ...caps,
            },
          }),
        );
      return Promise.resolve(jsonResponse({}, 404));
    }),
  );
  render(
    <SessionProvider>
      <DiagnosticReportPage params={routeParams({ id: REPORT })} />
    </SessionProvider>,
  );
  return calls;
}

describe("diagnostic report review and release", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  it("shows the critical banner and withholds release until a professional review exists", async () => {
    setup(report());
    expect(await screen.findByTestId("dx-report-detail")).toHaveAttribute(
      "data-criticality",
      "critical",
    );
    expect(screen.getByTestId("dx-critical-banner")).toBeInTheDocument();
    // Criticality rules render as readable text, never as serialized objects.
    expect(
      screen.getByText(/2823-3 critical \(3\.5-5\.1 mmol\/L\)/),
    ).toBeInTheDocument();
    expect(screen.queryByText(/\[object Object\]/)).not.toBeInTheDocument();
    expect(screen.getByTestId("dx-review-form")).toBeInTheDocument();
    expect(screen.getByTestId("dx-release-needs-review")).toBeInTheDocument();
    expect(screen.queryByTestId("dx-release-submit")).not.toBeInTheDocument();
    // Append-only correction is visible with its amended component.
    expect(screen.getByText(/Replaces/)).toBeInTheDocument();
    expect(screen.getByTestId("dx-component")).toHaveTextContent("6.9 mmol/L");
  });

  it("binds the review to the exact report version", async () => {
    const calls = setup(report());
    const user = userEvent.setup();
    await screen.findByTestId("dx-review-form");
    const submit = screen.getByTestId("dx-submit-review");
    expect(submit).toBeDisabled();
    await user.type(
      screen.getByTestId("dx-assessment"),
      "Hyperkalaemia confirmed on repeat; ECG unremarkable.",
    );
    await user.selectOptions(
      screen.getByTestId("dx-disposition"),
      "urgent_follow_up",
    );
    expect(submit).toBeEnabled();
    await user.click(submit);
    await waitFor(() =>
      expect(
        calls.some((c) => c.url.endsWith("/review") && c.method === "POST"),
      ).toBe(true),
    );
    const post = calls.find((c) => c.url.endsWith("/review"));
    expect(post?.body).toMatchObject({
      report_version: 2,
      disposition: "urgent_follow_up",
      clinical_assessment:
        "Hyperkalaemia confirmed on repeat; ECG unremarkable.",
      follow_ups: [],
    });
  });

  it("requires a bilingual explanation for an abnormal result before release and never releases automatically", async () => {
    const calls = setup(report({ reviews: [review()] }));
    const user = userEvent.setup();
    await screen.findByTestId("dx-release");
    expect(screen.queryByTestId("dx-review-form")).not.toBeInTheDocument();
    const submit = screen.getByTestId("dx-release-submit");
    expect(submit).toBeDisabled();
    await user.type(
      screen.getByTestId("dx-explanation-en"),
      "Your potassium is high; we will call you today.",
    );
    expect(submit).toBeDisabled();
    await user.type(
      screen.getByTestId("dx-explanation-es"),
      "Su potasio está alto; le llamaremos hoy.",
    );
    expect(submit).toBeEnabled();
    await user.click(submit);
    // Explicit confirmation step before the decision is recorded.
    expect(calls.some((c) => c.url.endsWith("/release"))).toBe(false);
    await user.click(screen.getByRole("button", { name: /^Confirm$/ }));
    await waitFor(() =>
      expect(calls.some((c) => c.url.endsWith("/release"))).toBe(true),
    );
    const post = calls.find((c) => c.url.endsWith("/release"));
    expect(post?.body).toMatchObject({
      report_version: 2,
      review_id: "rv-1",
      decision: "release",
      notify_patient: true,
    });
  });

  it("hides review and release controls from a reader without those capabilities", async () => {
    setup(report({ reviews: [review()] }), {
      can_review: false,
      can_release: false,
    });
    await screen.findByTestId("dx-report-detail");
    expect(screen.queryByTestId("dx-review-form")).not.toBeInTheDocument();
    expect(screen.queryByTestId("dx-release")).not.toBeInTheDocument();
    expect(screen.getByTestId("dx-reviews-list")).toHaveTextContent(
      "Dr. García",
    );
  });

  it("points a superseded version to the latest report", async () => {
    setup(report({ replaced_by: "rep-2", reviews: [review()] }));
    await screen.findByTestId("dx-superseded");
    expect(screen.queryByTestId("dx-release")).not.toBeInTheDocument();
    expect(
      screen.getByRole("link", { name: /Open the latest version/ }),
    ).toHaveAttribute("href", "/diagnostics/reports/rep-2");
  });

  it("selects only artifacts bound to the current version", () => {
    const art = (over: object) =>
      ({
        id: "a",
        artifact_type: "x",
        template: "t",
        prompt_version: null,
        model: null,
        model_version: null,
        route: null,
        status: "awaiting_review",
        autonomy_level: null,
        output: null,
        citations: [],
        limitations: [],
        synthetic: true,
        generated_at: null,
        review_decision: null,
        review_note: null,
        reviewed_at: null,
        reviewer_id: null,
        ...over,
      }) as ReportDetail["synthesis"][number];
    const r = report({
      synthesis: [
        art({ id: "old", report_version: 1 }),
        art({ id: "stale", status: "superseded" }),
      ],
      explanations: [
        art({ id: "e-pending" }),
        art({ id: "e-ok", review_decision: "approved" }),
      ],
    });
    expect(currentSynthesis(r)).toBeNull();
    expect(approvedExplanation(r)?.id).toBe("e-ok");
  });
});
