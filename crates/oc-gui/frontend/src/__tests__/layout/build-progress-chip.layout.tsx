/**
 * Real-layout guard for the project load chip: the progress fill covers the
 * share of the chip the load has finished, stays inside it, and the label is
 * painted above both the fill and the moving sweep.
 */
import { afterAll, afterEach, beforeAll, expect, it } from "vitest";
import { cleanup, render } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import path from "node:path";
import { chromium, type Browser } from "@playwright/test";
import { BuildProgressChip } from "../../components/BuildProgressChip";

const root = path.resolve(process.cwd(), "src/styles");
const css = [...readFileSync(path.join(root, "daw.css"), "utf8").matchAll(/@import\s+"([^"]+)";/g)]
  .map((match) => readFileSync(path.resolve(root, match[1]), "utf8")).join("\n");

let browser: Browser;
beforeAll(async () => { browser = await chromium.launch({ headless: true }); });
afterAll(async () => { await browser?.close(); });
afterEach(cleanup);

it("fills the finished share of the chip under a readable label", async () => {
  render(() => (
    <BuildProgressChip build={{ stage: "effects", label: "", done: 0, total: 0, progress: 500 }} />
  ));
  const page = await browser.newPage({ viewport: { width: 800, height: 200 } });
  try {
    // Transitions are off so the measurement reads the settled bar.
    await page.setContent(`<!doctype html><html data-theme="graphite"><head><meta charset="utf-8">
      <style>${css} *,*::before,*::after{transition:none!important;animation:none!important}</style>
      </head><body><div class="status-bar">${document.body.innerHTML}</div></body></html>`);
    const found = await page.evaluate(() => {
      const element = document.querySelector<HTMLElement>(".build-progress")!;
      const chip = element.getBoundingClientRect();
      const fill = document.querySelector(".build-progress-fill")!.getBoundingClientRect();
      const text = document.querySelector(".build-progress-text")!;
      const box = text.getBoundingClientRect();
      // Hit-testing skips layers that ignore the pointer; let the decorative
      // layers take part so the topmost painted element is what is found.
      for (const layer of document.querySelectorAll<HTMLElement>(".build-progress-fill, .build-progress-sweep")) {
        layer.style.pointerEvents = "auto";
      }
      const top = document.elementFromPoint(box.left + 4, box.top + box.height / 2);
      return {
        // The fill spans the padding box, inside the chip's border.
        share: fill.width / element.clientWidth,
        inside: fill.left >= chip.left - 0.5 && fill.right <= chip.right + 0.5
          && fill.top >= chip.top - 0.5 && fill.bottom <= chip.bottom + 0.5,
        labelOnTop: top === text || text.contains(top),
      };
    });
    expect(found.share).toBeCloseTo(0.5, 2);
    expect(found.inside).toBe(true);
    expect(found.labelOnTop).toBe(true);
  } finally {
    await page.close();
  }
});
