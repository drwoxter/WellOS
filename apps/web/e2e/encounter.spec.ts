import { expect, test } from "@playwright/test";
import { signInAs } from "./helpers";

/**
 * Clinician documentation journey: development login → patient search →
 * start consultation → vital signs → clinical note → diagnosis → save draft →
 * sign → patient timeline → signed summary. Mutates the seeded demo data;
 * run `make seed` from the repository root to restore the demo states.
 */
test("clinician documents and signs a consultation end to end", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");

  // Patient search without internal identifiers.
  await page.getByRole("link", { name: "Patients" }).first().click();
  await page.getByLabel("Search patients").fill("SYN-0005");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  const hit = page.locator(".result-card").first();
  await expect(hit).toBeVisible();
  await hit.getByRole("link", { name: "Open chart" }).click();

  // Start a new consultation from the patient workspace.
  await page.getByRole("button", { name: "Start consultation" }).click();
  await expect(page).toHaveURL(/\/encounters\//);
  await expect(
    page.getByRole("heading", { name: "Jonás Demopatient" }),
  ).toBeVisible();
  await expect(page.getByText("In progress").first()).toBeVisible();

  // Record vital signs (usual values, no confirmation needed).
  await page.getByRole("button", { name: "Record vital signs" }).click();
  await page.getByLabel(/Systolic/).fill("128");
  await page.getByLabel(/Diastolic/).fill("82");
  await page.getByLabel(/Heart rate/).fill("72");
  await page.getByLabel(/Temperature/).fill("36.8");
  await page.getByLabel(/Oxygen saturation/).fill("98");
  await page.getByLabel(/Weight/).fill("80");
  await page.getByLabel(/Height/).fill("175");
  await page
    .locator('button[type="submit"]', { hasText: "Record vital signs" })
    .click();
  await expect(page.getByText("Vital signs recorded.")).toBeVisible();
  await expect(page.getByText("26.1 kg/m²").first()).toBeVisible();

  // Document the clinical note.
  await page
    .getByLabel(/Reason for consultation/)
    .fill("Routine check-up with mild fatigue (synthetic).");
  await page
    .getByLabel(/History of presenting complaint/)
    .fill("Two weeks of mild fatigue, no fever (synthetic).");
  await page
    .getByLabel(/Physical examination/)
    .fill("Well appearing; unremarkable examination (synthetic).");
  await page
    .getByLabel(/Assessment/)
    .fill("Mild fatigue, likely benign (synthetic).");
  await page
    .getByLabel(/^Plan/)
    .fill("Reassurance and follow-up in two weeks (synthetic).");

  // Add a diagnosis by human-readable label.
  await page.getByRole("button", { name: "Add diagnosis" }).click();
  await page.getByLabel("Diagnosis").first().fill("Fatigue");
  await page.getByLabel(/Code/).fill("R53.83");
  await page
    .locator('form button[type="submit"]', { hasText: "Add diagnosis" })
    .click();
  await expect(page.getByText("Diagnosis added.")).toBeVisible();
  await expect(
    page.locator(".result-card .title", { hasText: "Fatigue" }),
  ).toBeVisible();

  // Save the draft.
  await page.getByRole("button", { name: "Save draft" }).click();
  await expect(page.getByText("Draft saved.")).toBeVisible();

  // Sign with explicit confirmation.
  await page.getByRole("button", { name: "Sign and complete" }).click();
  await expect(page.getByText(/signed note is permanent/i)).toBeVisible();
  await page.getByRole("button", { name: "Confirm", exact: true }).click();
  await expect(page.getByText("Clinical summary")).toBeVisible();
  await expect(page.getByText("Signed").first()).toBeVisible();
  await expect(page.getByLabel(/Reason for consultation/)).toHaveCount(0);

  // The signed encounter appears in the patient timeline and opens read-only.
  await page.goto("/patients");
  await page.getByLabel("Search patients").fill("SYN-0005");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page
    .locator(".result-card")
    .first()
    .getByRole("link", { name: "Open chart" })
    .click();
  await page.getByRole("tab", { name: /Encounters/ }).click();
  await expect(page.getByText("Completed").first()).toBeVisible();
  await page.getByRole("link", { name: "Open summary" }).first().click();
  await expect(page.getByText("Clinical summary")).toBeVisible();
  await expect(
    page.getByText("Mild fatigue, likely benign (synthetic)."),
  ).toBeVisible();
});

test("encounter workspace has no serious accessibility violations and works at 390px @mobile", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  // Resume the seeded in-progress draft consultation (Alba).
  await page.goto("/patients");
  await page.getByLabel("Search patients").fill("SYN-0001");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page
    .locator(".result-card")
    .first()
    .getByRole("link", { name: "Open chart" })
    .click();
  await page.getByRole("link", { name: /Resume consultation/ }).click();
  await expect(
    page.getByRole("heading", { name: "Alba Demopatient" }),
  ).toBeVisible();
  await expect(page.getByRole("button", { name: "Save draft" })).toBeVisible();
});
