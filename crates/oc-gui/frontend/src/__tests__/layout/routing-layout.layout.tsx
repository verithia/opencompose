/**
 * Real-layout guard for the mixer routing editor.
 *
 * The Solid components are rendered to static HTML in jsdom (the same props
 * the component tests use), the real CSS partials are concatenated in import
 * order, and the result is measured in headless Chrome at 320/900/1440px:
 * no horizontal overflow, no clipped routing button labels, an edge hit target
 * at least 8px tall in screen space, and a connection inspector that fits.
 *
 * Run with `npm run layout-check`. Screenshots land in
 * `tmp/layout-check/shots/routeui-{320,900,1440}.png`.
 */
import { afterAll, beforeAll, beforeEach, describe, expect, it } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import path from "node:path";
import { chromium, type Browser } from "@playwright/test";
import { TRANSPORT_STOPPED, invokeMock, makePattern, makeSnapshot, makeTrack } from "../setup";
import { store } from "../../stores/project";
import { Mixer } from "../../components/Mixer";

const STYLE_ROOT = path.resolve(process.cwd(), "src/styles");
const TMP_DIR = path.resolve(process.cwd(), "tmp/layout-check");
const SHOT_DIR = path.resolve(process.cwd(), "tmp/layout-check/shots");
const WIDTHS = [320, 900, 1440] as const;
const VIEWPORT_HEIGHT = 900;

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

function fixtureHtml(): string {
  const { container, unmount } = render(() => <Mixer />);
  // Select a send connection so the connection inspector is part of the shot.
  const edge = container.querySelector('[data-edge-hit="send:1:reverb_bus:"]');
  expect(edge).toBeTruthy();
  fireEvent.click(edge!);
  expect(container.querySelector(".connection-detail")).toBeTruthy();
  // Solid writes input/select state as live properties; freeze it into
  // attributes so the static fixture shows the same values as the app.
  for (const input of container.querySelectorAll("input")) {
    input.setAttribute("value", input.value);
    if (input.checked) input.setAttribute("checked", "");
  }
  for (const select of container.querySelectorAll("select")) {
    for (const option of select.options) {
      if (option.value === select.value) option.setAttribute("selected", "");
      else option.removeAttribute("selected");
    }
  }
  const markup = container.innerHTML;
  unmount();
  return `<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<title>routing layout guard</title>
<style>
${stylesheet()}
html, body { margin: 0; height: 100%; }
#harness { height: 100vh; min-height: 0; }
</style>
</head>
<body><div id="harness">${markup}</div></body>
</html>`;
}

interface Measurement {
  workspace: { clientWidth: number; scrollWidth: number };
  panel: { clientWidth: number; scrollWidth: number };
  document: { clientWidth: number; scrollWidth: number };
  buttons: {
    label: string;
    clientWidth: number;
    scrollWidth: number;
  }[];
  edgeHits: {
    id: string;
    stroke: number;
    scaleY: number;
    thickness: number;
    pointerEvents: string;
  }[];
  hitSamples: { id: string; above: boolean; below: boolean }[];
  connection: {
    found: boolean;
    clientWidth: number;
    scrollWidth: number;
    right: number;
    viewportWidth: number;
  };
}

async function measure(page: import("playwright-core").Page): Promise<Measurement> {
  return page.evaluate(() => {
    const box = (element: Element | null) => {
      const html = element as HTMLElement;
      return { clientWidth: html.clientWidth, scrollWidth: html.scrollWidth };
    };
    const workspace = document.querySelector(".mixer-routing-workspace");
    const panel = document.querySelector(".mixer-panel");
    const graph = document.querySelector(".bus-graph");

    const buttons = [...document.querySelectorAll(
      ".routing-toolbar .routing-btn, .connection-detail .routing-btn, .routing-node-actions .routing-btn",
    )].map((element) => ({
      label: (element.textContent ?? "").trim() || element.getAttribute("aria-label") || "?",
      clientWidth: (element as HTMLElement).clientWidth,
      scrollWidth: (element as HTMLElement).scrollWidth,
    }));

    const edgeHits = [...document.querySelectorAll(".bus-graph-edge-hit")].map((element) => {
      const hit = element as SVGGeometryElement;
      const ctm = hit.getScreenCTM();
      const stroke = parseFloat(getComputedStyle(hit).strokeWidth) || 0;
      const scaleY = ctm ? Math.hypot(ctm.c, ctm.d) : 0;
      return {
        id: hit.getAttribute("data-edge-hit") ?? "?",
        stroke,
        scaleY,
        thickness: stroke * scaleY,
        pointerEvents: getComputedStyle(hit).pointerEvents,
      };
    });

    // A screen-space hit check for the edges that are visible at this width:
    // 4px above/below the curve must still resolve to the transparent stroke.
    const samples: { id: string; above: boolean; below: boolean }[] = [];
    if (graph) {
      graph.scrollLeft = 0;
      graph.scrollTop = 0;
      const bounds = graph.getBoundingClientRect();
      for (const element of document.querySelectorAll(".bus-graph-edge-hit")) {
        const hit = element as SVGGeometryElement;
        const ctm = hit.getScreenCTM();
        if (!ctm) continue;
        const point = hit.getPointAtLength(hit.getTotalLength() / 2);
        const screen = new DOMPoint(point.x, point.y).matrixTransform(ctm);
        if (
          screen.x < bounds.left + 2 || screen.x > bounds.right - 2
          || screen.y < bounds.top + 2 || screen.y > bounds.bottom - 2
        ) {
          continue;
        }
        samples.push({
          id: hit.getAttribute("data-edge-hit") ?? "?",
          above: document.elementsFromPoint(screen.x, screen.y - 4).includes(hit),
          below: document.elementsFromPoint(screen.x, screen.y + 4).includes(hit),
        });
      }
    }

    const card = document.querySelector(".connection-detail");
    const cardRect = card?.getBoundingClientRect();
    return {
      workspace: box(workspace),
      panel: box(panel),
      document: {
        clientWidth: document.documentElement.clientWidth,
        scrollWidth: document.documentElement.scrollWidth,
      },
      buttons,
      edgeHits,
      hitSamples: samples,
      connection: {
        found: Boolean(card),
        clientWidth: (card as HTMLElement | null)?.clientWidth ?? 0,
        scrollWidth: (card as HTMLElement | null)?.scrollWidth ?? 0,
        right: cardRect?.right ?? 0,
        viewportWidth: window.innerWidth,
      },
    };
  });
}

