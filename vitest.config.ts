import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    environment: "jsdom",
    include: ["src/**/*.test.{ts,tsx}"],
    exclude: ["tests/e2e/**", "node_modules/**", "target/**"],
    setupFiles: ["./src/test/setup.ts"],
    restoreMocks: true,
  },
});
