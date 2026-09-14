import AxeBuilder from "@axe-core/playwright";
import { expect, test, type Locator, type Page } from "@playwright/test";
import { signInAs } from "./helpers";

/**
 * Patient 360 + explainable risk golden path against the seeded synthetic
 * `Riskdemo` scenarios (SYN-0101..SYN-0106): risk worklist → filters → technical
 * evidence → Patient 360 → dMind summary with explicit confirmation → start
 * consultation → cockpit Patient 360 summary → risk refresh after a confirmed
 * change. Acknowledge/review actions and the confirmed follow-up task mutate
 * the demo data, so reseed (`make seed`) between runs.
 */

/** Worklist card for one synthetic patient (card heading = patient name). */
function card(page: Page, name: string): Locator {
  return page.locator("ul.risk-worklist > li").filter({
    has: page.getByRole("heading", { level: 3, name }),
  });
}

async function expectNoSeriousViolations(page: Page) {
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

test.describe.configure({ mode: "serial" });

test("risk worklist: severity order, explanations, filters, evidence and review actions", async ({
  page,
}) => {
  test.setTimeout(120_000);
  await signInAs(page, "dr.garcia");
  await page.getByRole("link", { name: "Risk" }).first().click();
  await expect(page).toHaveURL(/\/risk$/);

  // Critical and high first; low risk hidden by default; insufficient data is
  // shown as such rather than as low risk.
  const items = page.locator("ul.risk-worklist > li");
  await expect(items.first()).toBeVisible();
  const levels = await items.evaluateAll((els) =>
    els.map((e) => e.getAttribute("data-level")),
  );
  expect(levels.slice(0, 2)).toEqual(["critical", "critical"]);
  expect(levels).not.toContain("low");
  expect(levels[levels.length - 1]).toBe("insufficient_data");

  const teresa = card(page, "Teresa Riskdemo");
  await expect(teresa).toContainText("SYN-0103");
  await expect(teresa).toContainText("Why:");
  await expect(teresa).toContainText(
    "A critical result has not been reviewed yet",
  );
  // Level is never colour alone: the badge carries a glyph and a label.
  await expect(teresa.locator(".risk-level").first()).toContainText(/Critical/);

  // Technical evidence is collapsed by default and links to source records.
  const evidence = teresa.locator("details.risk-technical").first();
  await expect(evidence).not.toHaveAttribute("open", "");
  await evidence.locator("> summary").click();
  await expect(evidence).toHaveAttribute("open", "");
  await expect(evidence).toContainText("risk-rules.v1");
  const diagnostic = evidence.locator('li[data-domain="diagnostic_result"]');
  await expect(diagnostic).toContainText("critical_result_unreviewed");
  await diagnostic.locator("details.risk-technical > summary").click();
  await expect(
    diagnostic.getByRole("link", { name: /Potassium/ }).first(),
  ).toBeVisible();

  // Insufficient data is explained, never presented as low risk.
  const ivan = card(page, "Iván Riskdemo");
  await expect(ivan).toHaveAttribute("data-level", "insufficient_data");
  await expect(ivan).toContainText("Insufficient data");
  await expect(ivan).toContainText(
    "the record is too sparse to assess. Review the patient directly.",
  );

  // Filters are applied server-side; reset restores the full list.
  await page
    .getByRole("combobox", { name: "Trend", exact: true })
    .selectOption("worsening");
  await expect(card(page, "Ramón Riskdemo")).toBeVisible();
  await expect(teresa).toHaveCount(0);
  await page
    .getByRole("combobox", { name: "Domain", exact: true })
    .selectOption("chronic_complexity");
  await expect(card(page, "Ramón Riskdemo")).toContainText(
    "Chronic complexity",
  );
  await page.getByRole("button", { name: "Reset filters" }).click();
  await expect(teresa).toBeVisible();
  await page.getByLabel("Include low risk").check();
  await expect(card(page, "Lucía Riskdemo")).toHaveAttribute(
    "data-level",
    "low",
  );
  await page.getByLabel("Include low risk").uncheck();

  // Acknowledge Nora (SYN-0105): audit-recorded review state, reviewer shown.
  const nora = card(page, "Nora Riskdemo");
  await nora.getByLabel("Review note (optional)").fill("Seen on rounds.");
  await nora.getByRole("button", { name: "Acknowledge" }).click();
  await expect(nora.getByText("Risk acknowledged.")).toBeVisible();
  await expect(nora.getByText("Acknowledged").first()).toBeVisible();
  await expect(nora.getByRole("button", { name: "Acknowledge" })).toHaveCount(
    0,
  );

  // Assign follow-up requires choosing a professional before confirming.
  await nora.getByRole("button", { name: "Assign follow-up" }).click();
  const confirm = nora.getByRole("button", { name: "Confirm assignment" });
  await expect(confirm).toBeDisabled();
  await nora.getByLabel("Assign to").selectOption({ label: "Nurse Ana Kim" });
  await confirm.click();
  await expect(nora.getByText("Follow-up assigned.")).toBeVisible();
  await expect(nora).toContainText("Nurse Ana Kim");

  // Review status filter reflects the new state.
  await page
    .getByRole("combobox", { name: "Review status", exact: true })
    .selectOption("acknowledged");
  await expect(nora).toBeVisible();
  await expect(teresa).toHaveCount(0);
  await page.getByRole("button", { name: "Reset filters" }).click();

  // Mark as reviewed on Hugo (SYN-0104, medication/allergy conflict).
  const hugo = card(page, "Hugo Riskdemo");
  await expect(hugo).toContainText("Medication and allergy safety");
  await hugo.getByRole("button", { name: "Mark as reviewed" }).click();
  await expect(hugo.getByText("Risk marked as reviewed.")).toBeVisible();
  await expect(hugo.getByText("Reviewed").first()).toBeVisible();
});

test("Patient 360 → dMind summary with explicit confirmation → consultation cockpit", async ({
  page,
}) => {
  test.setTimeout(180_000);
  await signInAs(page, "dr.garcia");
  await page.goto("/risk");
  const teresa = card(page, "Teresa Riskdemo");
  await teresa.getByRole("link", { name: "Open Patient 360" }).click();
  await expect(page).toHaveURL(/\/patients\/[^/]+\/360$/);

  // Everything important is visible without tabs.
  await expect(
    page.getByRole("heading", { level: 1, name: "Teresa Riskdemo" }),
  ).toBeVisible();
  await expect(page.getByText("SYN-0103").first()).toBeVisible();
  for (const name of [
    "Care team",
    "Unresolved safety issues",
    "Conditions and history",
    "Allergies and medication safety",
    "Recent encounters and notes",
    "Pending tasks, tests and follow-ups",
    "Diagnostic-result trends",
    "Preventive-care gaps",
    "Current risk",
  ]) {
    await expect(
      page.getByRole("region", { name: new RegExp(`^${name}`) }),
    ).toBeVisible();
  }
  const risk = page.getByRole("region", { name: /^Current risk/ });
  await expect(risk).toContainText("Critical");
  await expect(risk).toContainText("Diagnostic results");
  await expect(risk).toContainText("risk-rules.v1");
  await expect(
    page.getByRole("region", { name: /^Diagnostic-result trends/ }),
  ).toContainText("Potassium");
  await expect(page.getByText("Risk evolution").first()).toBeVisible();

  // dMind summary: clearly labelled, cites records, needs confirmation.
  const dmind = page.getByRole("region", { name: /dMind risk summary/ });
  await dmind.getByRole("button", { name: "Generate dMind summary" }).click();
  await expect(
    dmind.getByText("dMind summary generated — awaiting your review."),
  ).toBeVisible();
  await expect(dmind.getByText("AI-generated").first()).toBeVisible();
  await expect(dmind).toContainText("risk-summary.v1");
  await expect(dmind).toContainText("Cited sources");
  await expect(dmind).toContainText("Suggested follow-up");
  await expect(dmind.getByText("Requires confirmation").first()).toBeVisible();

  // Nothing is created until the clinician confirms explicitly.
  const pending = page.getByRole("region", {
    name: /^Pending tasks, tests and follow-ups/,
  });
  const before = (await pending.textContent()) ?? "";
  // Suggestions cannot become work while the summary awaits review.
  await expect(dmind).toContainText("Awaiting professional review");
  await expect(
    dmind.getByRole("button", { name: "Create follow-up task…" }),
  ).toHaveCount(0);
  await dmind.getByRole("button", { name: "Approve summary" }).click();
  await expect(dmind.getByText("Summary approved.")).toBeVisible();
  expect((await pending.textContent()) ?? "").toBe(before);
  await dmind
    .getByRole("button", { name: "Create follow-up task…" })
    .first()
    .click();
  await expect(
    dmind.getByRole("button", { name: "Confirm and create task" }),
  ).toBeVisible();
  await dmind.getByRole("button", { name: "Cancel" }).first().click();
  await expect(
    dmind.getByRole("button", { name: "Confirm and create task" }),
  ).toHaveCount(0);
  expect((await pending.textContent()) ?? "").toBe(before);

  await dmind
    .getByRole("button", { name: "Create follow-up task…" })
    .first()
    .click();
  await dmind.getByRole("button", { name: "Confirm and create task" }).click();
  await expect(
    dmind.getByText("Follow-up task created.").first(),
  ).toBeVisible();
  await expect(pending).toContainText("Address the overdue task or open alert");
  await expect(pending).toContainText("AI-generated");

  // Direct action: start the consultation from Patient 360.
  await page.getByRole("button", { name: "Start consultation" }).click();
  await expect(page).toHaveURL(/\/encounters\//);
  await expect(
    page.getByRole("heading", { name: "Teresa Riskdemo" }),
  ).toBeVisible();

  // Cockpit shows the Patient 360 summary before documentation starts.
  const cockpit = page.getByRole("region", {
    name: "Patient 360 before you start",
  });
  await expect(cockpit).toBeVisible();
  await expect(cockpit).toContainText("Critical");
  await expect(cockpit).toContainText("Diagnostic results");
  await expect(
    cockpit.getByRole("link", { name: "Open Patient 360" }),
  ).toHaveAttribute("href", /\/patients\/[^/]+\/360$/);
  // Evidence can be opened from the cockpit.
  await cockpit
    .locator(
      'li[data-domain="diagnostic_result"] details.risk-technical > summary',
    )
    .click();
  await expect(
    cockpit.getByRole("link", { name: /Potassium/ }).first(),
  ).toBeVisible();

  // Confirmed clinical change (vital signs) refreshes the risk read without
  // interrupting the consultation.
  await page.getByRole("button", { name: "Record vital signs" }).click();
  await page.getByLabel(/Systolic/).fill("182");
  await page.getByLabel(/Diastolic/).fill("104");
  await page.getByLabel(/Heart rate/).fill("96");
  await page
    .locator('button[type="submit"]', { hasText: "Record vital signs" })
    .click();
  await expect(page.getByText("Vital signs recorded.")).toBeVisible();
  await expect(
    cockpit.getByText("Risk updated after the last confirmed change."),
  ).toBeVisible();
  await expect(cockpit).toContainText("Acute safety");
  await expect(page.getByRole("button", { name: /Save draft/ })).toBeVisible();
});

test("risk worklist is keyboard navigable with visible focus", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await page.goto("/risk");
  const teresa = card(page, "Teresa Riskdemo");
  await expect(teresa).toBeVisible();
  const outer = teresa.locator("details.risk-technical").first();
  const summary = outer.locator("> summary");
  await summary.focus();
  await expect(summary).toBeFocused();
  const outline = await summary.evaluate(
    (el) => getComputedStyle(el).outlineStyle,
  );
  expect(outline).not.toBe("none");
  await page.keyboard.press("Enter");
  await expect(outer).toHaveAttribute("open", "");
  // Tab reaches the first domain's evidence disclosure; Enter opens it and the
  // evidence link becomes reachable by keyboard.
  await page.keyboard.press("Tab");
  const domainSummary = outer
    .locator("li.risk-domain details.risk-technical > summary")
    .first();
  await expect(domainSummary).toBeFocused();
  await page.keyboard.press("Enter");
  await page.keyboard.press("Tab");
  const focusedTag = await page.evaluate(
    () => document.activeElement?.tagName ?? "",
  );
  expect(["A", "SUMMARY"]).toContain(focusedTag);
});

test("risk pages have no serious accessibility violations", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await page.goto("/risk");
  await expect(card(page, "Teresa Riskdemo")).toBeVisible();
  await expectNoSeriousViolations(page);
  await card(page, "Teresa Riskdemo")
    .getByRole("link", { name: "Open Patient 360" })
    .click();
  await expect(
    page.getByRole("region", { name: /^Current risk/ }),
  ).toBeVisible();
  await expectNoSeriousViolations(page);
});

