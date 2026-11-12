import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import path from "node:path";
import { chromium, type Browser } from "@playwright/test";
import { OpenSynthEditor } from "../../components/OpenSynthEditor";
import { i18n } from "../../stores/i18n";
import { THEME_CATALOG } from "../../stores/theme";
import type { PresetDef } from "../../types/project";

const preset: PresetDef = {
  name: "Layout test", source: "builtin", plugin: "builtin.open_synth",
  program: null, patch: null, file: null, root_note: null, extends: null,
  params: [], effects: [],
};
const styleRoot = path.resolve(process.cwd(), "src/styles");
const manifest = readFileSync(path.join(styleRoot, "daw.css"), "utf8");
const stylesheet = [...manifest.matchAll(/@import\s+"([^"]+)";/g)]
  .map(match => readFileSync(path.resolve(styleRoot, match[1]), "utf8")).join("\n")
  + "\n" + readFileSync(path.join(styleRoot, "catppuccin.css"), "utf8")
  + "\n" + readFileSync(path.join(styleRoot, "synth.css"), "utf8");
let browser: Browser;

beforeAll(async () => { browser = await chromium.launch({ headless: true }); });
afterAll(async () => { await browser?.close(); });
afterEach(() => { cleanup(); i18n.setLocale("en"); });

function renderEditor() {
  return render(() => <OpenSynthEditor
    preset={preset}
    setParam={async () => null} setParams={async () => null}
    macroLearn={async () => ({ error: null, result: null })}
    loadPatch={async () => null} noteOn={async () => {}} noteOff={async () => {}}
    auditionPatch={async () => {}} stopAudition={async () => {}}
    auditioning={() => null} auditionStatus={() => null}
    libraryPatches={() => []} libraryWarnings={() => []} refreshLibrary={async () => null}
    patchActions={{ rename: async () => null, duplicate: async () => null,
      saveAs: async () => null, saveToLibrary: async () => null, randomize: async () => null }}
  />);
}

describe("synth control banks keep captions inside their own cells", () => {
  for (const scale of [0.7, 1, 1.5]) {
    for (const locale of ["en", "zh-Hans"] as const) {
      for (const theme of THEME_CATALOG[0].variants) {
        it(`${scale}x ${locale} ${theme.id}: oscillator, filter and envelope banks fit`, async () => {
          i18n.setLocale(locale);
          const fixture = renderEditor();
          // Webview zoom changes the logical viewport, rather than scaling a
          // fixed-width CSS box. Include a narrow dock independently of zoom.
          const page = await browser.newPage({ viewport: {
            width: Math.round(1400 / scale), height: Math.round(900 / scale),
          } });
          try {
            for (const tabKey of [null, "synth.ui.filter", "synth.ui.ampTab"] as const) {
              if (tabKey) {
                const tab = [...fixture.container.querySelectorAll(".synth-subtabs button")]
                  .find(button => button.textContent === i18n.t(tabKey).toUpperCase());
                expect(tab).toBeTruthy();
                fireEvent.click(tab!);
              }
              for (const dockWidth of [null, 380]) {
                await page.setContent(`<!doctype html><html data-theme="${theme.css}"><head>
                  <meta charset="utf-8"><style>${stylesheet}
                  html,body{margin:0} .layout-dock{width:${dockWidth ? `${dockWidth}px` : "100%"};max-width:100%}
                  </style></head><body><div class="layout-dock">${fixture.container.innerHTML}</div></body></html>`);
                const failures = await page.evaluate(() => {
                  const problems: string[] = [];
                  const rows = document.querySelectorAll<HTMLElement>(".synth-param-row");
                  if (rows.length === 0) problems.push("no control banks rendered");
                  for (const row of rows) {
                    // An oscillator row is `display: contents`: its cells are
                    // grid items of the surrounding control bank. Measure the
                    // bank, not the zero-sized row wrapper.
                    const bank = getComputedStyle(row).display === "contents"
                      ? row.parentElement
                      : row;
                    if (!bank) {
                      problems.push("control bank has no layout parent");
                      continue;
                    }
                    const bounds = bank.getBoundingClientRect();
                    if (getComputedStyle(bank).display !== "grid") {
                      problems.push("control-bank stylesheet missing");
                    }
                    for (const cell of [...row.children]) {
                      const box = cell.getBoundingClientRect();
                      if (box.left < bounds.left - 1 || box.right > bounds.right + 1) {
                        problems.push(`cell exceeds bank: ${cell.getAttribute("data-param")}`);
                      }
                      for (const caption of cell.querySelectorAll("label,output,.oc-dropdown-label")) {
                        const range = document.createRange();
                        range.selectNodeContents(caption);
                        for (const ink of range.getClientRects()) {
                          if (ink.left < box.left - 1 || ink.right > box.right + 1) {
                            problems.push(`text exceeds cell: ${caption.textContent}`);
                          }
                        }
                      }
                    }
                  }
                  return problems;
                });
                expect(failures, `${tabKey ?? "oscillators"}, dock ${dockWidth ?? "full"}`).toEqual([]);
                // Long captions must grow vertically without hiding their text.
                await page.locator(".synth-param-row label,.synth-param-row .oc-dropdown-label")
                  .evaluateAll(labels => labels.forEach(label => {
                    label.textContent = "Extended localized production control caption";
                  }));
                const clipped = await page.locator(".synth-param-row label,.synth-param-row .oc-dropdown-label")
                  .evaluateAll(labels => labels.filter(label => label.scrollWidth > label.clientWidth + 1)
                    .map(label => label.textContent));
                expect(clipped).toEqual([]);
              }
            }
          } finally { await page.close(); }
        });
      }
    }
  }
});
