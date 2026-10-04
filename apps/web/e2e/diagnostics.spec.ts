import { expect, test, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { signInAs } from "./helpers";

/**
 * dMind Clinical Orders & Diagnostics journeys on the synthetic fixtures:
 * clinician composes orders from a consultation through the deterministic
 * safety check, reviews and releases a critical report with an explicit
 * human decision, an administrator extends the runtime catalog, and a patient
 * reads released results in both languages and at 390 px. Mutates the seeded
 * data; run `make seed` from the repository root to restore the demo states.
 */

async function expectNoSeriousViolations(page: Page) {
  const results = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21aa", "wcag22aa"])
    .analyze();
  const serious = results.violations.filter(
    (v) => v.impact === "serious" || v.impact === "critical",
  );
  expect(
    serious.map((v) => `${v.id}: ${v.nodes.map((n) => n.target).join(", ")}`),
  ).toEqual([]);
}

async function signInAsPatient(page: Page, username: string): Promise<void> {
  await page.goto("/");
  await page
    .getByRole("button", { name: new RegExp(`Sign in as ${username}`) })
    .click();
  await expect(page).toHaveURL(/\/my\/appointments/);
}

test.describe.configure({ mode: "serial" });

test("clinician composes a diagnostic order through the deterministic safety check", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await signInAs(page, "dr.garcia");
  await page.goto("/patients");
  await page.getByLabel("Search patients").fill("SYN-0001");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page
    .locator(".result-card")
    .first()
    .getByRole("link", { name: "Open chart" })
    .click();
  await page.getByRole("link", { name: /Resume consultation/ }).click();

  const composer = page.getByTestId("order-composer");
  await expect(composer).toBeVisible();
  // Catalog contents come from the server; nothing is typed free-form.
  await composer.getByTestId("dx-search").fill("potas");
  const results = composer.getByRole("list", { name: "Catalog results" });
  const potassium = results
    .getByRole("listitem")
    .filter({ hasText: /^Potassium \[Moles\/volume\] in Serum/ })
    .first();
  await expect(potassium).toBeVisible();
  await potassium.getByRole("button", { name: "Add" }).click();
  await expect(composer.getByTestId("dx-items")).toContainText(
    "Potassium [Moles/volume] in Serum",
  );

  // Confirmation does not exist until the safety check has run.
  const confirm = composer.getByTestId("dx-confirm");
  await expect(confirm).toHaveCount(0);
  await composer
    .getByTestId("dx-indication")
    .fill("Fatigue with muscle cramps (synthetic).");
  await composer.getByTestId("dx-preflight").click();
  const safety = composer.getByTestId("dx-safety");
  await expect(safety).toBeVisible();
  await expect(safety.getByText("diagnostic-safety.v1")).toBeVisible();
  // Every warning must be acknowledged explicitly before confirming.
  const warnings = safety.getByRole("checkbox");
  const n = await warnings.count();
  if (n > 0) {
    await expect(confirm).toBeDisabled();
    for (let i = 0; i < n; i += 1) await warnings.nth(i).check();
  }
  await expect(confirm).toBeEnabled();
  await confirm.click();
  const placed = composer.getByTestId("dx-placed");
  await expect(placed).toBeVisible();
  await expect(placed).toContainText("Potassium [Moles/volume] in Serum");
  await expect(placed).toContainText(/placed/i);
});

test("clinician reviews and releases a critical report with an explicit decision", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await signInAs(page, "dr.garcia");
  await page.goto("/diagnostics");
  await expect(page.getByTestId("diagnostics-page")).toBeVisible();

  // Keyboard-operable tabs: ArrowRight moves to the review worklist.
  const ordersTab = page.getByRole("tab", { name: "Orders" });
  await ordersTab.focus();
  await page.keyboard.press("ArrowRight");
  await expect(
    page.getByRole("tab", { name: "Reports to review" }),
  ).toHaveAttribute("aria-selected", "true");

  const ecg = page
    .getByTestId("dx-review-row")
    .filter({ hasText: "12-lead ECG" })
    .first();
  await expect(ecg).toHaveAttribute("data-criticality", "critical");
  await ecg.getByRole("link", { name: /12-lead ECG/ }).click();
  await expect(page).toHaveURL(/\/diagnostics\/reports\//);
  const detail = page.getByTestId("dx-report-detail");
  await expect(detail).toHaveAttribute("data-criticality", "critical");
  await expect(page.getByTestId("dx-critical-banner")).toBeVisible();
  // Nothing is released automatically: release waits for the review.
  await expect(page.getByTestId("dx-release-needs-review")).toBeVisible();
  await expectNoSeriousViolations(page);

  await page
    .getByTestId("dx-assessment")
    .fill(
      "Critical conduction abnormality confirmed; patient contacted (synthetic).",
    );
  await page.getByTestId("dx-disposition").selectOption("immediate_contact");
  await page
    .getByTestId("dx-follow-up")
    .fill("Same-day cardiology assessment (synthetic).");
  await page.getByTestId("dx-submit-review").click();
  await expect(page.getByTestId("dx-reviews-list")).toContainText(
    "Immediate contact",
  );

  // Release needs a bilingual explanation for an abnormal result and a
  // second explicit confirmation.
  const release = page.getByTestId("dx-release");
  await expect(release).toBeVisible();
  const submit = page.getByTestId("dx-release-submit");
  await expect(submit).toBeDisabled();
  await page
    .getByTestId("dx-explanation-en")
    .fill(
      "Your heart tracing showed a finding that needs urgent follow-up; we have contacted you (synthetic).",
    );
  await page
    .getByTestId("dx-explanation-es")
    .fill(
      "Su electrocardiograma mostró un hallazgo que requiere seguimiento urgente; ya le hemos contactado (sintético).",
    );
  await expect(submit).toBeEnabled();
  await submit.click();
  await page.getByRole("button", { name: "Confirm", exact: true }).click();
  await expect(page.getByTestId("dx-release-list")).toContainText(
    "Release to patient",
  );
  await expect(page.getByTestId("dx-release-submit")).toHaveCount(0);
});

