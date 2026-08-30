import { NextRequest, NextResponse } from "next/server";

/**
 * Name of the HttpOnly cookie holding the opaque `wss_` session identifier.
 * The identifier is random, server-side hashed, and short-lived; the
 * underlying access credential is never stored in the browser.
 */
export const SESSION_COOKIE = "wellos_session";

/**
 * Name of the JavaScript-readable CSRF cookie. State-changing requests must
 * echo its value in the `x-csrf-token` header (double-submit); the API
 * verifies it against the session's hashed CSRF secret. SameSite=Strict
 * cookies are the first layer; this header check is the enforced one.
 */
export const CSRF_COOKIE = "wellos_csrf";

export const API_URL = process.env.WELLOS_API_URL ?? "http://localhost:8080";

/** Headers the browser may set that are forwarded to the API. */
const FORWARDED_HEADERS = [
  "x-purpose-of-use",
  "x-break-glass-reason",
  "x-csrf-token",
];

export function sessionCookieOptions() {
  return {
    httpOnly: true,
    secure: process.env.NODE_ENV === "production",
    sameSite: "strict" as const,
    path: "/",
    maxAge: 60 * 60 * 8,
  };
}

export function csrfCookieOptions() {
  return { ...sessionCookieOptions(), httpOnly: false };
}

/**
 * Forward a browser request to the backend API, attaching the opaque session
 * identifier from the HttpOnly cookie as the bearer credential. The API
 * validates the session (hash, expiry, inactivity, revocation) and enforces
 * the CSRF header on state-changing methods.
 */
export async function proxyToApi(
  req: NextRequest,
  path: string,
): Promise<NextResponse> {
  const session = req.cookies.get(SESSION_COOKIE)?.value;
  if (!session) {
    return NextResponse.json(
      { error: { code: "unauthenticated", message: "sign in required" } },
      { status: 401 },
    );
  }
  const headers: Record<string, string> = {
    Authorization: `Bearer ${session}`,
    "Content-Type": "application/json",
  };
  for (const name of FORWARDED_HEADERS) {
    const value = req.headers.get(name);
    if (value) headers[name] = value;
  }
  const res = await fetch(`${API_URL}${path}${req.nextUrl.search}`, {
    method: req.method,
    headers,
    body:
      req.method === "GET" || req.method === "HEAD"
        ? undefined
        : await req.text(),
    cache: "no-store",
  });
  const body = await res.text();
  return new NextResponse(body, {
    status: res.status,
    headers: { "Content-Type": "application/json" },
  });
}
