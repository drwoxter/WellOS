import { expect, test, type Page } from "@playwright/test";
import { pickCombo, setLanguage } from "./helpers";

/**
 * Patient self-service journey on the synthetic fixtures. `rep.alba` holds a
 * verified `self` grant; `rep.ortiz` is a parent/guardian with two dependants.
 * The booking test books and then cancels its own appointment so the seeded
 * self-service data is left unchanged.
 */

async function signInAsPatient(page: Page, username: string): Promise<void> {
  await page.goto("/");
  await page
    .getByRole("button", { name: new RegExp(`Sign in as ${username}`) })
    .click();
  await expect(page).toHaveURL(/\/my(\/|$)/);
  await page.goto("/my/appointments");
  await expect(page.getByTestId("my-appointments")).toBeVisible();
}

test.describe.configure({ mode: "serial" });

test("a patient requests, holds, confirms and cancels an appointment", async ({
  page,
}) => {
  test.setTimeout(180_000);
  await signInAsPatient(page, "rep.alba");
  await expect(page.getByText(/Synthetic development data/)).toBeVisible();
  // No clinical navigation is offered to the self-service role.
  await expect(
    page.locator(".sidebar").getByRole("link", { name: "Patients" }),
  ).toHaveCount(0);
  await expect(
    page.locator(".sidebar").getByRole("link", { name: "Results" }),
  ).toHaveCount(0);

  // Stage 1: the patient is derived from the grant, never typed in.
  await page.getByTestId("find-best-appointment").click();
  const panel = page.getByRole("tabpanel");
  await expect(panel.getByText(/Patient: Alba Demopatient/)).toBeVisible();
  await pickCombo(panel, "Service", "General medicine consultation");
  await panel.getByTestId("find-best-appointment").click();

  // Stage 2: ranked valid options with a concise explanation.
  const options = panel.getByRole("list", { name: "2. Best valid options" });
  await expect(options).toBeVisible();
  const first = options.getByRole("listitem").first();
  await expect(first.getByText("Why this option")).toBeVisible();
  await expect(
    panel
      .getByText(
        /Ranked by (a synthetic )?dMind|Options use deterministic ranking|Options ordered by deterministic rules/,
      )
      .first(),
  ).toBeVisible();
  // Patients never see the staff override field.
  await expect(panel.getByLabel(/Override reason/)).toHaveCount(0);

  // Stage 3: hold then confirm.
  await first.getByRole("button", { name: "Hold this option" }).click();
  await expect(first.getByText("Held")).toBeVisible();
  await first.getByRole("button", { name: "Confirm appointment" }).click();
  const booked = panel.getByTestId("booking-confirmed");
  await expect(booked.getByText("Appointment confirmed.")).toBeVisible();
  const icsHref = await booked
    .getByRole("link", { name: "Download calendar file" })
    .getAttribute("href");
  expect(icsHref).toMatch(/^\/api\/v1\/me\/appointments\/[0-9a-f-]+\/ics/);

  // The appointment is listed with history and a calendar download, then
  // cancelled within policy with a reason.
  await page.getByRole("tab", { name: "Appointments" }).click();
  const apptId = /appointments\/([0-9a-f-]+)\/ics/.exec(icsHref ?? "")?.[1];
  const row = page.locator(`[data-testid="worklist-row"][data-id="${apptId}"]`);
  await row.click();
  const card = page
    .getByTestId("appointment-card")
    .filter({ has: page.locator(`a[href="${icsHref}"]`) });
  await expect(card).toBeVisible();
  await expect(card.getByText("Confirmed")).toBeVisible();
  await card.getByRole("button", { name: "History" }).click();
  await expect(card.getByTestId("appointment-history")).toBeVisible();
  await card.getByRole("button", { name: "Cancel appointment" }).click();
  const box = card.getByRole("group", { name: "Cancel appointment" });
  await box
    .getByLabel("Cancellation reason")
    .selectOption("scheduling_conflict");
  await box.getByRole("button", { name: "Cancel appointment" }).click();
  await expect(page.getByText("Appointment cancelled.")).toBeVisible();
  // Cancelled appointments leave "Upcoming" and stay in the history with
  // their reason; the calendar download is only offered for active ones.
  await expect(card).toHaveCount(0);
  await page
    .getByRole("group", { name: "Range" })
    .getByRole("button", { name: "History" })
    .click();
  await row.click();
  const cancelled = page
    .getByTestId("appointment-card")
    .filter({ hasText: "Cancellation reason: scheduling_conflict" })
    .first();
  await expect(cancelled.getByText("Cancelled")).toBeVisible();
  await expect(
    cancelled.getByRole("link", { name: "Add to calendar" }),
  ).toHaveCount(0);
});

test("a guardian switches between dependants and sees the relationship", async ({
  page,
}) => {
  await signInAsPatient(page, "rep.ortiz");
  const switcher = page.getByLabel("Managing appointments for");
  await expect(switcher).toBeVisible();
  // Expired and revoked grants are not offered; relationships are explicit.
  const labels = await switcher.getByRole("option").allInnerTexts();
  expect(labels).toEqual(
    expect.arrayContaining([
      "Leo Ortiz (Parent or guardian)",
      "Ramón Ortiz (Authorized representative)",
    ]),
  );
  expect(labels).toHaveLength(2);
  await switcher.selectOption({
    label: "Ramón Ortiz (Authorized representative)",
  });
  await page.getByRole("tab", { name: "Find the best appointment" }).click();
  await expect(
    page.getByRole("tabpanel").getByText(/Patient: Ramón Ortiz/),
  ).toBeVisible();
});

test("self-service tabs are keyboard operable", async ({ page }) => {
  await signInAsPatient(page, "rep.alba");
  const first = page.getByRole("tab", { name: "Appointments" });
  await first.focus();
  await page.keyboard.press("ArrowRight");
  const find = page.getByRole("tab", { name: "Find the best appointment" });
  await expect(find).toBeFocused();
  await expect(find).toHaveAttribute("aria-selected", "true");
  await page.keyboard.press("End");
  await expect(page.getByRole("tab", { name: "Notifications" })).toBeFocused();
  await page.keyboard.press("Home");
  await expect(first).toBeFocused();
});

test("self-service is complete in Spanish", async ({ page }) => {
  await signInAsPatient(page, "rep.alba");
  await setLanguage(page, "es");
  await expect(page.getByRole("heading", { name: "Mis citas" })).toBeVisible();
  await expect(
    page.getByRole("tab", { name: "Encontrar la mejor cita" }),
  ).toBeVisible();
  await expect(page.getByRole("tab", { name: "Preferencias" })).toBeVisible();
  await page.getByRole("tab", { name: "Lista de espera" }).click();
  await expect(page.getByTestId("waitlist-section")).toBeVisible();
  await expect(page.getByText(/Datos sintéticos de desarrollo/)).toBeVisible();
});

test("self-service fits a 390px viewport @mobile", async ({ page }) => {
  await signInAsPatient(page, "rep.alba");
  await expect(page.locator(".mobile-nav")).toBeVisible();
  await page.getByRole("tab", { name: "Preferences" }).click();
  await expect(page.getByTestId("preferences-form")).toBeVisible();
  const overflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth -
      document.documentElement.clientWidth,
  );
  expect(overflow).toBe(0);
});
