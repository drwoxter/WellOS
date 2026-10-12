import {
  expect,
  request,
  test,
  type APIResponse,
  type Page,
} from "@playwright/test";
import AxeBuilder from "@axe-core/playwright";

const API = process.env.WELLOS_API_URL ?? "http://127.0.0.1:8080";

/** Dev sign-in through the BFF for operational roles without a sign-in card. */
async function signInViaBff(page: Page, username: string): Promise<void> {
  await page.goto("/");
  await page.evaluate(async (token) => {
    const res = await fetch("/api/session", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ token }),
    });
    if (!res.ok) throw new Error(`sign-in failed: ${res.status}`);
  }, `dev-${username}`);
}

/** Language switch tolerant of the stored per-user language (EN or ES labels). */
async function setLanguage(page: Page, lang: "en" | "es"): Promise<void> {
  const button = page
    .getByRole("button", { name: /Account menu|Menú de la cuenta/ })
    .first();
  if ((await button.getAttribute("aria-expanded")) !== "true") {
    await button.click();
  }
  const panel = page.getByRole("dialog", { name: /^(Account|Cuenta)$/ });
  await expect(panel).toBeVisible();
  await panel.getByLabel(/Language|Idioma/).selectOption(lang);
  await page.keyboard.press("Escape");
}

async function signOutViaBff(page: Page): Promise<void> {
  await page.evaluate(() => fetch("/api/session", { method: "DELETE" }));
}

async function ownPatientId(username: string): Promise<string> {
  const ctx = await request.newContext({
    baseURL: API,
    extraHTTPHeaders: { Authorization: `Bearer dev-${username}` },
  });
  const me = await (await ctx.get("/api/v1/me")).json();
  await ctx.dispose();
  return me.patients[0].patient_id as string;
}

/** Release leftovers and cancel any open episode left by a previous run so the
 *  human admission decision is repeatable against the same synthetic patient. */
