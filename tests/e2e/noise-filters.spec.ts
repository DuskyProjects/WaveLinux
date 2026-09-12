import { expect, test } from "@playwright/test";

test("switch noise filters, choose Gentle, and keep edits after reopening", async ({ page }, testInfo) => {
  await page.goto("/");
  await page.getByRole("button", { name: "FX", exact: true }).first().click();
  const drawer = page.locator(".wl-effects-drawer");
  const catalog = drawer.locator(".catalog-item");
  await catalog.filter({ hasText: "Noise Suppression" }).click();
  await catalog.filter({ hasText: "DeepFilterNet 3" }).click();
  const filter = drawer.locator(".effect-block").filter({ hasText: "DeepFilterNet 3" });
  await expect(filter).toHaveCount(1);
  await expect(drawer.locator(".effect-block").filter({ hasText: "RNNoise" })).toHaveCount(0);
  await filter.getByRole("button", { name: "Gentle", exact: true }).click();
  await expect(filter.getByRole("slider", { name: "Strength" })).toHaveValue("10");
  await expect(filter.getByRole("slider")).toHaveCount(1);
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "FX", exact: true }).first().click();
  await expect(filter.getByRole("slider", { name: "Strength" })).toHaveValue("10");
  await filter.screenshot({ path: `target/ui-review/deepfilter-${testInfo.project.name}.png` });
  await catalog.filter({ hasText: "Noise Suppression" }).click();
  await expect(filter).toHaveCount(0);
  const rnnoise = drawer.locator(".effect-block").filter({ hasText: "RNNoise" });
  await rnnoise.getByRole("button", { name: "Gentle", exact: true }).click();
  await expect(rnnoise.getByRole("slider", { name: "Strength" })).toHaveValue("10");
  await rnnoise.getByRole("button", { name: "Advanced", exact: true }).click();
  await expect(rnnoise.getByRole("slider", { name: "Noise Reduction", exact: true })).toHaveValue("6");
  await expect(rnnoise.getByRole("button", { name: "Speech Gate", exact: true })).toHaveAttribute("aria-pressed", "false");
});
