import { expect, test, type Page, type TestInfo } from "@playwright/test";
import { pickCombo, signInAs, signInAsRegistration, signOut } from "./helpers";

test.use({
  permissions: ["microphone"],
  launchOptions: {
    args: [
      "--use-fake-device-for-media-stream",
      "--use-fake-ui-for-media-stream",
    ],
  },
});

/**
 * Visual regression captures for the Experience Reset.
 *
 * Fixtures are generated relative to "now" (appointments, waiting times,
 * result timestamps), so pixel-exact baselines would drift daily. Each state
 * is instead captured as a test attachment under
 * `test-results/visual/<project>/` and verified structurally: the expected
 * landmarks are present and the page never scrolls horizontally.
 */

async function signInAsPatient(page: Page): Promise<void> {
  await page.goto("/");
  await page.getByRole("button", { name: /Sign in as rep\.alba/ }).click();
  await expect(page).toHaveURL(/\/my$/);
}

async function capture(page: Page, name: string, testInfo: TestInfo) {
  await page.waitForTimeout(600);
  const path = testInfo.outputPath(`${name}.png`);
  await page.screenshot({ path, fullPage: false });
  await testInfo.attach(name, { path, contentType: "image/png" });
  const overflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth -
      document.documentElement.clientWidth,
  );
  expect(overflow, `${name} must not scroll horizontally`).toBeLessThanOrEqual(
    0,
  );
}

test("visual: patient home (desktop)", async ({ page }, testInfo) => {
  await signInAsPatient(page);
  await expect(page).toHaveURL(/\/my$/);
  await expect(page.getByRole("main")).toBeVisible();
  await capture(page, "01-patient-home-desktop", testInfo);
});

test("visual: patient home (390px) @mobile", async ({ page }, testInfo) => {
  await signInAsPatient(page);
  await expect(page).toHaveURL(/\/my$/);
  await expect(page.locator(".mobile-nav")).toBeVisible();
  await capture(page, "02-patient-home-mobile", testInfo);
});

test("visual: clinical cockpit", async ({ page }, testInfo) => {
  await signInAs(page, "dr.garcia");
  await expect(page).toHaveURL(/\/dashboard$/);
  await expect(page.getByRole("main")).toBeVisible();
  await capture(page, "03-clinical-cockpit", testInfo);
});

test("visual: active consultation recording", async ({ page }, testInfo) => {
  await signInAs(page, "dr.garcia");
  await page.goto("/patients");
  await page.getByLabel("Search patients").fill("Carlos");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page
    .locator("li", { hasText: "Carlos Demopatient" })
    .first()
    .getByRole("link", { name: "Open chart" })
    .click();
  await page
    .getByRole("button", { name: /^(Start|Resume) consultation$/ })
    .first()
    .click();
  await expect(page).toHaveURL(/\/encounters\//);
  const dock = page.getByRole("region", { name: "Consultation recording" });
  await expect(dock).toBeVisible();
  await dock.getByRole("button", { name: "Record consultation" }).click();
  await dock
    .getByRole("button", { name: "Patient consented — start recording" })
    .click();
  await expect(dock.getByRole("button", { name: "Pause" })).toBeVisible();
  await capture(page, "04-consultation-recording", testInfo);
  await dock.getByRole("button", { name: "Pause" }).click();
});

test("visual: patient scheduling recommendations", async ({
  page,
}, testInfo) => {
  await signInAsPatient(page);
  await page.goto("/my/appointments");
  await page.getByTestId("find-best-appointment").click();
  const panel = page.getByRole("tabpanel");
  await pickCombo(panel, "Service", "General medicine consultation");
  await panel.getByTestId("find-best-appointment").click();
  await expect(
    panel.getByRole("list", { name: "2. Best valid options" }),
  ).toBeVisible();
  await capture(page, "05-patient-scheduling-recommendations", testInfo);
});

test("visual: staff scheduling and capacity", async ({ page }, testInfo) => {
  await signInAsRegistration(page);
  await page.goto("/scheduling");
  await page.getByRole("tab", { name: "Agenda" }).click();
  await expect(page.getByRole("tabpanel").first()).toBeVisible();
  await capture(page, "06a-staff-scheduling-agenda", testInfo);
  await page.getByRole("tab", { name: "Capacity pressure" }).click();
  await expect(page.getByRole("tabpanel").first()).toBeVisible();
  await capture(page, "06b-staff-capacity", testInfo);
});

test("visual: access and triage workspace", async ({ page }, testInfo) => {
  await signInAsRegistration(page);
  await page.goto("/access");
  await expect(page.getByRole("main")).toBeVisible();
  await capture(page, "07a-access-board", testInfo);
  await signOut(page);
  await signInAs(page, "nurse.kim");
  await page.goto("/access");
  await page.getByRole("link", { name: "Open triage" }).first().click();
  await expect(page).toHaveURL(/\/visits\/.+\/triage/);
  await capture(page, "07b-triage-workspace", testInfo);
});

test("visual: patient 360 and results trend", async ({ page }, testInfo) => {
  await signInAs(page, "dr.garcia");
  await page.goto("/risk");
  await page
    .locator('.risk-worklist [data-testid="worklist-row"]')
    .first()
    .click();
  await page
    .locator(".risk-worklist .worklist-detail")
    .getByRole("link", { name: "Open Patient 360" })
    .click();
  await expect(page).toHaveURL(/\/360$/);
  await expect(
    page.getByRole("region", { name: /^Current risk/ }),
  ).toBeVisible();
  await capture(page, "08-patient-360", testInfo);
});
