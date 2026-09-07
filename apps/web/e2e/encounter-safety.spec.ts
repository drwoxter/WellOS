import type { Page } from "@playwright/test";
import { expect, test } from "@playwright/test";
import { signInAs } from "./helpers";

async function openChart(page: Page, identifier: string): Promise<void> {
  await page.goto("/patients");
  await page.getByLabel("Search patients").fill(identifier);
  await page.getByRole("button", { name: "Search", exact: true }).click();
  await page
    .locator(".result-card")
    .first()
    .getByRole("link", { name: "Open chart" })
    .click();
  await expect(page).toHaveURL(/\/patients\//);
}

/** Hold every matching request for `ms` before letting it through. */
async function delayRequests(
  page: Page,
  pattern: RegExp,
  ms: number,
): Promise<void> {
  await page.route(pattern, async (route) => {
    await new Promise((r) => setTimeout(r, ms));
    await route.continue();
  });
}

test("edits typed during a delayed save stay unsaved and a delayed sign records exactly the visible draft", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  // A fresh consultation for Marta so the seeded demo states stay intact.
  await openChart(page, "SYN-0004");
  await page.getByRole("button", { name: "Start consultation" }).click();
  await expect(page).toHaveURL(/\/encounters\//);
  await expect(
    page.getByRole("heading", { name: "Marta Demopatient" }),
  ).toBeVisible();

  await delayRequests(page, /\/api\/v1\/encounters\/[^/]+\/note$/, 1500);

  const reason = page.getByLabel(/Reason for consultation/);
  await reason.fill("Headache for three days (synthetic).");
  await page.getByRole("button", { name: "Save draft" }).click();
  await expect(page.getByRole("button", { name: "Saving…" })).toBeDisabled();

  // Keep typing while the save is in flight.
  await reason.fill(
    "Headache for three days, worse in the morning (synthetic).",
  );
  await expect(
    page.getByText("Draft saved. Edits made while saving are not saved yet."),
  ).toBeVisible();
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  await expect(reason).toHaveValue(
    "Headache for three days, worse in the morning (synthetic).",
  );

  // Sign: the pending save-then-sign freezes the inputs so nothing typed now
  // can fall between the saved snapshot and the signed record.
  const assessment = page.getByLabel(/Assessment/);
  await assessment.fill("Tension-type headache (synthetic).");
  await page.getByRole("button", { name: "Sign and complete" }).click();
  await page.getByRole("button", { name: "Confirm", exact: true }).click();
  await expect(page.getByText("Signing…").first()).toBeVisible();
  await expect(assessment).toHaveAttribute("readonly", "");
  await assessment.pressSequentially(" ignored");
  await expect(assessment).toHaveValue("Tension-type headache (synthetic).");

  await expect(page.getByText("Clinical summary")).toBeVisible();
  await expect(
    page.getByText(
      "Headache for three days, worse in the morning (synthetic).",
    ),
  ).toBeVisible();
  await expect(
    page.getByText("Tension-type headache (synthetic).", { exact: true }),
  ).toBeVisible();
  await expect(page.getByText("ignored")).toHaveCount(0);
});

test("browser Back is confirmed while a note has unsaved edits", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await openChart(page, "SYN-0001");
  const chartUrl = page.url();
  await page.getByRole("link", { name: /Resume consultation/ }).click();
  await expect(page).toHaveURL(/\/encounters\//);
  const encounterUrl = page.url();

  const history = page.getByLabel(/History of presenting complaint/);
  const before = await history.inputValue();
  const typed = `${before} Unsaved browser edit (synthetic).`;
  await history.fill(typed);
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // Decline: URL, text and dirty state are untouched.
  const declined = page.waitForEvent("dialog");
  await page.goBack({ waitUntil: "commit" });
  const dialog = await declined;
  expect(dialog.message()).toMatch(/unsaved documentation/i);
  await dialog.dismiss();
  await expect(page).toHaveURL(encounterUrl);
  await expect(history).toHaveValue(typed);
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // Accept: navigation proceeds to the patient chart, one dialog only.
  let dialogs = 0;
  page.on("dialog", (d) => {
    dialogs += 1;
    void d.accept();
  });
  await page.goBack({ waitUntil: "commit" });
  await expect(page).toHaveURL(chartUrl);
  expect(dialogs).toBe(1);
});

test("an internal link is confirmed while a note has unsaved edits and proceeds when accepted", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await openChart(page, "SYN-0001");
  await page.getByRole("link", { name: /Resume consultation/ }).click();
  await expect(page).toHaveURL(/\/encounters\//);
  const encounterUrl = page.url();

  const history = page.getByLabel(/History of presenting complaint/);
  const typed = `${await history.inputValue()} Unsaved before a link (synthetic).`;
  await history.fill(typed);
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  const patientsLink = page.getByRole("link", { name: "Patients" }).first();

  // Decline: same URL, same text, still dirty.
  page.once("dialog", (d) => void d.dismiss());
  await patientsLink.click();
  await expect(page).toHaveURL(encounterUrl);
  await expect(history).toHaveValue(typed);
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // Accept: the link's destination is reached, with one dialog.
  let dialogs = 0;
  page.on("dialog", (d) => {
    dialogs += 1;
    void d.accept();
  });
  await patientsLink.click();
  await expect(page).toHaveURL(/\/patients$/);
  await expect(page.getByLabel("Search patients")).toBeVisible();
  expect(dialogs).toBe(1);
  // Back returns to the encounter directly: no duplicate entry was left.
  await page.goBack({ waitUntil: "commit" });
  await expect(page).toHaveURL(encounterUrl);
});

test("sign-out is confirmed before the session is revoked while a note has unsaved edits", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await openChart(page, "SYN-0001");
  await page.getByRole("link", { name: /Resume consultation/ }).click();
  await expect(page).toHaveURL(/\/encounters\//);
  const encounterUrl = page.url();

  const history = page.getByLabel(/History of presenting complaint/);
  const typed = `${await history.inputValue()} Unsaved before sign-out (synthetic).`;
  await history.fill(typed);
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  let revocations = 0;
  page.on("request", (req) => {
    if (req.method() === "DELETE" && req.url().endsWith("/api/session"))
      revocations += 1;
  });
  const signOut = page
    .locator(".topbar")
    .getByRole("button", { name: "Sign out" });

  // Decline: still signed in, on the same screen, with the same text.
  const messages: string[] = [];
  page.once("dialog", (d) => {
    messages.push(d.message());
    void d.dismiss();
  });
  await signOut.click();
  expect(messages).toHaveLength(1);
  expect(messages[0]).toMatch(/unsaved documentation/i);
  await expect(page).toHaveURL(encounterUrl);
  await expect(history).toHaveValue(typed);
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  expect(revocations).toBe(0);
  expect(await (await page.request.get("/api/session")).json()).toEqual({
    authenticated: true,
  });

  // Accept: one dialog, then signed out.
  let dialogs = 0;
  page.on("dialog", (d) => {
    dialogs += 1;
    void d.accept();
  });
  await signOut.click();
  await expect(page).toHaveURL(/\/$/);
  await expect(page.getByRole("button", { name: /Sign out/ })).toHaveCount(0);
  expect(revocations).toBe(1);
  expect(dialogs).toBe(1);
});

test("browser Forward that stays on the screen never asks and leaves the guard armed", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await openChart(page, "SYN-0001");
  const chartUrl = page.url();
  await page.getByRole("link", { name: /Resume consultation/ }).click();
  await expect(page).toHaveURL(/\/encounters\//);
  const encounterUrl = page.url();

  const history = page.getByLabel(/History of presenting complaint/);
  const before = await history.inputValue();
  const typed = `${before} Unsaved forward edit (synthetic).`;
  await history.fill(typed);
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  let dialogs = 0;
  page.on("dialog", (d) => {
    dialogs += 1;
    void d.accept();
  });

  // A same-URL entry ahead of the guard's duplicate (what a guard re-armed
  // before its predecessor's cleanup settled leaves behind), then Back and
  // Forward across it: the screen never changes, so nothing is asked.
  await page.evaluate(() =>
    window.history.pushState(window.history.state, "", window.location.href),
  );
  await page.goBack({ waitUntil: "commit" });
  await page.goForward({ waitUntil: "commit" });
  await expect(page).toHaveURL(encounterUrl);
  await expect(history).toHaveValue(typed);
  await expect(page.getByText("Unsaved changes")).toBeVisible();
  expect(dialogs).toBe(0);

  // The guard did not stand down: leaving backward still asks, exactly once,
  // and the accepted Back continues to the patient chart.
  await page.goBack({ waitUntil: "commit" });
  await expect(page).toHaveURL(encounterUrl);
  expect(dialogs).toBe(0);
  await page.goBack({ waitUntil: "commit" });
  await expect(page).toHaveURL(chartUrl);
  expect(dialogs).toBe(1);
});

test("a declined multi-entry history jump returns to the encounter with the draft intact", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await openChart(page, "SYN-0001");
  await page.getByRole("link", { name: /Resume consultation/ }).click();
  await expect(page).toHaveURL(/\/encounters\//);
  const encounterUrl = page.url();

  const history = page.getByLabel(/History of presenting complaint/);
  const before = await history.inputValue();
  const typed = `${before} Unsaved jump edit (synthetic).`;
  await history.fill(typed);
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // Jump three entries back at once (history menu): over the guard's
  // duplicate entry and the patient chart, straight to the directory.
  const declined = page.waitForEvent("dialog");
  await page.evaluate(() => window.history.go(-3));
  const dialog = await declined;
  expect(dialog.message()).toMatch(/unsaved documentation/i);
  await dialog.dismiss();
  await expect(page).toHaveURL(encounterUrl);
  await expect(history).toHaveValue(typed);
  await expect(page.getByText("Unsaved changes")).toBeVisible();

  // Still guarded: a plain Back asks again and, declined, stays put.
  const declinedAgain = page.waitForEvent("dialog");
  await page.goBack({ waitUntil: "commit" });
  await (await declinedAgain).dismiss();
  await expect(page).toHaveURL(encounterUrl);
  await expect(history).toHaveValue(typed);

  // Accepting the same jump lands on the directory with one dialog.
  let dialogs = 0;
  page.on("dialog", (d) => {
    dialogs += 1;
    void d.accept();
  });
  await page.evaluate(() => window.history.go(-3));
  await expect(page).toHaveURL(/\/patients$/);
  expect(dialogs).toBe(1);
});

test("historical order-only encounters are laboratory contexts, not resumable consultations", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  // Carlos only has seeded result-loop (order-only) encounters.
  await openChart(page, "SYN-0003");
  await expect(
    page.getByRole("button", { name: "Start consultation" }),
  ).toBeVisible();
  await expect(
    page.getByRole("link", { name: /Resume consultation/ }),
  ).toHaveCount(0);
  await expect(
    page
      .getByRole("link", { name: /Laboratory orders: Dr\. Gabriel García/ })
      .first(),
  ).toBeVisible();

  await page.getByRole("tab", { name: /Encounters/ }).click();
  await expect(
    page.getByRole("link", { name: /Resume consultation/ }),
  ).toHaveCount(0);
  await page.getByRole("link", { name: "Open orders" }).first().click();
  await expect(page).toHaveURL(/\/encounters\//);
  await expect(page.getByText(/only holds laboratory orders/i)).toBeVisible();
  await expect(page.getByLabel(/Reason for consultation/)).toHaveCount(0);
  await expect(
    page.getByRole("button", { name: "Sign and complete" }),
  ).toHaveCount(0);
  await expect(page.getByText(/Potassium/).first()).toBeVisible();
});
