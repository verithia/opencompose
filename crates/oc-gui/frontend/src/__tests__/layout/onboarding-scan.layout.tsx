/**
 * Real-layout guard for the first-run scan and suggestion pages.
 *
 * The guide is rendered in jsdom with each desktop platform's folders and the
 * bundled catalog, resolved for that platform the way the backend resolves it,
 * then measured in headless Chrome with the real stylesheet: nothing overflows
 * the card, long paths are clipped instead of widening a list, and a long list
 * scrolls inside its fold.
 *
 * Run with `npm run layout-check`. Screenshots land in
 * `tmp/layout-check/shots/onboarding-<page>-<platform>-<locale>-<theme>.png`.
 */
import { afterAll, afterEach, beforeAll, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { mkdirSync, readFileSync } from "node:fs";
import path from "node:path";
import { chromium, type Browser } from "@playwright/test";
import { Onboarding } from "../../components/Onboarding";
import { i18n } from "../../stores/i18n";
import { localScan } from "../../stores/localScan";
import { pluginStore } from "../../stores/pluginStore";
import { soundPacks } from "../../stores/soundPacks";
import type {
  FolderOrigin,
  FoundItem,
  LocalContentSummary,
  LocalScanProgress,
  PluginCatalogEntry,
  PluginInstall,
  SoundPackItem,
  SoundPackProgress,
} from "../../stores/pluginApi";
import { invokeMock, listenMock } from "../setup";

const STYLE_ROOT = path.resolve(process.cwd(), "src/styles");
const SHOT_DIR = path.resolve(process.cwd(), "tmp/layout-check/shots");
const manifest = readFileSync(path.join(STYLE_ROOT, "daw.css"), "utf8");
const stylesheet = [...manifest.matchAll(/@import\s+"([^"]+)";/g)]
  .map((match) => readFileSync(path.resolve(STYLE_ROOT, match[1]), "utf8")).join("\n")
  + "\n" + readFileSync(path.join(STYLE_ROOT, "catppuccin.css"), "utf8");
const catalog = JSON.parse(
  readFileSync(path.resolve(process.cwd(), "src/data/plugin-catalog.json"), "utf8"),
) as {
  entries: PluginCatalogEntry[];
  packs: (Omit<SoundPackItem, "installed" | "installed_version" | "path" | "source"> & { kind: string })[];
};

/** The bundled catalog's starter packs, as `sound_packs_list` reports them. */
const STARTER_PACKS: SoundPackItem[] = catalog.packs
  .filter((pack) => pack.kind === "sound_archive")
  .map((pack) => ({ ...pack, installed: false, installed_version: null, path: null, source: "index" }));

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
    // A disk image is unpacked headlessly on macOS unless it only wraps an
    // installer package.
    const archives = ["zip", "tar-gz", "tar-xz", "7z", ...(platform.startsWith("macos") ? ["dmg"] : [])];
    const unpacked = pinned?.archive
      ? archives.includes(pinned.archive)
      : /\.(zip|tar\.gz|tgz|tar\.xz|7z)$/i.test(pinned?.url ?? "");
    const headless = unpacked && !pinned?.wrapped && ["vst3", "vst2", "clap", "lv2"].includes(pinned?.format ?? "");
    return {
      ...entry,
      installed: false,
      installable: pinned !== undefined,
      install_mode: pinned ? (headless ? "extract" : "installer") : undefined,
    };
  });
}

interface PlatformFolders {
  plugins: [string, number, FolderOrigin][];
  /** Path, instruments, samples, origin. */
  sounds: [string, number, number, FolderOrigin][];
  /** Plugin format per item, in the platform's common order. */
  formats: string[];
}

const LONG = "Studio Libraries/Third-party plugins/Instruments";

