/**
 * Drag lifecycle and floating-panel placement.
 *
 * A drag must end even when no release arrives — a cancelled pointer, a lost
 * capture, or the window losing focus — and it must never leave a second
 * listener set behind. A floating panel must stay reachable: the title bar is
 * its only re-dock handle, so at least a grabbable strip of it stays inside the
 * window no matter where the panel is dragged, and a smaller window pulls it
 * back in.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makePattern, makeSnapshot, makeTrack } from "../setup";
import App from "../../App";
import { panelStore, type PanelId } from "../../stores/panels";
import { endPointerDrag } from "../../utils/pointerDrag";

const WINDOW_WIDTH = 1024;
const WINDOW_HEIGHT = 768;
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

/** The split divider only exists while a bottom panel is open. */
async function mountWithDivider() {
  const view = mountApp();
  panelStore.openBottom("pianoroll");
  await waitFor(() => expect(view.container.querySelector(".split-divider")).toBeTruthy());
  const main = view.container.querySelector(".daw-main") as HTMLElement;
  // jsdom has no layout: give the split container the geometry the drag reads.
  main.getBoundingClientRect = () => ({
    x: 0, y: CONTAINER_TOP, top: CONTAINER_TOP, left: 0, right: WINDOW_WIDTH,
    bottom: CONTAINER_TOP + CONTAINER_HEIGHT, width: WINDOW_WIDTH,
    height: CONTAINER_HEIGHT, toJSON: () => ({}),
  }) as DOMRect;
  // Drop the mount's own frame callbacks: from here the queue is only the drag.
  frames = [];
  return { view, main, divider: view.container.querySelector(".split-divider") as HTMLElement };
}

function yFor(ratio: number) {
  return CONTAINER_TOP + ratio * CONTAINER_HEIGHT;
}

async function mountFloating(id: PanelId) {
  const view = mountApp();
  panelStore.detachPanel(id);
  await waitFor(() => expect(view.container.querySelector(".floating-panel-title")).toBeTruthy());
  return {
    view,
    panel: () => panelStore.getFloating(id)!,
    title: view.container.querySelector(".floating-panel-title")!,
  };
}

/** Press the title at an offset from the panel corner and move to a point. */
function dragTitle(
  title: Element,
  panel: () => { x: number; y: number },
  to: { clientX: number; clientY: number },
) {
  fireEvent.pointerDown(title, {
    button: 0, pointerId: 1, clientX: panel().x + 10, clientY: panel().y + 10,
  });
  fireEvent.pointerMove(window, { pointerId: 1, ...to });
}

beforeEach(() => {
  frames = [];
  window.innerWidth = WINDOW_WIDTH;
  window.innerHeight = WINDOW_HEIGHT;
  document.body.style.cursor = "";
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
  // A failed assertion can leave a drag running; its window listeners would
  // then take the next test's pointer events.
  endPointerDrag();
  for (const panel of [...panelStore.floatingPanels]) panelStore.dockPanel(panel.id);
  panelStore.closeBottom();
  panelStore.closeSidebar();
  panelStore.setSidebarWidth(320);
  document.body.style.cursor = "";
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe("split divider drag lifecycle", () => {
  it("ends on pointercancel, clears the drag state and leaves no listener", async () => {
    const update = vi.spyOn(panelStore, "updateSplitRatio");
    const remove = vi.spyOn(window, "removeEventListener");
    const { main, divider } = await mountWithDivider();

    fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientY: yFor(0.55) });
    fireEvent.pointerMove(document, { pointerId: 1, clientY: yFor(0.6) });
    expect(main.classList.contains("resizing")).toBe(true);
    expect(document.body.style.cursor).toBe("row-resize");

    // The button came up over the native title bar: no pointerup, only cancel.
    fireEvent.pointerCancel(window, { pointerId: 1 });
    expect(main.classList.contains("resizing")).toBe(false);
    expect(document.body.style.cursor).toBe("");
    expect(remove).toHaveBeenCalledWith("pointermove", expect.any(Function), true);
    // A cancel still commits the last position, so the panel does not jump back.
    expect(panelStore.splitRatio()).toBeCloseTo(0.6, 6);

    update.mockClear();
    fireEvent.pointerMove(document, { pointerId: 1, clientY: yFor(0.8) });
    runFrame();
    expect(update).not.toHaveBeenCalled();
    expect(panelStore.splitRatio()).toBeCloseTo(0.6, 6);
  });

  it("ends when the window loses focus", async () => {
    const { main, divider } = await mountWithDivider();

    fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientY: yFor(0.55) });
    expect(main.classList.contains("resizing")).toBe(true);
    fireEvent.blur(window);
    expect(main.classList.contains("resizing")).toBe(false);
    expect(panelStore.isDividerDragging()).toBe(false);
  });

  it("ends when the pointer capture is lost", async () => {
    const { main, divider } = await mountWithDivider();

    fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientY: yFor(0.55) });
    expect(main.classList.contains("resizing")).toBe(true);
    fireEvent.lostPointerCapture(divider, { pointerId: 1 });
    expect(main.classList.contains("resizing")).toBe(false);
  });

  it("installs one listener set per drag, not one more per press", async () => {
    const update = vi.spyOn(panelStore, "updateSplitRatio");
    const { main, divider } = await mountWithDivider();

    // The first release never arrives, so the second press starts a new drag
    // while the old one is still registered.
    fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientY: yFor(0.55) });
    fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientY: yFor(0.55) });
    expect(main.classList.contains("resizing")).toBe(true);

    fireEvent.pointerMove(document, { pointerId: 1, clientY: yFor(0.65) });
    runFrame();
    // One move, one write: a stacked pair would apply it twice.
    expect(update).toHaveBeenCalledTimes(1);
    expect(panelStore.splitRatio()).toBeCloseTo(0.65, 6);

    fireEvent.pointerUp(window, { pointerId: 1 });
    expect(main.classList.contains("resizing")).toBe(false);
    expect(document.body.style.cursor).toBe("");
  });

  it("keeps the one-update-per-frame coalescing through a real release", async () => {
    const update = vi.spyOn(panelStore, "updateSplitRatio");
    const { divider } = await mountWithDivider();

    fireEvent.pointerDown(divider, { button: 0, pointerId: 1, clientY: yFor(0.55) });
    fireEvent.pointerMove(document, { pointerId: 1, clientY: yFor(0.6) });
    const queued = frames.length;
    fireEvent.pointerMove(document, { pointerId: 1, clientY: yFor(0.62) });
    expect(frames.length).toBe(queued);
    runFrame();
    expect(update).toHaveBeenCalledTimes(1);
    fireEvent.pointerUp(window, { pointerId: 1 });
    expect(update).toHaveBeenCalledTimes(1);
    expect(panelStore.targetSplitRatio()).toBeCloseTo(0.62, 6);
  });
});