async function cancelOpenEpisodes(
  username: string,
  patientId: string,
): Promise<void> {
  const ctx = await request.newContext({
    baseURL: API,
    extraHTTPHeaders: { Authorization: `Bearer dev-${username}` },
  });
  const must = async (res: APIResponse, what: string) => {
    if (!res.ok())
      throw new Error(`${what} failed: ${res.status()} ${await res.text()}`);
  };
  const body = await (
    await ctx.get(
      `/api/v1/operations/episodes?patient_id=${patientId}&open_only=true`,
    )
  ).json();
  const rows: { id: string }[] = body.episodes ?? [];
  const touched = new Set<string>();
  for (const ep of rows) {
    const detail = await (
      await ctx.get(`/api/v1/operations/episodes/${ep.id}`)
    ).json();
    for (const r of detail.placement_requests as {
      id: string;
      status: string;
      version: number;
    }[]) {
      if (r.status === "held") {
        await must(
          await ctx.post(
            `/api/v1/operations/placement-requests/${r.id}/release-hold`,
            {
              data: { expected_version: r.version, reason_code: "e2e_cleanup" },
            },
          ),
          "release-hold",
        );
      }
    }
    for (const p of detail.placements as {
      id: string;
      status: string;
      version: number;
      location_id: string;
    }[]) {
      touched.add(p.location_id);
      if (p.status === "occupied") {
        await must(
          await ctx.post(`/api/v1/operations/placements/${p.id}/release`, {
            data: { expected_version: p.version, reason_code: "e2e_cleanup" },
          }),
          "release",
        );
      }
    }
    const fresh = await (
      await ctx.get(`/api/v1/operations/episodes/${ep.id}`)
    ).json();
    await must(
      await ctx.post(`/api/v1/operations/episodes/${ep.id}/transition`, {
        data: {
          transition: "cancel",
          expected_version: fresh.episode.version,
          reason_code: "e2e_cleanup",
          note: "Synthetic episode left by a previous test run",
        },
      }),
      "cancel",
    );
  }
  // Positions released by previous runs stay in cleaning until the turnaround
  // job completes; stand in for that human/job step so capacity is repeatable.
  const closed = await (
    await ctx.get(`/api/v1/operations/episodes?patient_id=${patientId}`)
  ).json();
  for (const ep of (closed.episodes ?? []) as { id: string }[]) {
    const detail = await (
      await ctx.get(`/api/v1/operations/episodes/${ep.id}`)
    ).json();
    for (const p of detail.placements as { location_id: string }[])
      touched.add(p.location_id);
  }
  if (touched.size > 0) {
    const facilityId = (
      await (
        await ctx.get(
          `/api/v1/operations/episodes/${[...rows, ...(closed.episodes ?? [])][0].id}`,
        )
      ).json()
    ).episode.facility_id as string;
    const locBody = await (
      await ctx.get(
        `/api/v1/operations/locations?facility_id=${facilityId}&level=position`,
      )
    ).json();
    const locations = (
      Array.isArray(locBody) ? locBody : (locBody.locations ?? [])
    ) as { id: string; position_state: string | null; version: number }[];
    for (const loc of locations) {
      if (touched.has(loc.id) && loc.position_state === "cleaning") {
        await must(
          await ctx.post(`/api/v1/operations/locations/${loc.id}/state`, {
            data: {
              expected_version: loc.version,
              state: "available",
              reason_code: "e2e_cleanup",
            },
          }),
          "turnaround",
        );
      }
    }
  }
  await ctx.dispose();
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

test("operations manager reads the command centre, asks for a dMind brief and reviews it", async ({
  page,
}) => {
  await signInViaBff(page, "ops.lopez");
  await page.goto("/operations");
  const tiles = page.getByTestId("ops-tiles");
  await expect(tiles).toBeVisible();
  await expect(page.getByTestId("ops-freshness")).toHaveAttribute(
    "role",
    "status",
  );
  // Deterministic forecasts are honest about missing history.
  await expect(page.getByTestId("ops-forecast").first()).toContainText(
    /insufficient|history/i,
  );
  // Capacity bars carry a textual summary (no colour-only signal).
  await expect(page.getByRole("img").first()).toHaveAccessibleName(/\d/);

  await page.getByRole("button", { name: /brief/i }).click();
  const brief = page.getByTestId("ops-brief-outcome");
  await expect(brief).toBeVisible({ timeout: 20_000 });
  await expect(brief).toHaveAttribute(
    "data-artifact-status",
    "awaiting_review",
  );
  await brief.getByRole("button", { name: /accept as reviewed/i }).click();
  await expect(brief).toHaveAttribute("data-artifact-status", "approved");

  await expectNoSeriousViolations(page);

  await setLanguage(page, "es");
  await expect(
    page.getByRole("heading", {
      level: 1,
      name: /operaciones|centro de mando/i,
    }),
  ).toBeVisible();
  await setLanguage(page, "en");
});

test("bed coordinator runs the admission → placement → coordination → discharge loop and the patient sees only released facts", async ({
  page,
}) => {
  const albaId = await ownPatientId("rep.alba");
  await cancelOpenEpisodes("bed.ortiz", albaId);

  await signInViaBff(page, "bed.ortiz");
  await page.goto("/operations/flow");
  await expect(page.getByTestId("flow-queue")).toBeVisible();

  // 1. Human admission decision.
  const form = page.getByTestId("flow-admission");
  await form.getByLabel("Patient ID").fill(albaId);
  await form.getByLabel("Episode type").fill("inpatient");
  await form.getByLabel("Service").fill("internal_medicine");
  await form.getByLabel("Reason / note").fill("E2E synthetic admission");
  await form.getByRole("button", { name: "Record admission decision" }).click();

  // 2. Deterministic candidates, then human hold and confirmation.
  const req = page.getByTestId("flow-request");
  await expect(req).toBeVisible();
  const generate = req.getByRole("button", { name: "Generate candidates" });
  if (await generate.isVisible()) await generate.click();
  const feasible = page.getByTestId("flow-feasible");
  await expect(feasible).toBeVisible();
  await expect(req).toContainText("care-placement.v1");
  await feasible.getByRole("radio").first().check();
  await req.getByRole("button", { name: "Hold 30 min" }).click();
  await expect(req).toHaveAttribute("data-status", "held");
  await req
    .getByRole("button", { name: /Confirm (placement|held position)/ })
    .click();
  await expect(req).toHaveAttribute("data-status", "placed");

  // 3. Episode coordination: patient-visible work, discharge preparation.
  const coordLink = req.locator('a[href*="/coordination"]').first();
  const href = await coordLink.getAttribute("href");
  expect(href).toBeTruthy();
  await page.goto(href!);
  const episode = page.getByTestId("coord-episode");
  await expect(episode).toContainText(/active/i);

  await page.locator("summary", { hasText: "Add coordination task" }).click();
  const newTask = page.getByTestId("coord-new-task");
  await newTask.getByLabel("Title (English)").fill("Arrange transport home");
  await newTask
    .getByLabel("Title (Spanish)")
    .fill("Organizar transporte a casa");
  await newTask.getByLabel("Patient-visible").check();
  await newTask.locator('button[type="submit"]').click();
  const task = page
    .getByTestId("coord-task")
    .filter({ hasText: "Arrange transport home" });
  await expect(task).toBeVisible();

  await page
    .getByRole("button", { name: "Start discharge preparation" })
    .click();
  await expect(episode).toContainText(/discharge preparation/i);

  // 4. Patient/representative view: released facts only.
  await signOutViaBff(page);
  await signInViaBff(page, "rep.alba");
  await page.goto("/my/care");
  const myEpisode = page.getByTestId("mycare-episode").first();
  await expect(myEpisode).toBeVisible();
  await expect(myEpisode).toContainText("Arrange transport home");
  await expect(page.getByTestId("mycare-location")).not.toContainText(
    /no bed or chair/i,
  );
  await expect(page.getByTestId("mycare-episodes")).toContainText(
    /not medical clearance/i,
  );
  await expect(page.getByTestId("mycare-episodes")).not.toContainText(
    /dMind|forecast|staffing|barrier_category/i,
  );
  await setLanguage(page, "es");
  await expect(page.getByTestId("mycare-episode").first()).toContainText(
    "Organizar transporte a casa",
  );
  await setLanguage(page, "en");

  // 5. Human completion: discharge-ready → completed, bed released for cleaning.
  await signOutViaBff(page);
  await signInViaBff(page, "bed.ortiz");
  await page.goto(href!);
  await task.getByRole("button", { name: "Start" }).click();
  await task.getByRole("button", { name: "Complete" }).click();
  await expect(task).toContainText(/completed/i);
  await page
    .getByRole("button", { name: "Mark operationally discharge-ready" })
    .click();
  await expect(episode).toContainText(/discharge ready/i);
  const placement = page.getByTestId("coord-placement");
  await expect(placement).toHaveAttribute("data-status", "occupied");
  await placement
    .getByRole("button", { name: "Release placement (to cleaning)" })
    .click();
  await expect(page.getByTestId("coord-placement")).toHaveCount(0);
  await page
    .getByTestId("coord-episode")
    .getByRole("button", { name: "Complete" })
    .click();
  await expect(episode).toContainText(/completed/i);
  const history = page.getByTestId("coord-history");
  await expect(history).toContainText(/created/i);
  await expect(history).toContainText(/placed/i);
  await expect(history).toContainText(/placement released/i);
  await expect(history).toContainText(/discharge_ready → completed/i);
});

test("theatre and workforce coordinators see deterministic readiness and capability gaps", async ({
  page,
}) => {
  await signInViaBff(page, "theatre.ruiz");
  await page.goto("/operations/theatres");
  const cases = page.getByTestId("theatre-cases");
  await expect(cases).toBeVisible();
  await cases
    .getByRole("button", { name: /scheduled/i })
    .first()
    .click();
  const readiness = page.getByTestId("theatre-readiness");
  await expect(readiness).toContainText("theatre-conflicts.v1");

  await signOutViaBff(page);
  await signInViaBff(page, "wf.diaz");
  await page.goto("/operations/workforce");
  const shifts = page.getByTestId("wf-shifts");
  await expect(shifts).toBeVisible();
  await shifts.getByRole("button").first().click();
  await expect(page.getByTestId("wf-gaps")).toBeVisible();
  await expectNoSeriousViolations(page);
});

test("command centre fits a 390px viewport without horizontal overflow", async ({
  page,
}) => {
  await page.setViewportSize({ width: 390, height: 844 });
  await signInViaBff(page, "ops.lopez");
  await page.goto("/operations");
  await expect(page.getByTestId("ops-tiles")).toBeVisible();
  const overflow = await page.evaluate(
    () =>
      document.documentElement.scrollWidth -
      document.documentElement.clientWidth,
  );
  expect(overflow).toBe(0);
  await page.goto("/operations/flow");
  await expect(page.getByTestId("flow-queue")).toBeVisible();
  expect(
    await page.evaluate(
      () =>
        document.documentElement.scrollWidth -
        document.documentElement.clientWidth,
    ),
  ).toBe(0);
});

test("clinicians without operations permissions get an explicit denied state", async ({
  page,
}) => {
  await signInViaBff(page, "privacy.wolf");
  await page.goto("/operations");
  await expect(page.getByRole("alert").first()).toContainText(
    /permission|not allowed|denied|forbidden/i,
  );
});