const FOLDERS: Record<string, PlatformFolders> = {
  "macos-aarch64": {
    plugins: [
      ["/Library/Audio/Plug-Ins/VST3", 14, "standard"],
      ["/Library/Audio/Plug-Ins/CLAP", 8, "standard"],
      ["/Library/Audio/Plug-Ins/Components", 12, "standard"],
      ["/Users/you/Library/Audio/Plug-Ins/VST3", 3, "standard"],
      ["/Users/you/Library/Application Support/REAPER/UserPlugins/FX", 2, "host_settings"],
      [`/Volumes/Sample Drive/${LONG}/VST3`, 1, "user"],
    ],
    sounds: [
      ["/Users/you/Music/SoundFonts", 3, 0, "standard"],
      ["/Users/you/Documents/MuseScore4/SoundFonts", 1, 0, "standard"],
      ["/Users/you/Music/Ableton/User Library", 2, 1840, "convention"],
      ["/Users/you/Sounds/Orchestra", 6, 412, "user"],
    ],
    formats: ["vst3", "clap", "au"],
  },
  "windows-x86_64": {
    plugins: [
      ["C:\\Program Files\\Common Files\\VST3", 16, "standard"],
      ["C:\\Program Files\\Common Files\\CLAP", 6, "standard"],
      ["C:\\Program Files\\VSTPlugins", 9, "standard"],
      ["C:\\Program Files\\VST3", 2, "convention"],
      ["D:\\Audio\\VST Plugins", 5, "host_settings"],
      [`E:\\${LONG.replaceAll("/", "\\")}\\VST3`, 1, "user"],
    ],
    sounds: [
      ["C:\\Users\\you\\Documents\\SoundFonts", 2, 0, "standard"],
      ["C:\\Users\\you\\Documents\\MuseScore4\\SoundFonts", 1, 0, "standard"],
      ["C:\\Users\\you\\Documents\\Image-Line", 0, 2210, "convention"],
      ["D:\\Samples\\Drums", 0, 640, "user"],
    ],
    formats: ["vst3", "clap", "vst2"],
  },
  "linux-x86_64": {
    plugins: [
      ["/home/you/.vst3", 7, "standard"],
      ["/home/you/.clap", 5, "standard"],
      ["/usr/lib/lv2", 21, "standard"],
      ["/usr/lib/vst3", 4, "standard"],
      ["/opt/audio/clap", 2, "environment"],
      [`/mnt/media/${LONG}/lv2`, 1, "user"],
    ],
    sounds: [
      ["/usr/share/sounds/sf2", 1, 0, "standard"],
      ["/usr/share/sounds/sf3", 1, 0, "standard"],
      ["/home/you/Documents/lmms/samples", 2, 960, "convention"],
      ["/home/you/Sounds/Orchestra", 6, 412, "user"],
    ],
    formats: ["lv2", "vst3", "clap"],
  },
};

/** Neutral names: the harness lists a plausible library, not real products. */
const NAMES = [
  "Analog Poly", "Bass Station", "Chorus Ensemble", "Drum Machine", "Echo Tape", "FM Keys",
  "Granular Cloud", "Hall Reverb", "Ink Compressor", "Juno Strings", "Kick Designer", "Lo-Fi Crusher",
  "Mono Lead", "Noise Gate", "Organ Rotary", "Plate Reverb", "Quad Delay", "Ring Modulator",
  "Spring Reverb", "Tube Saturator", "Unison Pad", "Vocal Tuner", "Wavetable Synth", "Xylo Mallets",
];

