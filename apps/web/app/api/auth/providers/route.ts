import { NextResponse } from "next/server";
import { API_URL, apiUnavailable } from "@/lib/bff";

export const dynamic = "force-dynamic";

/**
 * Sign-in methods the API actually offers (identity provider and, in local
 * fixture builds only, synthetic development identities). The sign-in page
 * renders a control only for a method the server confirms.
 */
export async function GET() {
  try {
    const res = await fetch(`${API_URL}/api/v1/auth/providers`, {
      cache: "no-store",
    });
    if (!res.ok) return apiUnavailable();
    return NextResponse.json(await res.json());
  } catch {
    return apiUnavailable();
  }
}
