/**
 * Toolbar scale guard.
 *
 * The interface scale is a webview zoom, so the CSS viewport shrinks as the
 * scale grows: a 1400 px window is 933 CSS px at 1.5 and 700 at 2. This guard
 * renders the real Toolbar, inlines the real stylesheet, and measures every
 * `.toolbar-btn` in headless Chrome at both scales: each one must sit inside the
 * viewport, or — below 900 px, where the strip becomes a horizontal scroller —
 * inside that scroller. Preferences and Browse stay pinned at every width.
 *
 * Run with `npm run layout-check`.
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { render, waitFor } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import path from "node:path";
import { chromium, type Browser } from "@playwright/test";
import { invokeMock, makeSnapshot, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { i18n } from "../../stores/i18n";
import { Toolbar } from "../../components/Toolbar";
import { Timeline } from "../../components/Timeline";
import { Brand } from "../../components/Brand";
import { AiPanel } from "../../components/AiPanel";

const STYLE_ROOT = path.resolve(process.cwd(), "src/styles");
const WINDOW_WIDTH = 1400;
const SCALES = [1.5, 2] as const;

/** The daw.css manifest followed in order, inline in the fixture page. */
function stylesheet(): string {
  const manifest = readFileSync(path.join(STYLE_ROOT, "daw.css"), "utf8");
  const imports = [...manifest.matchAll(/@import\s+"([^"]+)";/g)]
    .map((match) => path.resolve(STYLE_ROOT, match[1]));
  expect(imports.length).toBeGreaterThan(10);
  return imports
    .map((file) => `/* ${path.basename(file)} */\n${readFileSync(file, "utf8")}`)
    .join("\n");
}

function mountFixture(includeTimeline: boolean, includeAi = false) {
  return render(() => <>
    <Brand />
    <Toolbar />
    {includeTimeline && <div class="daw-body"><div class="daw-main">
      <div class="split-top" style={{ height: "100%" }}><Timeline /></div>
    </div>{includeAi && <aside class="right-sidebar" style={{ width: "360px" }}><AiPanel /></aside>}</div>}
  </>);
}

/** Freeze the actual controls after their owning fixture has initialized. */
function frozenFixtureHtml(container: HTMLElement, scale: number, platform: string): string {
  for (const input of container.querySelectorAll("input")) {
    input.setAttribute("value", input.value);
  }
  for (const select of container.querySelectorAll("select")) {
    for (const option of select.options) {
      if (option.value === select.value) option.setAttribute("selected", "");
      else option.removeAttribute("selected");
    }
  }
  const markup = container.innerHTML;
  return `<!doctype html>
<html lang="en" data-platform="${platform}" style="--ui-scale: ${scale}">
<head>
<meta charset="utf-8">
<title>toolbar scale guard</title>
<style>
${stylesheet()}
html, body { margin: 0; height: 100%; }
.daw-root { height: 100vh; }
</style>
</head>
<body><div class="daw-root">${markup}</div></body>
</html>`;
}

function fixtureHtml(scale: number, platform = "macos", includeTimeline = false): string {
  const mounted = mountFixture(includeTimeline);
  try { return frozenFixtureHtml(mounted.container, scale, platform); }
  finally { mounted.unmount(); }
}

async function themeFixtureHtml(): Promise<string> {
  const mounted = mountFixture(true, true);
  try {
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("sessions_list", {}));
    // The session read is the final mocked startup read. Let its resolved
    // continuation and listener registrations finish while their owner is live.
    await new Promise<void>(resolve => setTimeout(resolve, 0));
    expect(mounted.container.querySelectorAll(".ai-quick-btn")).toHaveLength(4);
    return frozenFixtureHtml(mounted.container, 1, "macos");
  } finally { mounted.unmount(); }
}

interface ButtonBox {
  label: string;
  left: number;
  right: number;
  width: number;
  top: number;
  bottom: number;
  reachable: boolean;
}

let browser: Browser;
const unexpectedCommands: string[] = [];

