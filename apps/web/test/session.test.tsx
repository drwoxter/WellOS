import { beforeEach, describe, expect, it, vi } from "vitest";
import { act, render, screen, waitFor } from "@testing-library/react";
import { SessionProvider, useSession } from "@/lib/session";
import type { TenantMeta } from "@/lib/session";

function metaFor(username: string): TenantMeta {
  return {
    tenant: { id: "t", name: "Demo Tenant", cell: "eu" },
    user: { username, display_name: username, roles: ["physician"] },
    facilities: [],
  };
}

function jsonResponse(body: unknown, status = 200): Response {
  return new Response(JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

function Probe() {
  const { authenticated, meta, signIn, signOut } = useSession();
  return (
    <div>
      <span data-testid="user">{meta?.user.username ?? "none"}</span>
      <span data-testid="auth">{String(authenticated)}</span>
      <button onClick={() => void signIn("dev-second.user")}>in</button>
      <button onClick={() => void signOut()}>out</button>
    </div>
  );
}

describe("session metadata generations", () => {
  beforeEach(() => {
    vi.unstubAllGlobals();
  });

  it("ignores a stale metadata response after sign-out and sign-in as another user", async () => {
    let resolveFirstMeta: ((r: Response) => void) | null = null;
    let metaCalls = 0;
    vi.stubGlobal(
      "fetch",
      vi.fn((input: RequestInfo | URL, init?: RequestInit) => {
        const url = String(input);
        if (url === "/api/session" && init?.method === "POST")
          return Promise.resolve(jsonResponse({ ok: true }));
        if (url === "/api/session" && init?.method === "DELETE")
          return Promise.resolve(jsonResponse({ ok: true }));
        if (url === "/api/session")
          return Promise.resolve(jsonResponse({ authenticated: true }));
        if (url === "/api/v1/meta/tenant") {
          metaCalls += 1;
          if (metaCalls === 1) {
            // First session's request hangs until after the user switches.
            return new Promise<Response>((resolve) => {
              resolveFirstMeta = resolve;
            });
          }
          return Promise.resolve(jsonResponse(metaFor("second.user")));
        }
        return Promise.resolve(jsonResponse({}));
      }),
    );

    render(
      <SessionProvider>
        <Probe />
      </SessionProvider>,
    );
    await waitFor(() => expect(metaCalls).toBe(1));

    // Sign out (first user's metadata request still in flight), then sign
    // in as another user whose metadata resolves immediately.
    screen.getByText("out").click();
    await waitFor(() =>
      expect(screen.getByTestId("auth")).toHaveTextContent("false"),
    );
    screen.getByText("in").click();
    await waitFor(() =>
      expect(screen.getByTestId("user")).toHaveTextContent("second.user"),
    );

    // The first user's stale response finally arrives; it must be ignored.
    await act(async () => {
      resolveFirstMeta?.(jsonResponse(metaFor("first.user")));
      await Promise.resolve();
    });
    expect(screen.getByTestId("user")).toHaveTextContent("second.user");
  });
});
