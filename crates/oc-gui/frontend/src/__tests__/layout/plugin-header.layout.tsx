/**
 * Plugin panel header guard.
 *
 * The sidebar can be dragged as narrow as 220 px. At that width the header's
 * three items (Rescan, the plugin count, Check project) can squeeze the
 * button until its label wraps mid-word and clips at the right edge. This
 * guard renders the real component with the real stylesheet at 220 and 320 px
 * and measures: nothing leaves the header, the header never scrolls sideways,
 * and the Check project label stays on one line.
 *
 * Run with `npm run layout-check`.
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { render } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import path from "node:path";
import { chromium, type Browser } from "@playwright/test";
import { invokeMock, makeSnapshot } from "../setup";
import { store } from "../../stores/project";
import { i18n } from "../../stores/i18n";
import { PluginBrowser } from "../../components/PluginBrowser";

const STYLE_ROOT = path.resolve(process.cwd(), "src/styles");

/** The daw.css manifest followed in order, inline in the fixture page. */
function stylesheet(): string {
  const manifest = readFileSync(path.join(STYLE_ROOT, "daw.css"), "utf8");
  const imports = [...manifest.matchAll(/@import\s+"([^"]+)";/g)].map((match) =>
    path.resolve(STYLE_ROOT, match[1]),
  );
  return imports
    .map((file) => `/* ${path.basename(file)} */\n${readFileSync(file, "utf8")}`)
    .join("\n");
}

function fixtureHtml(width: number): string {
  const { container, unmount } = render(() => <PluginBrowser />);
  const markup = container.innerHTML;
  unmount();
  return `<!doctype html>
<html lang="en">
<head><meta charset="utf-8"><title>plugin header guard</title>
<style>
${stylesheet()}
html, body { margin: 0; height: 100%; }
.plugin-browser { width: ${width}px; }
</style></head>
<body>${markup}</body></html>`;
}

interface HeaderReport {
  left: number;
  right: number;
  scrollWidth: number;
  clientWidth: number;
  items: {
    text: string;
    left: number;
    right: number;
    button: boolean;
    nowrap: boolean;
    overflow: number;
  }[];
}

let browser: Browser;

beforeAll(async () => {
  browser = await chromium.launch({ headless: true });
  i18n.setLocale("en");
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "list_sampler_assets") return [];
    if (cmd === "soundfonts_list") return { dir: "", discovered: [], installed: [] };
    return makeSnapshot();
  });
  await store.initProject();
});

afterAll(async () => {
  await browser?.close();
});

describe("the plugin panel header at a narrow sidebar", () => {
  for (const width of [220, 320]) {
    it(`keeps every header item inside ${width}px`, async () => {
      const page = await browser.newPage({ viewport: { width: 764, height: 700 } });
      try {
        await page.setContent(fixtureHtml(width));
        const report: HeaderReport = await page.evaluate(() => {
          const header = document.querySelector(".plugin-browser-header");
          if (!header) throw new Error("no plugin header");
          const box = header.getBoundingClientRect();
          const items = [...header.children].map((element) => {
            const rect = element.getBoundingClientRect();
            const style = getComputedStyle(element);
            return {
              text: (element.textContent ?? "").trim().slice(0, 24),
              left: rect.left,
              right: rect.right,
              button: element.tagName === "BUTTON",
              nowrap: style.whiteSpace === "nowrap",
              overflow: element.scrollWidth - element.clientWidth,
            };
          });
          return {
            left: box.left,
            right: box.right,
            scrollWidth: header.scrollWidth,
            clientWidth: header.clientWidth,
            items,
          };
        });

        expect(report.items.length).toBeGreaterThanOrEqual(3);
        expect(report.scrollWidth).toBeLessThanOrEqual(report.clientWidth + 1);
        for (const item of report.items) {
          expect(item.right).toBeLessThanOrEqual(report.right + 1);
          expect(item.left).toBeGreaterThanOrEqual(report.left - 1);
          // A wrapped or clipped button label is the bug this guard exists for.
          if (item.button) {
            expect(item.nowrap).toBe(true);
            expect(item.overflow).toBeLessThanOrEqual(1);
          }
        }
      } finally {
        await page.close();
      }
    });
  }
});
