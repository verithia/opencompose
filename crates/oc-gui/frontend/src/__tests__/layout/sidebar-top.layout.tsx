/**
 * The right sidebar's panel switcher sits in one horizontal row above the panel.
 *
 * This guard renders the shell with the sidebar open, inlines the stylesheet,
 * and measures the header row in headless Chrome: the tabs share one line at
 * the top, the open panel title follows them, the collapse control sits at the
 * right end, and the panel content starts below the row at the sidebar's full width.
 *
 * Run with `npm run layout-check`.
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { render } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import path from "node:path";
import { chromium, type Browser } from "@playwright/test";
import { invokeMock, makeSnapshot, makeTrack, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { panelStore } from "../../stores/panels";
import { i18n } from "../../stores/i18n";
import App from "../../App";

const STYLE_ROOT = path.resolve(process.cwd(), "src/styles");
/** The width the operator drags the sidebar to; 220 px is its CSS minimum. */
const WIDE = 320;
const NARROW = 220;

function stylesheet(): string {
  const manifest = readFileSync(path.join(STYLE_ROOT, "daw.css"), "utf8");
  const imports = [...manifest.matchAll(/@import\s+"([^"]+)";/g)]
    .map((match) => path.resolve(STYLE_ROOT, match[1]));
  expect(imports.length).toBeGreaterThan(10);
  return imports
    .map((file) => `/* ${path.basename(file)} */\n${readFileSync(file, "utf8")}`)
    .join("\n");
}

function fixtureHtml(body: string): string {
  return `<!doctype html>
<html lang="en" data-platform="macos">
<head><meta charset="utf-8"><title>sidebar top row</title>
<style>
${stylesheet()}
html, body { margin: 0; height: 100%; overflow: hidden; }
#root { height: 100%; }
</style>
</head>
<body><div id="root">${body}</div></body></html>`;
}

interface Box { left: number; right: number; top: number; bottom: number; width: number; height: number }
interface RowGeometry {
  sidebar: Box;
  topbar: Box;
  content: Box;
  tabs: Box[];
  name: { text: string; box: Box; truncated: boolean };
  collapse: Box;
  topbarIsFirstChild: boolean;
  topbarOverflow: number;
  sidebarOverflow: number;
  collapseLabel: string;
}

let browser: Browser;
let markup = "";

beforeAll(async () => {
  browser = await chromium.launch({ headless: true });
  i18n.setLocale("en");
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "take_startup_project") return null;
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "get_undo_state") {
      return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    }
    if (cmd === "get_undo_history") return [];
    if (cmd === "list_recoveries") return [];
    if (cmd === "scan_plugins") return [];
    if (cmd === "workspace_current") {
      return { path: "/Users/me/songs", name: "songs", detached: false, chosen: true, projects: [], total_projects: 0 };
    }
    if (cmd === "workspaces_list") return [];
    return {
      ...makeSnapshot({ tracks: [makeTrack("bass", 0), makeTrack("keys", 1), makeTrack("drums", 2)] }),
      file_path: "/Users/me/songs/night-drive.ocp",
      track_count: 3,
      duration_seconds: 96,
    };
  });
  await store.initProject();

  const { container } = render(() => <App />);
  await new Promise((resolve) => setTimeout(resolve, 50));
  panelStore.setSidebarWidth(WIDE);
  panelStore.openSidebar("dsl");
  await new Promise((resolve) => setTimeout(resolve, 50));
  markup = container.innerHTML;
});

afterAll(async () => {
  panelStore.setSidebarWidth(320);
  panelStore.closeSidebar();
  await browser?.close();
});

