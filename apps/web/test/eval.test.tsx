import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  fireEvent,
  render,
  screen,
  waitFor,
  within,
} from "@testing-library/react";
import EvaluationsPage from "@/app/admin/dmind/evaluations/page";
import EvaluationRunPage from "@/app/admin/dmind/evaluations/[id]/page";
import { SessionProvider } from "@/lib/session";
import { passRate, statusTone } from "@/lib/eval";

const push = vi.fn();
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push, replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => "/admin/dmind/evaluations",
  useParams: () => ({ id: "run-1" }),
}));

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function meta(roles: string[]) {
  return {
    tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
    user: { username: "u", display_name: "Test User", roles },
    facilities: [],
  };
}

const SUITES = (canManage: boolean) => ({
  compiled: {
    suite_key: "dmind-core",
    suite_version: "1.0.0",
    rubric_version: "dmind-eval-rubric.v1",
    case_count: 55,
    corpus_hash: "abcdef0123456789abcdef",
    languages: ["en", "es"],
    operations: [
      {
        operation: "result_summary",
        output_schema: "result_summary.v1",
        deterministic_baseline: true,
        cases: 6,
      },
      {
        operation: "triage_proposal",
        output_schema: "triage_proposal.v1",
        deterministic_baseline: true,
        cases: 6,
      },
    ],
  },
  recorded: [],
  synthetic_tenant: true,
  can_manage: canManage,
  live_mode: "cli_only",
});

const TOTALS = {
  cases: 12,
  passed: 10,
  needs_review: 2,
  failed: 0,
  aborted: 0,
  fallback_verified: 0,
};

const RUN = {
  id: "run-1",
  suite_key: "dmind-core",
  suite_version: "1.0.0",
  rubric_version: "dmind-eval-rubric.v1",
  provider: "fake",
  model: "dmind-fake",
  external: false,
  synthetic_provider: true,
  mode: "offline",
  environment: "test",
  requested_via: "api",
  requested_by: "Ana Silva",
  operations: ["result_summary", "triage_proposal"],
  stop_on_high_severity: true,
  status: "completed",
  abort_reason: null,
  created_at: "2026-10-04T10:00:00Z",
  finished_at: "2026-10-04T10:00:05Z",
  summary: {
    totals: TOTALS,
    operations: { result_summary: TOTALS },
    languages: { en: TOTALS },
    safety_failures: [],
    calls: 12,
    input_tokens: 0,
    output_tokens: 0,
    estimated_cost: "0",
    currency: null,
    p50_latency_ms: 3,
    p95_latency_ms: 9,
  },
};

const EXECUTION = {
  id: "exec-1",
  case_key: "rs-typical-en",
  title: "Typical potassium summary",
  category: "typical",
  operation: "result_summary",
  language: "en",
  prompt_version: "v1",
  schema_version: "result_summary.v1",
  provider: "fake",
  model: "dmind-fake",
  input_hash: "0123456789abcdef0123",
  evidence_refs: ["obs:1"],
  baseline_hash: "fedcba9876543210fedc",
  baseline_output: { summary: "baseline" },
  provider_output: { summary: "provider" },
  execution_kind: "fresh",
  retry_count: 0,
  degraded: false,
  fallback_verified: false,
  status: "needs_review",
  failure_code: null,
  latency_ms: 4,
  input_tokens: null,
  output_tokens: null,
  estimated_cost: null,
  currency: null,
  metrics: [
    {
      metric: "schema_validity",
      verification: "deterministic",
      severity: "high",
      outcome: "pass",
      score: 1,
      detail: null,
    },
    {
      metric: "human_review",
      verification: "human_review_required",
      severity: "info",
      outcome: "review",
      score: null,
      detail: "clinical adequacy needs a human",
    },
  ],
  reviews: [],
};

function setup(
  roles: string[],
  opts: { lang?: "en" | "es"; detail?: boolean } = {},
) {
  if (opts.lang) localStorage.setItem("wellos.lang", opts.lang);
  const canManage = roles.includes("clinical_administrator");
  const posts: { url: string; body: unknown }[] = [];
  const fetchMock = vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input);
    if (url === "/api/session")
      return Promise.resolve(jsonResponse({ authenticated: true }));
    if (url === "/api/v1/meta/tenant")
      return Promise.resolve(jsonResponse(meta(roles)));
    if (url === "/api/v1/admin/dmind/evaluations/suites")
      return Promise.resolve(jsonResponse(SUITES(canManage)));
    if (
      url === "/api/v1/admin/dmind/evaluations/runs" &&
      init?.method === "POST"
    ) {
      posts.push({ url, body: JSON.parse(String(init.body)) });
      return Promise.resolve(
        jsonResponse(
          {
            run_id: "run-1",
            status: "completed",
            totals: TOTALS,
            safety_failures: [],
          },
          201,
        ),
      );
    }
    if (url === "/api/v1/admin/dmind/evaluations/runs")
      return Promise.resolve(
        jsonResponse({
          runs: [RUN],
          next_cursor: "2026-10-01T00:00:00Z|run-1",
        }),
      );
    if (
      url ===
      "/api/v1/admin/dmind/evaluations/runs?cursor=" +
        encodeURIComponent("2026-10-01T00:00:00Z|run-1")
    )
      return Promise.resolve(
        jsonResponse({ runs: [{ ...RUN, id: "run-0" }], next_cursor: null }),
      );
    if (url === "/api/v1/admin/dmind/evaluations/runs/run-1")
      return Promise.resolve(
        jsonResponse({
          run: RUN,
          executions: [EXECUTION],
          can_manage: canManage,
        }),
      );
    if (url.endsWith("/reviews") && init?.method === "POST") {
      posts.push({ url, body: JSON.parse(String(init.body)) });
      return Promise.resolve(
        jsonResponse(
          { id: "rv-1", decision: "accept_result", version: 1 },
          201,
        ),
      );
    }
    return Promise.resolve(jsonResponse({ error: "unexpected " + url }, 500));
  });
  vi.stubGlobal("fetch", fetchMock);
  render(
    <SessionProvider>
      {opts.detail ? <EvaluationRunPage /> : <EvaluationsPage />}
    </SessionProvider>,
  );
  return { posts, fetchMock };
}

