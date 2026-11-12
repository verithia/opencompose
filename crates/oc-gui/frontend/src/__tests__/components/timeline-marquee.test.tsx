/**
 * Marquee selection and multi-block delete in the timeline. A drag on empty
 * lane background paints a band; Backspace removes the picked blocks at once.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeNote, makePattern, makeTrack, getLastCallArgs } from "../setup";
import { store } from "../../stores/project";
import { resetGridDivision } from "../../stores/grid";
import { Timeline } from "../../components/Timeline";

const LANE_TOP = 22;
const LANE_HEIGHT = 64;

function range(start: [number, number, number], end: [number, number, number]) {
  return {
    start: { bar: start[0], beat: start[1], subdivision: start[2] },
    end: { bar: end[0], beat: end[1], subdivision: end[2] },
  };
}

interface BackendState {
  tracks: any[];
  patterns: any[];
}

/** Track 0 has clips at quarter 0-8 and 8-16; track 1 has a phrase at 0-8 and
 * an inline pattern at 16-24, so x = 200px is empty background in both lanes. */
function baseState(): BackendState {
  return {
    patterns: [makePattern("beat")],
    tracks: [
      {
        ...makeTrack("Keys", 0),
        phrases: [],
        clips: [
          { pattern: "beat", time_range: range([1, 1, 0], [3, 1, 0]) },
          { pattern: "beat", time_range: range([3, 1, 0], [5, 1, 0]) },
        ],
      },
      {
        ...makeTrack("Bass", 1),
        phrases: [{
          name: "melody",
          time_range: range([1, 1, 0], [3, 1, 0]),
          events: [makeNote({ start_beat: 0 })],
        }],
        clips: [],
        patterns: [{
          name: "main_beat",
          time_range: range([5, 1, 0], [7, 1, 0]),
          repeat: 2,
          hits: [],
        }],
      },
    ],
  };
}

function installBackend(state: BackendState) {
  invokeMock.mockImplementation(async (cmd: string, args?: any) => {
    const a = args ?? {};
    if (cmd === "get_transport_state") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    }
    if (cmd === "remove_blocks") {
      const blocks: any[] = [...(a.blocks ?? [])].sort((x, y) => y.index - x.index);
      for (const block of blocks) {
        const track = state.tracks[block.trackIndex];
        if (!track) continue;
        if (block.kind === "clip") track.clips.splice(block.index, 1);
        else if (block.kind === "phrase") track.phrases.splice(block.index, 1);
        else track.patterns.splice(block.index, 1);
      }
    }
    if (cmd === "move_clip") {
      const track = state.tracks[a.trackIndex];
      if (track) {
        track.clips[a.clipIndex] = {
          ...track.clips[a.clipIndex],
          time_range: {
            start: { bar: a.startBar, beat: a.startBeat, subdivision: a.startSub },
            end: { bar: a.startBar + 2, beat: a.startBeat, subdivision: a.startSub },
          },
        };
      }
    }
    // The real backend serializes a fresh tree per reply, so hand back copies.
    return makeSnapshot({ tracks: structuredClone(state.tracks), patterns: state.patterns });
  });
}

function stubGeometry(container: HTMLElement) {
  const column = container.querySelector(".timeline-lane-column") as HTMLElement;
  column.getBoundingClientRect = () =>
    ({ left: 0, top: 0, right: 2000, bottom: 2000, width: 2000, height: 2000, x: 0, y: 0, toJSON: () => ({}) }) as DOMRect;
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

async function mount(state: BackendState) {
  installBackend(state);
  await store.initProject();
  const view = render(() => <Timeline />);
  stubGeometry(view.container as HTMLElement);
  return view;
}

function lane(container: HTMLElement, index: number): HTMLElement {
  return container.querySelectorAll<HTMLElement>(".track-lane")[index];
}

function marquee(
  container: HTMLElement,
  from: { x: number; y: number },
  to: { x: number; y: number },
  modifiers: { shiftKey?: boolean } = {},
) {
  const row = Math.max(0, Math.floor((from.y - LANE_TOP) / LANE_HEIGHT));
  fireEvent.pointerDown(lane(container, row), { button: 0, pointerId: 1, clientX: from.x, clientY: from.y, ...modifiers });
  fireEvent.pointerMove(document, { pointerId: 1, clientX: to.x, clientY: to.y });
  fireEvent.pointerUp(document, { pointerId: 1, clientX: to.x, clientY: to.y });
}

/** The trailing click a pointer gesture leaves behind is swallowed for one tick. */
const settleClick = () => new Promise((resolve) => setTimeout(resolve, 0));

function commandCount(name: string) {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === name).length;
}

