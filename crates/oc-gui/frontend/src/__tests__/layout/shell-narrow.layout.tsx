/**
 * Shell scale guard.
 *
 * The interface scale is a webview zoom, so a 1147 px window is a ~764 CSS px
 * viewport at 1.5 and a smaller window goes lower at any scale. This guard
 * renders the real shell — sidebar open, workspace dialog open, projects loaded
 * — inlines the real stylesheet, and measures it in headless Chrome.
 *
 * At 764 and 640 nothing may leave the viewport, the open sidebar must float
 * over the timeline instead of squeezing it, the toolbar must stay one
 * scrolling row with the transport in reach, and the workspace dialog must stay
 * inside the window and never scroll sideways. At 1144 the shell must still
 * share width with the sidebar exactly as it does today.
 *
 * Run with `npm run layout-check`.
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { render } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import path from "node:path";
import { chromium, type Browser, type Page } from "@playwright/test";
import { invokeMock, makeSnapshot, makeTrack, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { workspaceStore } from "../../stores/workspace";
import { panelStore } from "../../stores/panels";
import { i18n } from "../../stores/i18n";
import App from "../../App";

const STYLE_ROOT = path.resolve(process.cwd(), "src/styles");
const TOOLBAR_HEIGHT = 48;
/** The width the operator dragged the sidebar to in order to read the panel. */
const SIDEBAR_REQUEST = 600;
const RESIZE_HANDLE_WIDTH = 4;

/** The narrow viewports the shell has to survive, taller than they are wide. */
const NARROW = [
  { width: 764, height: 550 },
  { width: 640, height: 480 },
];
const NORMAL = { width: 1144, height: 760 };

/**
 * The daw.css manifest followed in order, inline in the fixture page: every
 * import first, then the manifest's own rules, as a browser applies them.
 */
function stylesheet(): string {
  const manifest = readFileSync(path.join(STYLE_ROOT, "daw.css"), "utf8");
  const importPattern = /@import\s+"([^"]+)";/g;
  const imports = [...manifest.matchAll(importPattern)]
    .map((match) => path.resolve(STYLE_ROOT, match[1]));
  expect(imports.length).toBeGreaterThan(10);
  return [
    ...imports.map((file) => `/* ${path.basename(file)} */\n${readFileSync(file, "utf8")}`),
    `/* daw.css */\n${manifest.replace(importPattern, "")}`,
  ].join("\n");
}

function fixtureHtml(body: string): string {
  return `<!doctype html>
<html lang="en" data-platform="macos">
<head><meta charset="utf-8"><title>shell scale guard</title>
<style>
${stylesheet()}
html, body { margin: 0; height: 100%; overflow: hidden; }
#root { height: 100%; }
</style>
</head>
<body><div id="root">${body}</div></body></html>`;
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
    if (cmd === "plugin_catalog") return { platform: "macos-aarch64", entries: [], installed: [] };
    if (cmd === "workspace_current") {
      return {
        path: "/Users/me/Music/albums",
        name: "albums",
        detached: false,
        chosen: true,
        exists: true,
        projects: [
          { name: "night-drive.ocp", path: "/Users/me/Music/albums/night-drive.ocp" },
          { name: "slow-burn.ocp", path: "/Users/me/Music/albums/slow-burn.ocp" },
        ],
        total_projects: 2,
      };
    }
    // No local proxy: the shell under test carries no network notice.
    if (cmd === "detect_local_proxy") return null;
    if (cmd === "workspaces_list") {
      return Array.from({ length: 6 }, (_, index) => ({
        name: `workspace-with-a-rather-long-name-${index + 1}`,
        path: `/Users/me/Music/production/2026/albums-and-sessions/workspace-with-a-rather-long-name-${index + 1}`,
        opened: "2026-11-01T10:00:00Z",
        exists: index !== 4,
      }));
    }
    return {
      ...makeSnapshot({
        tracks: [makeTrack("bass", 0), makeTrack("keys", 1), makeTrack("drums", 2)],
      }),
      file_path: "/Users/me/Music/albums/night-drive.ocp",
      track_count: 3,
      duration_seconds: 96,
    };
  });
  await store.initProject();

  // The operator's state: a project open, a wide sidebar showing, chooser open.
  const { container } = render(() => <App />);
  await new Promise((resolve) => setTimeout(resolve, 50));
  panelStore.setSidebarWidth(SIDEBAR_REQUEST);
  panelStore.openSidebar("ai");
  workspaceStore.openChooser();
  await new Promise((resolve) => setTimeout(resolve, 50));
  markup = container.innerHTML;
});

