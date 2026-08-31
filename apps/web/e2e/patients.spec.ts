import { expect, test } from "@playwright/test";
import { signInAs } from "./helpers";

test("clinician searches for a patient and opens the workspace", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");

  await page.getByRole("link", { name: "Patients" }).first().click();
  await page.getByLabel("Search patients").fill("Demopatient");
  await page.getByRole("button", { name: "Search", exact: true }).click();

  const hit = page.locator(".result-card").first();
  await expect(hit).toBeVisible();
  await hit.getByRole("link", { name: "Open chart" }).click();

  // Patient workspace: header, safety information and timeline sections.
  await expect(page.getByText("Allergies").first()).toBeVisible();
  await expect(page.getByRole("tablist")).toBeVisible();
});

test("patient search shows a helpful no-results state", async ({ page }) => {
  await signInAs(page, "dr.garcia");
  await page.goto("/patients");
  await page.getByLabel("Search patients").fill("zzz-no-such-patient");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await expect(page.getByText(/No patients found for/)).toBeVisible();
});