beforeAll(async () => {
  browser = await chromium.launch({ headless: true });
  i18n.setLocale("en");
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string) => {
    switch (cmd) {
      case "get_project": return makeSnapshot();
      case "get_transport_state": return TRANSPORT_STOPPED();
      case "get_undo_state": return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
      case "get_undo_history":
      case "scan_plugins":
      case "sessions_list":
      case "ai_toolsets":
      case "ai_tools":
      case "workspaces_list": return [];
      case "set_loop_region": return { enabled: false, start_beats: null, end_beats: null };
      case "ai_batch_recover":
      case "ai_active_turn":
      case "preview_state": return null;
      case "load_ai_config": return { version: 2, active_provider: "", providers: [] };
      case "save_ai_config": return { path: "fixture-ai.yaml", warnings: [] };
      case "ai_key_status": return false;
      case "ai_warm_key": return undefined;
      case "ai_catalog": return { version: "fixture", providers: [] };
      case "ai_model_limits": return { provider_id: "fixture", model_id: "fixture", bundled: null, overridden: null };
      case "workspace_current": return { path: null, name: null, detached: true, projects: [] };
      default:
        unexpectedCommands.push(cmd);
        throw new Error(`Unexpected layout fixture command: ${cmd}`);
    }
  });
  await store.initProject();
});

afterAll(async () => {
  await browser?.close();
  expect(unexpectedCommands, "layout fixture received unhandled IPC").toEqual([]);
});

describe("the toolbar keeps its rendered size as the scale grows", () => {
  it("fits one row at 1.5x without horizontal overflow", async () => {
    const page = await browser.newPage({ viewport: { width: 764, height: 700 } });
    try {
      await page.setContent(fixtureHtml(1.5));
      const m = await page.evaluate(() => {
        const bar = document.querySelector(".daw-toolbar")!;
        const strip = document.querySelector(".toolbar-scroll")!;
        const box = bar.getBoundingClientRect();
        const btn = document.querySelector(".toolbar-btn:not(.text-btn)")!.getBoundingClientRect();
        return {
          height: Math.round(box.height),
          scrollWidth: strip.scrollWidth,
          clientWidth: strip.clientWidth,
          buttonWidth: btn.width,
          actionsRight: document.querySelector(".toolbar-actions")!.getBoundingClientRect().right,
          viewportWidth: window.innerWidth,
        };
      });
      // The controls are divided by the scale, so the row keeps its rendered
      // size and the pinned actions stay on screen even when the strip scrolls.
      expect(m.height).toBeLessThanOrEqual(48);
      expect(m.buttonWidth).toBeLessThan(30);
      expect(m.actionsRight).toBeLessThanOrEqual(m.viewportWidth + 1);
    } finally {
      await page.close();
    }
  });
});

describe("the toolbar with a window-controls block at 600px", () => {
  it("keeps Preferences and Browse inside the viewport", async () => {
    const page = await browser.newPage({ viewport: { width: 600, height: 900 } });
    try {
      await page.setContent(fixtureHtml(2));
      // The real app appends window controls after the actions; the strip must
      // yield before anything pinned to the right does.
      await page.evaluate(() => {
        const bar = document.querySelector(".daw-toolbar");
        const controls = document.createElement("div");
        controls.className = "window-controls";
        controls.textContent = "controls";
        controls.style.cssText = "flex:0 0 120px;width:120px";
        bar?.appendChild(controls);
      });
      for (const selector of [".preferences-btn", ".browse-btn"]) {
        const box = await page.locator(selector).boundingBox();
        expect(box, selector).not.toBeNull();
        expect(box!.x + box!.width, selector).toBeLessThanOrEqual(600 + 0.5);
        expect(box!.x, selector).toBeGreaterThanOrEqual(0);
      }
    } finally {
      await page.close();
    }
  });
});

