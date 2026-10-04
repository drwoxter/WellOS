#!/usr/bin/env node
// Complete dependency gate for apps/web.
//
// Runs `npm audit --json` over production AND development dependencies and
// fails on every critical or high finding, except for ONE exact, documented,
// time-limited development-tooling exception (see
// docs/security/dependency-risk-acceptances.md). There is no allowlist file
// and no severity-wide exemption: anything that is not byte-for-byte the
// accepted chain fails the gate.
//
// Usage: node scripts/check-npm-audit.mjs [--audit-json <file>] [--today YYYY-MM-DD]
// (the flags exist for the script's own tests; CI runs it without flags).

import { spawnSync } from "node:child_process";
import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const webRoot = join(here, "..");

// The single accepted, unpatched, development-only risk. Every field must
// match exactly; the acceptance expires on `expiresOn` and is dropped
// automatically once the advisory is no longer reported.
const ACCEPTED = Object.freeze({
  advisory: "GHSA-vfj7-8cjw-p6xm",
  cve: "CVE-2026-93687",
  pkg: "braces",
  version: "3.0.3",
  // Exact dev-only chain, root first.
  chain: Object.freeze([
    "eslint-config-next",
    "@next/eslint-plugin-next",
    "fast-glob",
    "micromatch",
    "braces",
  ]),
  expiresOn: "2026-11-01",
  document: "docs/security/dependency-risk-acceptances.md",
});

const BLOCKING = new Set(["critical", "high"]);

function parseArgs(argv) {
  const out = { auditJson: null, today: null };
  for (let i = 0; i < argv.length; i++) {
    if (argv[i] === "--audit-json") out.auditJson = argv[++i];
    else if (argv[i] === "--today") out.today = argv[++i];
    else throw new Error(`unknown argument ${argv[i]}`);
  }
  return out;
}

function runAudit() {
  const r = spawnSync("npm", ["audit", "--json"], {
    cwd: webRoot,
    encoding: "utf8",
    maxBuffer: 64 * 1024 * 1024,
  });
  if (r.error) throw r.error;
  // npm exits non-zero when it finds vulnerabilities; the JSON is still
  // complete. A missing or unparsable report is a hard failure.
  if (!r.stdout) {
    throw new Error(`npm audit produced no output (stderr: ${r.stderr})`);
  }
  return JSON.parse(r.stdout);
}

function todayIso(override) {
  if (override) return override;
  return new Date().toISOString().slice(0, 10);
}

function advisoryIdOf(via) {
  const m = /GHSA-[0-9a-z]{4}-[0-9a-z]{4}-[0-9a-z]{4}/i.exec(via.url ?? "");
  return m ? m[0] : null;
}

function lockEntry(lock, name) {
  return lock.packages?.[`node_modules/${name}`] ?? null;
}

