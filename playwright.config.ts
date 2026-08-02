import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./tests/e2e",
  fullyParallel: true,
  forbidOnly: Boolean(process.env.CI),
  retries: process.env.CI ? 1 : 0,
  reporter: process.env.CI ? "github" : "list",
  use: {
    baseURL: "http://127.0.0.1:1420",
    colorScheme: "dark",
    locale: "en-US",
    reducedMotion: "reduce",
    trace: "retain-on-failure",
  },
  webServer: {
    command: "node node_modules/.bin/vite --host 127.0.0.1 --port 1420",
    url: "http://127.0.0.1:1420",
    reuseExistingServer: !process.env.CI,
    timeout: 30_000,
  },
  projects: [
    {
      name: "desktop-100",
      use: { viewport: { width: 1440, height: 900 }, deviceScaleFactor: 1 },
    },
    {
      name: "desktop-125",
      use: { viewport: { width: 1152, height: 720 }, deviceScaleFactor: 1.25 },
    },
    {
      name: "desktop-150",
      use: { viewport: { width: 960, height: 640 }, deviceScaleFactor: 1.5 },
    },
  ],
});
