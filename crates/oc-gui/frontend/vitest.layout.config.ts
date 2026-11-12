import { defineConfig, mergeConfig } from "vitest/config";
import base from "./vitest.config.ts";

// Layout guards render the real Solid components into headless Chromium.
// Keep them separate from the default unit-test include pattern.
export default defineConfig(mergeConfig(base, {
  test: {
    include: ["src/__tests__/layout/**/*.layout.tsx"],
    testTimeout: 120_000,
    hookTimeout: 120_000,
  },
}));
