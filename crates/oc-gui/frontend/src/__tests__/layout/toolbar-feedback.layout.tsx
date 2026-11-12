import { afterAll, beforeAll, expect, it } from "vitest";
import { readFileSync } from "node:fs";
import path from "node:path";
import { chromium, type Browser } from "@playwright/test";

const root = path.resolve(process.cwd(), "src/styles");
const css = [...readFileSync(path.join(root, "daw.css"), "utf8").matchAll(/@import\s+"([^"]+)";/g)]
  .map(match => readFileSync(path.resolve(root, match[1]), "utf8")).join("\n");
let browser: Browser;
beforeAll(async () => { browser = await chromium.launch({ headless: true }); });
afterAll(async () => { await browser?.close(); });

it("keeps loop refusal text visible without changing crowded toolbar width or control positions", async () => {
  for (const width of [640, 1024]) {
    const page = await browser.newPage({ viewport: { width, height: 700 } });
    try {
      await page.setContent(`<style>${css}</style><div class="brand-block">OpenCompose</div><div class="daw-toolbar" style="width:${width}px;height:48px;box-sizing:border-box"><span id="logo" style="flex:0 0 100px">OpenCompose</span><div style="flex:1;min-width:0">Transport controls</div><button id="ai" style="flex:0 0 100px">AI</button></div>`);
      const before = await page.locator("#ai").boundingBox();
      const message = "Loop did not change: Drag in the ruler to set a loop region first. " + "long-localized-detail/".repeat(80);
      await page.locator(".daw-toolbar").evaluate((toolbar, text) => {
        const status = document.createElement("div");
        status.className = "transport-status";
        status.setAttribute("role", "status");
        status.textContent = text;
        toolbar.append(status);
      }, message);
      expect(await page.locator("#ai").boundingBox()).toEqual(before);
      expect(await page.locator(".transport-status").textContent()).toBe(message);
      const geometry = await page.evaluate(() => {
        const status = document.querySelector(".transport-status") as HTMLElement;
        const box = status.getBoundingClientRect();
        return { overflow: document.documentElement.scrollWidth - innerWidth,
          statusOverflow: status.scrollWidth - status.clientWidth,
          left: box.left, right: box.right, top: box.top,
          brandRight: document.querySelector(".brand-block")!.getBoundingClientRect().right, whiteSpace: getComputedStyle(status).whiteSpace };
      });
      expect(geometry.overflow).toBeLessThanOrEqual(1);
      expect(geometry.statusOverflow).toBeLessThanOrEqual(1);
      expect(geometry.left).toBeGreaterThanOrEqual(geometry.brandRight);
      expect(geometry.right).toBeLessThanOrEqual(width);
      expect(geometry.top).toBeGreaterThanOrEqual(48);
      expect(geometry.whiteSpace).toBe("normal");
    } finally { await page.close(); }
  }
});
