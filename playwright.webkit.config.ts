import { defineConfig } from "@playwright/test";
import base from "./playwright.config";

// Keep native-engine interaction coverage available independently of the
// Chromium screenshot suite. Install Playwright's WebKit runtime first.
export default defineConfig({
  ...base,
  testMatch: ["compressor-interaction.spec.ts", "noise-filters.spec.ts", "effect-controls.spec.ts"],
  projects: [
    {
      name: "desktop-webkit",
      use: {
        browserName: "webkit",
        viewport: { width: 1440, height: 900 },
      },
    },
  ],
});