describe("dMind evaluation cockpit", () => {
  beforeEach(() => {
    push.mockReset();
    localStorage.clear();
  });

  it("lets a clinical administrator start an offline run on selected operations", async () => {
    const { posts } = setup(["clinical_administrator"]);
    expect(
      await screen.findByTestId("eval-synthetic-banner"),
    ).toHaveTextContent(/Synthetic data only/);
    expect(
      await screen.findByRole("form", { name: "New run" }),
    ).toBeInTheDocument();
    fireEvent.click(screen.getByLabelText("triage_proposal"));
    fireEvent.click(screen.getByRole("button", { name: "Start run" }));
    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0].body).toEqual({
      mode: "offline",
      operations: ["triage_proposal"],
      stop_on_high_severity: true,
    });
    await waitFor(() =>
      expect(push).toHaveBeenCalledWith("/admin/dmind/evaluations/run-1"),
    );
    const table = await screen.findByTestId("eval-run-table");
    expect(table).toHaveTextContent("fake / dmind-fake");
    expect(table).toHaveTextContent("83%");
  });

  it("is read-only for a security auditor and denied to physicians", async () => {
    setup(["security_auditor"]);
    expect(await screen.findByText(/Read-only access/)).toBeInTheDocument();
    expect(
      screen.queryByRole("form", { name: "New run" }),
    ).not.toBeInTheDocument();
    expect(await screen.findByTestId("eval-run-table")).toBeInTheDocument();
  });

  it("denies physicians without calling the evaluation API", async () => {
    const { fetchMock } = setup(["physician"]);
    expect(
      await screen.findByText("Your role cannot access dMind evaluation."),
    ).toBeInTheDocument();
    const urls = fetchMock.mock.calls.map((c) => String(c[0]));
    expect(urls.some((u) => u.includes("/admin/dmind/evaluations"))).toBe(
      false,
    );
  });

  it("renders the run detail in Spanish with metrics and an append-only review form", async () => {
    const { posts } = setup(["clinical_administrator"], {
      lang: "es",
      detail: true,
    });
    expect(await screen.findByTestId("eval-totals")).toHaveTextContent(
      "Superados",
    );
    expect(
      screen.getByText("Sin fallos de seguridad de severidad alta."),
    ).toBeInTheDocument();
    const exec = await screen.findByTestId("eval-execution");
    expect(exec).toHaveTextContent("rs-typical-en");
    expect(exec).toHaveTextContent("Requiere revisión");
    fireEvent.click(screen.getByText(/Métricas \(2\)/));
    expect(screen.getByText("Requiere revisión humana")).toBeInTheDocument();
    const form = screen.getByRole("form", { name: "Registrar revisión" });
    fireEvent.change(form.querySelector("select")!, {
      target: { value: "flag_quality" },
    });
    fireEvent.submit(form);
    expect(
      await screen.findByText(
        "Una marca o solicitud de revisión necesita un comentario.",
      ),
    ).toBeInTheDocument();
    expect(posts).toHaveLength(0);
    fireEvent.change(form.querySelector("textarea")!, {
      target: { value: "nota sintética" },
    });
    fireEvent.click(screen.getByRole("button", { name: "Guardar revisión" }));
    await waitFor(() => expect(posts).toHaveLength(1));
    expect(posts[0].url).toBe(
      "/api/v1/admin/dmind/evaluations/executions/exec-1/reviews",
    );
    expect(posts[0].body).toEqual({
      decision: "flag_quality",
      comment: "nota sintética",
    });
  });

  it("hides the review form from auditors on the detail page", async () => {
    setup(["security_auditor"], { detail: true });
    await screen.findByTestId("eval-execution");
    expect(
      screen.queryByRole("form", { name: "Record review" }),
    ).not.toBeInTheDocument();
  });

  it("pages older runs through the cursor until the list is complete", async () => {
    const { fetchMock } = setup(["clinical_administrator"]);
    const table = await screen.findByTestId("eval-run-table");
    expect(within(table).getAllByRole("row")).toHaveLength(2);
    const more = screen.getByRole("button", { name: "Load older runs" });
    fireEvent.click(more);
    await waitFor(() =>
      expect(within(table).getAllByRole("row")).toHaveLength(3),
    );
    expect(
      screen.queryByRole("button", { name: "Load older runs" }),
    ).toBeNull();
    const urls = fetchMock.mock.calls.map((c) => String(c[0]));
    expect(urls.filter((u) => u.includes("/runs?cursor="))).toHaveLength(1);
  });

  it("derives tones and pass rates deterministically", () => {
    expect(statusTone("failed")).toBe("critical");
    expect(statusTone("provider_unavailable_fallback_verified")).toBe("warn");
    expect(passRate({ ...TOTALS, cases: 0 })).toBeNull();
    expect(passRate(TOTALS)).toBe(83);
  });
});
