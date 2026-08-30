import { NextRequest, NextResponse } from "next/server";
import { API_URL, SESSION_COOKIE, sessionCookieOptions } from "@/lib/bff";

export const dynamic = "force-dynamic";

/** Whether a server-side session exists. Never exposes the token itself. */
export async function GET(req: NextRequest) {
  const authenticated = Boolean(req.cookies.get(SESSION_COOKIE)?.value);
  return NextResponse.json({ authenticated });
}

/**
 * Establish a session: validate the submitted credential against the API,
 * then store it only in an HttpOnly cookie.
 */
export async function POST(req: NextRequest) {
  let token: unknown;
  try {
    const body: unknown = await req.json();
    token =
      typeof body === "object" && body !== null && "token" in body
        ? (body as { token: unknown }).token
        : undefined;
  } catch {
    token = undefined;
  }
  if (typeof token !== "string" || token.length === 0 || token.length > 4096) {
    return NextResponse.json(
      { error: { code: "validation_failed", message: "token is required" } },
      { status: 400 },
    );
  }
  const res = await fetch(`${API_URL}/api/v1/meta/tenant`, {
    headers: { Authorization: `Bearer ${token}` },
    cache: "no-store",
  });
  if (!res.ok) {
    const body = await res.text();
    return new NextResponse(body, {
      status: res.status,
      headers: { "Content-Type": "application/json" },
    });
  }
  const response = NextResponse.json({ authenticated: true });
  response.cookies.set(SESSION_COOKIE, token, sessionCookieOptions());
  return response;
}

/** Sign out: remove the session cookie. */
export async function DELETE() {
  const response = NextResponse.json({ authenticated: false });
  response.cookies.set(SESSION_COOKIE, "", {
    ...sessionCookieOptions(),
    maxAge: 0,
  });
  return response;
}
