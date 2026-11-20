/**
 * Samples panel height guard.
 *
 * The samples panel lives in the bottom panel, whose default split is the
 * shortest surface in the app. This guard renders the real component with the
 * real stylesheet, runs a search that returns local and market results, and
 * measures what the panel spends on chrome versus results at 1440 and 620 px:
 * the toolbar stays one line when it fits, every row stays one line, and a
 * 330 px panel shows several results instead of a header and half a row.
 *
 * Run with `npm run layout-check`.
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import path from "node:path";
import { chromium, type Browser } from "@playwright/test";
import { invokeMock, makeSnapshot } from "../setup";
import { store } from "../../stores/project";
import { i18n } from "../../stores/i18n";
import { SampleMarketPanel } from "../../components/SampleMarketPanel";

const STYLE_ROOT = path.resolve(process.cwd(), "src/styles");
const PANEL_CSS = path.resolve(process.cwd(), "src/components/SampleMarketPanel.css");

/** The daw.css manifest followed in order, inline in the fixture page. */
function stylesheet(): string {
  const manifest = readFileSync(path.join(STYLE_ROOT, "daw.css"), "utf8");
  const imports = [...manifest.matchAll(/@import\s+"([^"]+)";/g)].map((match) =>
    path.resolve(STYLE_ROOT, match[1]),
  );
  return [
    ...imports.map((file) => readFileSync(file, "utf8")),
    readFileSync(PANEL_CSS, "utf8"),
  ].join("\n");
}

const PROVIDERS = [
  { id: "openverse", label: "Openverse", requires_key: false, kinds: [], license: "Aggregated per result.", search: true, download: true, available: true, reason: null },
  { id: "wikimedia-commons", label: "Wikimedia Commons", requires_key: false, kinds: [], license: "Per file.", search: true, download: true, available: true, default_search: true, reason: null },
  { id: "internet-archive", label: "Internet Archive", requires_key: false, kinds: [], license: "Per file.", search: true, download: true, available: true, reason: null },
  { id: "freesound", label: "Freesound", requires_key: true, kinds: [], license: "Per sound label.", search: true, download: true, available: false, reason: "needs an API key" },
  { id: "jamendo", label: "Jamendo", requires_key: false, kinds: [], license: "Per file.", search: true, download: true, available: true, reason: null },
];

const LOCAL_ITEMS = Array.from({ length: 8 }, (_, index) => ({
  path: `/Users/me/Samples/Drums/Snare ${index + 1}.wav`,
  name: `Snare ${index + 1}`,
  folder: "/Users/me/Samples",
  relative: `Drums/Snare ${index + 1}.wav`,
  format: "wav",
  kind: "drum_one_shot",
  duration_seconds: 0.3 + index / 10,
}));

const MARKET_ITEMS = [
  {
    id: "openverse:snare",
    provider: "openverse",
    provider_name: "Openverse",
    name: "Snare hit 01",
    kind: "drum_one_shot",
    duration_seconds: 0.42,
    channels: 1,
    sample_rate: 44100,
    license: { id: "cc0-1.0", name: "CC0 1.0", url: null, attribution_required: false, share_alike: false, non_commercial: false, no_derivatives: false },
    attribution: "bassmosphere — CC0 1.0 — https://example.test/1",
    source_url: "https://example.test/1",
    preview_url: "https://example.test/1.mp3",
    download_url: "https://example.test/1.wav",
    quality: "original",
    tags: [],
    license_ok: true,
  },
  {
    id: "wikimedia-commons:snare-roll",
    provider: "wikimedia-commons",
    provider_name: "Wikimedia Commons",
    name: "Field snare rolls",
    kind: "loop",
    duration_seconds: 6.4,
    channels: 2,
    sample_rate: 48000,
    license: { id: "cc-by-4.0", name: "CC BY 4.0", url: null, attribution_required: true, share_alike: false, non_commercial: false, no_derivatives: false },
    attribution: "Preliner — CC BY 4.0 — https://example.test/2",
    source_url: "https://example.test/2",
    preview_url: "https://example.test/2.mp3",
    download_url: "https://example.test/2.wav",
    quality: "original",
    tags: [],
    license_ok: true,
  },
];

const EXTRA_MARKET_ITEMS = Array.from({ length: 4 }, (_, index) => ({
  ...MARKET_ITEMS[index % MARKET_ITEMS.length],
  id: `archive:snare-${index}`,
  provider: "internet-archive",
  provider_name: "Internet Archive",
  name: `Archive snare ${index + 1}`,
  attribution: `Creator ${index + 1} — CC0 1.0 — https://example.test/a${index}`,
}));

const ROWS = [...MARKET_ITEMS, ...EXTRA_MARKET_ITEMS];

async function fixtureHtml(width: number, height: number): Promise<string> {
  const { container, unmount } = render(() => <SampleMarketPanel />);
  // The search fans out to the markets the provider list enables, so wait for
  // the list before submitting or the search has nothing to ask.
  await waitFor(() =>
    expect(container.querySelector(".sample-market-provider")).not.toBeNull(),
  );
  const input = container.querySelector(".sample-market-search") as HTMLInputElement;
  fireEvent.input(input, { target: { value: "snare" } });
  fireEvent.submit(container.querySelector(".sample-market-header") as HTMLFormElement);
  await waitFor(() =>
    expect(
      container.querySelectorAll(".sample-market-row:not(.sample-market-row-skeleton)").length,
    ).toBeGreaterThanOrEqual(14),
  );
  const markup = container.innerHTML;
  unmount();
  return `<!doctype html>
<html lang="en" data-theme="dark">
<head><meta charset="utf-8"><title>samples panel guard</title>
<style>
${stylesheet()}
html, body { margin: 0; }
.sample-market-panel { width: ${width}px; height: ${height}px; }
</style></head>
<body>${markup}</body></html>`;
}

