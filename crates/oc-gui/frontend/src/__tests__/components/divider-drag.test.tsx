/**
 * Split-divider drag: the panel edge follows the pointer, and the pointer owns
 * the height while it is down.
 *
 * - every pointer sample in one animation frame collapses into one ratio
 *   update, at the *last* sample (a raw mousemove handler restarts a 200 ms
 *   height transition per event and the panel never catches up);
 * - the drag carries `resizing` on `.daw-main`, which is what cancels the
 *   height transition in `layout.css`;
 * - releasing removes the class, so the animation is back for programmatic
 *   open / collapse, and the ratio keeps the value the drag ended on.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import { invokeMock, makePattern, makeSnapshot, makeTrack } from "../setup";
import App from "../../App";
import { panelStore } from "../../stores/panels";

const CONTAINER_TOP = 40;
const CONTAINER_HEIGHT = 600;

let frames: FrameRequestCallback[] = [];

function runFrame() {
  const pending = frames;
  frames = [];
  for (const callback of pending) callback(performance.now());
}

function mountApp() {
  return render(() => <App />);
}

async function mountWithDivider() {
  const view = mountApp();
  panelStore.openBottom("pianoroll");
  await waitFor(() => expect(view.container.querySelector(".split-divider")).toBeTruthy());
  const main = view.container.querySelector(".daw-main") as HTMLElement;
  // jsdom has no layout: give the split container the geometry the drag reads.
  main.getBoundingClientRect = () => ({
    x: 0, y: CONTAINER_TOP, top: CONTAINER_TOP, left: 0, right: 1440, bottom: CONTAINER_TOP + CONTAINER_HEIGHT,
    width: 1440, height: CONTAINER_HEIGHT, toJSON: () => ({}),
  }) as DOMRect;
  const divider = view.container.querySelector(".split-divider") as HTMLElement;
  // Drop the mount's own frame callbacks: from here the queue is only the drag.
  frames = [];
  return { view, main, divider };
}

/** Pointer y for a split ratio, through the same container geometry. */
function yFor(ratio: number) {
  return CONTAINER_TOP + ratio * CONTAINER_HEIGHT;
}

beforeEach(() => {
  frames = [];
  vi.stubGlobal("requestAnimationFrame", (callback: FrameRequestCallback) => {
    frames.push(callback);
    return frames.length;
  });
  vi.stubGlobal("cancelAnimationFrame", (id: number) => {
    frames[id - 1] = () => {};
  });
  panelStore.endDividerDrag();
  panelStore.closeBottom();
  panelStore.updateSplitRatio(0.55);
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_undo_history") return [];
    if (cmd === "get_transport_state") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    }
    return makeSnapshot({ tracks: [makeTrack("Keys", 0)], patterns: [makePattern("beat")] });
  });
});

