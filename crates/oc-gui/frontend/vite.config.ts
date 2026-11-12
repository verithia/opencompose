import { defineConfig } from "vite";
import { bundledLicenses } from "./scripts/bundled-licenses.ts";
import solid from "vite-plugin-solid";

// https://v2.tauri.app/start/frontend/solidjs/
const host = process.env.TAURI_DEV_HOST;

export default defineConfig({
  plugins: [solid(), bundledLicenses()],
  // Prevent vite from obscuring Rust errors
  clearScreen: false,
  server: {
    // Bundle the checked-in manual; keep filesystem access limited to these roots.
    fs: { allow: [".", "../../../docs/manual"] },
    // Tauri expects a fixed port
    port: 5173,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 5174,
        }
      : undefined,
    watch: {
      // Tell vite to ignore watching `src-tauri`
      ignored: ["**/src-tauri/**"],
    },
  },
});