test("administrator adds a runtime orderable that clinicians can find", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await signInAs(page, "admin.silva");
  await page.goto("/diagnostics/catalog");
  await expect(page.getByTestId("dx-catalog")).toBeVisible();
  await page.getByTestId("dx-cat-add").click();
  const editor = page.getByTestId("dx-catalog-editor");
  await editor.getByTestId("dx-cat-code").fill("e2e_vitamin_d");
  await editor.getByTestId("dx-cat-name-en").fill("Vitamin D (E2E)");
  await editor.getByTestId("dx-cat-name-es").fill("Vitamina D (E2E)");
  // Scheduled fulfilment needs a scheduling service: save stays disabled.
  await expect(editor.getByTestId("dx-cat-save")).toBeDisabled();
  await editor
    .getByTestId("dx-cat-service")
    .selectOption("cardiology_diagnostics");
  await editor.getByTestId("dx-cat-save").click();
  await expect(editor).toBeHidden();
  await page.getByTestId("dx-cat-search").fill("e2e_vitamin");
  await expect(
    page.getByTestId("dx-cat-row").filter({ hasText: "Vitamin D (E2E)" }),
  ).toBeVisible();

  // Version history records the runtime addition.
  await page
    .getByTestId("dx-cat-row")
    .filter({ hasText: "Vitamin D (E2E)" })
    .getByTestId("dx-cat-history-toggle")
    .click();
  await expect(page.getByTestId("dx-cat-history")).toContainText("v1");
});

test("patient reads released results in Spanish", async ({ page }) => {
  await signInAsPatient(page, "rep.alba");
  await page.getByRole("link", { name: "My tests" }).first().click();
  await expect(page).toHaveURL(/\/my\/diagnostics/);
  const released = page.getByTestId("my-dx-released");
  await expect(released).toBeVisible();
  await expect(released).toContainText(/Lipid/);
  await expect(page.getByTestId("my-dx-disclaimer")).toBeVisible();
  // No staff controls or internal reasoning leak into the patient view.
  await expect(page.getByTestId("dx-release")).toHaveCount(0);
  await expect(page.getByText(/criticality_rules|input_hash/)).toHaveCount(0);

  await page.getByLabel("Language").first().selectOption("es");
  await expect(
    page.getByRole("heading", { name: "Resultados liberados" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Pruebas pendientes" }),
  ).toBeVisible();
});

test("diagnostics pages have no serious accessibility violations", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await page.goto("/diagnostics");
  await expect(page.getByTestId("diagnostics-page")).toBeVisible();
  await expectNoSeriousViolations(page);
  await page.getByRole("tab", { name: "Reports to review" }).click();
  await expectNoSeriousViolations(page);

  await page.context().clearCookies();
  await signInAsPatient(page, "rep.alba");
  await page.goto("/my/diagnostics");
  await expect(page.getByTestId("my-dx-released")).toBeVisible();
  await expectNoSeriousViolations(page);
});

test("patient results are usable at 390px @mobile", async ({ page }) => {
  await signInAsPatient(page, "rep.alba");
  await page.goto("/my/diagnostics");
  await expect(page.getByTestId("my-diagnostics")).toBeVisible();
  await expect(page.locator(".mobile-nav")).toBeVisible();
  await expect(page.getByTestId("my-dx-released")).toBeVisible();
  const width = await page.evaluate(() => document.documentElement.scrollWidth);
  expect(width).toBeLessThanOrEqual(390);
});

test("diagnostics worklist is usable at 390px @mobile", async ({ page }) => {
  await signInAs(page, "dr.garcia");
  await page.goto("/diagnostics");
  await expect(page.getByTestId("diagnostics-page")).toBeVisible();
  await expect(page.getByTestId("dx-order-rows")).toBeVisible();
  const width = await page.evaluate(() => document.documentElement.scrollWidth);
  expect(width).toBeLessThanOrEqual(390);
});
