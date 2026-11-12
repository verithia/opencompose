import { defineConfig } from "vitest/config";
import { bundledLicenses } from "./scripts/bundled-licenses.ts";
import solid from "vite-plugin-solid";

export default defineConfig({
  plugins: [solid(), bundledLicenses()],
  server: { fs: { allow: [".", "../../../docs/manual"] } },
  test: {
    environment: "jsdom",
    globals: true,
    // Whole-component tests render a full editor and load patches; under the
    // parallel worker pool a 5 s default marks healthy tests as failures.
    testTimeout: 20_000,
    hookTimeout: 20_000,
    setupFiles: ["./src/__tests__/setup.ts"],
    css: false,
    deps: {
      optimizer: {
        ssr: { include: ["solid-js"] },
      },
    },
    coverage: {
      provider: "v8",
      include: ["src/ai/**", "src/components/**", "src/data/**", "src/stores/**", "src/types/**", "src/utils/**"],
      exclude: ["src/__tests__/**", "src/assets/**"],
      reporter: ["text", "text-summary"],
      thresholds: {
        statements: 30,
        branches: 25,
        functions: 30,
        lines: 30,
      },
    },
  },
  resolve: {
    conditions: ["development", "browser"],
  },
});