afterAll(async () => {
  panelStore.setSidebarWidth(320);
  panelStore.closeSidebar();
  workspaceStore.closeChooser();
  await browser?.close();
});

/** Every element position the guard needs, measured in the page. */
interface Measurement {
  viewport: { width: number; height: number };
  docScrollWidth: number;
  outside: { selector: string; left: number; right: number }[];
  toolbar: { height: number; overflowX: string; flexWrap: string; scrollWidth: number; clientWidth: number };
  toolbarHits: { preferences: boolean; browse: boolean };
  transport: { count: number; outside: number };
  main: { left: number; right: number; width: number };
  sidebar: { position: string; left: number; right: number; width: number } | null;
  dialog: {
    left: number; right: number; top: number; bottom: number; width: number;
    scrollWidth: number; clientWidth: number;
    actionsWrap: string; actionsInside: boolean;
  } | null;
  overlay: { overflowY: string; scrollHeight: number; clientHeight: number } | null;
}

/** Renders the fixture at one viewport and reads the layout back. */
async function measure(viewport: { width: number; height: number }): Promise<Measurement> {
  const page: Page = await browser.newPage({ viewport });
  try {
    await page.setContent(fixtureHtml(markup));
    await page.waitForTimeout(80);
    return await page.evaluate(() => {
      const root = document.querySelector(".daw-root") as HTMLElement | null;
      const label = (element: Element) => {
        const cls = typeof element.className === "string" && element.className.trim()
          ? `.${element.className.trim().split(/\s+/).slice(0, 2).join(".")}`
          : "";
        return `${element.tagName.toLowerCase()}${cls}`;
      };
      // Content inside a horizontal scroller is reachable, not clipped: the
      // timeline canvas is wider than the window by design.
      const reachable = (element: Element) => {
        for (let parent = element.parentElement; parent && parent !== root; parent = parent.parentElement) {
          const style = getComputedStyle(parent);
          if ((style.overflowX === "auto" || style.overflowX === "scroll")
            && parent.scrollWidth > parent.clientWidth + 1) return true;
        }
        return false;
      };
      const rect = (selector: string) => {
        const element = document.querySelector(selector);
        if (!element) return null;
        const box = element.getBoundingClientRect();
        return {
          left: box.left, right: box.right, top: box.top, bottom: box.bottom, width: box.width,
          scrollWidth: element.scrollWidth, clientWidth: element.clientWidth,
        };
      };

      const width = window.innerWidth;
      const outside: { selector: string; left: number; right: number }[] = [];
      if (root) {
        for (const element of root.querySelectorAll("*")) {
          const box = element.getBoundingClientRect();
          if (box.width === 0 && box.height === 0) continue;
          if ((box.left < -1 || box.right > width + 1) && !reachable(element)) {
            outside.push({
              selector: label(element),
              left: Math.round(box.left * 10) / 10,
              right: Math.round(box.right * 10) / 10,
            });
          }
        }
      }

      const toolbarHit = (selector: string) => {
        const element = document.querySelector(selector);
        if (!element) return false;
        // The chooser is a modal over the shell; it is not what this guard
        // measures. Hide overlays for the hit test only.
        const overlays = [...document.querySelectorAll<HTMLElement>(".workspace-overlay, .preferences-overlay")];
        const previous = overlays.map((overlay) => overlay.style.display);
        for (const overlay of overlays) overlay.style.display = "none";
        const box = element.getBoundingClientRect();
        const hit = document.elementFromPoint(box.left + box.width / 2, box.top + box.height / 2);
        overlays.forEach((overlay, index) => { overlay.style.display = previous[index]; });
        return !!hit?.closest(".daw-toolbar");
      };
      const bar = document.querySelector(".daw-toolbar");
      const strip = document.querySelector(".toolbar-scroll") as HTMLElement | null;
      const stripStyle = strip ? getComputedStyle(strip) : null;
      const transport = [...document.querySelectorAll(".transport-btn")]
        .map((button) => button.getBoundingClientRect());
      const main = document.querySelector(".daw-main")!.getBoundingClientRect();
      const sidebar = document.querySelector(".right-sidebar");
      const sidebarBox = sidebar ? sidebar.getBoundingClientRect() : null;
      const dialog = rect(".workspace-chooser-dialog");
      const actions = document.querySelector(".workspace-actions");
      const dialogRight = dialog?.right ?? 0;
      const overlay = document.querySelector(".workspace-overlay") as HTMLElement | null;

      return {
        viewport: { width, height: window.innerHeight },
        docScrollWidth: document.documentElement.scrollWidth,
        outside,
        toolbarHits: {
          preferences: toolbarHit(".preferences-btn"),
          browse: toolbarHit(".browse-btn"),
        },
        toolbar: {
          height: Math.round(bar?.getBoundingClientRect().height ?? 0),
          overflowX: stripStyle?.overflowX ?? "",
          flexWrap: stripStyle?.flexWrap ?? "",
          scrollWidth: strip?.scrollWidth ?? 0,
          clientWidth: strip?.clientWidth ?? 0,
        },
        transport: {
          count: transport.length,
          outside: transport.filter((box) => box.left < -1 || box.right > width + 1).length,
        },
        main: {
          left: Math.round(main.left), right: Math.round(main.right), width: Math.round(main.width),
        },
        sidebar: sidebarBox ? {
          position: (sidebar && getComputedStyle(sidebar).position) || "",
          left: Math.round(sidebarBox.left), right: Math.round(sidebarBox.right), width: Math.round(sidebarBox.width),
        } : null,
        dialog: dialog ? {
          left: Math.round(dialog.left), right: Math.round(dialog.right),
          top: Math.round(dialog.top), bottom: Math.round(dialog.bottom), width: Math.round(dialog.width),
          scrollWidth: dialog.scrollWidth, clientWidth: dialog.clientWidth,
          actionsWrap: actions ? getComputedStyle(actions).flexWrap : "",
          actionsInside: actions
            ? [...actions.querySelectorAll("button")].every((button) => button.getBoundingClientRect().right <= dialogRight + 1)
            : false,
        } : null,
        overlay: overlay ? {
          overflowY: getComputedStyle(overlay).overflowY,
          scrollHeight: overlay.scrollHeight,
          clientHeight: overlay.clientHeight,
        } : null,
      };
    });
  } finally {
    await page.close();
  }
}

