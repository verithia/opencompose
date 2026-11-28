/**
 * Status bar guard: the whole bar, rendered by the real shell, while a project
 * loads. The interface scale is a webview zoom, so a 1400 px window at 150 %
 * is a ~933 CSS px viewport, and the open DSL sidebar takes its width out of
 * the bar. The loading chip, the transport time and the history toggle must
 * keep their whole text there: the stage and count are what tell the user the
 * load is moving, and a chip squeezed to a sliver hides them.
 */
import { afterAll, beforeAll, describe, expect, it } from "vitest";
import { render } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import path from "node:path";
import { chromium, webkit, type Browser } from "@playwright/test";
import { invokeMock, makeSnapshot, makeTrack, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { panelStore } from "../../stores/panels";
import { i18n } from "../../stores/i18n";
import App from "../../App";

const STYLE_ROOT = path.resolve(process.cwd(), "src/styles");

function stylesheet(): string {
  const manifest = readFileSync(path.join(STYLE_ROOT, "daw.css"), "utf8");
  const importPattern = /@import\s+"([^"]+)";/g;
  return [
    ...[...manifest.matchAll(importPattern)].map((match) => readFileSync(path.resolve(STYLE_ROOT, match[1]), "utf8")),
    manifest.replace(importPattern, ""),
  ].join("\n");
}

const BUILDS = {
  background: { stage: "background", label: "", done: 21, total: 54, generation: 3, progress: 412 },
  instrument: { stage: "instruments", label: "Pad With A Long Preset Name", done: 2, total: 54, generation: 3, progress: 40 },
};

// The desktop app paints with WebKit; Chromium guards the shared rules too.
const engines: Record<string, Browser> = {};
const markup: Record<string, string> = {};

beforeAll(async () => {
  engines.chromium = await chromium.launch({ headless: true });
  engines.webkit = await webkit.launch({ headless: true });
  i18n.setLocale("en");
  let build: object = BUILDS.background;
  invokeMock.mockReset();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "take_startup_project") return null;
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "project_build_progress") return build;
    if (cmd === "get_undo_state") return { can_undo: true, can_redo: false };
    if (cmd === "history_page" || cmd === "list_recoveries" || cmd === "scan_plugins") return [];
    return {
      ...makeSnapshot({
        name: "QA Dark Sidebar",
        tracks: Array.from({ length: 12 }, (_, index) => makeTrack(`lane${index + 1}`, index)),
      }),
      file_path: "/tmp/qa-dark-sidebar.ocp",
      duration_seconds: 118.5,
    };
  });
  await store.initProject();
  const { container } = render(() => <App />);
  panelStore.setSidebarWidth(320);
  panelStore.openSidebar("dsl");
  for (const [name, next] of Object.entries(BUILDS)) {
    build = next;
    const deadline = Date.now() + 3000;
    while (!container.querySelector(".build-progress-text")?.textContent?.includes(String(next.total))
      || (name === "instrument" && !container.querySelector(".build-progress-text")?.textContent?.includes(next.label))) {
      if (Date.now() > deadline) throw new Error(`the ${name} chip never rendered`);
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
    markup[name] = container.innerHTML;
  }
});

afterAll(async () => {
  panelStore.closeSidebar();
  await Promise.all(Object.values(engines).map((engine) => engine.close()));
});

async function measure(engine: string, body: string, viewport: { width: number; height: number }, theme: string) {
  const page = await engines[engine].newPage({ viewport });
  try {
    await page.setContent(`<!doctype html><html lang="en" data-theme="${theme}"><head><meta charset="utf-8">
      <style>${stylesheet()} html,body{margin:0;height:100%;overflow:hidden} #root{height:100%}
      *,*::before,*::after{transition:none!important;animation:none!important}</style></head>
      <body><div id="root">${body}</div></body></html>`);
    return await page.evaluate(() => {
      const bar = document.querySelector(".status-bar")!.getBoundingClientRect();
      // The chip may hold a full and a compact report; one of them is painted.
      const painted = (selector: string) => [...document.querySelectorAll<HTMLElement>(selector)]
        .find((element) => element.getBoundingClientRect().width > 1) ?? null;
      const whole = (selector: string) => {
        const element = painted(selector);
        if (!element) return { selector, missing: true };
        const box = element.getBoundingClientRect();
        return {
          selector,
          clipped: element.scrollWidth > element.clientWidth + 1,
          insideBar: box.left >= bar.left - 0.5 && box.right <= bar.right + 0.5,
          width: Math.round(box.width),
        };
      };
      const left = document.querySelector(".status-left")!.getBoundingClientRect();
      const state = document.querySelector(".status-left .status-chip.strong")!.getBoundingClientRect();
      const right = document.querySelector(".status-right")!.getBoundingClientRect();
      const name = document.querySelector<HTMLElement>(".build-progress-name");
      return {
        // The transport state leads the left side and must survive the squeeze.
        stateVisible: state.left >= left.left - 0.5 && state.right <= left.right + 0.5,
        overlap: right.left < left.right - 0.5,
        nameCut: name && name.getBoundingClientRect().width > 1 ? name.scrollWidth > name.clientWidth : null,
        compact: painted(".build-progress-compact") !== null,
        chip: whole(".status-chip.build-progress"),
        label: whole(".build-progress-text"),
        time: whole(".status-transport-time"),
        history: whole(".undo-history-toggle"),
      };
    });
  } finally {
    await page.close();
  }
}

describe("status bar while a project loads", () => {
  const cases = [
    // 1400 x 900 points at 150 %, with the sidebar sharing the width.
    { name: "150 % dark", viewport: { width: 933, height: 600 }, theme: "dark" },
    // 1147 points at 150 %: the sidebar floats over the main area.
    { name: "150 % narrow", viewport: { width: 764, height: 550 }, theme: "dark" },
    { name: "100 % light", viewport: { width: 1400, height: 900 }, theme: "light" },
  ];
  for (const { name, viewport, theme } of cases) {
    const runs = ["chromium", "webkit"].flatMap((engine) => Object.keys(BUILDS).map((build) => [engine, build]));
    it.each(runs)(`${name}, %s: the %s chip and the transport time keep their text`, async (engine, build) => {
      const found = await measure(engine, markup[build], viewport, theme);
      for (const part of [found.chip, found.time, found.history]) {
        expect(part, JSON.stringify(part)).toMatchObject({ insideBar: true });
      }
      // The stage and the count stay whole; only an instrument name may give way.
      expect(found.time).toMatchObject({ clipped: false });
      expect(found.label, JSON.stringify(found)).toMatchObject({ clipped: false });
      expect(found.chip.width, JSON.stringify(found)).toBeGreaterThanOrEqual(180);
      expect(found.stateVisible, JSON.stringify(found)).toBe(true);
      expect(found.overlap).toBe(false);
      // The long preset name is the one that shows it was cut, unless the
      // bar is too narrow for any of it and the stage and count stand alone.
      const narrow = viewport.width === 933;
      expect(found.compact).toBe(build === "instrument" && narrow);
      expect(found.nameCut).toBe(build === "instrument" && !narrow ? true : null);
    });
  }
});
