import { expect, test } from "@playwright/test";

test("threshold drags vertically, ignores horizontal movement, and persists", async ({
  page,
}, testInfo) => {
  await page.goto("/");
  await page.getByRole("button", { name: "Effects", exact: true }).click();
  await page
    .locator(".effects-view .catalog-item")
    .filter({ hasText: "Compressor" })
    .click();
  const compressor = page.getByRole("group", { name: "Compressor controls" });
  const slider = compressor.getByRole("slider", { name: "Threshold" });
  await compressor.scrollIntoViewIfNeeded();
  await expect(slider).toHaveAttribute("aria-orientation", "vertical");
  const rail = (await compressor
    .locator(".compressor-threshold-track")
    .boundingBox())!;
  const point = (db: number) => ({
    x: rail.x + rail.width / 2,
    y: rail.y + rail.height * (-db / 120),
  });
  const initial = Number(await slider.inputValue());
  const begin = point(initial),
    target = point(-40);
  await page.mouse.move(begin.x, begin.y);
  await page.mouse.down();
  await page.mouse.move(target.x, target.y, { steps: 8 });
  await expect(slider).toHaveValue("-40");
  // Crossing the rail sideways must leave the vertical value unchanged.
  await page.mouse.move(target.x + 65, target.y, { steps: 5 });
  await expect(slider).toHaveValue("-40");
  await page.mouse.up();
  await page.getByRole("button", { name: "Mixer", exact: true }).click();
  await page.getByRole("button", { name: "Effects", exact: true }).click();
  await expect(slider).toHaveValue("-40");
  await compressor.scrollIntoViewIfNeeded();
  await slider.focus();
  await slider.press("ArrowUp");
  await expect(slider).toHaveValue("-39.5");
  await slider.press("End");
  await expect(slider).toHaveValue("0");
  await slider.press("Home");
  await expect(slider).toHaveValue("-60");
  await slider.press("ArrowUp");
  await expect(slider).toHaveValue("-59.5");
  await compressor.getByRole("button", { name: "Pause display" }).click();
  await compressor.screenshot({
    path: `target/compressor-vertical/vertical-${testInfo.project.name}.png`,
  });
});
