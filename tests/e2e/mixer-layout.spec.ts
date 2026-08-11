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

async function scrollToEnd(scrollOwner: ReturnType<Page["locator"]>) {
  await scrollOwner.evaluate((element) => {
    element.scrollTop = element.scrollHeight - element.clientHeight;
  });
  await expect
    .poll(() =>
      scrollOwner.evaluate((element) =>
        Math.abs(element.scrollTop - (element.scrollHeight - element.clientHeight)),
      ),
    )
    .toBeLessThanOrEqual(1);
}

test("mixer mutations stay responsive and long labels remain contained", async ({ page }) => {
  await page.goto("/");
  await expect(page.getByRole("heading", { name: "Mixer" })).toBeVisible();

  const mute = page.getByRole("button", { name: "Mute Input in Monitor" });
  await expect(mute).toBeVisible();
  await mute.click();
  await expect(page.getByRole("button", { name: "Unmute Input in Monitor" })).toHaveClass(/active/);

  await page.getByRole("button", { name: "Source", exact: true }).click();
  const dialog = page.locator(".wl-dialog");
  await expect(dialog).toBeVisible();
  const longName = "ExtremelyLongUnbrokenCreatorMicrophoneSourceNameForLayoutValidation";
  await dialog.getByLabel("Name").fill(longName);
  await dialog.getByRole("button", { name: "Add Source", exact: true }).click();
  await expect(dialog).toBeHidden();

  const sourceName = page.locator(".wl-source-title strong", { hasText: longName });
  await expect(sourceName).toBeVisible();
  const labelDimensions = await sourceName.evaluate((element) => ({
    clientWidth: element.clientWidth,
    scrollWidth: element.scrollWidth,
    overflow: getComputedStyle(element).overflow,
    textOverflow: getComputedStyle(element).textOverflow,
  }));
  expect(labelDimensions.scrollWidth).toBeGreaterThan(labelDimensions.clientWidth);
  expect(labelDimensions.overflow).toBe("hidden");
  expect(labelDimensions.textOverflow).toBe("ellipsis");

  const sourceSubtitle = sourceName.locator("xpath=following-sibling::span");
  const subtitleDimensions = await sourceSubtitle.evaluate((element) => ({
    clientWidth: element.clientWidth,
    scrollWidth: element.scrollWidth,
    overflow: getComputedStyle(element).overflow,
    textOverflow: getComputedStyle(element).textOverflow,
  }));
  expect(subtitleDimensions.scrollWidth).toBeGreaterThan(subtitleDimensions.clientWidth);
  expect(subtitleDimensions.overflow).toBe("hidden");
  expect(subtitleDimensions.textOverflow).toBe("ellipsis");

  const documentOverflow = await page.evaluate(() => ({
    horizontal: document.documentElement.scrollWidth - document.documentElement.clientWidth,
    vertical: document.documentElement.scrollHeight - document.documentElement.clientHeight,
  }));
  expect(documentOverflow.horizontal).toBeLessThanOrEqual(1);
  expect(documentOverflow.vertical).toBeLessThanOrEqual(1);

  await expect(page.getByText("Source added", { exact: true })).toBeHidden({ timeout: 5_000 });
  await scrollToEnd(page.locator(".wl-matrix-scroll"));
  await expect(sourceName).toBeVisible();
  await expectPageScreenshot(page, "mixer-long-label-page.png");
});