function summaryFor(platform: string, sparse: boolean): LocalContentSummary {
  const folders = FOLDERS[platform];
  const formats = folders.formats;
  const count = sparse ? 4 : NAMES.length;
  const plugins: FoundItem[] = NAMES.slice(0, count).map((name, index) => ({
    name,
    detail: index % 3 === 0 ? "Example Audio" : "Open Instruments Collective",
    format: formats[index % formats.length],
    path: `${folders.plugins[0][0]}/${name}`,
    readable: index !== 5,
  }));
  const formatCounts = (items: FoundItem[], order: string[]) =>
    order
      .map((format) => ({ format, count: items.filter((item) => item.format === format).length }))
      .filter((row) => row.count > 0);
  const instruments: FoundItem[] = (sparse ? ["Upright Piano"] : ["Concert Grand", "Felt Piano", "GM Bank", "Nylon Guitar", "Orchestra Strings", "Tape Choir"])
    .map((name, index) => ({
      name,
      detail: folders.sounds[index % folders.sounds.length][0],
      format: ["sf2", "sfz", "sf3"][index % 3],
      path: `${folders.sounds[0][0]}/${name}`,
      readable: true,
    }));
  const scale = (value: number) => (sparse ? Math.min(value, 1) : value);
  const soundFolders = (column: 1 | 2) =>
    folders.sounds.map(([folderPath, instrumentCount, sampleCount, origin]) => ({
      path: folderPath,
      origin,
      exists: true,
      count: scale(column === 1 ? instrumentCount : sampleCount),
      library_id: origin === "user" ? "library" : null,
    }));
  return {
    plugins: {
      total: plugins.length,
      formats: formatCounts(plugins, ["vst3", "clap", "au", "lv2", "vst2"]),
      folders: [
        ...folders.plugins.map(([folderPath, folderCount, origin]) => ({
          path: folderPath, origin, exists: true, count: sparse ? Math.min(folderCount, 1) : folderCount,
        })),
        { path: "/nonexistent", origin: "standard", exists: false, count: 0 },
      ],
      items: plugins,
      truncated: false,
    },
    instruments: {
      total: instruments.length,
      formats: formatCounts(instruments, ["sf2", "sf3", "sfz", "sampler_asset"]),
      folders: soundFolders(1),
      items: instruments,
      truncated: false,
    },
    samples: {
      total: sparse ? 4 : 5_062,
      formats: [],
      folders: soundFolders(2),
      items: [],
      truncated: false,
    },
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
  pluginStore.reset();
  localScan.reset();
  soundPacks.reset();
  listenMock.mockImplementation(async () => () => {});
  i18n.setLocale("en");
});

function answer(platform: string, summary: LocalContentSummary | Promise<LocalContentSummary>) {
  invokeMock.mockImplementation(async (command: string) => {
    if (command === "plugin_catalog") {
      return { platform, entries: resolved(platform), installed: [], source: "bundled" };
    }
    if (command === "local_content_scan" || command === "local_content_summary") return summary;
    if (command === "soundfonts_list") return { dir: "", items: [] };
    if (command === "sound_packs_list") return { dir: "/sounds", items: STARTER_PACKS };
    return null;
  });
}

async function shoot(container: HTMLElement, theme: string, name: string, checks: (page: import("@playwright/test").Page) => Promise<string[]>) {
  const page = await browser.newPage({ viewport: { width: 1280, height: 1600 } });
  try {
    await page.setContent(`<!doctype html><html data-theme="${theme}"><head>
      <meta charset="utf-8"><style>${stylesheet} html,body{margin:0}
      *,*::before,*::after{animation:none!important;transition:none!important}</style></head>
      <body>${container.innerHTML}</body></html>`);
    expect(await checks(page)).toEqual([]);
    await page.screenshot({ path: path.join(SHOT_DIR, `onboarding-${name}.png`), fullPage: true });
  } finally {
    await page.close();
  }
}

/** Everything that must stay inside the card, plus any page-specific checks. */
const fitsCard = (page: import("@playwright/test").Page, selectors: string) =>
  page.evaluate((selectors) => {
    const found: string[] = [];
    const card = document.querySelector(".onboarding-card")!.getBoundingClientRect();
    for (const element of document.querySelectorAll(selectors)) {
      const box = element.getBoundingClientRect();
      if (box.left < card.left - 1 || box.right > card.right + 1) {
        found.push(`overflows: ${element.className} ${element.textContent?.slice(0, 40)}`);
      }
    }
    for (const list of document.querySelectorAll(".onboarding-found-list, .onboarding-folder-list")) {
      const box = list.getBoundingClientRect();
      if (box.height > 200) found.push(`list does not scroll: ${list.className}`);
    }
    return found;
  }, selectors);

