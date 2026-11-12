/**
 * Drag behaviour for every timeline block kind: named pattern clips, inline
 * phrases and inline drum patterns. The project must change on release only.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeNote, makePattern, makeTrack, getLastCallArgs } from "../setup";
import { store } from "../../stores/project";
import { resetGridDivision } from "../../stores/grid";
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

interface BackendState {
  tracks: any[];
  patterns: any[];
}

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

function installBackend(state: BackendState) {
  invokeMock.mockImplementation(async (cmd: string, args?: any) => {
    const a = args ?? {};
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    if (cmd === "move_clip") {
      state.tracks = state.tracks.map((track, index) => index !== a.trackIndex ? track : {
        ...track,
        clips: track.clips.map((clip: any, clipIndex: number) => clipIndex !== a.clipIndex ? clip : {
          ...clip,
          time_range: moveRange(a.startBar, a.startBeat, a.startSub),
        }),
      });
    }
    if (cmd === "move_clip_to_track") {
      const clip = state.tracks[a.fromTrackIndex].clips[a.clipIndex];
      const moved = { ...clip, time_range: moveRange(a.startBar, a.startBeat, a.startSub) };
      state.tracks = state.tracks.map((track, index) => {
        if (index === a.fromTrackIndex) return { ...track, clips: track.clips.filter((_: any, i: number) => i !== a.clipIndex) };
        if (index === a.toTrackIndex) return { ...track, clips: [...track.clips, moved] };
        return track;
      });
    }
    if (cmd === "move_phrase") {
      state.tracks = state.tracks.map((track, index) => index !== a.trackIndex ? track : {
        ...track,
        phrases: track.phrases.map((phrase: any, phraseIndex: number) => phraseIndex !== a.phraseIndex ? phrase : {
          ...phrase,
          time_range: moveRange(a.startBar, a.startBeat, a.startSub),
        }),
      });
    }
    if (cmd === "move_phrase_to_track") {
      const phrase = state.tracks[a.fromTrackIndex].phrases[a.phraseIndex];
      const moved = { ...phrase, time_range: moveRange(a.startBar, a.startBeat, a.startSub) };
      state.tracks = state.tracks.map((track, index) => {
        if (index === a.fromTrackIndex) return { ...track, phrases: track.phrases.filter((_: any, i: number) => i !== a.phraseIndex) };
        if (index === a.toTrackIndex) return { ...track, phrases: [...track.phrases, moved] };
        return track;
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
    if (cmd === "extend_phrase_range") {
      state.tracks = state.tracks.map((track, index) => index !== a.trackIndex ? track : {
        ...track,
        phrases: track.phrases.map((phrase: any, phraseIndex: number) => phraseIndex !== a.phraseIndex ? phrase : {
          ...phrase,
          time_range: { ...phrase.time_range, end: { bar: a.endBar, beat: a.endBeat, subdivision: a.endSub } },
        }),
      });
    }
    if (cmd === "move_inline_pattern") {
      state.tracks = state.tracks.map((track, index) => index !== a.trackIndex ? track : {
        ...track,
        patterns: track.patterns.map((pattern: any, patternIndex: number) => patternIndex !== a.patternIndex ? pattern : {
          ...pattern,
          time_range: moveRange(a.startBar, a.startBeat, a.startSub),
        }),
      });
    }
    return makeSnapshot({ tracks: state.tracks, patterns: state.patterns });
  });
}

async function mount(state: BackendState) {
  installBackend(state);
  await store.initProject();
  const view = render(() => <Timeline />);
  stubGeometry(view.container as HTMLElement);
  return view;
}

function commandCount(name: string) {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === name).length;
}

function baseState(): BackendState {
  return {
    patterns: [makePattern("beat")],
    tracks: [
      { ...makeTrack("Drums", 0), phrases: [], clips: [{ pattern: "beat", time_range: range([1, 1, 0], [3, 1, 0]) }] },
      { ...makeTrack("Bass", 1), phrases: [], clips: [] },
    ],
  };
}

beforeEach(async () => {
  invokeMock.mockClear();
  resetGridDivision();
  await store.initProject();
});

describe("timeline block drag", () => {
  it("moves a named clip and writes the new start into the project", async () => {
    const state = baseState();
    const view = await mount(state);
    const clip = view.container.querySelector(".clip-block") as HTMLElement;

    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });

    await vi.waitFor(() => expect(commandCount("move_clip")).toBe(1));
    expect(getLastCallArgs("move_clip")).toMatchObject({
      trackIndex: 0, clipIndex: 0, startBar: 1, startBeat: 3, startSub: 0,
    });
    await vi.waitFor(() => expect(store.project.tracks[0].clips[0].time_range.start)
      .toMatchObject({ bar: 1, beat: 3, subdivision: 0 }));
  });

  it("drags a named clip across tracks into the target lane", async () => {
    const state = baseState();
    const view = await mount(state);
    const clip = view.container.querySelector(".clip-block") as HTMLElement;

    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + LANE_HEIGHT + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + LANE_HEIGHT + 10 });

    await vi.waitFor(() => expect(commandCount("move_clip_to_track")).toBe(1));
    expect(getLastCallArgs("move_clip_to_track")).toMatchObject({
      fromTrackIndex: 0, clipIndex: 0, toTrackIndex: 1, startBar: 1, startBeat: 3,
    });
    await vi.waitFor(() => expect(store.project.tracks[0].clips.length).toBe(0));
    expect(store.project.tracks[1].clips.length).toBe(1);
  });

  it("leaves a clip on its track when the backend refuses a cross-kind move", async () => {
    // The backend owns the rule that a pattern clip never lands on a
    // `performance:` track; the timeline must not select a clip it refused.
    const state = baseState();
    state.tracks[1] = {
      ...makeTrack("Perf", 1),
      phrases: [],
      clips: [],
      performance: {
        events: [{ quarter: 0, channel: 0, message: { type: "note_on", note: 60, velocity: 90 } }],
      },
    };
    installBackend(state);
    const backend = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "move_clip_to_track") {
        throw "Invalid value for Perf.performance: performance commands cannot coexist with note-derived content";
      }
      return backend(cmd, args);
    });
    await store.initProject();
    const view = render(() => <Timeline />);
    stubGeometry(view.container as HTMLElement);
    store.setBlockSelection([]);
    const clip = view.container.querySelector(".clip-block") as HTMLElement;

    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + LANE_HEIGHT + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + LANE_HEIGHT + 10 });

    await vi.waitFor(() => expect(store.error()).toContain("performance commands cannot coexist"));
    expect(store.project.tracks[0].clips.length).toBe(1);
    expect(store.project.tracks[1].clips.length).toBe(0);
    // The clip did not move, so it stays the picked block; no phantom pick on
    // the lane the move was refused for.
    expect(store.blockSelection()).toMatchObject([
      { kind: "clip", trackIndex: 0, index: 0 },
    ]);
  });

  it("drags an inline phrase to a snapped start and updates the project", async () => {
    const state = baseState();
    state.tracks[0] = {
      ...state.tracks[0],
      clips: [],
      phrases: [{
        name: "melody",
        time_range: range([1, 1, 0], [3, 1, 0]),
        events: [makeNote({ start_beat: 0 })],
      }],
    };
    const view = await mount(state);
    const phrase = view.container.querySelector(".phrase-note-block") as HTMLElement;
    expect(phrase).toBeTruthy();

    fireEvent.pointerDown(phrase, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    // 50px at 24px/beat is 2.083 beats; the 1/16 grid snaps it to 2.
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 50, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 50, clientY: LANE_TOP + 10 });

    await vi.waitFor(() => expect(commandCount("move_phrase")).toBe(1));
    expect(getLastCallArgs("move_phrase")).toMatchObject({
      trackIndex: 0, phraseIndex: 0, startBar: 1, startBeat: 3, startSub: 0,
    });
    await vi.waitFor(() => expect(store.project.tracks[0].phrases[0]!.time_range!.start)
      .toMatchObject({ bar: 1, beat: 3, subdivision: 0 }));
  });

  it("moves an inline phrase across tracks", async () => {
    const state = baseState();
    state.tracks[0] = {
      ...state.tracks[0],
      clips: [],
      phrases: [{
        name: "melody",
        time_range: range([1, 1, 0], [3, 1, 0]),
        events: [makeNote({ start_beat: 0 })],
      }],
    };
    const view = await mount(state);
    const phrase = view.container.querySelector(".phrase-note-block") as HTMLElement;

    fireEvent.pointerDown(phrase, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + LANE_HEIGHT + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + LANE_HEIGHT + 10 });

    await vi.waitFor(() => expect(commandCount("move_phrase_to_track")).toBe(1));
    expect(getLastCallArgs("move_phrase_to_track")).toMatchObject({
      fromTrackIndex: 0, phraseIndex: 0, toTrackIndex: 1, startBar: 1, startBeat: 3,
    });
    await vi.waitFor(() => expect(store.project.tracks[0].phrases.length).toBe(0));
    expect(store.project.tracks[1].phrases.length).toBe(1);
    expect(store.project.tracks[1].phrases[0]!.name).toBe("melody");
  });

  it("drags an inline drum pattern without losing its repeat count", async () => {
    const state = baseState();
    state.tracks[0] = {
      ...state.tracks[0],
      clips: [],
      patterns: [{
        name: "main_beat",
        time_range: range([1, 1, 0], [2, 1, 0]),
        repeat: 4,
        hits: [{ drum: "kick", position: { beat: 1, subdivision: 0 }, dynamics: "MF", articulation: null, modifiers: [] }],
      }],
    };
    const view = await mount(state);
    const pattern = view.container.querySelector(".pattern-block") as HTMLElement;
    expect(pattern).toBeTruthy();

    fireEvent.pointerDown(pattern, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });

    await vi.waitFor(() => expect(commandCount("move_inline_pattern")).toBe(1));
    expect(getLastCallArgs("move_inline_pattern")).toMatchObject({
      trackIndex: 0, patternIndex: 0, startBar: 1, startBeat: 3, startSub: 0,
    });
    await vi.waitFor(() => expect(store.project.tracks[0].patterns[0]!.time_range!.start)
      .toMatchObject({ bar: 1, beat: 3, subdivision: 0 }));
    expect(store.project.tracks[0].patterns[0]!.repeat).toBe(4);
  });

  it("follows the pointer with the block itself and commits exactly once on release", async () => {
    const state = baseState();
    state.tracks[0] = {
      ...state.tracks[0],
      clips: [],
      phrases: [{
        name: "melody",
        time_range: range([1, 1, 0], [3, 1, 0]),
        events: [makeNote({ start_beat: 0 })],
      }],
    };
    const view = await mount(state);
    const phrase = view.container.querySelector(".phrase-note-block") as HTMLElement;

    fireEvent.pointerDown(phrase, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 24, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 72, clientY: LANE_TOP + 10 });

    // The preview is local: the block follows the pointer, nothing is written yet.
    expect(commandCount("move_phrase")).toBe(0);
    expect(commandCount("move_phrase_to_track")).toBe(0);
    expect(phrase.classList.contains("drag-follow")).toBe(true);
    expect(phrase.style.left).toBe("72px");
    // The block stands in for itself inside its own lane; no second ghost.
    expect(view.container.querySelector(".clip-drag-ghost")).toBeFalsy();

    fireEvent.pointerUp(document, { pointerId: 1, clientX: 72, clientY: LANE_TOP + 10 });
    await vi.waitFor(() => expect(commandCount("move_phrase")).toBe(1));
    // The project change rebuilds the lane, so look the block up again.
    await vi.waitFor(() => {
      const committed = view.container.querySelector(".phrase-note-block") as HTMLElement;
      expect(committed.style.left).toBe("72px");
      expect(committed.classList.contains("drag-follow")).toBe(false);
    });
  });

  it("resizes a named clip from its right edge without moving the start", async () => {
    const state = baseState();
    const view = await mount(state);
    const clip = view.container.querySelector(".clip-block") as HTMLElement;
    const handle = clip.querySelector(".phrase-resize-handle") as HTMLElement;

    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 192, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 240, clientY: LANE_TOP + 10 });
    // The edge is dragged, not the block: the width grows, the start stays.
    expect(clip.style.width).toBe("240px");
    expect(clip.classList.contains("drag-follow")).toBe(false);
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 240, clientY: LANE_TOP + 10 });

    await vi.waitFor(() => expect(commandCount("resize_clip")).toBe(1));
    expect(getLastCallArgs("resize_clip")).toMatchObject({
      trackIndex: 0, clipIndex: 0, endBar: 3, endBeat: 3, endSub: 0,
    });
    await vi.waitFor(() => expect(store.project.tracks[0].clips[0]!.time_range.end)
      .toMatchObject({ bar: 3, beat: 3, subdivision: 0 }));
    expect(store.project.tracks[0].clips[0]!.time_range.start)
      .toMatchObject({ bar: 1, beat: 1, subdivision: 0 });
  });

  it("resizes an inline phrase from its right edge", async () => {
    const state = baseState();
    state.tracks[0] = {
      ...state.tracks[0],
      clips: [],
      phrases: [{
        name: "melody",
        time_range: range([1, 1, 0], [3, 1, 0]),
        events: [makeNote({ start_beat: 0 })],
      }],
    };
    const view = await mount(state);
    const phrase = view.container.querySelector(".phrase-note-block") as HTMLElement;
    const handle = phrase.querySelector(".phrase-resize-handle") as HTMLElement;

    fireEvent.pointerDown(handle, { button: 0, pointerId: 1, clientX: 192, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 216, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 216, clientY: LANE_TOP + 10 });

    await vi.waitFor(() => expect(commandCount("extend_phrase_range")).toBe(1));
    expect(getLastCallArgs("extend_phrase_range")).toMatchObject({
      trackIndex: 0, phraseIndex: 0, endBar: 3, endBeat: 2, endSub: 0,
    });
    await vi.waitFor(() => expect(store.project.tracks[0].phrases[0]!.time_range!.end)
      .toMatchObject({ bar: 3, beat: 2, subdivision: 0 }));
  });

  it("treats a mouse release with another pointer id as the end of the drag", async () => {
    const state = baseState();
    const view = await mount(state);
    const clip = view.container.querySelector(".clip-block") as HTMLElement;

    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });
    expect(clip.classList.contains("drag-follow")).toBe(true);

    // A release the webview reports with a different id must not strand the session.
    fireEvent.pointerUp(document, { pointerId: 2, clientX: 48, clientY: LANE_TOP + 10 });
    await vi.waitFor(() => expect(commandCount("move_clip")).toBe(1));
    await vi.waitFor(() => expect(store.project.tracks[0].clips[0]!.time_range!.start)
      .toMatchObject({ bar: 1, beat: 3, subdivision: 0 }));

    // The next drag grabs the relocated clip, so it uses a fresh grab offset.
    // The lane rows are rebuilt from the snapshot, so stub the new geometry.
    stubGeometry(view.container as HTMLElement);
    const moved = view.container.querySelector(".clip-block") as HTMLElement;
    expect(moved.style.left).toBe("48px");
    fireEvent.pointerDown(moved, { button: 0, pointerId: 1, clientX: 60, clientY: LANE_TOP + 10 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 84, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 84, clientY: LANE_TOP + 10 });
    await vi.waitFor(() => expect(commandCount("move_clip")).toBe(2));
    expect(getLastCallArgs("move_clip")).toMatchObject({
      trackIndex: 0, clipIndex: 0, startBar: 1, startBeat: 4, startSub: 0,
    });
  });

  it("does not commit a phrase drag below the pointer threshold", async () => {
    const state = baseState();
    state.tracks[0] = {
      ...state.tracks[0],
      clips: [],
      phrases: [{
        name: "melody",
        time_range: range([1, 1, 0], [3, 1, 0]),
        events: [makeNote({ start_beat: 0 })],
      }],
    };
    const view = await mount(state);
    const phrase = view.container.querySelector(".phrase-note-block") as HTMLElement;

    fireEvent.pointerDown(phrase, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 2, clientY: LANE_TOP + 10 });

    expect(commandCount("move_phrase")).toBe(0);
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(store.project.tracks[0].phrases[0]!.time_range!.start).toMatchObject({ bar: 1, beat: 1, subdivision: 0 });
  });

  it("measures the lanes once per drag instead of once per move", async () => {
    const state = baseState();
    const view = await mount(state);
    // Count the lane rect reads the drag is allowed to make.
    const reads = Array.from(view.container.querySelectorAll<HTMLElement>(".track-lane"))
      .map((lane) => {
        const stub = lane.getBoundingClientRect.bind(lane);
        const spy = vi.fn(() => stub());
        lane.getBoundingClientRect = spy as unknown as typeof lane.getBoundingClientRect;
        return spy;
      });
    const rectReads = () => reads.reduce((sum, spy) => sum + spy.mock.calls.length, 0);

    const clip = view.container.querySelector(".clip-block") as HTMLElement;
    fireEvent.pointerDown(clip, { button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10 });
    // The first move of a drag may measure; no later move may measure again.
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 24, clientY: LANE_TOP + 10 });
    const afterFirstMove = rectReads();
    expect(afterFirstMove).toBeGreaterThan(0);

    for (let step = 1; step <= 6; step++) {
      fireEvent.pointerMove(document, {
        pointerId: 1,
        clientX: 24 + step * 12,
        clientY: LANE_TOP + 10 + (step % 2) * LANE_HEIGHT,
      });
    }

    expect(rectReads()).toBe(afterFirstMove);
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 120, clientY: LANE_TOP + 10 });
  });
});

describe("named clip slip", () => {
  /** A four-beat clip over a pattern whose content reaches beat 7. */
  function slipState(): BackendState {
    const beats = [1, 3, 5, 7];
    return {
      patterns: [{
        ...makePattern("beat"),
        hits: beats.map((beat) => ({
          drum: "kick",
          position: { beat, subdivision: 0 },
          dynamics: "MF",
          articulation: null,
          modifiers: [],
        })),
      }],
      tracks: [
        { ...makeTrack("Drums", 0), phrases: [], clips: [{ pattern: "beat", time_range: range([1, 1, 0], [2, 1, 0]) }] },
      ],
    };
  }

  it("slips the content with Shift+Alt and writes the offset once on release", async () => {
    const view = await mount(slipState());
    stubGeometry(view.container as HTMLElement);
    const clip = view.container.querySelector(".clip-block") as HTMLElement;

    fireEvent.pointerDown(clip, {
      button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10,
      altKey: true, shiftKey: true,
    });
    // 24 px is one beat at this zoom; dragging left hides the pattern head.
    fireEvent.pointerMove(document, { pointerId: 1, clientX: -24, clientY: LANE_TOP + 10 });
    expect(view.container.querySelector(".clip-slip-readout")!.textContent).toContain("Content offset 1 q");
    expect(commandCount("set_pattern_clip_offset")).toBe(0);
    expect(commandCount("move_clip")).toBe(0);

    fireEvent.pointerUp(document, { pointerId: 1, clientX: -24, clientY: LANE_TOP + 10 });

    await vi.waitFor(() => expect(commandCount("set_pattern_clip_offset")).toBe(1));
    expect(getLastCallArgs("set_pattern_clip_offset")).toMatchObject({
      trackIndex: 0, clipIndex: 0, offsetQuarters: 1,
    });
    expect(commandCount("begin_undo_gesture")).toBe(1);
    // The block never moved: slip changes content only.
    expect(commandCount("move_clip")).toBe(0);
    expect(commandCount("add_clip")).toBe(0);
  });

  it("keeps copy on a plain Alt-drag", async () => {
    const view = await mount(slipState());
    stubGeometry(view.container as HTMLElement);
    const clip = view.container.querySelector(".clip-block") as HTMLElement;

    fireEvent.pointerDown(clip, {
      button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10, altKey: true,
    });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 48, clientY: LANE_TOP + 10 });

    await vi.waitFor(() => expect(commandCount("add_clip")).toBe(1));
    expect(commandCount("set_pattern_clip_offset")).toBe(0);
  });

  it("refuses a clip that already shows the whole pattern", async () => {
    const state = slipState();
    // The pattern's content is two beats; a four-beat clip hides nothing.
    state.patterns[0] = makePattern("beat");
    const view = await mount(state);
    stubGeometry(view.container as HTMLElement);
    const clip = view.container.querySelector(".clip-block") as HTMLElement;

    fireEvent.pointerDown(clip, {
      button: 0, pointerId: 1, clientX: 0, clientY: LANE_TOP + 10,
      altKey: true, shiftKey: true,
    });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: -24, clientY: LANE_TOP + 10 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: -24, clientY: LANE_TOP + 10 });

    expect(commandCount("set_pattern_clip_offset")).toBe(0);
    expect(store.error()).toBe("Nothing to slip: this clip already shows the whole pattern.");
  });
});
