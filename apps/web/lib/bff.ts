import { NextRequest, NextResponse } from "next/server";

/** Name of the HttpOnly session cookie holding the backend bearer token. */
export const SESSION_COOKIE = "wellos_session";

export const API_URL = process.env.WELLOS_API_URL ?? "http://localhost:8080";

/** Headers the browser may set that are forwarded to the API. */
const FORWARDED_HEADERS = ["x-purpose-of-use", "x-break-glass-reason"];

export function sessionCookieOptions() {
  return {
    httpOnly: true,
    secure: process.env.NODE_ENV === "production",
    sameSite: "strict" as const,
    path: "/",
    maxAge: 60 * 60 * 8,
  };
}

/**
 * Forward a browser request to the backend API, attaching the bearer token
 * from the HttpOnly session cookie. Tokens never reach browser JavaScript.
 */
export async function proxyToApi(
  req: NextRequest,
  path: string,
): Promise<NextResponse> {
  const token = req.cookies.get(SESSION_COOKIE)?.value;
  if (!token) {
    return NextResponse.json(
      { error: { code: "unauthenticated", message: "sign in required" } },
      { status: 401 },
    );
  }
  const headers: Record<string, string> = {
    Authorization: `Bearer ${token}`,
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