describe("sidebar handle drag lifecycle", () => {
  async function mountWithSidebar() {
    panelStore.setSidebarWidth(320);
    panelStore.openSidebar("dsl");
    const view = mountApp();
    await waitFor(() => expect(view.container.querySelector(".sidebar-resize-handle")).toBeTruthy());
    return view.container.querySelector(".sidebar-resize-handle") as HTMLElement;
  }

  it("ends on pointercancel, so a later move keeps the width", async () => {
    const handle = await mountWithSidebar();

    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 600 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 590 });
    expect(panelStore.sidebarWidth()).toBe(330);
    expect(document.body.style.cursor).toBe("col-resize");

    fireEvent.pointerCancel(window, { pointerId: 1 });
    expect(document.body.style.cursor).toBe("");

    fireEvent.pointerMove(document, { pointerId: 1, clientX: 400 });
    expect(panelStore.sidebarWidth()).toBe(330);
  });

  it("installs one listener set per drag, not one more per press", async () => {
    const handle = await mountWithSidebar();
    const setWidth = vi.spyOn(panelStore, "setSidebarWidth");

    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 600 });
    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 600 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 590 });
    expect(setWidth).toHaveBeenCalledTimes(1);
    expect(panelStore.sidebarWidth()).toBe(330);
  });
});

describe("floating panel placement", () => {
  it("clamps a dragged panel so a 40 px strip of its title bar stays reachable", async () => {
    const { panel, title } = await mountFloating("dsl");
    const width = panel().width;

    dragTitle(title, panel, { clientX: -2000, clientY: -2000 });
    expect(panel().x).toBe(40 - width);
    expect(panel().x + panel().width).toBe(40);
    expect(panel().y).toBe(0);

    dragTitle(title, panel, { clientX: 5000, clientY: 5000 });
    expect(panel().x).toBe(WINDOW_WIDTH - 40);
    expect(panel().y).toBe(WINDOW_HEIGHT - 32);
  });

  it("re-clamps every floating panel after the window shrinks", async () => {
    const { panel, title } = await mountFloating("dsl");

    dragTitle(title, panel, { clientX: 5000, clientY: 5000 });
    expect(panel().x).toBe(WINDOW_WIDTH - 40);
    fireEvent.pointerUp(window, { pointerId: 1 });

    window.innerWidth = 400;
    window.innerHeight = 300;
    fireEvent.resize(window);

    expect(panel().x).toBe(400 - 40);
    expect(panel().y).toBe(300 - 32);
  });

  it("still resizes, minimizes and re-docks on a title double-click", async () => {
    const { view, panel, title } = await mountFloating("dsl");
    const startWidth = panel().width;
    const startHeight = panel().height;

    const handle = view.container.querySelector(".floating-panel-resize")!;
    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 100, clientY: 100 });
    fireEvent.pointerMove(window, { pointerId: 1, clientX: 160, clientY: 140 });
    expect(panel().width).toBe(startWidth + 60);
    expect(panel().height).toBe(startHeight + 40);
    fireEvent.pointerUp(window, { pointerId: 1 });

    fireEvent.click(view.container.querySelector(".floating-panel-btn")!);
    expect(panel().minimized).toBe(true);

    fireEvent.dblClick(title);
    expect(panelStore.isFloating("dsl")).toBe(false);
    expect(panelStore.sidebarOpen()).toBe(true);
  });
});