afterEach(() => {
  // End any drag a failed assertion left open, or its document listeners would
  // stack onto the next test's.
  fireEvent.pointerUp(window, { pointerId: 1 });
  panelStore.endDividerDrag();
  panelStore.closeBottom();
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("split divider drag", () => {
  it("collapses every move inside one frame into a single ratio update", async () => {
    const update = vi.spyOn(panelStore, "updateSplitRatio");
    const { divider } = await mountWithDivider();

    fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientY: yFor(0.55) });
    fireEvent.pointerMove(document, { pointerId: 1, clientY: yFor(0.55) - 2 });
    // The first move arms exactly one frame; the eleven that follow add none.
    const queued = frames.length;
    for (let step = 2; step <= 12; step++) {
      fireEvent.pointerMove(document, { pointerId: 1, clientY: yFor(0.55) - step * 2 });
    }
    expect(update).not.toHaveBeenCalled();
    expect(frames.length).toBe(queued);

    runFrame();
    expect(update).toHaveBeenCalledTimes(1);
    // The last pointer position wins, not the first.
    expect(panelStore.splitRatio()).toBeCloseTo(0.55 - 24 / CONTAINER_HEIGHT, 6);
    fireEvent.pointerUp(window, { pointerId: 1 });
  });

  it("never writes more than one ratio per frame while the drag runs", async () => {
    const update = vi.spyOn(panelStore, "updateSplitRatio");
    const { divider } = await mountWithDivider();
    fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientY: yFor(0.55) });
    fireEvent.pointerMove(document, { pointerId: 1, clientY: yFor(0.55) + 1 });
    const queued = frames.length;

    for (let frame = 0; frame < 5; frame++) {
      for (let step = 0; step < 4; step++) {
        fireEvent.pointerMove(document, { pointerId: 1, clientY: yFor(0.55) + frame * 8 + step * 2 });
      }
      // Four pointer samples, still a single pending frame.
      expect(frames.length, `frame ${frame} queued ${frames.length} callbacks`).toBe(queued);
      const before = update.mock.calls.length;
      runFrame();
      expect(update.mock.calls.length - before, `frame ${frame} ratio writes`).toBe(1);
    }
    fireEvent.pointerUp(window, { pointerId: 1 });
  });

  it("marks the split as resizing so the height transition is off, and clears it on release", async () => {
    const { view, main, divider } = await mountWithDivider();
    expect(main.classList.contains("resizing")).toBe(false);

    fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientY: yFor(0.55) });
    fireEvent.pointerMove(document, { pointerId: 1, clientY: yFor(0.55) + 10 });
    runFrame();
    expect(main.classList.contains("resizing")).toBe(true);

    fireEvent.pointerUp(window, { pointerId: 1 });
    expect(main.classList.contains("resizing")).toBe(false);
    expect(view.container.querySelector(".daw-main")).toBe(main);
  });

  it("targets the drag class from the stylesheet and keeps the open/close animation", () => {
    const css = readFileSync(`${process.cwd()}/src/styles/daw/layout.css`, "utf8");
    // The panels still animate for programmatic ratio changes.
    const baseBottom = css.match(/\.split-bottom\s*{[^}]*}/)?.[0] ?? "";
    const baseTop = css.match(/\.split-top\s*{[^}]*}/)?.[0] ?? "";
    expect(baseBottom).toMatch(/transition:\s*height\s+200ms/);
    expect(baseTop).toMatch(/transition:\s*height\s+200ms/);
    // ...and the drag cancels exactly that transition on exactly those panels.
    const dragRule = css.match(/\.daw-main\.resizing[^{]*{[^}]*}/)?.[0] ?? "";
    expect(dragRule).toContain(".split-top");
    expect(dragRule).toContain(".split-bottom");
    expect(dragRule).toMatch(/transition:\s*none/);
  });

  it("keeps the clamp and the ratio after release", async () => {
    const { divider } = await mountWithDivider();

    fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientY: yFor(0.55) });
    fireEvent.pointerMove(document, { pointerId: 1, clientY: CONTAINER_TOP + CONTAINER_HEIGHT * 2 });
    runFrame();
    expect(panelStore.splitRatio()).toBe(0.85);

    fireEvent.pointerMove(document, { pointerId: 1, clientY: CONTAINER_TOP - 400 });
    runFrame();
    expect(panelStore.splitRatio()).toBe(0.2);

    fireEvent.pointerMove(document, { pointerId: 1, clientY: yFor(0.4) });
    runFrame();
    fireEvent.pointerUp(window, { pointerId: 1 });
    expect(panelStore.splitRatio()).toBeCloseTo(0.4, 6);
    expect(panelStore.targetSplitRatio()).toBeCloseTo(0.4, 6);
    expect(panelStore.isDividerDragging()).toBe(false);

    // No further writes once the pointer is up.
    const update = vi.spyOn(panelStore, "updateSplitRatio");
    fireEvent.pointerMove(document, { pointerId: 1, clientY: yFor(0.8) });
    runFrame();
    expect(update).not.toHaveBeenCalled();
    expect(panelStore.splitRatio()).toBeCloseTo(0.4, 6);
  });

  it("applies the last pending position on release instead of dropping it", async () => {
    const { divider } = await mountWithDivider();
    fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientY: yFor(0.55) });
    fireEvent.pointerMove(document, { pointerId: 1, clientY: yFor(0.65) });
    // Release before the frame: the final pointer position must still land.
    fireEvent.pointerUp(window, { pointerId: 1 });
    expect(panelStore.splitRatio()).toBeCloseTo(0.65, 6);
  });

  it("does nothing when the pointer never moved", async () => {
    const update = vi.spyOn(panelStore, "updateSplitRatio");
    const { divider } = await mountWithDivider();
    fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientY: yFor(0.55) });
    fireEvent.pointerUp(window, { pointerId: 1 });
    expect(update).not.toHaveBeenCalled();
    expect(panelStore.splitRatio()).toBe(0.55);
  });
});
