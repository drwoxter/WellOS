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

/** Fallback cookie lifetime when the API's expiry is unavailable. */
const DEFAULT_SESSION_SECS = 60 * 60 * 8;

/**
 * Cookie lifetime derived from the server-side session's absolute expiry,
 * so the browser cookie and the API session expire together regardless of
 * the configured `WELLOS_SESSION_ABSOLUTE_SECS`.
 */
function cookieMaxAge(expiresAt?: string): number {
  if (!expiresAt) return DEFAULT_SESSION_SECS;
  const secs = Math.floor((Date.parse(expiresAt) - Date.now()) / 1000);
  return Number.isFinite(secs) && secs > 0 ? secs : DEFAULT_SESSION_SECS;
}

/**
 * Secure follows the deployment environment (`WELLOS_ENV`), not the Next.js
 * build mode: only explicit local development may send cookies over plain
 * HTTP. When `WELLOS_ENV` is unset, fall back to the build mode.
 */
function secureCookies(): boolean {
  const env = process.env.WELLOS_ENV;
  if (env) return env !== "development";
  return process.env.NODE_ENV === "production";
}

export function sessionCookieOptions(expiresAt?: string) {
  return {
    httpOnly: true,
    secure: secureCookies(),
    sameSite: "strict" as const,
    path: "/",
    maxAge: cookieMaxAge(expiresAt),
  };
}

export function csrfCookieOptions(expiresAt?: string) {
  return { ...sessionCookieOptions(expiresAt), httpOnly: false };
}

/** Bounded 503 returned when the backend API cannot be reached. */
export function apiUnavailable(): NextResponse {
  return NextResponse.json(
    {
      error: {
        code: "upstream_unavailable",
        message: "the service is temporarily unavailable",
      },
    },
    { status: 503 },
  );
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
  let res: Response;
  try {
    res = await fetch(`${API_URL}${path}${req.nextUrl.search}`, {
      method: req.method,
      headers,
      body:
        req.method === "GET" || req.method === "HEAD"
          ? undefined
          : await req.text(),
      cache: "no-store",
    });
  } catch {
    return apiUnavailable();
  }
  const body = await res.text();
  return new NextResponse(body, {
    status: res.status,
    headers: { "Content-Type": "application/json" },
  });
}