describe("mixer routing layout", () => {
  let browser: Browser;

  beforeAll(async () => {
    mkdirSync(TMP_DIR, { recursive: true });
    mkdirSync(SHOT_DIR, { recursive: true });
    browser = await chromium.launch({ channel: "chrome", headless: true });
  });

  afterAll(async () => {
    await browser?.close();
  });

  beforeEach(async () => {
    invokeMock.mockClear();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "get_undo_state") {
        return {
          can_undo: false, can_redo: false,
          undo_label: null, redo_label: null, undo_labels: [], redo_labels: [],
        };
      }
      return makeSnapshot({
        tracks: [
          makeTrack("Keys", 0),
          {
            ...makeTrack("Drums", 1),
            mix: {
              volume_db: -6, pan: 0.5, mute: false, solo: false,
              sends: [{ target: "reverb_bus", amount: 0.4 }],
            },
          },
          {
            ...makeTrack("Bass", 2),
            mix: {
              volume_db: -3, pan: 0, mute: false, solo: false,
              sends: [{ target: "glue", amount: 0.25, pre_fader: true }],
            },
          },
        ],
        buses: [
          { name: "reverb_bus", routes: ["master"], effects: [] },
          { name: "glue", routes: ["reverb_bus", "master"], effects: [] },
        ],
        patterns: [makePattern("beat")],
      });
    });
    await store.initProject();
  });

  it("keeps the routing editor unclipped at 320, 900 and 1440px", async () => {
    const html = fixtureHtml();
    const fixturePath = path.join(TMP_DIR, "routing-layout.html");
    writeFileSync(fixturePath, html);

    const page = await browser.newPage({ viewport: { width: 1440, height: VIEWPORT_HEIGHT } });
    const consoleErrors: string[] = [];
    page.on("pageerror", (error) => consoleErrors.push(error.message));

    try {
      for (const width of WIDTHS) {
        await page.setViewportSize({ width, height: VIEWPORT_HEIGHT });
        await page.setContent(html, { waitUntil: "load" });
        await page.waitForSelector(".mixer-panel");
        const layout = await measure(page);

        expect(layout.workspace.scrollWidth, `${width}px workspace overflow`)
          .toBeLessThanOrEqual(layout.workspace.clientWidth + 1);
        expect(layout.panel.scrollWidth, `${width}px panel overflow`)
          .toBeLessThanOrEqual(layout.panel.clientWidth + 1);
        expect(layout.document.scrollWidth, `${width}px document overflow`)
          .toBeLessThanOrEqual(layout.document.clientWidth + 1);

        expect(layout.buttons.length, `${width}px routing buttons`).toBeGreaterThan(0);
        for (const button of layout.buttons) {
          expect(button.scrollWidth, `${width}px clipped "${button.label}"`)
            .toBeLessThanOrEqual(button.clientWidth + 1);
        }

        expect(layout.edgeHits.length, `${width}px edge hit targets`).toBeGreaterThan(0);
        for (const hit of layout.edgeHits) {
          expect(hit.pointerEvents, `${width}px ${hit.id} pointer-events`).toBe("stroke");
          expect(hit.stroke, `${width}px ${hit.id} stroke width`).toBeGreaterThanOrEqual(10);
          expect(hit.thickness, `${width}px ${hit.id} screen thickness`).toBeGreaterThanOrEqual(8);
        }
        expect(layout.hitSamples.length, `${width}px visible hit samples`).toBeGreaterThan(0);
        for (const sample of layout.hitSamples) {
          expect(sample.above, `${width}px ${sample.id} 4px above the curve`).toBe(true);
          expect(sample.below, `${width}px ${sample.id} 4px below the curve`).toBe(true);
        }

        expect(layout.connection.found, `${width}px connection inspector`).toBe(true);
        expect(layout.connection.scrollWidth, `${width}px connection overflow`)
          .toBeLessThanOrEqual(layout.connection.clientWidth + 1);
        expect(layout.connection.right, `${width}px connection off-screen`)
          .toBeLessThanOrEqual(layout.connection.viewportWidth + 1);

        await page.locator(".connection-detail").scrollIntoViewIfNeeded();
        await page.screenshot({
          path: path.join(SHOT_DIR, `routeui-${width}.png`),
          animations: "disabled",
        });
      }

      expect(consoleErrors, "page errors").toEqual([]);
    } finally {
      await page.close();
    }
  });
});
