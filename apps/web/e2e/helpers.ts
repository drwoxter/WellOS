import type { Page } from "@playwright/test";
import { expect } from "@playwright/test";

/** Sign in through the development demo role cards. */
export async function signInAs(page: Page, username: string): Promise<void> {
  await page.goto("/");
  await page
    .getByRole("button", { name: new RegExp(`Sign in as ${username}`) })
    .click();
  await expect(page).toHaveURL(/\/dashboard/);
  await expect(page.getByText("Critical open results")).toBeVisible();
}