/** Renders the captured shell at one sidebar width and reads the row back. */
async function measureSidebar(width: number): Promise<RowGeometry> {
  const page = await browser.newPage({ viewport: { width: 1144, height: 760 } });
  try {
    await page.setContent(fixtureHtml(markup));
    await page.evaluate((requested) => {
      const sidebar = document.querySelector(".right-sidebar") as HTMLElement | null;
      if (sidebar) sidebar.style.width = `${requested}px`;
    }, width);
    await page.waitForTimeout(80);
    return await page.evaluate(() => {
      const sidebar = document.querySelector(".right-sidebar") as HTMLElement;
      const topbar = sidebar.querySelector(".sidebar-topbar") as HTMLElement;
      const content = sidebar.querySelector(".sidebar-content") as HTMLElement;
      const name = sidebar.querySelector(".sidebar-tab-name") as HTMLElement;
      const collapse = sidebar.querySelector(".sidebar-collapse") as HTMLElement;
      const box = (element: Element) => {
        const rect = element.getBoundingClientRect();
        return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom, width: rect.width, height: rect.height };
      };
      const tabs = [...sidebar.querySelectorAll(".sidebar-tab")].map(box);
      return {
        sidebar: box(sidebar),
        topbar: box(topbar),
        content: box(content),
        tabs,
        name: {
          text: (name.textContent ?? "").trim(),
          box: box(name),
          truncated: name.scrollWidth > name.clientWidth + 1,
        },
        collapse: box(collapse),
        topbarIsFirstChild: sidebar.firstElementChild === topbar,
        topbarOverflow: topbar.scrollWidth - topbar.clientWidth,
        sidebarOverflow: sidebar.scrollWidth - sidebar.clientWidth,
        collapseLabel: collapse.getAttribute("aria-label") ?? "",
      };
    });
  } finally {
    await page.close();
  }
}

describe("the sidebar panel switcher", () => {
  it("renders the tabs in one row above the panel, named after the open one", async () => {
    const m = await measureSidebar(WIDE);

    expect(m.topbarIsFirstChild).toBe(true);
    // One line, directly under the sidebar's top edge.
    expect(m.topbar.top).toBeCloseTo(m.sidebar.top, 0);
    expect(m.topbar.height).toBeLessThanOrEqual(31);
    expect(m.tabs).toHaveLength(4);
    const tops = m.tabs.map((tab) => Math.round(tab.top));
    expect(new Set(tops).size, `tab rows: ${tops.join(",")}`).toBe(1);
    // Left to right, in the declared order, all inside the row.
    for (let index = 0; index < m.tabs.length; index += 1) {
      const tab = m.tabs[index];
      expect(tab.top).toBeGreaterThanOrEqual(m.topbar.top - 1);
      expect(tab.bottom).toBeLessThanOrEqual(m.topbar.bottom + 1);
      if (index > 0) expect(tab.left).toBeGreaterThan(m.tabs[index - 1].left);
    }
    // The open panel is named after the icons, and the row is one line.
    expect(m.name.text).toBe(i18n.t("panel.dsl"));
    expect(m.name.box.left).toBeGreaterThanOrEqual(m.tabs[3].right - 1);
    expect(m.name.box.bottom).toBeLessThanOrEqual(m.topbar.bottom + 1);
    // The collapse control closes the row on the right.
    expect(m.collapseLabel).toBe(i18n.t("panel.collapseSidebar"));
    expect(m.collapse.width).toBeGreaterThanOrEqual(20);
    expect(m.collapse.left).toBeGreaterThanOrEqual(m.name.box.left);
    expect(m.collapse.right).toBeLessThanOrEqual(m.topbar.right + 1);
    expect(m.collapse.right).toBeGreaterThan(m.tabs[3].right);
    expect(m.topbarOverflow).toBeLessThanOrEqual(1);
  });

  it("gives the panel the width the old tab column took", async () => {
    const m = await measureSidebar(WIDE);

    // The content starts under the row and spans the sidebar: no 36 px column
    // is reserved on the left.
    expect(m.content.top).toBeGreaterThanOrEqual(m.topbar.bottom - 1);
    expect(Math.abs(m.content.top - m.topbar.bottom)).toBeLessThanOrEqual(1);
    expect(m.content.left - m.sidebar.left).toBeLessThanOrEqual(1);
    expect(m.sidebar.right - m.content.right).toBeLessThanOrEqual(1);
    expect(m.sidebar.width - m.content.width).toBeLessThanOrEqual(1);
    expect(m.sidebarOverflow).toBeLessThanOrEqual(1);
  });

  it("keeps the row one line wide at the 220 px minimum", async () => {
    const m = await measureSidebar(NARROW);

    expect(m.sidebar.width).toBeLessThanOrEqual(NARROW + 1);
    expect(m.topbarOverflow).toBeLessThanOrEqual(1);
    expect(m.sidebarOverflow).toBeLessThanOrEqual(1);
    // Everything in the row stays inside it, including the collapse control.
    expect(m.collapse.right).toBeLessThanOrEqual(m.topbar.right + 1);
    expect(m.collapse.width).toBeGreaterThanOrEqual(20);
    expect(m.name.box.right).toBeLessThanOrEqual(m.topbar.right + 1);
    expect(new Set(m.tabs.map((tab) => Math.round(tab.top))).size).toBe(1);
    expect(m.content.width).toBeGreaterThan(NARROW - 2);
  });
});