describe("the shell at a narrow CSS width", () => {
  for (const viewport of NARROW) {
    it(`keeps every element inside ${viewport.width}x${viewport.height}`, async () => {
      const m = await measure(viewport);

      expect(m.viewport.width).toBe(viewport.width);
      // The document itself never scrolls sideways.
      expect(m.docScrollWidth, `document scrollWidth at ${viewport.width}px`).toBeLessThanOrEqual(m.viewport.width);
      // Nothing is clipped at either edge, except content a scroller can reach.
      expect(m.outside, `elements outside ${viewport.width}px: ${JSON.stringify(m.outside)}`).toEqual([]);
    });
  }
});

describe("the shell below 900px", () => {
  for (const viewport of NARROW) {
    it(`floats the open sidebar instead of narrowing the timeline at ${viewport.width}px`, async () => {
      const m = await measure(viewport);

      // The timeline keeps the full window width behind the sidebar.
      expect(m.main.left).toBe(0);
      expect(m.main.width).toBeGreaterThanOrEqual(m.viewport.width - 1);
      // The sidebar is still there, still readable, still inside the window.
      expect(m.sidebar).not.toBeNull();
      expect(m.sidebar!.position).toBe("absolute");
      expect(m.sidebar!.width).toBeGreaterThanOrEqual(220);
      expect(m.sidebar!.right).toBeLessThanOrEqual(m.viewport.width + 1);
      // A width dragged out to 600 px is capped, not run off the edge.
      expect(m.sidebar!.width).toBeLessThanOrEqual(Math.round(m.viewport.width * 0.86));
    });

    it(`keeps the toolbar to one scrolling row with the transport in reach at ${viewport.width}px`, async () => {
      const m = await measure(viewport);

      // One row, not two or three wrapped ones.
      expect(m.toolbar.height).toBeLessThanOrEqual(TOOLBAR_HEIGHT + 1);
      // The strip scrolls horizontally instead of growing taller.
      expect(m.toolbar.overflowX).toBe("auto");
      expect(m.toolbar.flexWrap).toBe("nowrap");
      expect(m.toolbar.scrollWidth).toBeGreaterThan(m.toolbar.clientWidth);
      // Every transport control is visible without scrolling (write automation
      // now lives in the settings popover).
      expect(m.transport.count).toBe(6);
      expect(m.transport.outside, "transport controls outside the viewport").toBe(0);
      // The floating sidebar cannot paint over the pinned actions.
      expect(m.toolbarHits, `toolbar actions covered at ${viewport.width}px`).toEqual({
        preferences: true,
        browse: true,
      });
    });
  }
});

