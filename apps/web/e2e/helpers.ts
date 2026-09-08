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

/** Registration staff land on the access board instead of the cockpit. */
export async function signInAsRegistration(page: Page): Promise<void> {
  await page.goto("/");
  await page.getByRole("button", { name: /Sign in as reg\.rivera/ }).click();
  await expect(page).toHaveURL(/\/access/);
  await expect(page.getByRole("tab", { name: "Arrivals" })).toBeVisible();
}

/** Sign out from the desktop top bar and wait for the sign-in screen. */
export async function signOut(page: Page): Promise<void> {
  await page
    .locator(".topbar")
    .getByRole("button", { name: "Sign out" })
    .click();
  await expect(page).toHaveURL(/\/$/);
  await expect(
    page.getByRole("button", { name: /Sign in as/ }).first(),
  ).toBeVisible();
}
