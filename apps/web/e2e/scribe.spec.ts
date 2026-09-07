import { expect, test } from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";
import { signInAs } from "./helpers";

/**
 * Smart consultation cockpit and AI scribe journey. Chromium's fake media
 * device replaces the microphone, so the recording is synthetic and the
 * deterministic offline provider produces the same transcript on every run.
 * Mutates the seeded demo data (signs a consultation for Carlos); run
 * `make seed` from the repository root to restore the demo states.
 */
test.use({
  permissions: ["microphone"],
  launchOptions: {
    args: [
      "--use-fake-device-for-media-stream",
      "--use-fake-ui-for-media-stream",
    ],
  },
});

async function expectNoSeriousViolations(
  page: import("@playwright/test").Page,
) {
  const results = await new AxeBuilder({ page })
    .withTags(["wcag2a", "wcag2aa", "wcag21aa", "wcag22aa"])
    .analyze();
  const serious = results.violations.filter(
    (v) => v.impact === "serious" || v.impact === "critical",
  );
  expect(
    serious.map((v) => `${v.id}: ${v.nodes.map((n) => n.target).join(", ")}`),
  ).toEqual([]);
}

test("clinician records, reviews the scribe draft and signs from the cockpit", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");

  // Dashboard cockpit: prominent Start consultation + patient picker.
  const start = page.getByRole("region", { name: "Start consultation" });
  await expect(start).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Draft consultations" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Recent dMind activity" }),
  ).toBeVisible();
  await start.getByLabel("Choose a patient").fill("SYN-0003");
  await start.getByRole("button", { name: "Search", exact: true }).click();
  await expect(start.getByText("Carlos Demopatient")).toBeVisible();
  // Fresh seed: "Start consultation"; after an interrupted run the same
  // action resumes the open consultation instead of creating a second one.
  await start
    .getByRole("button", { name: /^(Start|Resume) consultation$/ })
    .click();
  await expect(page).toHaveURL(/\/encounters\//);
  await expect(
    page.getByRole("heading", { name: "Carlos Demopatient" }),
  ).toBeVisible();
  await expect(page.getByText("In progress").first()).toBeVisible();

  // Patient brief and diagnostic history with assistive trend commentary.
  await expect(
    page.getByRole("heading", { name: "Patient brief" }),
  ).toBeVisible();
  await expect(
    page.getByRole("heading", { name: "Diagnostic history" }),
  ).toBeVisible();
  const history = page.locator("details.diagnostic-history");
  if (!(await history.evaluate((el) => (el as HTMLDetailsElement).open))) {
    await history.locator("> summary").click();
  }
  const glucose = page.locator("li.diag-group", { hasText: /Glucose/ });
  await expect(glucose).toBeVisible();
  await expect(glucose.getByText(/mg\/dL/).first()).toBeVisible();
  await expect(page.locator(".diagnostic-history .diag-ai")).toBeVisible();

  // Recording: consent → record → pause → resume → finish → processing → draft.
  const dock = page.getByRole("region", { name: "Consultation recording" });
  await expect(dock).toBeVisible();
  await dock.getByRole("button", { name: "Record consultation" }).click();
  await expect(dock.getByText("Patient consent required")).toBeVisible();
  await dock
    .getByRole("button", { name: "Patient consented — start recording" })
    .click();
  await expect(dock.getByRole("button", { name: "Pause" })).toBeVisible();
  await page.waitForTimeout(1_200);
  await dock.getByRole("button", { name: "Pause" }).click();
  await expect(dock.getByRole("button", { name: "Resume" })).toBeVisible();
  await dock.getByRole("button", { name: "Resume" }).click();
  await page.waitForTimeout(600);
  await dock.getByRole("button", { name: "Finish" }).click();
  await expect(dock.getByText("Draft ready for review")).toBeVisible({
    timeout: 20_000,
  });

  // Structured review: transcript with timecodes, flags, sections.
  const review = page.getByRole("region", { name: "dMind scribe draft" });
  await expect(review).toBeVisible();
  await expect(
    review.getByText("Assistive draft — requires your review"),
  ).toBeVisible();
  await expect(
    review.getByRole("heading", { name: "Proposed note sections" }),
  ).toBeVisible();
  await expect(review.locator(".transcript-seg").first()).toBeVisible();
  await expect(review.locator(".timecode").first()).toHaveText(/^\d{2}:\d{2}/);
  await review
    .getByRole("button", { name: /Go to transcript segment/ })
    .first()
    .click();
  await expect(review.locator(".transcript-seg.highlighted")).toBeVisible();

  // Typed text is never overwritten: pre-fill Assessment, then Apply-all.
  await page
    .getByLabel(/Assessment/)
    .fill("Clinician assessment typed first (synthetic).");
  await review
    .getByRole("button", { name: /Insert all into empty sections/ })
    .click();
  await expect(
    review.getByText("Inserted into the note. Review and save when ready."),
  ).toBeVisible();
  await expect(page.getByLabel(/Assessment/)).toHaveValue(
    "Clinician assessment typed first (synthetic).",
  );
  await expect(page.getByLabel(/Reason for consultation/)).not.toHaveValue("");

  // Explicit append below clinician text for the non-empty section.
  const assessmentRow = review.locator(".scribe-section", {
    hasText: "Assessment",
  });
  await assessmentRow
    .getByRole("button", { name: "Append below my text" })
    .click();
  await expect(page.getByLabel(/Assessment/)).toHaveValue(
    /^Clinician assessment typed first \(synthetic\)\.\n\n.+/,
  );

  // Optional sections that received scribe text are expanded so the
  // inserted content is visible; clinician text then extends it.
  const followUp = page.getByLabel(/Follow-up instructions/);
  await expect(followUp).toBeVisible();
  await expect(followUp).not.toHaveValue("");
  await followUp.fill("Return in two weeks (synthetic).");
  await page.getByRole("button", { name: "Save draft" }).click();
  await expect(page.getByText("Draft saved.")).toBeVisible();
  await page.getByRole("button", { name: "Sign and complete" }).click();
  await expect(page.getByText(/signed note is permanent/i)).toBeVisible();
  await page.getByRole("button", { name: "Confirm", exact: true }).click();
  await expect(page.getByText("Clinical summary")).toBeVisible();
  await expect(page.getByText("Signed").first()).toBeVisible();
  await expect(page.getByLabel(/Reason for consultation/)).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Record consultation" }),
  ).toHaveCount(0);
});

