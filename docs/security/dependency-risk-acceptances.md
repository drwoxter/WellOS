# Dependency risk acceptances

Temporary, explicitly scoped acceptances of unpatched dependency advisories.
Each entry is enforced mechanically by `apps/web/scripts/check-npm-audit.mjs`
(the complete-dependency gate in CI); the production-dependency gate
(`npm audit --omit=dev --audit-level=high`) accepts nothing. An acceptance is
not a fix: it records that a known vulnerability is present in development
tooling, why it cannot reach production, who owns it and when it must be
reviewed again. Entries expire; an expired entry fails CI.

| Field | Value |
|---|---|
| Advisory | [GHSA-vfj7-8cjw-p6xm](https://github.com/advisories/GHSA-vfj7-8cjw-p6xm) · CVE-2026-93687 — `braces` stack-exhaustion denial of service through deeply nested brace patterns (CWE-674) |
| Package / version | `braces@3.0.3` (vulnerable range `<= 3.0.3`; no patched release published at acceptance time) |
| Exact dependency path | `eslint-config-next@16.3.6 → @next/eslint-plugin-next@16.3.6 → fast-glob@3.3.1 → micromatch@4.0.8 → braces@3.0.3` |
| Scope | `apps/web` development tooling only (`devDependencies`). `braces` and every link of the path are `dev: true` in `apps/web/package-lock.json`; the validator re-checks this on every run. |
| Production reachability | None. `braces` is not in the production dependency graph (`npm audit --omit=dev` reports 0 vulnerabilities) and is not part of the Next.js runtime bundle. It is executed only by ESLint's file-glob expansion over repository-controlled paths during `npm run lint`, never on user input, never in the API or the browser. |
| Rationale | The only `npm audit fix` is a breaking downgrade to `eslint-config-next@14`, which is incompatible with Next.js 16 / ESLint 9. Forks, unpublished commits, local patched packages and a Next.js downgrade were ruled out. The complete audit stays enforced so that any *other* critical/high finding, or any change to this package, version, advisory or path, still fails CI. |
| Upstream tracking | https://github.com/micromatch/braces/issues/70 · advisory references: https://nvd.nist.gov/vuln/detail/CVE-2026-93687 |
| Owner | WellOS repository maintainers (`drwoxter`) |
| Accepted on | 2026-10-04 |
| Expires on | **2026-11-01** — the gate fails on and after this date until the entry is renewed with a fresh review or removed because a patched `braces` is installed |
| Automatic retirement | As soon as `npm audit` stops reporting GHSA-vfj7-8cjw-p6xm (patched `braces` or a lint toolchain update), the validator no longer applies the exception and prints a reminder to delete this entry. |

## How the gates run

```bash
cd apps/web
npm audit --omit=dev --audit-level=high      # production gate: must be clean
node scripts/check-npm-audit.mjs             # complete gate: fails on anything but the entry above
```

Both run in the `security` job of `.github/workflows/ci.yml` without
`continue-on-error`; the secret scan (gitleaks) runs in its own job so it is
executed and blocking even when a dependency gate fails.
