import { expect, test, type Page } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { setLanguage } from "./helpers";

/** Dev sign-in through the BFF for roles without a sign-in card. */
async function signInViaBff(page: Page, username: string): Promise<void> {
  await page.goto("/");
  await page.evaluate(async (token) => {
    const res = await fetch("/api/session", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ token }),
    });
    if (!res.ok) throw new Error(`sign-in failed: ${res.status}`);
  }, `dev-${username}`);
}

async function horizontalOverflow(page: Page): Promise<number> {
  return page.evaluate(
    () =>
      document.documentElement.scrollWidth -
      document.documentElement.clientWidth,
  );
}

test.describe.configure({ mode: "serial" });

test("clinical administrator runs an offline evaluation and records a review", async ({
  page,
}) => {
  test.setTimeout(180_000);
  await signInViaBff(page, "admin.silva");
  await page.goto("/admin/dmind/evaluations");
  await expect(
    page.getByRole("heading", { name: "dMind evaluation" }),
  ).toBeVisible();
  await expect(page.getByTestId("eval-synthetic-banner")).toContainText(
    "Synthetic data only",
  );
  await expect(page.getByText("Suite dmind-core 1.0.0")).toBeVisible();
  await expect(
    page.getByText("Live provider evaluation is only available"),
  ).toBeVisible();

  const form = page.getByRole("form", { name: "New run" });
  await form.getByLabel("result_summary").check();
  await form.getByLabel("triage_proposal").check();
  await form.getByRole("button", { name: "Start run" }).click();

  await expect(page).toHaveURL(/\/admin\/dmind\/evaluations\/[0-9a-f-]{36}$/, {
    timeout: 60_000,
  });
  await expect(page.getByRole("heading", { name: /Run detail/ })).toBeVisible();
  const totals = page.getByTestId("eval-totals");
  await expect(totals).toContainText("Cases");
  await expect(totals).toContainText("Passed");
  await expect(
    page.getByText("No high-severity safety failures."),
  ).toBeVisible();
  const executions = page.getByTestId("eval-execution");
  await expect(executions.first()).toBeVisible();
  expect(await executions.count()).toBe(12);

  // Metrics distinguish verification classes; outputs are inspectable.
  const first = executions.first();
  await first.getByText(/^Metrics \(/).click();
  await expect(first.getByRole("table")).toContainText("Deterministic");
  await expect(first.getByRole("table")).toContainText("Human review required");
  await first.getByRole("button", { name: "Show outputs" }).click();
  await expect(
    first.getByRole("region", { name: "Deterministic baseline" }),
  ).toBeVisible();
  await expect(
    first.getByRole("region", { name: "Provider output" }),
  ).toBeVisible();

  // Append-only human review.
  const review = first.getByRole("form", { name: "Record review" });
  await review.getByLabel("Decision").selectOption("flag_quality");
  await review.getByLabel("Comment").fill("Synthetic reviewer note (e2e).");
  await review.getByRole("button", { name: "Save review" }).click();
  await expect(first.locator("ol").getByText("Flag quality")).toBeVisible();
  await expect(first.getByText("Synthetic reviewer note (e2e).")).toBeVisible();

  // Listed back on the cockpit with the fixture provider and pass rate.
  await page.getByRole("link", { name: "Back to runs" }).click();
  const table = page.getByTestId("eval-run-table");
  await expect(table).toContainText("fake");
  await expect(table.getByRole("row").nth(1)).toContainText("%");
});

test("security auditor has read-only access, in Spanish", async ({ page }) => {
  await signInViaBff(page, "audit.stone");
  await page.goto("/admin/dmind/evaluations");
  await expect(
    page.getByRole("heading", { name: "dMind evaluation" }),
  ).toBeVisible();
  await expect(page.getByText(/Read-only access/)).toBeVisible();
  await expect(page.getByRole("form", { name: "New run" })).toHaveCount(0);
  await setLanguage(page, "es");
  await expect(
    page.getByRole("heading", { name: "Evaluación dMind" }),
  ).toBeVisible();
  await expect(page.getByText(/Acceso de solo lectura/)).toBeVisible();
  await expect(page.getByTestId("eval-synthetic-banner")).toContainText(
    "Solo datos sintéticos",
  );
  await page.getByRole("link", { name: "Abrir ejecución" }).first().click();
  await expect(
    page.getByRole("heading", { name: /Detalle de la ejecución/ }),
  ).toBeVisible();
  await expect(page.getByTestId("eval-totals")).toContainText("Superados");
  await expect(
    page.getByRole("form", { name: "Registrar revisión" }),
  ).toHaveCount(0);
  await expect(page.getByRole("form", { name: "Record review" })).toHaveCount(
    0,
  );
  // Navigation entry is present for auditors.
  await expect(
    page.getByRole("link", { name: "Evaluación dMind" }).first(),
  ).toBeVisible();
});

test("physicians do not see the evaluation cockpit", async ({ page }) => {
  await signInViaBff(page, "dr.garcia");
  await page.goto("/dashboard");
  await expect(
    page.getByRole("link", { name: "dMind evaluation" }),
  ).toHaveCount(0);
  await page.goto("/admin/dmind/evaluations");
  await expect(
    page.getByText("Your role cannot access dMind evaluation."),
  ).toBeVisible();
});

test("evaluation cockpit fits 390px, passes axe and respects reduced motion", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await signInViaBff(page, "admin.silva");
  for (const route of ["/admin/dmind/evaluations"]) {
    await page.goto(route);
    await expect(page.getByTestId("eval-run-table")).toBeVisible();
    const results = await new AxeBuilder({ page })
      .withTags(["wcag2a", "wcag2aa", "wcag22aa"])
      .analyze();
    const serious = results.violations.filter(
      (v) => v.impact === "serious" || v.impact === "critical",
    );
    expect(serious, JSON.stringify(serious, null, 2)).toEqual([]);
  }
  await page.getByRole("link", { name: "Open run" }).first().click();
  await expect(page.getByTestId("eval-totals")).toBeVisible();
  const detailAxe = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag22aa"])
    .analyze();
  const seriousDetail = detailAxe.violations.filter(
    (v) => v.impact === "serious" || v.impact === "critical",
  );
  expect(seriousDetail, JSON.stringify(seriousDetail, null, 2)).toEqual([]);
  const pills = await page.evaluate(() =>
    Array.from(document.querySelectorAll<HTMLElement>(".pill"))
      .slice(0, 6)
      .map((el) => getComputedStyle(el).transitionProperty),
  );
  expect(pills.length).toBeGreaterThan(0);
  expect(pills.every((t) => t === "none")).toBe(true);
  const primary = page.getByRole("link", { name: "Back to runs" });
  await primary.hover();
  const pageEnter = await page.evaluate(() =>
    Array.from(document.querySelectorAll<HTMLElement>(".page-enter > *"))
      .slice(0, 4)
      .map((el) => getComputedStyle(el).animationName),
  );
  expect(pageEnter.every((a) => a === "none")).toBe(true);

  await page.setViewportSize({ width: 390, height: 844 });
  await page.goto("/admin/dmind/evaluations");
  await expect(page.getByRole("main")).toBeVisible();
  expect(await horizontalOverflow(page)).toBeLessThanOrEqual(0);
  await page.getByRole("link", { name: "Open run" }).first().click();
  await expect(page.getByTestId("eval-totals")).toBeVisible();
  expect(await horizontalOverflow(page)).toBeLessThanOrEqual(0);
});
