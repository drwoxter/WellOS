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
 * Validate the server-side session (hash match, revocation, absolute expiry,
 * inactivity) rather than merely checking cookie presence.
 */
export async function GET(req: NextRequest) {
  const session = req.cookies.get(SESSION_COOKIE)?.value;
  if (!session) {
    return NextResponse.json({ authenticated: false });
  }
  try {
    const res = await fetch(`${API_URL}/api/v1/auth/session`, {
      headers: { Authorization: `Bearer ${session}` },
      cache: "no-store",
    });
    return NextResponse.json({ authenticated: res.ok });
  } catch {
    return apiUnavailable();
  }
}

/**
 * Sign in: exchange the submitted credential for a fresh opaque server-side
 * session. Only the random session identifier and the CSRF token are stored
 * as cookies; the credential itself is never persisted. A prior session is
 * revoked only after the new credential exchanges successfully (fixation
 * protection without logging out on a failed attempt).
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
  let res: Response;
  try {
    res = await fetch(`${API_URL}/api/v1/auth/session`, {
      method: "POST",
      headers: {
        Authorization: `Bearer ${token}`,
        "Content-Type": "application/json",
      },
      cache: "no-store",
    });
  } catch {
    return apiUnavailable();
  }
  if (!res.ok) {
    const body = await res.text();
    return new NextResponse(body, {
      status: res.status,
      headers: { "Content-Type": "application/json" },
    });
  }
  const session = (await res.json()) as {
    session_token: string;
    csrf_token: string;
    expires_at?: string;
  };
  const previous = req.cookies.get(SESSION_COOKIE)?.value;
  if (previous) {
    await revokeSession(previous, req.cookies.get(CSRF_COOKIE)?.value);
  }
  const response = NextResponse.json({ authenticated: true });
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

/** Sign out: revoke the server-side session and remove both cookies. */
export async function DELETE(req: NextRequest) {
  const session = req.cookies.get(SESSION_COOKIE)?.value;
  if (session) {
    await revokeSession(session, req.cookies.get(CSRF_COOKIE)?.value);
  }
  const response = NextResponse.json({ authenticated: false });
  response.cookies.set(SESSION_COOKIE, "", {
    ...sessionCookieOptions(),
    maxAge: 0,
  });
  response.cookies.set(CSRF_COOKIE, "", {
    ...csrfCookieOptions(),
    maxAge: 0,
  });
  return response;
}

async function revokeSession(session: string, csrf: string | undefined) {
  try {
    await fetch(`${API_URL}/api/v1/auth/session`, {
      method: "DELETE",
      headers: {
        Authorization: `Bearer ${session}`,
        ...(csrf ? { "x-csrf-token": csrf } : {}),
      },
      cache: "no-store",
    });
  } catch {
    // Revocation is best-effort here; expired/invalid sessions are already
    // unusable server-side.
  }
}