beforeEach(async () => {
  invokeMock.mockClear();
  resetGridDivision();
  await store.initProject();
});

afterEach(async () => {
  await settleClick();
  store.setBlockSelection([]);
});

describe("timeline marquee selection", () => {
  it("selects every clip and pattern the band touches (left to right)", async () => {
    const state = baseState();
    const view = await mount(state);

    // x 192-500 covers quarters 8-20.8 across both lanes: clip 1 (8-16) and
    // the inline pattern (16-24) are touched, the phrase and clip 0 are not.
    marquee(view.container as HTMLElement, { x: 192, y: LANE_TOP + LANE_HEIGHT + 10 }, { x: 500, y: LANE_TOP + 10 });

    expect(store.blockSelection()).toEqual([
      { kind: "clip", trackIndex: 0, index: 1 },
      { kind: "pattern", trackIndex: 1, index: 0 },
    ]);
    expect(view.container.querySelectorAll(".phrase-block.selected").length).toBe(2);
    expect(view.container.querySelector(".timeline-marquee")).toBeFalsy();
  });

  it("paints the rubber band while dragging and marks the touched blocks", async () => {
    const view = await mount(baseState());

    fireEvent.pointerDown(lane(view.container as HTMLElement, 0), {
      button: 0, pointerId: 1, clientX: 400, clientY: LANE_TOP + 10,
    });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 300, clientY: LANE_TOP + 40 });

    const band = view.container.querySelector(".timeline-marquee") as HTMLElement;
    expect(band).toBeTruthy();
    expect(band.style.left).toBe("300px");
    expect(band.style.width).toBe("100px");
    // Right-to-left: the band is dashed while it contains instead of touching.
    expect(band.classList.contains("enclosing")).toBe(true);

    fireEvent.pointerUp(document, { pointerId: 1, clientX: 300, clientY: LANE_TOP + 40 });
    expect(view.container.querySelector(".timeline-marquee")).toBeFalsy();
  });

  it("only takes fully contained blocks on a right-to-left drag", async () => {
    const view = await mount(baseState());

    // Same box as the left-to-right case, dragged backwards: the pattern
    // (16-24) sticks out past 20.8, so only the clip is contained.
    marquee(
      view.container as HTMLElement,
      { x: 500, y: LANE_TOP + 10 },
      { x: 192, y: LANE_TOP + LANE_HEIGHT + 10 },
    );

    expect(store.blockSelection()).toEqual([{ kind: "clip", trackIndex: 0, index: 1 }]);
  });

  it("keeps the previous selection when shift-clicking another block", async () => {
    const view = await mount(baseState());
    const [firstClip, secondClip] = Array.from(view.container.querySelectorAll<HTMLElement>(".clip-block"));

    fireEvent.click(secondClip);
    fireEvent.click(firstClip, { shiftKey: true });

    expect(store.blockSelection()).toEqual([
      { kind: "clip", trackIndex: 0, index: 1 },
      { kind: "clip", trackIndex: 0, index: 0 },
    ]);
    expect(view.container.querySelectorAll(".clip-block.selected").length).toBe(2);
  });

  it("adds a second marquee band to the selection when shift is held", async () => {
    const view = await mount(baseState());

    marquee(
      view.container as HTMLElement,
      { x: 400, y: LANE_TOP + 10 },
      { x: 100, y: LANE_TOP + 10 },
    );
    expect(store.blockSelection()).toEqual([{ kind: "clip", trackIndex: 0, index: 1 }]);

    // Shift starts from the existing pick and unions the new hits.
    fireEvent.pointerDown(lane(view.container as HTMLElement, 1), {
      button: 0, pointerId: 2, clientX: 400, clientY: LANE_TOP + LANE_HEIGHT + 10, shiftKey: true,
    });
    // The pick above ends at x=100; the band below starts on empty lane 1.
    fireEvent.pointerMove(document, { pointerId: 2, clientX: 520, clientY: LANE_TOP + LANE_HEIGHT + 10 });
    fireEvent.pointerUp(document, { pointerId: 2, clientX: 520, clientY: LANE_TOP + LANE_HEIGHT + 10 });

    expect(store.blockSelection()).toEqual([
      { kind: "clip", trackIndex: 0, index: 1 },
      { kind: "pattern", trackIndex: 1, index: 0 },
    ]);
  });

  it("keeps the selection across a re-render", async () => {
    const view = await mount(baseState());

    marquee(
      view.container as HTMLElement,
      { x: 400, y: LANE_TOP + 10 },
      { x: 100, y: LANE_TOP + 10 },
    );
    expect(view.container.querySelectorAll(".phrase-block.selected").length).toBe(1);

    // Zooming rebuilds the playhead and every block; the pick must stay.
    fireEvent.click(view.container.querySelector('[aria-label="Zoom in"]') as HTMLElement);

    expect(store.blockSelection()).toEqual([{ kind: "clip", trackIndex: 0, index: 1 }]);
    expect(view.container.querySelectorAll(".phrase-block.selected").length).toBe(1);
  });

  it("clears the selection on an empty lane click and on Escape", async () => {
    const view = await mount(baseState());
    const container = view.container as HTMLElement;

    marquee(container, { x: 400, y: LANE_TOP + 10 }, { x: 100, y: LANE_TOP + 10 });
    expect(store.blockSelection()).toEqual([{ kind: "clip", trackIndex: 0, index: 1 }]);

    await settleClick();
    fireEvent.click(lane(container, 0));
    expect(store.blockSelection()).toEqual([]);

    marquee(container, { x: 400, y: LANE_TOP + 10 }, { x: 100, y: LANE_TOP + 10 });
    expect(store.blockSelection().length).toBe(1);

    fireEvent.keyDown(container.querySelector(".timeline-panel") as HTMLElement, { key: "Escape" });
    expect(store.blockSelection()).toEqual([]);
  });

  it("clears the selection when another project is loaded", async () => {
    const view = await mount(baseState());

    marquee(
      view.container as HTMLElement,
      { x: 400, y: LANE_TOP + 10 },
      { x: 100, y: LANE_TOP + 10 },
    );
    expect(store.blockSelection().length).toBe(1);

    await store.initProject();
    expect(store.blockSelection()).toEqual([]);
  });
});

