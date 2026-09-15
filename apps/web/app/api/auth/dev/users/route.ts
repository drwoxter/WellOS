import { NextResponse } from "next/server";
import { API_URL, apiUnavailable } from "@/lib/bff";

export const dynamic = "force-dynamic";

/**
 * Synthetic sign-in discovery. The API serves this route only from a
 * `dev-fixtures` build with development authentication enabled in
 * `WELLOS_ENV=development|test`; everywhere else it answers 404 and the
 * sign-in page offers the identity-provider flow only. The browser bundle
 * carries no usernames and never decides on its own that dev auth exists.
 */
export async function GET() {
  try {
    const res = await fetch(`${API_URL}/api/v1/auth/dev/users`, {
      cache: "no-store",
    });
    if (!res.ok) {
      return NextResponse.json(
        { error: { code: "not_found", message: "not available" } },
        { status: 404 },
      );
    }
    return NextResponse.json(await res.json());
  } catch {
    return apiUnavailable();
  }
}
