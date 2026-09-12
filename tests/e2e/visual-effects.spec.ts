import { expect, test } from "@playwright/test";

for (const theme of ["wavelink3", "wavelink3_dark"]) {
  test(`visual tone controls remain simple and usable in ${theme}`, async ({
    page,
  }, testInfo) => {
    await page.addInitScript(
      (theme) => localStorage.setItem("wavelinux.ui.themeId.v1", theme),
      theme,
    );
    await page.goto("/");
    await page.getByRole("button", { name: "FX", exact: true }).first().click();
    const drawer = page.locator(".wl-effects-drawer");
    const eq = drawer.getByRole("group", { name: "8-band equalizer" });
    const bass = eq.getByRole("slider", { name: "Bass · 63 Hz gain" });
    await bass.scrollIntoViewIfNeeded();
    const plot = (await eq.locator(".eq-plot").boundingBox())!;
    const dot = (await bass.boundingBox())!;
    await page.mouse.move(dot.x + dot.width / 2, dot.y + dot.height / 2);
    await page.mouse.down();
    await page.mouse.move(dot.x + dot.width / 2, plot.y + plot.height / 4, {
      steps: 8,
    });
    await expect(bass).toHaveAttribute("aria-valuenow", "6");
    await page.mouse.up();
    // Verify the stored edit after remounting the drawer.
    await page.keyboard.press("Escape");
    await page.getByRole("button", { name: "FX", exact: true }).first().click();
    await expect(bass).toHaveAttribute("aria-valuenow", "6");
    await bass.press("ArrowDown");
    await expect(bass).toHaveAttribute("aria-valuenow", "5.5");
    await eq
      .getByRole("slider", { name: "Boxiness · 500 Hz gain" })
      .press("PageDown");
    await eq.getByRole("slider", { name: "Air · 8 kHz gain" }).press("PageUp");
    await eq.scrollIntoViewIfNeeded();
    await eq.locator("xpath=ancestor::article").screenshot({
      path: `target/ui-review/equalizer-${theme}-${testInfo.project.name}.png`,
    });
    await eq.getByRole("button", { name: "Reset EQ" }).click();
    for (const band of await eq.getByRole("slider").all())
      await expect(band).toHaveAttribute("aria-valuenow", "0");

    await drawer
      .locator(".catalog-item")
      .filter({ hasText: "Compressor" })
      .click();
    const compressor = drawer.getByRole("group", {
      name: "Compressor controls",
    });
    await compressor.scrollIntoViewIfNeeded();
    const block = compressor.locator("xpath=ancestor::article");
    await expect(block.getByRole("slider")).toHaveCount(1);
    await expect(compressor.getByText("Demo signal")).toBeVisible();
    const threshold = compressor.getByRole("slider", { name: "Threshold" });
    await threshold.fill("-32");
    await threshold.press("Enter");
    await expect(threshold).toHaveValue("-32");
    await page.keyboard.press("Escape");
    await page.getByRole("button", { name: "FX", exact: true }).first().click();
    await compressor.scrollIntoViewIfNeeded();
    await expect(threshold).toHaveValue("-32");
    const chart = (await compressor
      .locator(".compressor-chart")
      .boundingBox())!;
    const line = compressor.locator(".compressor-threshold-line");
    const lineBox = (await line.boundingBox())!;
    await page.mouse.move(
      lineBox.x + lineBox.width / 2,
      lineBox.y + lineBox.height / 2,
    );
    await page.mouse.down();
    await page.mouse.move(
      chart.x + chart.width / 2,
      chart.y + chart.height / 8,
      { steps: 8 },
    );
    await page.mouse.up();
    await expect(threshold).toHaveValue("-15");
    await compressor.getByRole("button", { name: "Pause display" }).click();
    const frozenPath = await compressor
      .locator(".compressor-wave-before")
      .getAttribute("d");
    await expect(compressor.getByText("Display paused")).toBeVisible();
    await block.screenshot({
      path: `target/ui-review/compressor-${theme}-${testInfo.project.name}.png`,
    });
    expect(
      await compressor.locator(".compressor-wave-before").getAttribute("d"),
    ).toBe(frozenPath);
    await block.getByRole("button", { name: "Advanced" }).click();
    await expect(block.getByRole("slider")).toHaveCount(5);
    await expect(
      block.getByRole("slider", { name: "Ratio", exact: true }),
    ).toBeVisible();
    const bounds = await drawer
      .locator(".wl-effects-drawer-scroll")
      .evaluate((element) => ({
        width: element.clientWidth,
        content: element.scrollWidth,
      }));
    expect(bounds.content).toBeLessThanOrEqual(bounds.width + 1);
    await compressor.getByRole("button", { name: "Resume display" }).click();
    await expect
      .poll(() =>
        compressor.locator(".compressor-wave-before").getAttribute("d"),
      )
      .not.toBe(frozenPath);
  });
}

