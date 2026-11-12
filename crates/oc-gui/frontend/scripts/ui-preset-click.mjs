// Preset open budget probe: opening a plugin preset loads it behind the pane.
//
// The native plugin load is simulated as a 2.5 s async IPC round trip, which is
// what the blocking-pool command costs the webview. Row selection must paint
// before the load starts, and the pane must stay interactive while it runs.
//
// Run against a Vite dev server (localhost, not 127.0.0.1):
//   OCP_UI_URL=http://localhost:5187 node scripts/ui-preset-click.mjs

import { chromium, expect } from "@playwright/test";
import { mkdir } from "node:fs/promises";
import path from "node:path";
import { baseUrl, createFullFeaturedSnapshot, installTauriMock, invokeCommands } from "./ui-fixture.mjs";

const outDir = path.resolve("tmp/ui-preset-click");
// A load of this length is what a large plugin costs before its params arrive.
const LOAD_DELAY_MS = 2500;
// Selection to first paint budget for a single click.
const CLICK_BUDGET_MS = 50;
const LONG_TASK_BUDGET_MS = 50;
const PRESETS = ["Preset A", "Preset B", "Preset C", "Preset D", "Preset E"];

function importedPreset(name) {
  return {
    name,
    source: "vst3",
    plugin: "vst3:halcyon_2",
    program: null,
    patch: null,
    file: null,
    root_note: null,
    extends: null,
    params: [["serum_file", { String: `/tmp/${name}.SerumPreset` }]],
    effects: [],
  };
}

const snapshot = createFullFeaturedSnapshot();
snapshot.project.presets.push(...PRESETS.map(importedPreset));

await mkdir(outDir, { recursive: true });

const browser = await chromium.launch({ headless: true });
const page = await browser.newPage({ viewport: { width: 1440, height: 960 }, deviceScaleFactor: 1 });
const pageErrors = [];
page.on("pageerror", (error) => pageErrors.push(error.message));
await installTauriMock(page, snapshot);

// Simulate a native plugin load that takes its time off the main thread. A fixed
// params payload keeps the detail pane usable after the wait.
await page.addInitScript((delayMs) => {
  const internals = window.__TAURI_INTERNALS__;
  const original = internals.invoke;
  internals.invoke = async (cmd, args) => {
    if (cmd === "load_plugin_for_preset") {
      await new Promise((resolve) => setTimeout(resolve, delayMs));
      return [{
        id: 1,
        name: "Cutoff",
        module: "Oscillators/Osc 1",
        min: 0,
        max: 1,
        default: 0.5,
        current: 0.5,
        display: "0.5000",
        flags: 0,
      }];
    }
    return original(cmd, args);
  };
}, LOAD_DELAY_MS);

try {
  await page.goto(baseUrl, { waitUntil: "domcontentloaded" });
  await page.waitForSelector(".daw-root", { timeout: 10_000 });
  await page.keyboard.press("KeyP");
  await page.waitForSelector(".preset-list-item", { timeout: 10_000 });
  await expect(page.locator(".preset-list-item")).toHaveCount(snapshot.project.presets.length);

  await page.evaluate(() => {
    window.__OCP_LONG_TASKS__ = [];
    window.__OCP_CLICK_MS__ = null;
    new PerformanceObserver((list) => {
      for (const entry of list.getEntries()) {
        window.__OCP_LONG_TASKS__.push({ start: entry.startTime, duration: entry.duration });
      }
    }).observe({ type: "longtask", buffered: false });
  });

  const measurements = [];
  for (const name of PRESETS) {
    const item = page.locator(".preset-list-item", { hasText: name }).first();
    await item.scrollIntoViewIfNeeded();
    const box = await item.boundingBox();
    if (!box) throw new Error(`preset ${name} has no box`);

    await page.evaluate(() => {
      window.__OCP_CLICK_MS__ = null;
      document.querySelector(".preset-list-items").addEventListener("click", () => {
        const start = performance.now();
        requestAnimationFrame(() => requestAnimationFrame(() => {
          window.__OCP_CLICK_MS__ = performance.now() - start;
        }));
      }, { capture: true, once: true });
    });

    await page.mouse.click(box.x + box.width / 2, box.y + box.height / 2);
    await page.waitForFunction(() => window.__OCP_CLICK_MS__ !== null);
    const clickMs = await page.evaluate(() => window.__OCP_CLICK_MS__);
    await expect(page.locator(".preset-detail-name")).toHaveText(name);
    measurements.push({ name, clickMs });
  }

  const longTasks = await page.evaluate(() => window.__OCP_LONG_TASKS__);
  const invokes = await invokeCommands(page);
  const loadCalls = invokes.filter((cmd) => cmd === "load_plugin_for_preset").length;

  console.log("preset click measurements:");
  for (const { name, clickMs } of measurements) {
    console.log(`  ${name}: click_to_paint=${clickMs.toFixed(1)} ms`);
  }
  const worstClick = Math.max(...measurements.map((entry) => entry.clickMs));
  const worstLongTask = longTasks.reduce((worst, entry) => Math.max(worst, entry.duration), 0);
  console.log(`worst_click_to_paint=${worstClick.toFixed(1)} ms budget=${CLICK_BUDGET_MS} ms`);
  console.log(`long_tasks=${longTasks.length} worst=${worstLongTask.toFixed(1)} ms budget=${LONG_TASK_BUDGET_MS} ms`);
  console.log(`load_plugin_for_preset calls=${loadCalls}`);

  // The pane opens on a spinner, then the parameter table replaces it.
  await page.waitForSelector(".preset-params-loading", { timeout: 10_000 });
  await page.waitForSelector(".plugin-params-grouped", { timeout: 10_000 });
  await expect(page.getByLabel("Reload parameters")).toBeVisible();

  if (pageErrors.length) throw new Error(`Browser errors: ${pageErrors.join("; ")}`);
  if (loadCalls !== PRESETS.length) throw new Error(`Expected one load per opened preset, got ${loadCalls}`);
  if (worstClick > CLICK_BUDGET_MS) throw new Error(`Selection took ${worstClick.toFixed(1)} ms (budget ${CLICK_BUDGET_MS} ms)`);
  if (worstLongTask > LONG_TASK_BUDGET_MS) throw new Error(`Main thread blocked for ${worstLongTask.toFixed(1)} ms (budget ${LONG_TASK_BUDGET_MS} ms)`);
  console.log(`passed: five preset selections stayed within ${CLICK_BUDGET_MS} ms`);
} catch (error) {
  try {
    await page.screenshot({ path: path.join(outDir, "failure.png"), fullPage: true, timeout: 5_000 });
  } catch (_) {}
  throw error;
} finally {
  await browser.close();
}