test("recording controls are keyboard operable and the workspace passes WCAG checks", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await page.goto("/patients");
  await page.getByLabel("Search patients").fill("SYN-0001");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page
    .locator(".result-card")
    .first()
    .getByRole("link", { name: "Open chart" })
    .click();
  await page.getByRole("link", { name: /Resume consultation/ }).click();
  const dock = page.getByRole("region", { name: "Consultation recording" });
  await expect(dock).toBeVisible();

  const record = dock.getByRole("button", { name: "Record consultation" });
  await record.focus();
  await expect(record).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(dock.getByText("Patient consent required")).toBeVisible();
  await page.keyboard.press("Tab");
  await expect(
    dock.getByRole("button", { name: "Patient consented — start recording" }),
  ).toBeFocused();
  await page.keyboard.press("Tab");
  await expect(dock.getByRole("button", { name: "Not now" })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(record).toBeVisible();
  await expectNoSeriousViolations(page);
});

test("dashboard cockpit customization is keyboard operable and stores layout only", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  const customize = page.getByRole("button", { name: "Customize dashboard" });
  await customize.focus();
  await page.keyboard.press("Enter");
  await expect(
    page.getByRole("button", { name: "Hide: Draft consultations" }),
  ).toBeVisible();
  await page.getByRole("button", { name: "Hide: Draft consultations" }).click();
  await expect(
    page.getByRole("heading", { name: "Draft consultations" }),
  ).toHaveCount(0);
  await page.getByRole("radio", { name: "Compact" }).check();
  const stored = await page.evaluate(() =>
    window.localStorage.getItem("wellos.cockpit.v1"),
  );
  expect(stored).not.toBeNull();
  expect(stored).not.toMatch(/Demopatient|SYN-/);
  expect(JSON.parse(stored ?? "{}")).toMatchObject({
    hidden: ["drafts"],
    density: "compact",
  });
  await page.getByRole("button", { name: "Restore role defaults" }).click();
  await expect(
    page.getByRole("heading", { name: "Draft consultations" }),
  ).toBeVisible();
  await expectNoSeriousViolations(page);
});

test("390px viewport keeps recording controls visible and passes WCAG checks @mobile", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await expect(
    page.getByRole("region", { name: "Start consultation" }),
  ).toBeVisible();
  await page.goto("/patients");
  await page.getByLabel("Search patients").fill("SYN-0001");
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page
    .locator(".result-card")
    .first()
    .getByRole("link", { name: "Open chart" })
    .click();
  await page.getByRole("link", { name: /Resume consultation/ }).click();
  const dock = page.getByRole("region", { name: "Consultation recording" });
  await expect(dock).toBeVisible();
  await expect(
    dock.getByRole("button", { name: "Record consultation" }),
  ).toBeVisible();
  // No horizontal overflow at 390px.
  const overflow = await page.evaluate(
    () => document.documentElement.scrollWidth > window.innerWidth + 1,
  );
  expect(overflow).toBe(false);
  await page
    .getByRole("button", { name: "Save draft" })
    .scrollIntoViewIfNeeded();
  await expect(dock).toBeInViewport();
  await expectNoSeriousViolations(page);
});
