import { expect, test, type Page } from "@playwright/test";

async function expectPageScreenshot(page: Page, name: string) {
  await page.evaluate(async () => {
    await document.fonts.ready;
    await new Promise<void>((resolve) => {
      requestAnimationFrame(() => requestAnimationFrame(() => resolve()));
    });
  });
  const screenshot = await page.screenshot({
    animations: "disabled",
    caret: "hide",
  });
  expect(screenshot).toMatchSnapshot(name, { maxDiffPixels: 300 });
}

test("FX drawer remains usable and contained at desktop scaling", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Mixer" })).toBeVisible();

  await page.getByRole("button", { name: "FX", exact: true }).first().click();
  const dialog = page.getByRole("dialog", { name: "Mixer controls" });
  const drawer = page.locator(".wl-effects-drawer");
  const scrollOwner = drawer.locator(".wl-effects-drawer-scroll");
  const equalizer = drawer.getByRole("group", { name: "8-band equalizer" });
  await expect(drawer).toBeVisible();
  await expect(dialog).toBeVisible();
  await expect(equalizer).toBeVisible();

  const sliders = equalizer.getByRole("slider");
  await expect(sliders).toHaveCount(8);
  await expect(drawer.getByRole("slider", { name: "Strength" })).toHaveCount(2);
  const advancedButtons = drawer.getByRole("button", { name: "Advanced" });
  await expect(advancedButtons).toHaveCount(2);
  await expect(drawer.getByTitle("Copy effect")).toHaveCount(0);
  await expect(drawer.getByTitle("Bypass effect")).toHaveCount(0);
  await expect(drawer.getByRole("button", { name: "Paste" })).toHaveCount(0);
  for (let index = 0; index < 8; index += 1) {
    const box = await sliders.nth(index).boundingBox();
    expect(box, `EQ band ${index + 1} has a layout box`).not.toBeNull();
    expect(box!.height, `EQ band ${index + 1} is vertical`).toBeGreaterThan(box!.width * 2);
  }

  const advancedEffect = advancedButtons.first().locator("xpath=ancestor::article");
  const collapsedSliderCount = await advancedEffect.getByRole("slider").count();
  await advancedButtons.first().click();
  await expect(advancedButtons.first()).toHaveAttribute("aria-expanded", "true");
  await expect(advancedEffect.getByText("Parameters", { exact: true })).toBeVisible();
  expect(await advancedEffect.getByRole("slider").count()).toBeGreaterThan(collapsedSliderCount);

  const dimensions = await scrollOwner.evaluate((element) => ({
    clientHeight: element.clientHeight,
    clientWidth: element.clientWidth,
    scrollHeight: element.scrollHeight,
    scrollWidth: element.scrollWidth,
    overflowY: getComputedStyle(element).overflowY,
  }));
  expect(["auto", "scroll"]).toContain(dimensions.overflowY);
  expect(dimensions.scrollHeight).toBeGreaterThan(dimensions.clientHeight);
  expect(dimensions.scrollWidth).toBeLessThanOrEqual(dimensions.clientWidth + 1);

  await expectPageScreenshot(page, "fx-drawer-page.png");

  await scrollOwner.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
  });
  await expect.poll(() => scrollOwner.evaluate((element) => element.scrollTop)).toBeGreaterThan(0);
  await expect(drawer.getByText("Catalog", { exact: true })).toBeVisible();

  const documentOverflow = await page.evaluate(() => ({
    horizontal: document.documentElement.scrollWidth - document.documentElement.clientWidth,
    vertical: document.documentElement.scrollHeight - document.documentElement.clientHeight,
  }));
  expect(documentOverflow.horizontal).toBeLessThanOrEqual(1);
  expect(documentOverflow.vertical).toBeLessThanOrEqual(1);

  await expectPageScreenshot(page, "fx-drawer-catalog-page.png");

  await page.keyboard.press("Shift+Tab");
  expect(
    await page.evaluate(() => Boolean(document.activeElement?.closest(".wl-drawer-layer"))),
  ).toBe(true);
  await page.keyboard.press("Escape");
  await expect(drawer).toBeHidden();

  await page.getByRole("button", { name: "FX", exact: true }).first().click();
  await dialog.getByRole("button", { name: "Close mixer drawer" }).click({ position: { x: 5, y: 5 } });
  await expect(drawer).toBeHidden();
});

test("effect strength readouts stay inside the full Effects view", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Effects", exact: true }).first().click();

  const effectsView = page.locator(".effects-view");
  const effectBlocks = effectsView.locator(".effect-chain > .effect-block");
  const limiter = effectBlocks.filter({ hasText: "Limiter" });
  await expect(limiter).toBeVisible();
  await limiter.getByRole("button", { name: "Broadcast -1 dB" }).click();

  await effectsView.locator(".catalog-item").filter({ hasText: "Noise Gate" }).click();
  const gate = effectBlocks.filter({ hasText: "Noise Gate" });
  await expect(gate).toBeVisible();
  const gateStrength = gate.getByRole("slider", { name: "Strength" });
  await gateStrength.fill("86.2");
  await gateStrength.press("Enter");

  for (const [block, expectedValue] of [
    [gate, "86.2% (-26.9 dB)"],
    [limiter, "50% (-1 dB ceiling)"],
  ] as const) {
    const row = block.locator(".fader-row.compact").first();
    await expect(row.locator(":scope > strong")).toHaveText(expectedValue);
    const bounds = await row.evaluate((element) => {
      const value = element.querySelector(":scope > strong");
      const rowRect = element.getBoundingClientRect();
      const valueRect = value?.getBoundingClientRect();
      return {
        clientWidth: element.clientWidth,
        rowRight: rowRect.right,
        scrollWidth: element.scrollWidth,
        valueRight: valueRect?.right ?? Number.POSITIVE_INFINITY,
      };
    });
    expect(bounds.scrollWidth).toBeLessThanOrEqual(bounds.clientWidth + 1);
    expect(bounds.valueRight).toBeLessThanOrEqual(bounds.rowRight + 1);
  }

  const horizontalOverflow = await page.evaluate(
    () => document.documentElement.scrollWidth - document.documentElement.clientWidth,
  );
  expect(horizontalOverflow).toBeLessThanOrEqual(1);
});