test("visual effects fit the full Effects page and cancelled EQ gestures do not save", async ({
  page,
}, testInfo) => {
  await page.goto("/");
  await page
    .getByRole("button", { name: "Effects", exact: true })
    .first()
    .click();
  const view = page.locator(".effects-view");
  await view.locator(".catalog-item").filter({ hasText: "Compressor" }).click();
  for (const name of ["8-band equalizer", "Compressor controls"]) {
    const editor = view.getByRole("group", { name });
    await editor.scrollIntoViewIfNeeded();
    const dimensions = await editor.evaluate((element) => ({
      width: element.clientWidth,
      scroll: element.scrollWidth,
    }));
    expect(dimensions.scroll).toBeLessThanOrEqual(dimensions.width + 1);
    await editor.screenshot({
      path: `target/ui-review/full-${name === "8-band equalizer" ? "eq" : "compressor"}-${testInfo.project.name}.png`,
    });
  }
  const bass = view.getByRole("slider", { name: "Bass · 63 Hz gain" });
  await bass.scrollIntoViewIfNeeded();
  const initial = await bass.getAttribute("aria-valuenow");
  const dot = (await bass.boundingBox())!;
  await page.mouse.move(dot.x + dot.width / 2, dot.y + dot.height / 2);
  await page.mouse.down();
  await page.mouse.move(dot.x + dot.width / 2, dot.y - 20);
  await bass.dispatchEvent("pointercancel", { pointerId: 1 });
  await page.mouse.up();
  await expect(bass).toHaveAttribute("aria-valuenow", initial!);
  await page
    .getByRole("button", { name: "Mixer", exact: true })
    .first()
    .click();
  await page
    .getByRole("button", { name: "Effects", exact: true })
    .first()
    .click();
  await expect(bass).toHaveAttribute("aria-valuenow", initial!);
});

test("EQ frequency, width and shapes survive reopening, and Flat clears cuts", async ({
  page,
}, testInfo) => {
  await page.goto("/");
  const open = () =>
    page.getByRole("button", { name: "FX", exact: true }).first().click();
  await open();
  const eq = page.getByRole("group", { name: "8-band equalizer" });
  const dot = eq.locator(".eq-point").first();
  await dot.scrollIntoViewIfNeeded();
  const plot = (await eq.locator(".eq-plot").boundingBox())!;
  const bounds = (await dot.boundingBox())!;
  await page.mouse.move(
    bounds.x + bounds.width / 2,
    bounds.y + bounds.height / 2,
  );
  await page.mouse.down();
  await page.mouse.move(
    bounds.x +
      bounds.width / 2 +
      ((plot.width - 2) * Math.log(500 / 63)) / Math.log(1000),
    bounds.y + bounds.height / 2 + (plot.height - 2) * 0.125,
    { steps: 8 },
  );
  await page.mouse.up();
  await expect(dot).toHaveAttribute("aria-valuenow", "-3");
  await expect(
    eq.getByRole("spinbutton", { name: "Frequency (Hz)" }),
  ).toHaveValue("500");
  const width = eq.getByRole("spinbutton", { name: "Width (Q)" });
  await width.fill("0.5");
  await width.press("Enter");
  await eq.getByRole("combobox", { name: "Filter shape" }).selectOption("1");
  await page.keyboard.press("Escape");
  await open();
  await expect(dot).toHaveAttribute("aria-valuenow", "-3");
  await expect(
    eq.getByRole("spinbutton", { name: "Frequency (Hz)" }),
  ).toHaveValue("500");
  await expect(width).toHaveValue("0.5");
  await expect(eq.getByRole("combobox", { name: "Filter shape" })).toHaveValue(
    "1",
  );
  await eq.getByRole("combobox", { name: "Filter shape" }).selectOption("3");
  await expect(eq.locator(".eq-response")).not.toHaveAttribute(
    "d",
    /^M0\.00,150\.00 L4\.17,150\.00/,
  );
  await dot.hover();
  await expect(eq.getByRole("tooltip")).toContainText("500 Hz");
  await eq
    .locator("xpath=ancestor::article")
    .screenshot({
      path: `target/parametric-eq/controls-${testInfo.project.name}.png`,
    });
  await eq
    .locator("xpath=ancestor::article")
    .getByRole("button", { name: "Flat", exact: true })
    .click();
  await expect(eq.getByRole("combobox", { name: "Filter shape" })).toHaveValue(
    "0",
  );
  await expect(
    eq.getByRole("spinbutton", { name: "Frequency (Hz)" }),
  ).toHaveValue("63");
  await expect(width).toHaveValue("0.9");
  await expect(dot).toHaveAttribute("aria-valuenow", "0");
});
