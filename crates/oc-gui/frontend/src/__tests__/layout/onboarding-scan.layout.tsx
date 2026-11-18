/**
 * Real-layout guard for the last first-run page: what this computer has, the
 * folders searched, and one recommendation per role.
 *
 * The guide is rendered in jsdom against the bundled catalog, resolved for each
 * desktop platform the way the backend resolves it, then measured in headless
 * Chrome with the real stylesheet: nothing overflows the card, the folder
 * buttons stay inside their cards, and long folder paths are clipped instead of
 * widening the list.
 *
 * Run with `npm run layout-check`. Screenshots land in
 * `tmp/layout-check/shots/onboarding-<platform>-<locale>-<theme>.png`.
 */
import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen } from "@solidjs/testing-library";
import { mkdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { chromium, type Browser } from "@playwright/test";
import { Onboarding } from "../../components/Onboarding";
import { i18n } from "../../stores/i18n";
import { pluginStore } from "../../stores/pluginStore";
import type { PluginCatalogEntry, PluginInstall } from "../../stores/pluginApi";
import { invokeMock } from "../setup";

const STYLE_ROOT = path.resolve(process.cwd(), "src/styles");
const SHOT_DIR = path.resolve(process.cwd(), "tmp/layout-check/shots");
const manifest = readFileSync(path.join(STYLE_ROOT, "daw.css"), "utf8");
const stylesheet = [...manifest.matchAll(/@import\s+"([^"]+)";/g)]
  .map((match) => readFileSync(path.resolve(STYLE_ROOT, match[1]), "utf8")).join("\n")
  + "\n" + readFileSync(path.join(STYLE_ROOT, "catppuccin.css"), "utf8");
const catalog = JSON.parse(
  readFileSync(path.resolve(process.cwd(), "src/data/plugin-catalog.json"), "utf8"),
) as { entries: PluginCatalogEntry[] };

/** The backend's answer for one platform, in the resolver's terms: the
 *  best-ranked build installs when it is pinned, and only an unpacked bundle
 *  format installs without the vendor's installer. */
function resolved(platform: string): PluginCatalogEntry[] {
  const formatRank: Record<string, number> = { vst3: 0, clap: 1, vst2: 2, lv2: 3, au: 4, standalone: 5 };
  const rank = (install: PluginInstall) => (install.kind === "vendor_page" ? 10 : 0) + formatRank[install.format];
  return catalog.entries.map((entry) => {
    const chosen = entry.installs
      .filter((install) => install.platform === platform)
      .sort((left, right) => rank(left) - rank(right))[0] as
      (PluginInstall & { sha256?: string; wrapped?: boolean }) | undefined;
    const pinned = chosen && chosen.kind !== "vendor_page" && Boolean(chosen.sha256) ? chosen : undefined;
    const unpacked = pinned?.archive
      ? ["zip", "tar-gz"].includes(pinned.archive)
      : /\.(zip|tar\.gz|tgz)$/i.test(pinned?.url ?? "");
    const headless = unpacked && !pinned?.wrapped && ["vst3", "vst2", "clap", "lv2"].includes(pinned?.format ?? "");
    return {
      ...entry,
      installed: false,
      installable: pinned !== undefined,
      install_mode: pinned ? (headless ? "extract" : "installer") : undefined,
    };
  });
}

const folder = (folderPath: string, count: number, origin: "standard" | "user" = "standard") => ({
  path: folderPath, origin, exists: true, count,
});

const SUMMARY = {
  plugins: {
    total: 57,
    formats: [
      { format: "vst3", count: 18 },
      { format: "clap", count: 11 },
      { format: "au", count: 15 },
      { format: "vst2", count: 13 },
    ],
    folders: [
      folder("/Library/Audio/Plug-Ins/VST3", 15),
      folder("/Library/Audio/Plug-Ins/CLAP", 11),
      folder("/Library/Audio/Plug-Ins/Components", 15),
      folder("/Library/Audio/Plug-Ins/VST", 13),
      folder("/Users/you/Library/Audio/Plug-Ins/VST3", 3),
      folder("/Volumes/Sample Drive/Studio Libraries/Third-party plugins/Instruments/VST3", 0, "user"),
    ],
  },
  sounds: {
    total: 4,
    formats: [{ format: "sf2", count: 2 }, { format: "sf3", count: 1 }, { format: "sfz", count: 1 }],
    folders: [folder("/Users/you/Music/SoundFonts", 3), folder("/Users/you/Sounds/Orchestra", 1, "user")],
  },
  scanning: true,
};

let browser: Browser;
beforeAll(async () => {
  browser = await chromium.launch({ headless: true });
  mkdirSync(SHOT_DIR, { recursive: true });
});
afterAll(async () => { await browser?.close(); });
afterEach(() => {
  cleanup();
  pluginStore.reset();
  i18n.setLocale("en");
});

async function renderPluginStep(platform: string) {
  invokeMock.mockImplementation(async (command: string) => {
    if (command === "plugin_catalog") {
      return { platform, entries: resolved(platform), installed: [], source: "bundled" };
    }
    if (command === "local_content_summary") return SUMMARY;
    if (command === "soundfonts_list") return { dir: "", items: [] };
    return null;
  });
  const fixture = render(() => <Onboarding onDone={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: i18n.t("onboarding.next") }));
  fireEvent.click(screen.getByRole("button", { name: i18n.t("onboarding.ai.defer") }));
  await screen.findByLabelText(i18n.t("onboarding.local.pluginsCount", { count: 57 }));
  await screen.findAllByRole("button", { name: i18n.t("onboarding.plugins.install") });
  fixture.container.querySelector("details")?.setAttribute("open", "");
  return fixture;
}

