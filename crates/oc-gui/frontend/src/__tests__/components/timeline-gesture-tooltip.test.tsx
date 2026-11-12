/**
 * A pointer overlay owns the timeline: from pointer-down to commit or cancel
 * for a block gesture, and for as long as a context menu is open. The hover
 * tooltip must stay out of the way and come back on the next hover.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import { invokeMock, makePattern, makeSnapshot, makeTrack, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { i18n } from "../../stores/i18n";
import { Timeline } from "../../components/Timeline";
import { TooltipLayer } from "../../components/Tooltip";
import { closeContextMenu, ContextMenuLayer } from "../../components/ContextMenu";

const LANE_TOP = 22;
const LANE_HEIGHT = 64;

function range(start: [number, number, number], end: [number, number, number]) {
  return {
    start: { bar: start[0], beat: start[1], subdivision: start[2] },
    end: { bar: end[0], beat: end[1], subdivision: end[2] },
  };
}

/** Keeps a two-bar span when a block moves to a new start. */
function moveRange(bar: number, beat: number, sub: number) {
  const startQuarter = (bar - 1) * 4 + (beat - 1) + sub / 4;
  const endQuarter = startQuarter + 8;
  return range(
    [bar, beat, sub],
    [Math.floor(endQuarter / 4) + 1, (endQuarter % 4) + 1, 0],
  );
}

interface BackendState {
  tracks: any[];
  patterns: any[];
}

