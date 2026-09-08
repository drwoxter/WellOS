import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Locator, type Page } from "@playwright/test";
import { signInAs, signInAsRegistration, signOut } from "./helpers";

/**
 * Care-team golden path: development login → register a new synthetic patient
 * and an appointment → arrival → nurse triage (deterministic floor + dMind
 * suggestion) → assignment to a named physician → internal alert → start
 * consultation from the ready card → handoff card in the encounter → sign.
 * Every seeded patient already has an open access episode, so each run
 * registers its own patient (unique identifier) and leaves the seed untouched.
 */

const PATIENT = "Nuria Demopatient";
const IDENTIFIER = `E2E-${Date.now().toString(36).toUpperCase()}`;

/** Register a new synthetic patient; the app opens the chart on success. */
async function registerPatient(page: Page): Promise<void> {
  await page.goto("/patients");
  await page.getByLabel("Family name").fill("Demopatient");
  await page.getByLabel("Given name").fill("Nuria");
  await page.getByLabel("Date of birth").fill("1984-06-30");
  await page.getByLabel("Sex").selectOption("female");
  await page.getByLabel("Identifier").fill(IDENTIFIER);
  await page.getByRole("button", { name: "Register patient" }).click();
  await expect(page).toHaveURL(/\/patients\/.+/);
  await expect(page.getByRole("heading", { name: PATIENT })).toBeVisible();
}

/** The visit card for this run's patient, in the given state. */
function visitItem(scope: Page | Locator, status: string): Locator {
  return scope
    .getByRole("listitem", { name: `${PATIENT} — ${status}` })
    .filter({ hasText: IDENTIFIER });
}