describe("the toolbar at a large interface scale", () => {
  for (const scale of SCALES) {
    it(`keeps every control inside a ${WINDOW_WIDTH}px window at scale ${scale}`, async () => {
      const width = Math.round(WINDOW_WIDTH / scale);
      const page = await browser.newPage({ viewport: { width, height: 900 } });
      try {
        await page.setContent(fixtureHtml(scale));

        const boxes: ButtonBox[] = await page.evaluate(() => {
          const strip = document.querySelector(".toolbar-scroll");
          // Below 900 px the strip scrolls, so a control past the right edge is
          // reachable as long as it sits in that scroller.
          const scrolls = !!strip && strip.scrollWidth > strip.clientWidth + 1;
          return [...document.querySelectorAll(".toolbar-btn")].map((element) => {
            const rect = element.getBoundingClientRect();
            return {
              label:
                element.getAttribute("aria-label")
                ?? (element.textContent ?? "").trim()
                ?? "?",
              left: rect.left,
              right: rect.right,
              width: rect.width,
              top: rect.top,
              bottom: rect.bottom,
              reachable: scrolls && !!strip?.contains(element),
            };
          });
        });

        // The bar carries a real control set, not a truncated one.
        expect(boxes.length).toBeGreaterThanOrEqual(18);

        const outside = boxes.filter(
          (box) => box.width <= 0 || box.left < -0.5 || (box.right > width + 0.5 && !box.reachable),
        );
        expect(
          outside,
          `controls outside the ${width}px viewport: ${JSON.stringify(outside)}`,
        ).toEqual([]);

        // The two actions a large scale could push away are reachable.
        const preferences = await page.locator(".preferences-btn").boundingBox();
        const browse = await page.locator(".browse-btn").boundingBox();
        expect(preferences).not.toBeNull();
        expect(browse).not.toBeNull();
        expect(preferences!.x + preferences!.width).toBeLessThanOrEqual(width + 0.5);
        expect(browse!.x + browse!.width).toBeLessThanOrEqual(width + 0.5);

        // Nothing spills out of the document either.
        const overflow = await page.evaluate(
          () => document.documentElement.scrollWidth - window.innerWidth,
        );
        expect(overflow).toBeLessThanOrEqual(1);
      } finally {
        await page.close();
      }
    });
  }
});


describe("the brand corner leaves the actual timeline controls reachable", () => {
  for (const platform of ["macos", "windows", "linux"]) {
    for (const scale of [0.7, 1, 1.5]) {
      it(`${platform} at ${scale}x keeps Add Track below the brand at ordinary and narrow widths`, async () => {
        expect(store.project.tracks).toHaveLength(0);
        const page = await browser.newPage();
        try {
          for (const width of [Math.round(WINDOW_WIDTH / scale), 640]) {
            await page.setViewportSize({ width, height: Math.round(900 / scale) });
            await page.setContent(fixtureHtml(scale, platform, true));
            const geometry = await page.evaluate(() => {
              const brand = document.querySelector(".brand-block")!.getBoundingClientRect();
              const toolbar = document.querySelector(".daw-toolbar")!.getBoundingClientRect();
              const body = document.querySelector(".daw-body")!.getBoundingClientRect();
              const spacer = document.querySelector(".timeline-ruler-spacer")!.getBoundingClientRect();
              const add = document.querySelector<HTMLButtonElement>(".track-headers .toolbar-btn")!;
              const addBox = add.getBoundingClientRect();
              const row = add.parentElement!.getBoundingClientRect();
              const reachable = [0.25, 0.5, 0.75].map(fraction => {
                const hit = document.elementFromPoint(addBox.left + addBox.width / 2, addBox.top + addBox.height * fraction);
                return hit === add || (hit !== null && add.contains(hit));
              });
              return {
                brandBottom: brand.bottom, rowTop: row.top, spacerBottom: spacer.bottom,
                toolbarBottom: toolbar.bottom, bodyTop: body.top,
                addWidth: addBox.width, addHeight: addBox.height, reachable,
              };
            });
            expect(geometry.brandBottom, `${width}px: brand paints into the add-track row`).toBeLessThanOrEqual(geometry.rowTop + 0.5);
            expect(geometry.brandBottom).toBeLessThanOrEqual(geometry.spacerBottom + 0.5);
            expect(geometry.bodyTop).toBeGreaterThanOrEqual(geometry.toolbarBottom - 0.5);
            expect(geometry.addWidth).toBeGreaterThan(0);
            expect(geometry.addHeight).toBeGreaterThan(0);
            expect(geometry.reachable, `${width}px: Add Track is covered`).toEqual([true, true, true]);
          }
        } finally { await page.close(); }
      });
    }
  }
});


