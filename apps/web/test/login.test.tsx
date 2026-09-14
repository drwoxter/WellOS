import { beforeEach, describe, expect, it, vi } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import SignInPage from "@/app/page";
import { SessionProvider } from "@/lib/session";

const push = vi.fn();
const replace = vi.fn();
vi.mock("next/navigation", () => ({
  useRouter: () => ({ push, replace, prefetch: vi.fn() }),
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

/** Server-reported synthetic identities (only a dev-fixtures build serves these). */
const DEV_USERS = {
  environment: "development",
  synthetic: true,
  users: [
    {
      username: "dr.garcia",
      display_name: "Dr. Gabriel García (Physician)",
      tenant_name: "Hospital Demo Norte",
      roles: ["physician"],
    },
    {
      username: "nurse.kim",
      display_name: "Nurse Ana Kim",
      tenant_name: "Hospital Demo Norte",
      roles: ["nurse"],
    },
    {
      username: "reg.rivera",
      display_name: "Rosa Rivera (Registration)",
      tenant_name: "Hospital Demo Norte",
      roles: ["registration_staff"],
    },
    {
      username: "privacy.wolf",
      display_name: "Petra Wolf (Privacy Officer)",
      tenant_name: "Hospital Demo Norte",
      roles: ["privacy_officer"],
    },
  ],
};

/** A development server: dev auth on, no identity provider configured. */
function devServer(
  extra?: (url: string, init?: RequestInit) => Response | undefined,
) {
  mockFetch((url, init) => {
    const handled = extra?.(url, init);
    if (handled) return handled;
    if (url === "/api/auth/providers") {
      return jsonResponse({
        environment: "development",
        oidc: false,
        development: true,
      });
    }
    if (url === "/api/auth/dev/users") return jsonResponse(DEV_USERS);
    if (url === "/api/session") return jsonResponse({ authenticated: false });
    return jsonResponse({}, 200);
  });
}

describe("development demo login", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    push.mockClear();
    replace.mockClear();
  });

  it("shows role cards with a development-only badge from the server list", async () => {
    devServer();
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
    expect(screen.getAllByText(/Synthetic tenant/).length).toBe(4);
    expect(
      screen.queryByRole("button", { name: /identity provider/ }),
    ).not.toBeInTheDocument();
  });

  it("signs in as the selected demo user and navigates to the dashboard", async () => {
    const calls: { url: string; body?: string }[] = [];
    devServer((url, init) => {
      calls.push({ url, body: init?.body as string | undefined });
      if (url === "/api/session" && init?.method === "POST") {
        return jsonResponse({ ok: true });
      }
      return undefined;
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

  it("routes registration staff to /access without racing the dashboard redirect", async () => {
    devServer((url, init) => {
      if (url === "/api/session" && init?.method === "POST") {
        return jsonResponse({ ok: true });
      }
      return undefined;
    });
    render(
      <SessionProvider>
        <SignInPage />
      </SessionProvider>,
    );
    const card = await screen.findByRole("button", { name: /reg\.rivera/ });
    await userEvent.click(card);
    await waitFor(() => expect(push).toHaveBeenCalledWith("/access"));
    expect(replace).not.toHaveBeenCalled();
  });

  it("redirects an already-authenticated visitor to the dashboard", async () => {
    devServer((url) =>
      url === "/api/session"
        ? jsonResponse({ authenticated: true })
        : undefined,
    );
    render(
      <SessionProvider>
        <SignInPage />
      </SessionProvider>,
    );
    await waitFor(() => expect(replace).toHaveBeenCalledWith("/dashboard"));
    expect(push).not.toHaveBeenCalled();
  });

  it("shows an error when sign-in fails", async () => {
    devServer((url, init) => {
      if (url === "/api/session" && init?.method === "POST") {
        return jsonResponse({ error: { message: "invalid token" } }, 401);
      }
      return undefined;
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

describe("production sign-in", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
    push.mockClear();
    replace.mockClear();
  });

  it("offers only the identity provider when the server reports no development auth", async () => {
    const requested: string[] = [];
    mockFetch((url) => {
      requested.push(url);
      if (url === "/api/auth/providers") {
        return jsonResponse({
          environment: "production",
          oidc: true,
          development: false,
        });
      }
      if (url === "/api/session") return jsonResponse({ authenticated: false });
      return jsonResponse({}, 404);
    });
    render(
      <SessionProvider>
        <SignInPage />
      </SessionProvider>,
    );
    expect(
      await screen.findByRole("button", { name: /identity provider/ }),
    ).toHaveAttribute("href", "/api/auth/oidc/login");
    expect(screen.queryByText("Development only")).not.toBeInTheDocument();
    expect(screen.queryByText(/Sign in as/)).not.toBeInTheDocument();
    expect(requested).not.toContain("/api/auth/dev/users");
  });

  it("never shows development cards when only the user list is reachable", async () => {
    mockFetch((url) => {
      if (url === "/api/auth/providers") {
        return jsonResponse({
          environment: "staging",
          oidc: true,
          development: false,
        });
      }
      if (url === "/api/auth/dev/users") return jsonResponse(DEV_USERS);
      if (url === "/api/session") return jsonResponse({ authenticated: false });
      return jsonResponse({}, 404);
    });
    render(
      <SessionProvider>
        <SignInPage />
      </SessionProvider>,
    );
    await screen.findByRole("button", { name: /identity provider/ });
    expect(screen.queryByText(/dr\.garcia/)).not.toBeInTheDocument();
  });

  it("shows an honest error when sign-in methods cannot be loaded", async () => {
    mockFetch((url) => {
      if (url === "/api/auth/providers") return jsonResponse({}, 503);
      if (url === "/api/session") return jsonResponse({ authenticated: false });
      return jsonResponse({}, 404);
    });
    render(
      <SessionProvider>
        <SignInPage />
      </SessionProvider>,
    );
    expect(await screen.findByRole("alert")).toHaveTextContent(
      /temporarily unavailable/,
    );
    expect(screen.queryByRole("button")).not.toBeInTheDocument();
  });
});
