import { chromium } from "@playwright/test";
import { mkdir } from "node:fs/promises";
import { readFile } from "node:fs/promises";
import path from "node:path";
import { baseUrl, createFullFeaturedSnapshot, installTauriMock } from "./ui-fixture.mjs";

const outDir = path.resolve("tmp/ui-screenshots");
const examplePath = path.resolve("../../../examples/demo_full_featured.ocp");

await mkdir(outDir, { recursive: true });

const browser = await chromium.launch({ headless: true });
const page = await browser.newPage({ viewport: { width: 1440, height: 960 }, deviceScaleFactor: 1 });
const pageErrors = [];
page.on("pageerror", (error) => pageErrors.push(error.message));
const dslSource = await readFile(examplePath, "utf8");

// The first-run language chooser would otherwise block the timeline.
await page.addInitScript(() => window.localStorage.setItem("opencompose.locale", "en"));

await installTauriMock(page, createFullFeaturedSnapshot(dslSource));

try {
  await page.goto(baseUrl, { waitUntil: "domcontentloaded" });
  await page.waitForSelector(".daw-root", { timeout: 10_000 });
  await page.locator(".clip-block", { hasText: "lead_chorus" }).first().dblclick();
  await page.waitForSelector(".pianoroll-toolbar", { timeout: 10_000 });
  await page.locator("[data-pianoroll-grid]").evaluate((element) => element.scrollTop = 260);

  await page.getByLabel("Preferences").click();
  const families = await page.locator(".theme-family-select option").evaluateAll((options) => options.map((option) => option.value));
  for (const family of families) {
    await page.selectOption(".theme-family-select", family);
    const variants = await page.locator(".theme-variant-select option").evaluateAll((options) => options.map((option) => option.value));
    for (const variant of variants) {
      await page.selectOption(".theme-variant-select", variant);
      const cssTheme = await page.evaluate(() => document.documentElement.dataset.theme);
      // Let the theme transition settle before capturing.
      await page.waitForTimeout(300);
      await page.screenshot({ path: path.join(outDir, `${family}-${variant}-${cssTheme}.png`), fullPage: true });
    }
  }
  await page.keyboard.press("Escape");
  await page.locator(".preferences-modal").waitFor({ state: "detached" });
  await page.screenshot({ path: path.join(outDir, "timeline-latest.png"), fullPage: true });

  await page.keyboard.press("KeyM");
  await page.screenshot({ path: path.join(outDir, "mixer-open.png"), fullPage: true });

  if (pageErrors.length) throw new Error(`Browser errors: ${pageErrors.join("; ")}`);
  console.log(`Saved UI screenshots to ${outDir}`);
} finally {
  await browser.close();
}