describe("first-run plugin page layout", () => {
  for (const platform of ["macos-aarch64", "windows-x86_64", "linux-x86_64"]) {
    for (const locale of ["en", "zh-Hans"] as const) {
      for (const theme of ["graphite", "ivory"]) {
        it(`${platform} ${locale} ${theme}: counts, folders and picks fit the card`, async () => {
          i18n.setLocale(locale);
          const fixture = await renderPluginStep(platform);
          const page = await browser.newPage({ viewport: { width: 1280, height: 1100 } });
          try {
            await page.setContent(`<!doctype html><html data-theme="${theme}"><head>
              <meta charset="utf-8"><style>${stylesheet} html,body{margin:0}
              *,*::before,*::after{animation:none!important;transition:none!important}</style></head>
              <body>${fixture.container.innerHTML}</body></html>`);
            const problems = await page.evaluate(() => {
              const found: string[] = [];
              const card = document.querySelector(".onboarding-card")!.getBoundingClientRect();
              const inside = (element: Element, bounds: DOMRect, label: string) => {
                const box = element.getBoundingClientRect();
                if (box.left < bounds.left - 1 || box.right > bounds.right + 1) found.push(label);
              };
              for (const element of document.querySelectorAll(".onboarding-local-card, .onboarding-tool")) {
                inside(element, card, `card overflows: ${element.textContent?.slice(0, 40)}`);
              }
              for (const local of document.querySelectorAll(".onboarding-local-card")) {
                const bounds = local.getBoundingClientRect();
                for (const child of local.children) inside(child, bounds, `local card child overflows`);
              }
              for (const row of document.querySelectorAll(".onboarding-folder-list li")) {
                inside(row, card, `folder row overflows: ${row.textContent?.slice(0, 40)}`);
              }
              if (document.querySelectorAll(".onboarding-tool").length < 3) found.push("fewer than three picks");
              return found;
            });
            expect(problems).toEqual([]);
            await page.screenshot({
              path: path.join(SHOT_DIR, `onboarding-${platform}-${locale}-${theme}.png`),
              fullPage: true,
            });
          } finally {
            await page.close();
          }
        });
      }
    }
  }
});
