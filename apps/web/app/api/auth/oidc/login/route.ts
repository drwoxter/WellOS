import { NextRequest, NextResponse } from "next/server";
import {
  API_URL,
  LOGIN_BINDING_COOKIE,
  apiUnavailable,
  clientAddressHeaders,
  loginBindingCookieOptions,
} from "@/lib/bff";

export const dynamic = "force-dynamic";

/**
 * Begin a browser OIDC login. The API mints the server-side login
 * transaction (state/nonce/PKCE verifier stay server-side) and returns the
 * provider authorization URL; the browser is redirected there. A one-time
 * binding secret is held in an HttpOnly cookie so only the initiating
 * browser can complete the callback. No return path is accepted from the
 * caller — the post-login destination is fixed, so there is no
 * open-redirect surface.
 */
export async function GET(req: NextRequest) {
  let res: Response;
  try {
    res = await fetch(`${API_URL}/api/v1/auth/oidc/login`, {
      method: "POST",
      headers: clientAddressHeaders(req),
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
  const { authorize_url, binding_token, binding_max_age_secs } =
    (await res.json()) as {
      authorize_url: string;
      binding_token: string;
      binding_max_age_secs: number;
    };
  const response = NextResponse.redirect(authorize_url, 302);
  response.cookies.set(
    LOGIN_BINDING_COOKIE,
    binding_token,
    loginBindingCookieOptions(binding_max_age_secs),
  );
  return response;
}