function stubGeometry(container: HTMLElement) {
  const ruler = container.querySelector(".timeline-ruler") as HTMLElement;
  ruler.getBoundingClientRect = () =>
    ({ left: 0, top: 0, right: 2000, bottom: LANE_TOP, width: 2000, height: LANE_TOP, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;
  const lanes = Array.from(container.querySelectorAll<HTMLElement>(".track-lane"));
  lanes.forEach((lane, index) => {
    const top = LANE_TOP + index * LANE_HEIGHT;
    lane.getBoundingClientRect = () =>
      ({ left: 0, top, right: 2000, bottom: top + LANE_HEIGHT, width: 2000, height: LANE_HEIGHT, x: 0, y: top, toJSON: () => ({}) }) as DOMRect;
  });
}

function installBackend(state: BackendState) {
  invokeMock.mockImplementation(async (cmd: string, args?: any) => {
    const a = args ?? {};
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "move_clip") {
      state.tracks = state.tracks.map((track, index) => index !== a.trackIndex ? track : {
        ...track,
        clips: track.clips.map((clip: any, clipIndex: number) => clipIndex !== a.clipIndex ? clip : {
          ...clip,
          time_range: moveRange(a.startBar, a.startBeat, a.startSub),
        }),
      });
    }
    if (cmd === "resize_clip") {
      state.tracks = state.tracks.map((track, index) => index !== a.trackIndex ? track : {
        ...track,
        clips: track.clips.map((clip: any, clipIndex: number) => clipIndex !== a.clipIndex ? clip : {
          ...clip,
          time_range: { ...clip.time_range, end: { bar: a.endBar, beat: a.endBeat, subdivision: a.endSub } },
        }),
      });
    }
    if (cmd === "add_clip") {
      state.tracks = state.tracks.map((track, index) => index !== a.trackIndex ? track : {
        ...track,
        clips: [...track.clips, { pattern: a.patternName, time_range: moveRange(a.startBar, a.startBeat, a.startSub) }],
      });
    }
    return makeSnapshot({ tracks: state.tracks, patterns: state.patterns });
  });
}

function baseState(): BackendState {
  return {
    patterns: [makePattern("beat")],
    tracks: [{
      ...makeTrack("Drums", 0),
      phrases: [],
      clips: [{ pattern: "beat", time_range: range([1, 1, 0], [3, 1, 0]) }],
    }],
  };
}

async function mountWithClip() {
  const state = baseState();
  installBackend(state);
  await store.initProject();
  const view = render(() => <><Timeline /><TooltipLayer /><ContextMenuLayer /></>);
  stubGeometry(view.container as HTMLElement);
  const clip = view.container.querySelector(".clip-block") as HTMLElement;
  return { ...view, clip, state };
}

function tooltip(): HTMLElement | null {
  return document.querySelector(".oc-tooltip");
}

function tooltipRow(label: string): string | undefined {
  const row = [...document.querySelectorAll(".oc-tooltip-row")]
    .find((candidate) => candidate.querySelector(".oc-tooltip-label")?.textContent === label);
  return row?.querySelector(".oc-tooltip-value")?.textContent ?? undefined;
}

/** Hover a block with the tooltip delay already inside fake timers. */
function hover(block: HTMLElement) {
  fireEvent.mouseEnter(block);
  vi.advanceTimersByTime(400);
}

beforeEach(async () => {
  invokeMock.mockClear();
  i18n.setLocale("en");
  await store.initProject();
});

afterEach(() => {
  closeContextMenu();
  vi.useRealTimers();
});

describe("hover tooltip during a block gesture", () => {
  it("hides from pointer-down through the drop and returns on the next hover", async () => {
    const { container, clip, state } = await mountWithClip();
    vi.useFakeTimers();
    hover(clip);
    expect(tooltip()).toBeTruthy();
    expect(tooltipRow("Bars")).toBe("1.1 → 3.1");

    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    expect(tooltip()).toBeNull();

    fireEvent.pointerMove(document, { pointerId: 1, buttons: 1, clientX: 48, clientY: LANE_TOP + 10 });
    expect(tooltip()).toBeNull();

    fireEvent.pointerUp(document, { pointerId: 1, button: 0, clientX: 48, clientY: LANE_TOP + 10 });
    expect(tooltip()).toBeNull();

    vi.useRealTimers();
    await vi.waitFor(() => expect(state.tracks[0].clips[0].time_range.start)
      .toMatchObject({ bar: 1, beat: 3, subdivision: 0 }));
    vi.useFakeTimers();
    const moved = container.querySelector(".clip-block") as HTMLElement;
    fireEvent.mouseLeave(moved);
    hover(moved);
    expect(tooltip()).toBeTruthy();
    expect(tooltipRow("Bars")).toBe("1.3 → 3.3");
  });

  it("stays hidden while the right edge is resized and returns afterwards", async () => {
    const { container, clip, state } = await mountWithClip();
    vi.useFakeTimers();
    hover(clip);
    expect(tooltip()).toBeTruthy();

    const handle = clip.querySelector(".phrase-resize-handle") as HTMLElement;
    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    expect(tooltip()).toBeNull();
    fireEvent.pointerMove(document, { pointerId: 1, buttons: 1, clientX: 96, clientY: LANE_TOP + 10 });
    expect(tooltip()).toBeNull();
    fireEvent.pointerUp(document, { pointerId: 1, button: 0, clientX: 96, clientY: LANE_TOP + 10 });
    expect(tooltip()).toBeNull();

    vi.useRealTimers();
    await vi.waitFor(() => expect(state.tracks[0].clips[0].time_range.end)
      .toMatchObject({ bar: 2, beat: 1, subdivision: 0 }));
    vi.useFakeTimers();
    const resized = container.querySelector(".clip-block") as HTMLElement;
    fireEvent.mouseLeave(resized);
    hover(resized);
    expect(tooltip()).toBeTruthy();
    expect(tooltipRow("Bars")).toBe("1.1 → 2.1");
  });

  it("stays hidden for an alt-drag duplicate", async () => {
    const { container, clip } = await mountWithClip();
    vi.useFakeTimers();
    hover(clip);
    expect(tooltip()).toBeTruthy();

    fireEvent.pointerDown(clip, { button: 0, altKey: true, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    expect(tooltip()).toBeNull();
    fireEvent.pointerMove(document, { pointerId: 1, buttons: 1, clientX: 48, clientY: LANE_TOP + 10 });
    expect(tooltip()).toBeNull();
    fireEvent.pointerUp(document, { pointerId: 1, button: 0, clientX: 48, clientY: LANE_TOP + 10 });
    expect(tooltip()).toBeNull();

    vi.useRealTimers();
    await vi.waitFor(() => expect(container.querySelectorAll(".clip-block").length).toBe(2));
    vi.useFakeTimers();
    const original = container.querySelector(".clip-block") as HTMLElement;
    fireEvent.mouseLeave(original);
    hover(original);
    expect(tooltip()).toBeTruthy();
  });

  it("stays hidden after Escape cancels the gesture", async () => {
    const { clip } = await mountWithClip();
    vi.useFakeTimers();
    hover(clip);
    expect(tooltip()).toBeTruthy();

    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, buttons: 1, clientX: 48, clientY: LANE_TOP + 10 });
    expect(tooltip()).toBeNull();
    fireEvent.keyDown(document, { key: "Escape" });
    expect(tooltip()).toBeNull();

    hover(clip);
    expect(tooltip()).toBeTruthy();
  });

  it("does not reappear when the pointer crosses another block mid-gesture", async () => {
    const state = baseState();
    state.tracks.push({
      ...makeTrack("Bass", 1),
      phrases: [],
      clips: [{ pattern: "beat", time_range: range([1, 1, 0], [3, 1, 0]) }],
    });
    installBackend(state);
    await store.initProject();
    const view = render(() => <><Timeline /><TooltipLayer /><ContextMenuLayer /></>);
    stubGeometry(view.container as HTMLElement);
    const [first, second] = Array.from(view.container.querySelectorAll<HTMLElement>(".clip-block"));
    vi.useFakeTimers();
    hover(first);
    expect(tooltip()).toBeTruthy();

    fireEvent.pointerDown(first, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, {
      pointerId: 1, buttons: 1, clientX: 24, clientY: LANE_TOP + LANE_HEIGHT + 10,
    });
    fireEvent.mouseLeave(first);
    fireEvent.mouseEnter(second);
    vi.advanceTimersByTime(400);
    expect(tooltip()).toBeNull();
  });
});

describe("hover tooltip while a context menu is open", () => {
  it("hides on open, stays hidden over other blocks, and returns after close", async () => {
    const state = baseState();
    state.tracks.push({
      ...makeTrack("Bass", 1),
      phrases: [],
      clips: [{ pattern: "beat", time_range: range([1, 1, 0], [3, 1, 0]) }],
    });
    installBackend(state);
    await store.initProject();
    const view = render(() => <><Timeline /><TooltipLayer /><ContextMenuLayer /></>);
    stubGeometry(view.container as HTMLElement);
    const [first, second] = Array.from(view.container.querySelectorAll<HTMLElement>(".clip-block"));
    vi.useFakeTimers();
    hover(first);
    expect(tooltip()).toBeTruthy();

    fireEvent.contextMenu(first, { clientX: 120, clientY: 90 });
    expect(document.querySelector(".clip-context-menu")).toBeTruthy();
    expect(tooltip()).toBeNull();

    // Another lane's block must not raise a card over the open menu.
    fireEvent.mouseLeave(first);
    fireEvent.mouseEnter(second);
    vi.advanceTimersByTime(400);
    expect(tooltip()).toBeNull();

    closeContextMenu();
    fireEvent.mouseLeave(second);
    hover(second);
    expect(tooltip()).toBeTruthy();
  });

  it("does not raise a card when a block is entered with a menu already open", async () => {
    const { clip } = await mountWithClip();
    vi.useFakeTimers();
    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });
    fireEvent.mouseEnter(clip);
    vi.advanceTimersByTime(400);
    expect(tooltip()).toBeNull();

    closeContextMenu();
    fireEvent.mouseLeave(clip);
    hover(clip);
    expect(tooltip()).toBeTruthy();
  });
});
