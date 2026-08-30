import { NextRequest, NextResponse } from "next/server";
import {
  API_URL,
  CSRF_COOKIE,
  SESSION_COOKIE,
  apiUnavailable,
  csrfCookieOptions,
  sessionCookieOptions,
} from "@/lib/bff";

export const dynamic = "force-dynamic";

/**
 * Provider redirect target. The code/state are exchanged server-side by the
 * API (single-use transaction, PKCE, nonce binding, full token validation);
 * on success only the opaque `wss_` session and `wsc_` CSRF cookies are set
 * and the browser is redirected to a fixed internal path. Provider tokens
 * never reach the browser, and failures redirect without echoing details.
 */
export async function GET(req: NextRequest) {
  const params = req.nextUrl.searchParams;
  let res: Response;
  try {
    res = await fetch(`${API_URL}/api/v1/auth/oidc/callback`, {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        code: params.get("code"),
        state: params.get("state"),
        error: params.get("error"),
      }),
      cache: "no-store",
    });
  } catch {
    return apiUnavailable();
  }
  if (!res.ok) {
    // Fixed internal failure destination; no provider or error details in
    // the URL beyond a generic flag.
    return NextResponse.redirect(new URL("/?login=failed", req.nextUrl), 302);
  }
  const session = (await res.json()) as {
    session_token: string;
    csrf_token: string;
    expires_at?: string;
  };
  const response = NextResponse.redirect(
    new URL("/worklist", req.nextUrl),
    302,
  );
  response.cookies.set(
    SESSION_COOKIE,
    session.session_token,
    sessionCookieOptions(session.expires_at),
  );
  response.cookies.set(
    CSRF_COOKIE,
    session.csrf_token,
    csrfCookieOptions(session.expires_at),
  );
  return response;
}