interface PanelReport {
  toolbarHeight: number;
  listHeight: number;
  panelScrollWidth: number;
  panelClientWidth: number;
  rowHeights: number[];
  visibleRows: number;
  columns: number;
  firstLineTops: number[];
  durationDisplay: string;
  commercialLabelDisplay: string;
}

let browser: Browser;

beforeAll(async () => {
  browser = await chromium.launch({ headless: true });
  i18n.setLocale("en");
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "sample_market_providers") return PROVIDERS;
    if (cmd === "local_sample_search") {
      return { items: LOCAL_ITEMS, truncated: false, folders: 3 };
    }
    if (cmd === "sample_market_search") {
      return {
        status: "ok",
        query: "snare",
        kind: null,
        offset: 0,
        limit: 20,
        providers: PROVIDERS,
        pages: [],
        count: ROWS.length,
        next_offset: null,
        cursor: "next-page",
        items: ROWS,
      };
    }
    return makeSnapshot();
  });
  await store.initProject();
});

afterAll(async () => {
  await browser?.close();
});

async function report(width: number, height: number): Promise<PanelReport> {
  const page = await browser.newPage({ viewport: { width: width + 40, height: height + 40 } });
  try {
    await page.setContent(await fixtureHtml(width, height));
    return await page.evaluate(() => {
      const panel = document.querySelector(".sample-market-panel") as HTMLElement;
      const toolbar = document.querySelector(".sample-market-toolbar") as HTMLElement;
      const list = document.querySelector(".sample-market-list") as HTMLElement;
      const listBox = list.getBoundingClientRect();
      const rows = [...list.querySelectorAll<HTMLElement>(
        ".sample-market-row:not(.sample-market-row-skeleton)",
      )];
      const grid = document.querySelector(".sample-market-grid") as HTMLElement;
      const duration = document.querySelector(".sample-market-meta .sample-market-duration") as HTMLElement;
      const commercial = document.querySelector(".sample-market-check > span") as HTMLElement;
      return {
        toolbarHeight: toolbar.getBoundingClientRect().height,
        listHeight: listBox.height,
        panelScrollWidth: panel.scrollWidth,
        panelClientWidth: panel.clientWidth,
        rowHeights: rows.map((row) => row.getBoundingClientRect().height),
        visibleRows: rows.filter((row) => {
          const box = row.getBoundingClientRect();
          return box.top >= listBox.top - 1 && box.bottom <= listBox.bottom + 1;
        }).length,
        columns: getComputedStyle(grid).gridTemplateColumns.split(" ").length,
        firstLineTops: rows.slice(0, 2).map((row) => Math.round(row.getBoundingClientRect().top)),
        durationDisplay: getComputedStyle(duration).display,
        commercialLabelDisplay: getComputedStyle(commercial).display,
      };
    });
  } finally {
    await page.close();
  }
}

describe("the samples panel in a short bottom panel", () => {
  it("spends 330 px on results, not chrome, at full width", async () => {
    const panel = await report(1440, 330);
    // Search, duration, licence, kinds and markets share one toolbar line.
    expect(panel.toolbarHeight).toBeLessThanOrEqual(36);
    expect(panel.listHeight).toBeGreaterThanOrEqual(260);
    // Two results share each line, every result is one line, and the short
    // panel still shows more than a dozen of them.
    expect(panel.columns).toBe(2);
    expect(panel.firstLineTops[0]).toBe(panel.firstLineTops[1]);
    expect(Math.max(...panel.rowHeights)).toBeLessThanOrEqual(26);
    expect(panel.visibleRows).toBeGreaterThanOrEqual(12);
    expect(panel.panelScrollWidth).toBeLessThanOrEqual(panel.panelClientWidth + 1);
    expect(panel.durationDisplay).not.toBe("none");
    expect(panel.commercialLabelDisplay).not.toBe("none");
  });

  it("wraps to two toolbar lines and drops details at half width", async () => {
    const panel = await report(620, 330);
    // The search line and the filter line; never a third row per control.
    expect(panel.toolbarHeight).toBeGreaterThan(36);
    expect(panel.toolbarHeight).toBeLessThanOrEqual(70);
    expect(panel.listHeight).toBeGreaterThanOrEqual(230);
    // One column at this width: a 300 px cell cannot hold a result.
    expect(panel.columns).toBe(1);
    expect(Math.max(...panel.rowHeights)).toBeLessThanOrEqual(26);
    expect(panel.visibleRows).toBeGreaterThanOrEqual(6);
    // A narrow panel drops detail text rather than scrolling sideways or
    // clipping a duration mid-value.
    expect(panel.panelScrollWidth).toBeLessThanOrEqual(panel.panelClientWidth + 1);
    expect(panel.durationDisplay).toBe("none");
    expect(panel.commercialLabelDisplay).toBe("none");
  });
});
