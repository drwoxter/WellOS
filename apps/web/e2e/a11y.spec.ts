import { expect, test } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { signInAs } from "./helpers";

async function expectNoSeriousViolations(
  page: import("@playwright/test").Page,
) {
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

test("login page has no serious accessibility violations", async ({ page }) => {
  await page.goto("/");
  await page.getByText("Development only").waitFor();
  await expectNoSeriousViolations(page);
});

test("dashboard has no serious accessibility violations", async ({ page }) => {
  await signInAs(page, "dr.garcia");
  await expectNoSeriousViolations(page);
});

test("patients page has no serious accessibility violations", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await page.goto("/patients");
  await page.getByLabel("Search patients").waitFor();
  await expectNoSeriousViolations(page);
});

test("encounter workspace has no serious accessibility violations", async ({
  page,
}) => {
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
  await page.getByRole("button", { name: "Save draft" }).waitFor();
  await expectNoSeriousViolations(page);
});

test("results page has no serious accessibility violations", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await page.goto("/results");
  await page.getByLabel("Criticality").waitFor();
  await expectNoSeriousViolations(page);
});