test("roles without risk permission do not see the worklist", async ({
  page,
}) => {
  await page.goto("/");
  await page.getByRole("button", { name: /Sign in as reg\.rivera/ }).click();
  await expect(page).toHaveURL(/\/access/);
  await expect(page.getByRole("link", { name: "Risk" })).toHaveCount(0);
  await page.goto("/risk");
  await expect(page.locator("p[role='alert']")).toContainText(
    "You do not have permission to view this information.",
  );
  await expect(page.locator("ul.risk-worklist")).toHaveCount(0);
});

test("risk worklist and Patient 360 in Spanish", async ({ page }) => {
  await signInAs(page, "dr.garcia");
  await page.getByLabel("Language").first().selectOption("es");
  await page.goto("/risk");
  await expect(
    page.getByRole("heading", { name: "Lista de trabajo de riesgo" }).first(),
  ).toBeVisible();
  const teresa = card(page, "Teresa Riskdemo");
  await expect(teresa).toContainText("Crítico");
  await expect(teresa).toContainText("Motivo:");
  await expect(teresa.getByText("Evidencia técnica").first()).toBeVisible();
  await teresa.getByRole("link", { name: "Abrir Paciente 360" }).click();
  await expect(
    page.getByRole("region", { name: /^Riesgo actual/ }),
  ).toBeVisible();
  await expect(
    page.getByRole("region", {
      name: /^Alergias y seguridad de la medicación/,
    }),
  ).toBeVisible();
  await expect(page.getByText("Evolución del riesgo").first()).toBeVisible();
});

test("390px: worklist cards stack and Patient 360 is single-column @mobile", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await page.goto("/risk");
  const teresa = card(page, "Teresa Riskdemo");
  await expect(teresa).toBeVisible();
  const width = await teresa.evaluate((el) => el.getBoundingClientRect().width);
  expect(width).toBeLessThanOrEqual(390);
  await expect(page.locator(".mobile-nav")).toBeVisible();
  await teresa.getByRole("link", { name: "Open Patient 360" }).click();
  const grid = page.locator(".p360-grid");
  await expect(grid).toBeVisible();
  const columns = await grid.evaluate(
    (el) => getComputedStyle(el).gridTemplateColumns.split(" ").length,
  );
  expect(columns).toBe(1);
  await expect(
    page.getByText(/^(Start|Resume) consultation$/).first(),
  ).toBeVisible();
});