describe("timeline multi-block delete", () => {
  it("removes exactly the selected blocks in one undo step", async () => {
    const state = baseState();
    const view = await mount(state);
    const [firstClip, secondClip] = Array.from(view.container.querySelectorAll<HTMLElement>(".clip-block"));

    fireEvent.click(secondClip);
    fireEvent.click(firstClip, { shiftKey: true });
    fireEvent.click(view.container.querySelector(".pattern-block") as HTMLElement, { shiftKey: true });
    expect(store.blockSelection().length).toBe(3);

    fireEvent.keyDown(document.body, { key: "Backspace" });

    await vi.waitFor(() => expect(commandCount("remove_blocks")).toBe(1));
    expect(getLastCallArgs("remove_blocks")).toEqual({
      blocks: [
        { kind: "clip", trackIndex: 0, index: 1 },
        { kind: "clip", trackIndex: 0, index: 0 },
        { kind: "pattern", trackIndex: 1, index: 0 },
      ],
    });
    // One command is one undo entry; the single-clip path must not run.
    expect(commandCount("remove_clip")).toBe(0);
    expect(commandCount("remove_audio_clip")).toBe(0);

    await vi.waitFor(() => expect(store.project.tracks[0].clips.length).toBe(0));
    expect(store.project.tracks[1].patterns.length).toBe(0);
    expect(store.project.tracks[1].phrases.length).toBe(1);
    expect(store.blockSelection()).toEqual([]);
  });

  it("does nothing when the selection is empty", async () => {
    await mount(baseState());
    fireEvent.keyDown(document.body, { key: "Backspace" });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(commandCount("remove_blocks")).toBe(0);
    expect(commandCount("remove_clip")).toBe(0);
  });

  it("does nothing while focus is inside a text field", async () => {
    const view = await mount(baseState());
    const clip = view.container.querySelectorAll<HTMLElement>(".clip-block")[1];
    fireEvent.click(clip);
    expect(store.blockSelection().length).toBe(1);

    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();
    fireEvent.keyDown(input, { key: "Backspace" });
    fireEvent.keyDown(input, { key: "Delete" });
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(commandCount("remove_blocks")).toBe(0);
    expect(commandCount("remove_clip")).toBe(0);
    expect(store.blockSelection().length).toBe(1);
    input.remove();
  });

  it("does nothing while a dialog owns the keyboard", async () => {
    const view = await mount(baseState());
    fireEvent.click(view.container.querySelectorAll<HTMLElement>(".clip-block")[1]);
    expect(store.blockSelection().length).toBe(1);

    const dialog = document.createElement("div");
    dialog.setAttribute("role", "dialog");
    const button = document.createElement("button");
    dialog.appendChild(button);
    document.body.appendChild(dialog);
    button.focus();
    fireEvent.keyDown(button, { key: "Backspace" });
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(commandCount("remove_blocks")).toBe(0);
    expect(commandCount("remove_clip")).toBe(0);
    dialog.remove();
  });
});

