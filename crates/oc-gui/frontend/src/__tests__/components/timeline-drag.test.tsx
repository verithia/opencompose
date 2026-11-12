/**
 * Clip drag behaviour: grid snapping, one commit on release, Escape cancel,
 * cross-track moves and alt-drag copies.
 */
import { describe, it, expect, beforeEach } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack, makePattern, getLastCallArgs } from "../setup";
import { store } from "../../stores/project";
import { setGridDivision, resetGridDivision, gridDivision } from "../../stores/grid";
import { Timeline } from "../../components/Timeline";

const LANE_TOP = 22;
const LANE_HEIGHT = 64;

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

function clipSnapshot() {
  return makeSnapshot({
    patterns: [makePattern("beat")],
    tracks: [{
      ...makeTrack("Drums", 0),
      phrases: [],
      clips: [{
        pattern: "beat",
        time_range: {
          start: { bar: 1, beat: 1, subdivision: 0 },
          end: { bar: 3, beat: 1, subdivision: 0 },
        },
      }],
    }],
  });
}

async function mountWithClip() {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    if (cmd === "move_clip" || cmd === "move_clip_to_track" || cmd === "add_clip") return clipSnapshot();
    return clipSnapshot();
  });
  await store.initProject();
  const view = render(() => <Timeline />);
  stubGeometry(view.container as HTMLElement);
  const clip = view.container.querySelector(".clip-block") as HTMLElement;
  return { ...view, clip };
}

function clipDragCalls(cmd: string) {
  return invokeMock.mock.calls.filter(([name]) => name === cmd);
}

beforeEach(async () => {
  invokeMock.mockClear();
  resetGridDivision();
  await store.initProject();
});

describe("clip drag", () => {
  it("snaps to the grid and commits one moveClip on release", async () => {
    const { clip } = await mountWithClip();
    setGridDivision(4); // 1/16 = 0.25 quarter
    expect(gridDivision()).toBe(4);

    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });

    expect(clipDragCalls("move_clip").length).toBe(1);
    expect(getLastCallArgs("move_clip")).toMatchObject({
      trackIndex: 0, clipIndex: 0, startBar: 1, startBeat: 3, startSub: 0,
    });
  });

  it("follows the selected grid: 1/8 snaps to the half beat", async () => {
    const { clip } = await mountWithClip();
    setGridDivision(2); // 1/8 = 0.5 quarter

    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    // 60px / 24px-per-beat = 2.5 beats; a 1/8 grid keeps the .5.
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 60, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 60, clientY: LANE_TOP + 10 });

    expect(getLastCallArgs("move_clip")).toMatchObject({ startBar: 1, startBeat: 3, startSub: 2 });
  });

  it("moves the grabbed clip itself while the project stays untouched", async () => {
    const { clip, container } = await mountWithClip();
    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });

    // The block follows the pointer in its own lane; no second ghost stands in.
    expect(clip.classList.contains("dragging")).toBe(true);
    expect(clip.classList.contains("drag-follow")).toBe(true);
    expect(clip.style.left).toBe("48px");
    expect(container.querySelector(".clip-drag-ghost")).toBeFalsy();
    expect(clipDragCalls("move_clip").length).toBe(0);

    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });
    expect(clip.classList.contains("drag-follow")).toBe(false);
  });

  it("cancels on Escape without calling the store", async () => {
    const { clip } = await mountWithClip();
    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 72, clientY: LANE_TOP + 10 });
    expect(clip.classList.contains("drag-follow")).toBe(true);

    fireEvent.keyDown(document, { key: "Escape" });
    expect(clip.classList.contains("drag-follow")).toBe(false);
    expect(clip.style.left).toBe("0px");

    fireEvent.pointerUp(document, { pointerId: 1, clientX: 72, clientY: LANE_TOP + 10 });
    expect(clipDragCalls("move_clip").length).toBe(0);
    expect(clipDragCalls("move_clip_to_track").length).toBe(0);
    expect(clipDragCalls("add_clip").length).toBe(0);
  });

  it("commits nothing when the pointer did not move past the threshold", async () => {
    const { clip } = await mountWithClip();
    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 2, clientY: LANE_TOP + 10 });
    expect(clipDragCalls("move_clip").length).toBe(0);
  });

  it("moves the clip to the track under the pointer", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        patterns: [makePattern("beat")],
        tracks: [
          { ...makeTrack("Drums", 0), phrases: [], clips: [{
            pattern: "beat",
            time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 3, beat: 1, subdivision: 0 } },
          }] },
          { ...makeTrack("Bass", 1), phrases: [], clips: [] },
        ],
      });
    });
    await store.initProject();
    const { container } = render(() => <Timeline />);
    stubGeometry(container as HTMLElement);
    const clip = container.querySelector(".clip-block") as HTMLElement;

    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + LANE_HEIGHT + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + LANE_HEIGHT + 10 });

    expect(clipDragCalls("move_clip_to_track").length).toBe(1);
    expect(clipDragCalls("move_clip").length).toBe(0);
    expect(getLastCallArgs("move_clip_to_track")).toMatchObject({
      fromTrackIndex: 0, clipIndex: 0, toTrackIndex: 1,
      startBar: 1, startBeat: 3, startSub: 0,
    });
  });

  it("refuses a drop below the last lane instead of taking the last track", async () => {
    const { clip } = await mountWithClip();
    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    // 200 px below the only lane is over the bus rows, not a track.
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 210 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 210 });

    expect(clipDragCalls("move_clip").length).toBe(0);
    expect(clipDragCalls("move_clip_to_track").length).toBe(0);
  });

  it("alt-drag copies the clip with the same pattern name", async () => {
    const { clip } = await mountWithClip();
    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10, altKey: true });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });

    expect(clipDragCalls("move_clip").length).toBe(0);
    expect(clipDragCalls("add_clip").length).toBe(1);
    expect(getLastCallArgs("add_clip")).toMatchObject({
      trackIndex: 0, patternName: "beat", startBar: 1, startBeat: 3, startSub: 0,
    });
  });

  it("does not start a drag from the resize handle", async () => {
    const { clip } = await mountWithClip();
    const handle = clip.querySelector(".phrase-resize-handle") as HTMLElement;
    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 180, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 220, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 220, clientY: LANE_TOP + 10 });
    expect(clipDragCalls("move_clip").length).toBe(0);
  });
});
