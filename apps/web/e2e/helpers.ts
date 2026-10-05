import type { Locator, Page } from "@playwright/test";
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

/** Open the account menu (language, appearance, sign out) if it is closed. */
export async function openAccountMenu(page: Page): Promise<Locator> {
  const button = page.getByRole("button", { name: "Account menu" }).first();
  if ((await button.getAttribute("aria-expanded")) !== "true") {
    await button.click();
  }
  const panel = page.getByRole("dialog", { name: "Account" });
  await expect(panel).toBeVisible();
  return panel;
}

/** Switch the interface language from the account menu. */
export async function setLanguage(
  page: Page,
  lang: "en" | "es",
): Promise<void> {
  const panel = await openAccountMenu(page);
  await panel.getByLabel("Language").selectOption(lang);
  await page.keyboard.press("Escape");
}

/** Click "Sign out" in the account menu (the confirmation, if any, is left to the caller). */
export async function clickSignOut(page: Page): Promise<void> {
  const panel = await openAccountMenu(page);
  await panel.getByRole("button", { name: "Sign out" }).click();
}

/** Sign out from the account menu and wait for the sign-in screen. */
export async function signOut(page: Page): Promise<void> {
  await clickSignOut(page);
  await expect(page).toHaveURL(/\/$/);
  await expect(
    page.getByRole("button", { name: /Sign in as/ }).first(),
  ).toBeVisible();
}

/** Pick an option in a searchable combobox by its visible label. */
export async function pickCombo(
  scope: Page | Locator,
  label: string,
  option: string,
): Promise<void> {
  const input = scope.getByRole("combobox", { name: label, exact: true });
  await input.fill(option);
  await scope
    .getByRole("listbox")
    .getByRole("option", { name: new RegExp(`^${option}`) })
    .first()
    .click();
  await expect(input).toHaveValue(option);
}
