import { expect, test } from "@playwright/test";
import { signInAs } from "./helpers";

test("desktop shows the sidebar navigation", async ({ page }) => {
  await signInAs(page, "dr.garcia");
  await expect(page.locator(".sidebar")).toBeVisible();
  await expect(page.locator(".mobile-nav")).toBeHidden();
});

test("390px viewport shows the compact mobile navigation @mobile", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await expect(page.locator(".mobile-nav")).toBeVisible();
  await expect(page.locator(".sidebar")).toBeHidden();
  // Result cards replace the table on small screens.
  await page.goto("/results");
  await expect(page.locator(".mobile-only").first()).toBeVisible();
});
