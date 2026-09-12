import { expect, test } from "@playwright/test";

test("keyboard changes apply on key release and Advanced values stay accurate", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "FX", exact: true }).first().click();
  const drawer = page.locator(".wl-effects-drawer");
  await drawer.locator(".catalog-item").filter({ hasText: "DeepFilterNet 3" }).click();
  const filter = drawer.locator(".effect-block").filter({ hasText: "DeepFilterNet 3" });
  const gentle = filter.getByRole("button", { name: "Gentle", exact: true });
  await gentle.click();
  const strength = filter.getByRole("slider", { name: "Strength" });
  await strength.focus();
  await page.keyboard.down("End");
  await expect(strength).toHaveValue("100");
  await expect(gentle).toHaveAttribute("aria-pressed", "true");
  await page.keyboard.up("End");
  await expect(gentle).toHaveAttribute("aria-pressed", "false");
  await expect(strength).toBeFocused();
  await strength.press("Home");
  await filter.getByRole("button", { name: "Advanced", exact: true }).click();
  await expect(filter.getByRole("slider", { name: "Noise Reduction", exact: true })).toHaveValue("0");

  await drawer.locator(".catalog-item").filter({ hasText: "High-Pass Filter" }).click();
  const highpass = drawer.locator(".effect-block").filter({ hasText: "High-Pass Filter" });
  await highpass.getByRole("button", { name: "Advanced", exact: true }).click();
  await highpass.getByRole("slider", { name: "Cutoff", exact: true }).press("End");
  await expect(highpass.getByRole("slider", { name: "Strength" })).toHaveAttribute("aria-valuetext", "100% (500 Hz)");
});

test("dragging a strength slider commits when released outside the control", async ({ page }) => {
  await page.goto("/");
  await page.getByRole("button", { name: "FX", exact: true }).first().click();
  const drawer = page.locator(".wl-effects-drawer");
  await drawer.locator(".catalog-item").filter({ hasText: "DeepFilterNet 3" }).click();
  const filter = drawer.locator(".effect-block").filter({ hasText: "DeepFilterNet 3" });
  const gentle = filter.getByRole("button", { name: "Gentle", exact: true });
  await gentle.click();
  const strength = filter.getByRole("slider", { name: "Strength" });
  await strength.scrollIntoViewIfNeeded();
  const box = (await strength.boundingBox())!;
  await page.mouse.move(box.x + box.width * 0.1, box.y + box.height / 2);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width * 0.7, box.y - 35, { steps: 8 });
  await page.mouse.up();
  await expect(gentle).toHaveAttribute("aria-pressed", "false");
  const committed = Number(await strength.inputValue());
  expect(committed).toBeGreaterThan(50);
  await page.keyboard.press("Escape");
  await page.getByRole("button", { name: "FX", exact: true }).first().click();
  await expect(strength).toHaveValue(String(committed));
});
