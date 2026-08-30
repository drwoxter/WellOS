import { NextResponse } from "next/server";
import { API_URL, apiUnavailable } from "@/lib/bff";

export const dynamic = "force-dynamic";

/**
 * Begin a browser OIDC login. The API mints the server-side login
 * transaction (state/nonce/PKCE verifier stay server-side) and returns the
 * provider authorization URL; the browser is redirected there. No return
 * path is accepted from the caller — the post-login destination is fixed,
 * so there is no open-redirect surface.
 */
export async function GET() {
  let res: Response;
  try {
    res = await fetch(`${API_URL}/api/v1/auth/oidc/login`, {
      method: "POST",
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
  const { authorize_url } = (await res.json()) as { authorize_url: string };
  return NextResponse.redirect(authorize_url, 302);
}
