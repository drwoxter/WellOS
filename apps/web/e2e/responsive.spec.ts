import { expect, test, type Page } from "@playwright/test";
import { signInAs, signInAsRegistration, signOut } from "./helpers";

test("desktop shows the sidebar navigation", async ({ page }) => {
  await signInAs(page, "dr.garcia");
  await expect(page.locator(".sidebar")).toBeVisible();
  await expect(page.locator(".mobile-nav")).toBeHidden();
});

test("390px viewport shows the compact mobile navigation @mobile", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await expect(page.locator(".mobile-nav")).toBeVisible();
  await expect(page.locator(".sidebar")).toBeHidden();
  // Result cards replace the table on small screens.
  await page.goto("/results");
  await expect(page.locator(".mobile-only").first()).toBeVisible();
});

const STAFF_ROUTES = [
  "/dashboard",
  "/patients",
  "/results",
  "/risk",
  "/diagnostics",
  "/worklist",
];
const OPS_ROUTES = ["/scheduling", "/access"];
const PATIENT_ROUTES = ["/my", "/my/appointments", "/my/diagnostics"];

async function signInAsPatient(page: Page): Promise<void> {
  await page.goto("/");
  await page.getByRole("button", { name: /Sign in as rep\.alba/ }).click();
  await expect(page).toHaveURL(/\/my$/);
}

async function horizontalOverflow(page: Page): Promise<number> {
  return page.evaluate(
    () =>
      document.documentElement.scrollWidth -
      document.documentElement.clientWidth,
  );
}

for (const size of [
  { width: 1440, height: 900 },
  { width: 1024, height: 768 },
]) {
  test(`${size.width}×${size.height}: routes never scroll horizontally`, async ({
    page,
  }) => {
    await page.setViewportSize(size);
    await signInAs(page, "dr.garcia");
    for (const route of STAFF_ROUTES) {
      await page.goto(route);
      await expect(page.getByRole("main")).toBeVisible();
      expect(await horizontalOverflow(page), route).toBeLessThanOrEqual(0);
    }
    await signOut(page);
    await signInAsRegistration(page);
    for (const route of OPS_ROUTES) {
      await page.goto(route);
      await expect(page.getByRole("main")).toBeVisible();
      expect(await horizontalOverflow(page), route).toBeLessThanOrEqual(0);
    }
    await signOut(page);
    await signInAsPatient(page);
    for (const route of PATIENT_ROUTES) {
      await page.goto(route);
      await expect(page.getByRole("main")).toBeVisible();
      expect(await horizontalOverflow(page), route).toBeLessThanOrEqual(0);
    }
  });
}

test("390px: no horizontal scroll and 44px touch targets @mobile", async ({
  page,
}) => {
  await signInAsPatient(page);
  for (const route of PATIENT_ROUTES) {
    await page.goto(route);
    await expect(page.getByRole("main")).toBeVisible();
    expect(await horizontalOverflow(page), route).toBeLessThanOrEqual(0);
    const small = await page.evaluate(() => {
      const nodes = Array.from(
        document.querySelectorAll<HTMLElement>(
          '.mobile-nav a, .mobile-nav button, button.btn-primary, a.btn-primary, .btn[data-variant="primary"]',
        ),
      );
      return nodes
        .filter((el) => el.offsetParent !== null)
        .map((el) => el.getBoundingClientRect())
        .filter((r) => r.width > 0 && (r.width < 44 || r.height < 44))
        .map((r) => `${Math.round(r.width)}×${Math.round(r.height)}`);
    });
    expect(small, `${route} touch targets`).toEqual([]);
  }
  await signOut(page);
  await signInAs(page, "dr.garcia");
  for (const route of STAFF_ROUTES) {
    await page.goto(route);
    await expect(page.getByRole("main")).toBeVisible();
    expect(await horizontalOverflow(page), route).toBeLessThanOrEqual(0);
  }
});

test("reduced motion disables decorative animation", async ({ page }) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await signInAs(page, "dr.garcia");
  await expect(page).toHaveURL(/\/dashboard$/);
  const orb = page.locator(".dmind-orb").first();
  await expect(orb).toBeVisible();
  const animations = await orb.evaluate((el) => [
    getComputedStyle(el, "::before").animationName,
    getComputedStyle(el, "::after").animationName,
  ]);
  expect(animations.every((a) => a === "none")).toBe(true);
  const transitions = await page.evaluate(() =>
    Array.from(document.querySelectorAll<HTMLElement>(".priority-tile"))
      .slice(0, 3)
      .map((el) => getComputedStyle(el).transitionProperty),
  );
  expect(transitions.every((t) => t === "none" || t === "all")).toBe(true);
});
