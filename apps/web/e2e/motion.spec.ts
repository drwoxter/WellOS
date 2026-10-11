import { expect, test } from "@playwright/test";
import { signInAs } from "./helpers";

/**
 * Motion & depth layer: pressable 3D buttons, pointer-tracking tilt and page
 * choreography are decorative only — they must vanish under
 * prefers-reduced-motion and never change layout, focus or semantics.
 */

test("primary buttons are raised, lift on hover and sink on press", async ({
  page,
}) => {
  await page.goto("/");
  const card = page.getByRole("button", { name: /Sign in as dr\.garcia/ });
  await expect(card).toBeVisible();
  const resting = await card.evaluate((el) => getComputedStyle(el).boxShadow);
  expect(resting).not.toBe("none");
  await card.hover();
  await page.waitForTimeout(350);
  const hovered = await card.evaluate((el) => ({
    transform: getComputedStyle(el).transform,
    tilting: el.classList.contains("is-tilting"),
    rx: el.style.getPropertyValue("--rx"),
  }));
  expect(hovered.tilting).toBe(true);
  expect(hovered.rx).toMatch(/deg$/);
  expect(hovered.transform).not.toBe("none");
  const box = await card.boundingBox();
  expect(box).not.toBeNull();
  await page.mouse.move(box!.x + box!.width / 2, box!.y + box!.height / 2);
  await page.mouse.down();
  const pressed = await card.evaluate((el) => ({
    pressed: el.classList.contains("is-pressed"),
    px: el.style.getPropertyValue("--px"),
  }));
  expect(pressed.pressed).toBe(true);
  expect(pressed.px).toMatch(/px$/);
  await page.mouse.up();
  await expect(page).toHaveURL(/\/dashboard$/);
});

test("page content enters with choreography and no horizontal overflow", async ({
  page,
}) => {
  await signInAs(page, "dr.garcia");
  await expect(page).toHaveURL(/\/dashboard$/);
  const main = page.getByRole("main");
  await expect(main).toHaveClass(/page-enter/);
  const names = await main.evaluate((el) =>
    Array.from(el.children)
      .slice(0, 3)
      .map((c) => getComputedStyle(c).animationName),
  );
  expect(names.length).toBeGreaterThan(0);
  expect(names.every((n) => n === "rise")).toBe(true);
  await page.waitForTimeout(700);
  const overflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth -
      document.documentElement.clientWidth,
  );
  expect(overflow).toBeLessThanOrEqual(0);
  const primary = page
    .locator("button.primary:not(:disabled), a.button.primary")
    .first();
  if (await primary.count()) {
    await expect(primary).toBeVisible();
    const shadow = await primary.evaluate(
      (el) => getComputedStyle(el).boxShadow,
    );
    expect(
      (shadow.match(/(?:rgba?|color)\(/g) ?? []).length,
    ).toBeGreaterThanOrEqual(3);
  }
});

test("reduced motion removes tilt, ripple and page choreography", async ({
  page,
}) => {
  await page.emulateMedia({ reducedMotion: "reduce" });
  await page.goto("/");
  const card = page.getByRole("button", { name: /Sign in as dr\.garcia/ });
  await card.hover();
  await page.waitForTimeout(200);
  expect(await card.evaluate((el) => el.classList.contains("is-tilting"))).toBe(
    false,
  );
  expect(await card.evaluate((el) => getComputedStyle(el).transform)).toBe(
    "none",
  );
  await card.click();
  await expect(page).toHaveURL(/\/dashboard$/);
  const main = page.getByRole("main");
  const anims = await main.evaluate((el) => [
    getComputedStyle(el).animationName,
    ...Array.from(el.children)
      .slice(0, 3)
      .map((c) => getComputedStyle(c).animationName),
  ]);
  expect(anims.every((a) => a === "none")).toBe(true);
  const launcher = page.locator(".dmind-launcher");
  if (await launcher.count()) {
    expect(
      await launcher.evaluate((el) => getComputedStyle(el).animationName),
    ).toBe("none");
  }
  const hero = page.locator(".hero").first();
  if (await hero.count()) {
    const heroAnims = await hero.evaluate((el) => [
      getComputedStyle(el, "::before").animationName,
      getComputedStyle(el, "::after").animationName,
    ]);
    expect(heroAnims.every((a) => a === "none")).toBe(true);
  }
});
