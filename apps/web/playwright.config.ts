import { defineConfig, devices } from "@playwright/test";

/**
 * Browser-level tests for the clinical workspace.
 *
 * Requires local Postgres (`make up && make migrate && make seed` from the
 * repository root). The API server and the Next.js dev server are started
 * automatically; reseed (`make seed`) between runs that complete workflow
 * transitions, because closing a loop mutates the demo data.
 */
export default defineConfig({
  testDir: "./e2e",
  fullyParallel: false,
  workers: 1,
  retries: 0,
  reporter: [["list"]],
  use: {
    baseURL: "http://localhost:3000",
    trace: "retain-on-failure",
  },
  projects: [
    {
      name: "desktop",
      use: { ...devices["Desktop Chrome"] },
      grepInvert: /@mobile/,
    },
    {
      name: "mobile-390",
      use: {
        ...devices["Desktop Chrome"],
        viewport: { width: 390, height: 844 },
      },
      grep: /@mobile/,
    },
  ],
  webServer: [
    {
      command:
        "bash -c 'cd ../.. && DATABASE_URL=${DATABASE_URL:-postgres://wellos:wellos_dev@localhost:5432/wellos} WELLOS_ENV=development WELLOS_DEV_AUTH=true WELLOS_BIND_ADDR=127.0.0.1:8080 cargo run -p wellos-server'",
      url: "http://127.0.0.1:8080/health",
      reuseExistingServer: true,
      timeout: 180_000,
    },
    {
      command: "npm run dev",
      url: "http://localhost:3000",
      reuseExistingServer: true,
      timeout: 120_000,
    },
  ],
});
