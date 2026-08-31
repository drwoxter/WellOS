import { expect, test } from "@playwright/test";
import { signInAs } from "./helpers";

/**
 * Golden clinician path: development sign-in → dashboard → critical result →
 * review → patient notification → closure. Mutates the seeded demo data;
 * run `make seed` from the repository root to restore the demo states.
 */
test("clinician completes the closed-loop workflow on a critical result", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");

  // Dashboard → critical result in the priority list (≤ 3 interactions).
  const critical = page.locator(".result-card.critical").first();
  await expect(critical).toBeVisible();
  await critical.getByRole("link", { name: "Open result" }).click();

  await expect(
    page.getByText("Critical result — requires clinician review"),
  ).toBeVisible();
  await expect(
    page.getByRole("list", { name: "Workflow progress" }),
  ).toBeVisible();

  async function transition(buttonName: string) {
    await page.getByLabel("Workflow notes").fill(`E2E: ${buttonName}`);
    await page.getByRole("button", { name: buttonName, exact: true }).click();
    await expect(page.getByRole("dialog")).toBeVisible();
    await page.getByRole("button", { name: "Confirm" }).click();
    await expect(page.getByText("Action recorded.")).toBeVisible();
  }

  await transition("Mark reviewed");
  await transition("Record patient notification");
  await transition("Close loop");

  // The stepper ends on the closed state and no further action is offered.
  await expect(page.locator('.stepper li[aria-current="step"]')).toContainText(
    "Closed",
  );
  await expect(page.getByLabel("Workflow notes")).toHaveCount(0);
});
