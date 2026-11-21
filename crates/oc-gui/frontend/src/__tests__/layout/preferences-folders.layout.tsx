/**
 * Real-layout guard for the Folders page in Preferences.
 *
 * The page is rendered in jsdom with a realistic folder list, one folder
 * selected, then measured in headless Chrome with the real stylesheets: every
 * row and group head stays one line inside the list, long paths are clipped
 * rather than widening it, and the details strip wraps a long path inside the
 * block.
 *
 * Run with `npm run layout-check`. Screenshots land in
 * `tmp/layout-check/shots/preferences-folders-<locale>-<theme>.png`.
 */
import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { mkdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { chromium, type Browser } from "@playwright/test";
import { PreferencesModal } from "../../components/PreferencesModal";
import { i18n } from "../../stores/i18n";
import { localScan } from "../../stores/localScan";
import type { LocalContentSummary, SearchedFolder } from "../../stores/pluginApi";
import { invokeMock, listenMock } from "../setup";

const STYLE_ROOT = path.resolve(process.cwd(), "src/styles");
const SHOT_DIR = path.resolve(process.cwd(), "tmp/layout-check/shots");
const manifest = readFileSync(path.join(STYLE_ROOT, "daw.css"), "utf8");
const stylesheet = [...manifest.matchAll(/@import\s+"([^"]+)";/g)]
  .map((match) => readFileSync(path.resolve(STYLE_ROOT, match[1]), "utf8")).join("\n")
  + "\n" + readFileSync(path.join(STYLE_ROOT, "catppuccin.css"), "utf8")
  + "\n" + readFileSync(path.resolve(process.cwd(), "src/components/preferences/Preferences.css"), "utf8");

const LONG = "/Users/mira/Library/Application Support/com.opencompose.app/Sounds/Installed Packs/Orchestral Strings Ensemble (Long Articulations)";

const folder = (path: string, origin: SearchedFolder["origin"], count: number, extra: Partial<SearchedFolder> = {}): SearchedFolder =>
  ({ path, origin, exists: true, count, ...extra });

function summary(): LocalContentSummary {
  const plugins = [
    folder("/Volumes/Studio/Plugins", "user", 9),
    folder("/Users/mira/Music/Custom VST", "user", 4),
    folder("/Library/Audio/Plug-Ins/VST3", "standard", 42),
    folder("/Library/Audio/Plug-Ins/Components", "standard", 18),
    folder("/Library/Audio/Plug-Ins/LV2", "standard", 0, { exists: false }),
    folder("/opt/audio/plugins", "environment", 3),
    folder("D:\\Audio\\VST3", "host_settings", 5),
  ];
  const sounds = [
    folder("/Users/mira/Music/My Instruments", "user", 6, { library_id: "lib-1" }),
    folder("/Volumes/Archive/Drum Samples", "user", 0, { library_id: "lib-2" }),
    folder(LONG, "managed", 34),
    folder("/Users/mira/Documents/Samples", "convention", 0),
  ];
  const kind = (folders: SearchedFolder[], total: number) => ({ total, formats: [], folders, items: [], truncated: false });
  return {
    plugins: kind(plugins, 81),
    instruments: kind(sounds, 40),
    samples: kind(sounds.map((entry, index) => ({ ...entry, count: [128, 2048, 412, 96][index] })), 2684),
    scanning: false,
  };
}

let browser: Browser;
beforeAll(async () => {
  browser = await chromium.launch({ headless: true });
  mkdirSync(SHOT_DIR, { recursive: true });
});
afterAll(async () => { await browser?.close(); });
afterEach(() => {
  cleanup();
  localScan.reset();
  i18n.setLocale("en");
});

describe("Folders preferences page layout", () => {
  for (const locale of ["en", "zh-Hans"] as const) {
    for (const theme of ["graphite", "catppuccin-latte"]) {
      it(`${locale} ${theme}: rows stay one line and the details fit the block`, async () => {
        i18n.setLocale(locale);
        listenMock.mockImplementation(async () => () => {});
        invokeMock.mockImplementation(async (command: string) =>
          command === "local_content_summary" || command === "local_content_scan" ? summary() : null);
        render(() => <PreferencesModal open onClose={() => {}} />);
        fireEvent.click(document.querySelector('[data-page="folders"]')!);
        await screen.findByText(i18n.t("folders.plugins"));
        fireEvent.click(screen.getByTitle(LONG));

        const page = await browser.newPage({ viewport: { width: 1280, height: 860 } });
        try {
          await page.setContent(`<!doctype html><html data-theme="${theme}"><head>
            <meta charset="utf-8"><style>${stylesheet} html,body{margin:0}
            *,*::before,*::after{animation:none!important;transition:none!important}</style></head>
            <body>${document.body.innerHTML}</body></html>`);
          const found = await page.evaluate(() => {
            const problems: string[] = [];
            const block = document.querySelector(".preferences-folder-browser")!.getBoundingClientRect();
            const inside = (selector: string) => {
              for (const element of document.querySelectorAll(selector)) {
                const box = element.getBoundingClientRect();
                if (box.left < block.left - 1 || box.right > block.right + 1) {
                  problems.push(`overflows: ${element.className} ${element.textContent?.slice(0, 40)}`);
                }
              }
            };
            inside(".preferences-folder-row, .preferences-folder-row > *, .preferences-folder-group-head, .preferences-folder-details *, .preferences-folder-tab");
            for (const element of document.querySelectorAll(".preferences-folder-row")) {
              const height = element.getBoundingClientRect().height;
              if (height > 32) problems.push(`row wraps (${height}px): ${element.textContent?.slice(0, 40)}`);
            }
            for (const element of document.querySelectorAll(".preferences-folder-group-head")) {
              const height = element.getBoundingClientRect().height;
              if (height > 30) problems.push(`group head wraps (${height}px): ${element.textContent?.slice(0, 40)}`);
            }
            for (const element of document.querySelectorAll(".preferences-folder-name")) {
              if (element.getBoundingClientRect().width < 24) problems.push(`name squeezed: ${element.textContent}`);
            }
            // The details strip stays at the foot of the visible page.
            const pageBox = document.querySelector(".preferences-page")!.getBoundingClientRect();
            const details = document.querySelector(".preferences-folder-details")!.getBoundingClientRect();
            if (details.bottom > pageBox.bottom + 1) problems.push("details below the page");
            if (details.bottom < pageBox.bottom - 1) problems.push(`rows show beneath the details: ${pageBox.bottom - details.bottom}px`);
            if (details.top < pageBox.top) problems.push("details above the page");
            return problems;
          });
          expect(found).toEqual([]);
          await page.screenshot({ path: path.join(SHOT_DIR, `preferences-folders-${locale}-${theme}.png`) });
          // Scrolled to the end, the last row clears the strip instead of hiding under it.
          const hidden = await page.evaluate(() => {
            const scroller = document.querySelector(".preferences-page")!;
            scroller.scrollTop = scroller.scrollHeight;
            const strip = document.querySelector(".preferences-folder-details")!.getBoundingClientRect();
            const rows = [...document.querySelectorAll(".preferences-folder-row")];
            const last = rows[rows.length - 1].getBoundingClientRect();
            return last.bottom > strip.top + 1 ? `last row under the details by ${last.bottom - strip.top}px` : null;
          });
          expect(hidden).toBeNull();
          await page.screenshot({ path: path.join(SHOT_DIR, `preferences-folders-${locale}-${theme}-end.png`) });
        } finally {
          await page.close();
        }
      });
    }
  }
});
