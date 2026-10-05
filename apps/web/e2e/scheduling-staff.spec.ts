import { expect, test, type Page } from "@playwright/test";
import { pickCombo, setLanguage, signInAsRegistration } from "./helpers";

/**
 * Staff scheduling journeys on the synthetic fixtures: the three-stage
 * "Find the best appointment" flow (need → valid options → hold and
 * confirmation), the capability-gated console for a transport coordinator,
 * runtime catalog administration and keyboard operation of the console tabs.
 * Every run books its own synthetic appointment and cancels it at the end so
 * the seeded agenda is left as it was.
 */

/** A seeded synthetic patient that does not already hold a future booking. */
const PATIENT_QUERY = "SYN-0003";

async function signInAsRole(
  page: Page,
  username: string,
  landing: RegExp,
): Promise<void> {
  await page.goto("/");
  await page
    .getByRole("button", { name: new RegExp(`Sign in as ${username}`) })
    .click();
  await expect(page).toHaveURL(landing);
}

test.describe.configure({ mode: "serial" });

test("registration staff find, hold, confirm and cancel an appointment", async ({
  page,
}) => {
  test.setTimeout(180_000);
  await signInAsRegistration(page);
  await page.goto("/scheduling");
  await expect(
    page.getByRole("heading", { name: "Scheduling console" }),
  ).toBeVisible();
  // Synthetic-provider notice is rendered from server metadata.
  await expect(page.getByText(/Synthetic development data/)).toBeVisible();

  // Stage 1: pick the patient and describe the need.
  await page.getByLabel("Search patient").fill(PATIENT_QUERY);
  await page
    .getByRole("search")
    .getByRole("button", { name: "Search" })
    .click();
  await page
    .getByRole("list", { name: "Patients" })
    .getByRole("button")
    .first()
    .click();
  const stepper = page.getByRole("list", { name: "Find the best appointment" });
  await expect(stepper.getByText("1. Need and constraints")).toBeVisible();
  await pickCombo(page, "Service", "General medicine consultation");
  await page.getByTestId("find-best-appointment").click();

  // Stage 2: only deterministically valid options, each with reasons.
  const options = page.getByRole("list", { name: "2. Best valid options" });
  await expect(options).toBeVisible();
  const first = options.getByRole("listitem").first();
  await expect(first.getByText(/^#1 · /)).toBeVisible();
  await expect(first.getByText("Why this option")).toBeVisible();
  await expect(page.getByText(/Matcher: access-matcher\.v1/)).toBeVisible();
  // Ranking provenance is always stated (dMind or deterministic).
  await expect(
    page
      .getByText(
        /Ranked by (a synthetic )?dMind|Options use deterministic ranking|Options ordered by deterministic rules/,
      )
      .first(),
  ).toBeVisible();

  // Stage 3: hold, then confirm.
  await first.getByRole("button", { name: "Hold this option" }).click();
  await expect(first.getByText("Held")).toBeVisible();
  await expect(first.getByText(/Hold expires/)).toBeVisible();
  await first.getByRole("button", { name: "Confirm appointment" }).click();
  const booked = page.getByTestId("booking-confirmed");
  await expect(booked).toBeVisible();
  await expect(booked.getByText("Appointment confirmed.")).toBeVisible();
  const icsHref = await booked
    .getByRole("link", { name: "Download calendar file" })
    .getAttribute("href");
  expect(icsHref).toMatch(/^\/api\/v1\/appointments\/[0-9a-f-]+\/ics/);

  // The confirmed appointment appears in the console worklist; cancel it with
  // a mandatory reason so the fixture agenda stays stable between runs.
  await page.getByRole("button", { name: "Start over" }).click();
  await page.getByRole("tab", { name: "Appointments" }).click();
  const panel = page.locator("#scheduling-panel");
  const apptId = /appointments\/([0-9a-f-]+)\/ics/.exec(icsHref ?? "")?.[1];
  const row = panel.locator(
    `[data-testid="worklist-row"][data-id="${apptId}"]`,
  );
  await row.click();
  const card = panel
    .getByRole("listitem")
    .filter({ has: page.locator(`a[href="${icsHref}"]`) });
  await expect(card).toBeVisible();
  await expect(card.getByText(PATIENT_QUERY)).toBeVisible();
  await card.getByRole("button", { name: "Cancel appointment" }).click();
  const confirm = panel.getByRole("group", { name: "Cancel appointment" });
  await confirm
    .getByLabel("Reason", { exact: true })
    .fill("E2E cleanup (synthetic).");
  // Staff may cancel inside the notice window only with an explicit override.
  await confirm
    .getByLabel(/outside tenant policy/)
    .fill("E2E cleanup inside the notice window (synthetic).");
  await confirm.getByRole("button", { name: "Confirm", exact: true }).click();
  await expect(page.getByText("Appointment updated.")).toBeVisible();
  await page.getByLabel("State").selectOption("cancelled");
  await row.click();
  const cancelled = panel
    .getByRole("listitem")
    .filter({ has: page.locator(`a[href="${icsHref}"]`) });
  await expect(cancelled.getByText("Cancelled", { exact: true })).toBeVisible();
  await expect(
    cancelled.getByText(/Override reason: E2E cleanup inside/),
  ).toBeVisible();
});

test("console tabs are keyboard operable", async ({ page }) => {
  await signInAsRegistration(page);
  await page.goto("/scheduling");
  const find = page.getByRole("tab", { name: "Find the best appointment" });
  await find.focus();
  await page.keyboard.press("ArrowRight");
  await expect(page.getByRole("tab", { name: "Agenda" })).toBeFocused();
  await expect(page.getByRole("tab", { name: "Agenda" })).toHaveAttribute(
    "aria-selected",
    "true",
  );
  await page.keyboard.press("ArrowLeft");
  await expect(find).toBeFocused();
  // Wraps around from the first to the last tab.
  await page.keyboard.press("ArrowLeft");
  await expect(
    page.getByRole("tab", { name: "Transport coordination" }),
  ).toBeFocused();
});

test("transport coordinators only see the transport worklist", async ({
  page,
}) => {
  await signInAsRole(page, "transport.ruiz", /\/scheduling/);
  const tabs = page.getByRole("tab");
  await expect(tabs).toHaveCount(1);
  await expect(tabs.first()).toHaveText("Transport coordination");
  await expect(
    page.getByText(/Emergency transport requires an authorized human decision/),
  ).toBeVisible();
  // No clinical navigation for the logistics role.
  await expect(
    page.locator(".sidebar").getByRole("link", { name: "Patients" }),
  ).toHaveCount(0);
});

test("administrators add a previously unknown specialty at runtime", async ({
  page,
}) => {
  await signInAsRole(page, "admin.silva", /\/dashboard/);
  await page.goto("/scheduling/catalog");
  await page.getByRole("tab", { name: "Specialties" }).click();
  await page.getByTestId("add-entry").click();
  const code = `e2e_specialty_${Date.now().toString(36)}`;
  const editor = page.getByTestId("catalog-editor");
  await editor.locator("#ce-code").fill(code);
  await editor.locator("#ce-en").fill("E2E runtime specialty");
  await editor
    .locator("#ce-es")
    .fill("Especialidad E2E en tiempo de ejecución");
  await editor.getByRole("button", { name: "Create entry" }).click();
  const table = page.getByTestId("catalog-table");
  const row = table.getByRole("row").filter({ hasText: code });
  await expect(row).toBeVisible();
  await expect(row.getByText("E2E runtime specialty")).toBeVisible();
  // Lifecycle is non-destructive: deactivate keeps the row and its history.
  await row.getByRole("button", { name: "Deactivate" }).click();
  const box = row.getByRole("group", { name: "Deactivate" });
  await box
    .getByLabel("Change reason")
    .fill("E2E lifecycle check (synthetic).");
  await box.getByRole("button", { name: "Confirm", exact: true }).click();
  await expect(page.getByText("Entry deactivated.")).toBeVisible();
  // Inactive entries leave the default list (non-destructive lifecycle) and
  // remain inspectable with their history.
  await expect(row).toHaveCount(0);
  await page.getByLabel("Include inactive").check();
  await expect(row.getByText("Inactive")).toBeVisible();
  await row.getByRole("button", { name: "History" }).click();
  await expect(page.getByTestId("catalog-history")).toBeVisible();
});

test("console switches to Spanish without losing state", async ({ page }) => {
  await signInAsRegistration(page);
  await page.goto("/scheduling");
  await page.getByRole("tab", { name: "Capacity pressure" }).click();
  await setLanguage(page, "es");
  await expect(
    page.getByRole("heading", { name: "Consola de programación" }),
  ).toBeVisible();
  await expect(
    page.getByRole("tab", { name: "Presión de capacidad" }),
  ).toHaveAttribute("aria-selected", "true");
  await expect(page.getByText(/Datos sintéticos de desarrollo/)).toBeVisible();
});

test("console fits a 390px viewport without horizontal scrolling @mobile", async ({
  page,
}) => {
  await signInAsRegistration(page);
  await page.goto("/scheduling");
  await expect(
    page.getByRole("heading", { name: "Scheduling console" }),
  ).toBeVisible();
  await page.getByRole("tab", { name: "Waitlist recovery" }).click();
  await expect(page.locator("#scheduling-panel")).toBeVisible();
  const overflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth -
      document.documentElement.clientWidth,
  );
  expect(overflow).toBe(0);
});
