import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import SignInPage from "@/app/page";
import { SessionProvider } from "@/lib/session";

const push = vi.fn();
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push, replace: vi.fn(), prefetch: vi.fn() }),
  usePathname: () => "/",
}));

function mockFetch(handler: (url: string, init?: RequestInit) => Response) {
  vi.stubGlobal(
    "fetch",
    vi.fn((input: RequestInfo | URL, init?: RequestInit) =>
      Promise.resolve(handler(String(input), init)),
    ),
  );
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

describe("development demo login", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    push.mockClear();
    process.env.NEXT_PUBLIC_WELLOS_DEV_AUTH = "true";
  });

  it("shows role cards with a development-only badge", async () => {
    mockFetch(() => jsonResponse({ authenticated: false }));
    render(
      <SessionProvider>
        <SignInPage />
      </SessionProvider>,
    );
    expect(await screen.findByText("Development only")).toBeInTheDocument();
    expect(screen.getByText(/dr\.garcia/)).toBeInTheDocument();
    expect(screen.getByText(/nurse\.kim/)).toBeInTheDocument();
    expect(screen.getByText(/reg\.rivera/)).toBeInTheDocument();
    expect(screen.getByText(/privacy\.wolf/)).toBeInTheDocument();
  });

  it("signs in as the selected demo user and navigates to the dashboard", async () => {
    const calls: { url: string; body?: string }[] = [];
    mockFetch((url, init) => {
      calls.push({ url, body: init?.body as string | undefined });
      if (url === "/api/session" && init?.method === "POST") {
        return jsonResponse({ ok: true });
      }
      if (url === "/api/session") return jsonResponse({ authenticated: false });
      return jsonResponse({}, 200);
    });
    render(
      <SessionProvider>
        <SignInPage />
      </SessionProvider>,
    );
    const card = await screen.findByRole("button", { name: /dr\.garcia/ });
    await userEvent.click(card);
    await waitFor(() => expect(push).toHaveBeenCalledWith("/dashboard"));
    const signInCall = calls.find((c) => c.body);
    expect(signInCall?.body).toContain("dev-dr.garcia");
  });

  it("shows an error when sign-in fails", async () => {
    mockFetch((url, init) => {
      if (url === "/api/session" && init?.method === "POST") {
        return jsonResponse({ error: { message: "invalid token" } }, 401);
      }
      return jsonResponse({ authenticated: false });
    });
    render(
      <SessionProvider>
        <SignInPage />
      </SessionProvider>,
    );
    const card = await screen.findByRole("button", { name: /dr\.garcia/ });
    await userEvent.click(card);
    expect(await screen.findByRole("alert")).toHaveTextContent("invalid token");
    expect(push).not.toHaveBeenCalled();
  });
});