function main() {
  const args = parseArgs(process.argv.slice(2));
  const today = todayIso(args.today);
  const report = args.auditJson
    ? JSON.parse(readFileSync(args.auditJson, "utf8"))
    : runAudit();
  const lock = JSON.parse(
    readFileSync(join(webRoot, "package-lock.json"), "utf8"),
  );
  const vulns = report.vulnerabilities ?? {};
  const failures = [];
  const blocking = Object.values(vulns).filter((v) => BLOCKING.has(v.severity));
  const names = new Set(blocking.map((v) => v.name));

  // Everything that is blocking must be explained by the accepted chain.
  const chainSet = new Set(ACCEPTED.chain);
  const exceptionInPlay = names.has(ACCEPTED.pkg);
  if (exceptionInPlay) {
    if (today >= ACCEPTED.expiresOn) {
      failures.push(
        `the temporary acceptance of ${ACCEPTED.advisory} (${ACCEPTED.pkg}@${ACCEPTED.version}) ` +
          `expired on ${ACCEPTED.expiresOn}; a new review is required (${ACCEPTED.document})`,
      );
    }
    const root = vulns[ACCEPTED.pkg];
    if (root.severity !== "high") {
      failures.push(
        `${ACCEPTED.pkg} is reported as ${root.severity}; only a high finding is accepted`,
      );
    }
    const advisories = (root.via ?? []).filter((v) => typeof v === "object");
    const ids = advisories.map(advisoryIdOf);
    if (ids.length !== 1 || ids[0] !== ACCEPTED.advisory) {
      failures.push(
        `${ACCEPTED.pkg} advisories are ${JSON.stringify(ids)}; only ${ACCEPTED.advisory} is accepted`,
      );
    }
    const entry = lockEntry(lock, ACCEPTED.pkg);
    if (!entry) {
      failures.push(`${ACCEPTED.pkg} is not in package-lock.json`);
    } else {
      if (entry.version !== ACCEPTED.version) {
        failures.push(
          `${ACCEPTED.pkg} is installed at ${entry.version}; the acceptance covers ${ACCEPTED.version} only`,
        );
      }
      if (entry.dev !== true) {
        failures.push(
          `${ACCEPTED.pkg} is not a dev-only dependency (lockfile dev flag: ${entry.dev})`,
        );
      }
    }
    // The dependency path must be exactly the accepted chain: each link's
    // only vulnerability source is the next link, and each is dev-only.
    for (let i = 0; i < ACCEPTED.chain.length - 1; i++) {
      const parent = ACCEPTED.chain[i];
      const child = ACCEPTED.chain[i + 1];
      const v = vulns[parent];
      if (!v) {
        failures.push(
          `${parent} is not reported; the accepted chain no longer matches`,
        );
        continue;
      }
      const via = (v.via ?? []).map((x) =>
        typeof x === "string" ? x : `advisory:${advisoryIdOf(x)}`,
      );
      if (via.length !== 1 || via[0] !== child) {
        failures.push(
          `${parent} is vulnerable via ${JSON.stringify(via)}; accepted path requires only ${child}`,
        );
      }
      const e = lockEntry(lock, parent);
      if (!e || e.dev !== true) {
        failures.push(
          `${parent} is not a dev-only dependency in package-lock.json`,
        );
      }
    }
    for (const link of ACCEPTED.chain) {
      const v = vulns[link];
      if (v && v.severity !== "high") {
        failures.push(
          `${link} is reported as ${v.severity}; the acceptance covers a high finding only`,
        );
      }
    }
    const rootEffects = root.effects ?? [];
    if (
      rootEffects.length !== 1 ||
      rootEffects[0] !== ACCEPTED.chain[ACCEPTED.chain.length - 2]
    ) {
      failures.push(
        `${ACCEPTED.pkg} affects ${JSON.stringify(rootEffects)}; only ${ACCEPTED.chain[3]} is accepted`,
      );
    }
  }
  for (const v of blocking) {
    if (!exceptionInPlay || !chainSet.has(v.name)) {
      const ids = (v.via ?? [])
        .filter((x) => typeof x === "object")
        .map(advisoryIdOf);
      failures.push(
        `${v.severity}: ${v.name} ${v.range} ${ids.length ? ids.join(",") : "(transitive)"}`,
      );
    }
  }

  const meta = report.metadata?.vulnerabilities ?? {};
  console.log(
    `npm audit (all dependencies): critical=${meta.critical ?? 0} high=${meta.high ?? 0} ` +
      `moderate=${meta.moderate ?? 0} low=${meta.low ?? 0}`,
  );
  if (failures.length) {
    console.error("\nDependency gate FAILED:");
    for (const f of failures) console.error(`  - ${f}`);
    process.exit(1);
  }
  if (exceptionInPlay) {
    console.log(
      [
        "",
        "ACCEPTED, UNPATCHED DEVELOPMENT-TOOLING RISK (this is NOT a fixed vulnerability):",
        `  ${ACCEPTED.advisory} / ${ACCEPTED.cve} in ${ACCEPTED.pkg}@${ACCEPTED.version}`,
        `  reached only through the dev-only lint chain ${ACCEPTED.chain.join(" -> ")}`,
        "  not present in production dependencies (npm audit --omit=dev is clean, enforced separately)",
        `  acceptance expires ${ACCEPTED.expiresOn}; see ${ACCEPTED.document}`,
        "",
      ].join("\n"),
    );
  } else {
    console.log(
      `No blocking findings. The ${ACCEPTED.advisory} acceptance is not in use; ` +
        `retire it in ${ACCEPTED.document} if the advisory has been resolved upstream.`,
    );
  }
}

main();