/** `datetime-local` value for a time later today (browser-local clock). */
function laterToday(): string {
  const d = new Date();
  d.setHours(d.getHours() + 1, 0, 0, 0);
  const pad = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${pad(d.getMonth() + 1)}-${pad(d.getDate())}T${pad(d.getHours())}:${pad(d.getMinutes())}`;
}

test.describe.configure({ mode: "serial" });

test("registration → arrival → triage → assignment → alert → consultation", async ({
  page,
}) => {
  test.setTimeout(180_000);

  // ── Registration staff: appointment and arrival from the patient workspace ──
  await signInAsRegistration(page);
  await registerPatient(page);
  const visitCard = page.getByRole("region", { name: "Today's visit" });
  await expect(visitCard.getByText("No visit today.")).toBeVisible();
  await visitCard.getByRole("button", { name: "Register arrival" }).click();
  await visitCard.getByRole("radio", { name: "Appointment" }).check();
  await visitCard.getByLabel("Scheduled for").fill(laterToday());
  await visitCard
    .getByLabel(/Reason for visit/)
    .fill("Cough and shortness of breath for two days (synthetic).");
  await visitCard
    .getByRole("button", { name: "Register", exact: true })
    .click();

  const scheduled = visitItem(visitCard, "Scheduled");
  await expect(scheduled).toBeVisible();
  await expect(
    scheduled.getByText("Appointment · General medicine"),
  ).toBeVisible();
  // Registration staff never see triage or consultation actions.
  await expect(visitCard.getByRole("link", { name: /triage/i })).toHaveCount(0);
  await expect(
    visitCard.getByRole("button", { name: "Start consultation" }),
  ).toHaveCount(0);

  await scheduled.getByRole("button", { name: "Mark arrived" }).click();
  const arrived = visitItem(visitCard, "Arrived");
  await expect(arrived).toBeVisible();
  await expect(arrived.getByText("Waiting")).toBeVisible();

  // The access board lists the arrival in the triage queue by name only.
  await page.goto("/access");
  await page.getByRole("tab", { name: "Triage" }).click();
  const boardCard = visitItem(page, "Arrived");
  await expect(boardCard).toBeVisible();
  await expect(boardCard).not.toContainText(
    /[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}/,
  );
  await signOut(page);

  // ── Nurse: cockpit queue → full access board → triage ──
  await signInAs(page, "nurse.kim");
  const queue = page.getByRole("region", { name: "Triage queue" });
  await expect(queue.getByRole("listitem").first()).toBeVisible();
  await queue.getByRole("link", { name: "Open access board" }).click();
  await expect(page).toHaveURL(/\/access/);
  await expect(
    page.getByRole("tab", { name: "Triage", selected: true }),
  ).toBeVisible();
  await visitItem(page, "Arrived")
    .getByRole("link", { name: "Open triage" })
    .click();
  await expect(page).toHaveURL(/\/visits\/.+\/triage/);
  await expect(page.getByRole("heading", { name: PATIENT })).toBeVisible();
  await expect(
    page.getByRole("textbox", { name: "Reason for visit" }),
  ).toHaveValue("Cough and shortness of breath for two days (synthetic).");

  await page
    .getByRole("group", { name: "Main concerns" })
    .getByRole("checkbox", { name: "Shortness of breath" })
    .check();
  await page.getByLabel(/^Systolic/).fill("138");
  await page.getByLabel(/^Diastolic/).fill("86");
  await page.getByLabel(/^Heart rate/).fill("104");
  await page.getByLabel(/^Respiratory rate/).fill("24");
  await page.getByLabel(/^Temperature/).fill("38.2");
  await page.getByLabel(/^Oxygen saturation/).fill("92");
  await page
    .getByLabel("Triage note")
    .fill("Speaking full sentences, mild distress (synthetic).");
  await page.getByRole("button", { name: "Save triage" }).click();
  await expect(page.getByText("Triage saved.")).toBeVisible();

  // Deterministic floor: SpO₂ < 94 % raises the minimum to Urgent and the
  // lower options are not selectable.
  const floor = page.getByRole("region", { name: "Safety floor" });
  await expect(
    floor.getByText("Urgent", { exact: true }).first(),
  ).toBeVisible();
  await expect(floor.getByText(/Oxygen saturation below 94%/)).toBeVisible();
  const priority = page.getByLabel(/Operational priority/);
  await expect(
    priority.locator("option", { hasText: "Standard" }),
  ).toBeDisabled();
  await expect(
    priority.locator("option", { hasText: "Non-urgent" }),
  ).toBeDisabled();

  // dMind suggestion is assistive only and needs an explicit human decision.
  await page.getByRole("button", { name: "Ask dMind" }).click();
  await expect(
    page.getByText("Assistive suggestion — your decision is required"),
  ).toBeVisible();
  await expect(page.getByText(/Proposed priority: Urgent/)).toBeVisible();
  await expect(page.getByText("Facts used")).toBeVisible();
  await page.getByRole("button", { name: "Accept", exact: true }).click();
  await expect(page.getByText("Your decision was recorded.")).toBeVisible();
  await expect(priority).toHaveValue("urgent");

  // Route to a named professional and complete the triage.
  await page.getByRole("radio", { name: "Named professional" }).check();
  await page
    .getByRole("combobox", { name: "Named professional" })
    .selectOption({ label: "Dr. Gabriel García (Physician)" });
  await page.getByRole("button", { name: "Complete triage and route" }).click();
  await expect(
    page.getByText("Triage completed. The care team has been alerted."),
  ).toBeVisible();
  await expect(page.getByText("Ready for consultation").first()).toBeVisible();
  await expect(page.getByText(/Assigned to Dr. Gabriel García/)).toBeVisible();
  // Nurses never start the consultation.
  await expect(
    page.getByRole("button", { name: "Start consultation" }),
  ).toHaveCount(0);
  await signOut(page);

  // ── Physician: alert, ready card, handoff, sign ──
  await signInAs(page, "dr.garcia");
  const alerts = page.getByRole("region", { name: "Alerts for you" });
  const readyAlerts = alerts.locator(".result-card", {
    hasText: `Patient ready — ${PATIENT}`,
  });
  const openAlerts = readyAlerts.filter({
    has: page.getByRole("button", { name: "Acknowledge" }),
  });
  await expect(openAlerts.first()).toBeVisible();
  await expect(openAlerts.first()).toContainText("SpO2 92%");
  const openBefore = await openAlerts.count();
  await openAlerts.first().getByRole("button", { name: "Acknowledge" }).click();
  await expect(openAlerts).toHaveCount(openBefore - 1);
  await expect(
    readyAlerts.filter({ hasText: "Acknowledged" }).first(),
  ).toBeVisible();

  const ready = page.getByRole("region", { name: "Ready for consultation" });
  const readyCard = visitItem(ready, "Ready for consultation");
  await expect(
    readyCard.getByText(/Assigned to Dr. Gabriel García/),
  ).toBeVisible();
  await readyCard.getByRole("button", { name: "Start consultation" }).click();
  await expect(page).toHaveURL(/\/encounters\//);
  await expect(page.getByRole("heading", { name: PATIENT })).toBeVisible();

  const handoff = page.getByRole("region", { name: "Arrival and triage" });
  await expect(handoff.getByText("Appointment", { exact: true })).toBeVisible();
  await expect(
    handoff.getByText("In consultation", { exact: true }),
  ).toBeVisible();
  await expect(handoff.getByText("Urgent").first()).toBeVisible();
  await expect(handoff.getByText(/Triaged by Nurse Ana Kim/)).toBeVisible();
  await expect(
    handoff.getByText("Shortness of breath", { exact: true }),
  ).toBeVisible();
  await expect(
    handoff.getByText(/Oxygen saturation below 94%/).first(),
  ).toBeVisible();
  await expect(
    handoff.getByText("Speaking full sentences, mild distress (synthetic)."),
  ).toBeVisible();
  // The handoff is a read-only record of the access workflow.
  await expect(handoff.getByRole("textbox")).toHaveCount(0);

  await page
    .getByLabel(/Reason for consultation/)
    .fill("Cough and dyspnoea, febrile (synthetic).");
  await page
    .getByLabel(/Assessment/)
    .fill("Probable lower respiratory tract infection (synthetic).");
  await page.getByRole("button", { name: "Save draft" }).click();
  await expect(page.getByText("Draft saved.")).toBeVisible();
  await page.getByRole("button", { name: "Sign and complete" }).click();
  await page.getByRole("button", { name: "Confirm", exact: true }).click();
  await expect(page.getByText("Clinical summary")).toBeVisible();

  // Signing the encounter closes the visit.
  await page.goto("/access");
  await page.getByRole("tab", { name: "Closed today" }).click();
  await expect(visitItem(page, "Completed")).toBeVisible();
});

test("access board and triage workspace are keyboard operable with no serious a11y violations", async ({
  page,
}) => {
  await signInAs(page, "nurse.kim");
  await page.goto("/access");
  await expect(
    page.getByRole("tab", { name: "Triage", selected: true }),
  ).toBeVisible();

  // Tabs are real tabs: focus, then switch with Enter.
  await page.getByRole("tab", { name: "Ready" }).focus();
  await page.keyboard.press("Enter");
  await expect(
    page.getByRole("tab", { name: "Ready", selected: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("listitem", { name: /Jonás Demopatient — Ready/ }),
  ).toBeVisible();

  const accessScan = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag22aa"])
    .analyze();
  expect(
    accessScan.violations.filter((v) =>
      ["serious", "critical"].includes(v.impact ?? ""),
    ),
  ).toEqual([]);

  // Seeded triage in progress (Diego): open from the queue with the keyboard.
  await page.getByRole("tab", { name: "Triage" }).click();
  const link = page
    .getByRole("listitem", { name: /Diego Demopatient — In triage/ })
    .getByRole("link", { name: "Continue triage" });
  await link.focus();
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/\/visits\/.+\/triage/);
  await expect(
    page.getByRole("heading", { name: "Diego Demopatient" }),
  ).toBeVisible();

  // Red flags are semantic checkboxes toggled with Space.
  const redFlag = page
    .getByRole("group", { name: "Red flags" })
    .getByRole("checkbox", { name: "Chest pain" });
  const before = await redFlag.isChecked();
  await redFlag.focus();
  await page.keyboard.press("Space");
  await expect(redFlag).toBeChecked({ checked: !before });
  await page.keyboard.press("Space");
  await expect(redFlag).toBeChecked({ checked: before });

  const triageScan = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag22aa"])
    .analyze();
  expect(
    triageScan.violations.filter((v) =>
      ["serious", "critical"].includes(v.impact ?? ""),
    ),
  ).toEqual([]);
});

test("access board and triage workspace switch to Spanish", async ({
  page,
}) => {
  await signInAs(page, "nurse.kim");
  await page.goto("/access");
  await page.getByLabel("Language").first().selectOption("es");
  await expect(
    page.getByRole("heading", { name: "Acceso de pacientes" }).first(),
  ).toBeVisible();
  await expect(page.getByRole("tab", { name: "Llegadas" })).toBeVisible();
  await expect(
    page.getByRole("tab", { name: "Triaje", selected: true }),
  ).toBeVisible();
  await page
    .getByRole("listitem", { name: /Diego Demopatient — En triaje/ })
    .getByRole("link", { name: "Continuar triaje" })
    .click();
  await expect(
    page.getByRole("heading", { name: "Triaje", exact: true }),
  ).toBeVisible();
  await expect(
    page.getByRole("button", { name: "Guardar triaje" }),
  ).toBeVisible();
  await expect(page.getByText("Piso de seguridad").first()).toBeVisible();
});

test("access board and triage workspace fit a 390px phone @mobile", async ({
  page,
}) => {
  await signInAs(page, "nurse.kim");
  await page.goto("/access");
  await expect(page.getByRole("tab", { name: "Triage" })).toBeVisible();
  const card = page.getByRole("listitem", {
    name: /Marta Demopatient — Arrived/,
  });
  await expect(card).toBeVisible();
  await expect(card.getByRole("link", { name: "Open triage" })).toBeVisible();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);

  await card.getByRole("link", { name: "Open triage" }).click();
  await expect(page).toHaveURL(/\/visits\/.+\/triage/);
  await expect(page.getByRole("button", { name: "Save triage" })).toBeVisible();
  await expect(page.getByLabel(/^Oxygen saturation/)).toBeVisible();
  expect(
    await page.evaluate(
      () => document.documentElement.scrollWidth <= window.innerWidth,
    ),
  ).toBe(true);
});