describe("marquee leaves no text selection behind", () => {
  afterEach(() => window.getSelection()?.removeAllRanges());

  it("drops a stale selection and blocks a new one for the whole drag", async () => {
    const view = await mount(baseState());
    const container = view.container as HTMLElement;
    const label = container.querySelector(".clip-block .phrase-block-name")!.firstChild!;

    // A previous click may have left text selected; a browser drag would add
    // to it. Recreate that state before the marquee starts.
    const stale = document.createRange();
    stale.selectNodeContents(label);
    window.getSelection()!.removeAllRanges();
    window.getSelection()!.addRange(stale);
    expect(window.getSelection()!.toString()).not.toBe("");

    const laneElement = lane(container, 0);
    fireEvent.pointerDown(laneElement, { button: 0, pointerId: 7, clientX: 400, clientY: LANE_TOP + 10 });
    expect(window.getSelection()!.toString()).toBe("");

    const selectStart = new Event("selectstart", { bubbles: true, cancelable: true });
    label.parentElement!.dispatchEvent(selectStart);
    expect(selectStart.defaultPrevented).toBe(true);

    // Even a selection the engine adds anyway must not survive the gesture.
    const during = document.createRange();
    during.selectNodeContents(label);
    window.getSelection()!.addRange(during);

    fireEvent.pointerMove(document, { pointerId: 7, clientX: 300, clientY: LANE_TOP + 40 });
    fireEvent.pointerUp(document, { pointerId: 7, clientX: 300, clientY: LANE_TOP + 40 });

    expect(window.getSelection()!.toString()).toBe("");
  });

  it("lets text selection start again once the gesture ends", async () => {
    const view = await mount(baseState());
    const container = view.container as HTMLElement;

    marquee(container, { x: 400, y: LANE_TOP + 10 }, { x: 100, y: LANE_TOP + 10 });

    const selectStart = new Event("selectstart", { bubbles: true, cancelable: true });
    document.body.dispatchEvent(selectStart);
    expect(selectStart.defaultPrevented).toBe(false);
  });
});

describe("marquee does not steal the clip drag", () => {
  it("still moves a clip when the press starts on it", async () => {
    const view = await mount(baseState());
    const container = view.container as HTMLElement;
    const clip = container.querySelectorAll<HTMLElement>(".clip-block")[1];

    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 200, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 248, clientY: LANE_TOP + 10 });

    // The clip owns the gesture: a ghost follows it, no rubber band appears.
    expect(container.querySelector(".timeline-marquee")).toBeFalsy();
    expect(clip.classList.contains("drag-follow")).toBe(true);

    fireEvent.pointerUp(document, { pointerId: 1, clientX: 248, clientY: LANE_TOP + 10 });

    await vi.waitFor(() => expect(commandCount("move_clip")).toBe(1));
    expect(commandCount("remove_blocks")).toBe(0);
    expect(store.blockSelection()).toEqual([{ kind: "clip", trackIndex: 0, index: 1 }]);
  });
});
