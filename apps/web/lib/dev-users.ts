import type { Lang, TKey } from "./i18n";
import { t } from "./i18n";

/** A synthetic development identity served by the API's fixture endpoint. */
export type DevUser = {
  username: string;
  display_name: string;
  tenant_name: string;
  roles: string[];
};

export type DevUsersResponse = {
  environment: string;
  synthetic: boolean;
  users: DevUser[];
};

/** Sign-in methods the server confirms it offers. */
export type AuthProviders = {
  environment: string;
  oidc: boolean;
  development: boolean;
};

export async function fetchAuthProviders(): Promise<AuthProviders> {
  const res = await fetch("/api/auth/providers", { cache: "no-store" });
  if (!res.ok) throw new Error(`sign-in methods unavailable (${res.status})`);
  const body = (await res.json()) as AuthProviders;
  return {
    environment: String(body.environment ?? ""),
    oidc: body.oidc === true,
    development: body.development === true,
  };
}

/**
 * Discover synthetic sign-in identities. Resolves to `null` when the server
 * does not offer development authentication (deployed environments, or a
 * build without `dev-fixtures`), in which case only the identity-provider
 * flow is shown. Network failures propagate so the page can show an honest
 * error instead of silently hiding the development option.
 */
export async function fetchDevUsers(): Promise<DevUsersResponse | null> {
  const res = await fetch("/api/auth/dev/users", { cache: "no-store" });
  if (res.status === 404) return null;
  if (!res.ok) throw new Error(`dev users unavailable (${res.status})`);
  const body = (await res.json()) as DevUsersResponse;
  if (body.synthetic !== true || !Array.isArray(body.users)) return null;
  return body;
}

/** Role-appropriate landing route after sign-in. */
export function homeForRoles(roles: string[]): string {
  if (roles.includes("registration_staff")) return "/access";
  return "/dashboard";
}

const ROLE_LABELS: Record<string, TKey> = {
  physician: "roleClinician",
  nurse: "roleNurse",
  registration_staff: "roleRegistration",
  privacy_officer: "rolePrivacy",
  laboratory_professional: "roleLaboratory",
  pharmacist: "rolePharmacist",
  clinical_administrator: "roleClinicalAdmin",
  security_auditor: "roleSecurityAuditor",
  research_user: "roleResearch",
  break_glass_authorized: "roleBreakGlass",
};

/** Human-readable role list; unknown roles fall back to their identifier. */
export function roleLabels(lang: Lang, roles: string[]): string {
  return roles
    .map((r) => (r in ROLE_LABELS ? t(lang, ROLE_LABELS[r]) : r))
    .join(" · ");
}