describe("the brand lockup sits below the caption row", () => {
  for (const platform of ["macos", "windows", "linux"]) {
    it(`${platform} keeps the lockup in the lower half of the toolbar band`, async () => {
      const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
      try {
        await page.setContent(fixtureHtml(1, platform));
        const geometry = await page.evaluate(() => {
          const lockup = document.querySelector(".brand-lockup")!.getBoundingClientRect();
          const toolbar = document.querySelector(".daw-toolbar")!.getBoundingClientRect();
          return { top: lockup.top - toolbar.top, height: toolbar.height };
        });
        // A caption row owns the top of the window on every platform, so the
        // wordmark sits in the lower half of the band instead of against it.
        expect(geometry.top).toBeGreaterThanOrEqual(geometry.height * 0.5);
      } finally { await page.close(); }
    });
  }
});

describe("custom buttons follow live theme changes without replacing their DOM", () => {
  it("updates toolbar, Add Track and AI quick actions through light-dark-light", async () => {
    const page = await browser.newPage({ viewport: { width: 1400, height: 900 } });
    try {
      await page.setContent(await themeFixtureHtml());
      await page.evaluate(() => {
        const selectors = [".preferences-btn", ".track-headers .toolbar-btn", ".ai-quick-btn"];
        Reflect.set(window, "themeCycleButtons", selectors.map(selector => document.querySelector(selector)));
      });
      for (const theme of ["ivory", "graphite", "ivory"]) {
        await page.evaluate(next => { document.documentElement.dataset.theme = next; }, theme);
        // Existing color transitions are part of the production styles.
        await page.waitForTimeout(180);
        const metrics = await page.evaluate(() => {
          const selectors = [".preferences-btn", ".track-headers .toolbar-btn", ".ai-quick-btn"];
          // The later panels.css custom-button rule owns both toolbar and
          // Add Track fills; AI quick actions retain their own surface token.
          const backgroundTokens = ["--bg-input", "--bg-input", "--bg-surface"];
          const foregroundTokens = ["--text-primary", "--text-primary", "--text-secondary"];
          const previous = Reflect.get(window, "themeCycleButtons") as Element[];
          const tokenColor = (token: string) => {
            const probe = document.createElement("span");
            probe.style.color = `var(${token})`;
            document.body.append(probe);
            const color = getComputedStyle(probe).color;
            probe.remove();
            return color;
          };
          const luminance = (color: string) => {
            const channels = color.match(/[\d.]+/g)!.slice(0, 3).map(Number).map(value => {
              const unit = value / 255;
              return unit <= 0.04045 ? unit / 12.92 : ((unit + 0.055) / 1.055) ** 2.4;
            });
            return channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722;
          };
          return selectors.map((selector, index) => {
            const button = document.querySelector<HTMLElement>(selector)!;
            const style = getComputedStyle(button);
            const box = button.getBoundingClientRect();
            const foreground = luminance(style.color);
            const background = luminance(style.backgroundColor);
            return {
              selector, sameNode: button === previous[index], appearance: style.appearance,
              foreground: style.color, expectedForeground: tokenColor(foregroundTokens[index]),
              background: style.backgroundColor, expectedBackground: tokenColor(backgroundTokens[index]),
              contrast: (Math.max(foreground, background) + 0.05) / (Math.min(foreground, background) + 0.05),
              visible: box.width > 0 && box.height > 0 && box.left >= 0 && box.right <= window.innerWidth,
            };
          });
        });
        for (const metric of metrics) {
          expect(metric.sameNode, `${theme} ${metric.selector}: control was replaced`).toBe(true);
          expect(metric.appearance, `${theme} ${metric.selector}`).toBe("none");
          expect(metric.background, `${theme} ${metric.selector}: stale fill`).toBe(metric.expectedBackground);
          expect(metric.foreground, `${theme} ${metric.selector}: stale foreground`).toBe(metric.expectedForeground);
          expect(metric.contrast, `${theme} ${metric.selector}: text contrast`).toBeGreaterThanOrEqual(4.5);
          expect(metric.visible, `${theme} ${metric.selector}: geometry`).toBe(true);
        }
      }
    } finally { await page.close(); }
  });
});