describe("the workspace dialog at a narrow width", () => {
  for (const viewport of NARROW) {
    it(`stays inside and never scrolls sideways at ${viewport.width}px`, async () => {
      const m = await measure(viewport);
      expect(m.dialog).not.toBeNull();
      const dialog = m.dialog!;

      expect(dialog.left).toBeGreaterThanOrEqual(-1);
      expect(dialog.right).toBeLessThanOrEqual(m.viewport.width + 1);
      expect(dialog.width).toBeLessThanOrEqual(m.viewport.width);
      // No horizontal clipping of the body: the dialog scrolls vertically only.
      expect(dialog.scrollWidth).toBeLessThanOrEqual(dialog.clientWidth + 1);
      // The action buttons wrap instead of running off the card.
      expect(dialog.actionsWrap).toBe("wrap");
      expect(dialog.actionsInside).toBe(true);
      // A dialog taller than the window scrolls in its overlay.
      expect(m.overlay!.overflowY).toBe("auto");
      if (dialog.bottom > m.viewport.height) {
        expect(m.overlay!.scrollHeight).toBeGreaterThan(m.overlay!.clientHeight);
      }
    });
  }
});

describe("the shell at a normal width", () => {
  it(`still shares width with the sidebar at ${NORMAL.width}px`, async () => {
    const m = await measure(NORMAL);

    expect(m.docScrollWidth).toBeLessThanOrEqual(m.viewport.width);
    expect(m.outside).toEqual([]);
    // The sidebar is in flow, keeps the width it was dragged to, and takes it
    // out of the timeline.
    expect(m.sidebar).not.toBeNull();
    expect(m.sidebar!.position).toEqual("static");
    expect(m.sidebar!.width).toBe(SIDEBAR_REQUEST);
    expect(m.main.width + m.sidebar!.width + RESIZE_HANDLE_WIDTH).toBe(m.viewport.width);
    // The toolbar is one row at every width; the brand corner keeps its size,
    // so the strip may scroll, but the pinned actions stay on screen.
    expect(m.toolbar.flexWrap).toBe("nowrap");
    expect(m.toolbar.overflowX).toBe("auto");
    // The chooser is centred and whole.
    const dialog = m.dialog!;
    expect(dialog.left).toBeGreaterThanOrEqual(-1);
    expect(dialog.right).toBeLessThanOrEqual(m.viewport.width + 1);
    expect(dialog.scrollWidth).toBeLessThanOrEqual(dialog.clientWidth + 1);
    expect(dialog.width).toBe(560);
  });
});