describe("first-run scan page layout", () => {
  for (const locale of ["en", "zh-Hans"] as const) {
    it(`${locale}: progress shows both phases`, async () => {
      i18n.setLocale(locale);
      let emit!: (payload: LocalScanProgress) => void;
      listenMock.mockImplementation(async (_event: string, handler: (event: { payload: LocalScanProgress }) => void) => {
        emit = (payload) => handler({ payload });
        return () => {};
      });
      answer("macos-aarch64", new Promise<LocalContentSummary>(() => {}));
      const fixture = render(() => <Onboarding onDone={() => {}} />);
      await new Promise((resolve) => setTimeout(resolve, 0));
      await screen.findByRole("progressbar", { name: i18n.t("onboarding.scan.plugins") });
      emit({ phase: "plugins", done: 17, total: 41, current: "Wavetable Synth.vst3" });
      await screen.findByText(i18n.t("onboarding.scan.readingPlugin", { name: "Wavetable Synth.vst3", done: "17", total: "41" }));
      await shoot(fixture.container, "graphite", `progress-${locale}`, (page) =>
        fitsCard(page, ".onboarding-scan-phase, .onboarding-scan-add"));
    });
  }

  for (const platform of Object.keys(FOLDERS)) {
    for (const locale of ["en", "zh-Hans"] as const) {
      for (const theme of ["graphite", "ivory"]) {
        it(`${platform} ${locale} ${theme}: results and folders fit the card`, async () => {
          i18n.setLocale(locale);
          answer(platform, summaryFor(platform, false));
          const fixture = render(() => <Onboarding onDone={() => {}} />);
          await screen.findByText(i18n.t("onboarding.scan.titleDone"));
          for (const fold of fixture.container.querySelectorAll("details")) fold.setAttribute("open", "");
          await shoot(fixture.container, theme, `scan-${platform}-${locale}-${theme}`, (page) =>
            fitsCard(page, ".onboarding-local-card, .onboarding-found-list li, .onboarding-folder-list li"));
        });

        it(`${platform} ${locale} ${theme}: suggestions fit the card`, async () => {
          i18n.setLocale(locale);
          answer(platform, summaryFor(platform, true));
          const fixture = render(() => <Onboarding onDone={() => {}} />);
          await screen.findByText(i18n.t("onboarding.scan.titleDone"));
          fireEvent.click(screen.getByRole("button", { name: i18n.t("onboarding.next") }));
          await screen.findAllByRole("button", { name: i18n.t("onboarding.plugins.install") });
          await shoot(fixture.container, theme, `picks-${platform}-${locale}-${theme}`, async (page) => {
            const found = await fitsCard(page, ".onboarding-tool");
            const tools = await page.evaluate(() => document.querySelectorAll(".onboarding-tool").length);
            if (tools < 6) found.push(`only ${tools} suggestions`);
            return found;
          });
        });
      }
    }
  }

  for (const locale of ["en", "zh-Hans"] as const) {
    it(`${locale}: a starter pack install fits the card`, async () => {
      i18n.setLocale(locale);
      let emit!: (progress: SoundPackProgress) => void;
      listenMock.mockImplementation(async (event: string, handler: (event: { payload: SoundPackProgress }) => void) => {
        if (event === "sound_pack_install_progress") {
          emit = (progress) => handler({ payload: progress });
        }
        return () => {};
      });
      answer("macos-aarch64", summaryFor("macos-aarch64", true));
      const original = invokeMock.getMockImplementation()!;
      invokeMock.mockImplementation((command: string, ...args: unknown[]) =>
        command === "sound_pack_install" ? new Promise(() => {}) : original(command, ...args));
      const fixture = render(() => <Onboarding onDone={() => {}} />);
      await screen.findByText(i18n.t("onboarding.scan.titleDone"));
      fireEvent.click(screen.getByRole("button", { name: i18n.t("onboarding.next") }));

      const card = (await screen.findByText("GeneralUser GS")).closest("article")!;
      fireEvent.click(card.querySelector(".onboarding-tool-action button")!);
      await waitFor(() => expect(emit).toBeDefined());
      emit({ id: "christian-collins.generaluser-gs", phase: "extract", done: 58, total: 100 });
      await waitFor(() => expect(card.querySelector('[role="progressbar"]')).toHaveAttribute("aria-valuenow", "58"));
      await screen.findByText(i18n.t("onboarding.starters.phase.extract"));

      await shoot(fixture.container, "graphite", `install-${locale}`, (page) =>
        fitsCard(page, ".onboarding-tool, .onboarding-progress, .onboarding-tool-attribution, .onboarding-tool-action"));
    });
  }
});
