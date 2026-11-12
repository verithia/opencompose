/**
 * Piano Roll clip origin.
 *
 * The roll's time origin is the opened clip's start and its end is that clip's
 * last bar: x = 0 is the clip's first beat, x = width is its exclusive end, and
 * nothing outside the span exists. A mid-bar clip shows its partial bar at the
 * left edge with truthful (absolute) bar numbers.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { getLastCallArgs, invokeMock, makeSnapshot, makeTrack, makeNote, makePattern } from "../setup";
import { store } from "../../stores/project";
import { PianoRoll, beatAtX, xForBeat } from "../../components/PianoRoll";
import { projectClock } from "../../utils/projectClock";
import { Toolbar } from "../../components/Toolbar";

const CLIP_START_BEATS = 16; // bar 5 in 4/4
const DEFAULT_PX_PER_BEAT = 40;

beforeEach(async () => {
  invokeMock.mockClear();
  const track = makeTrack("lead_wt", 0);
  track.phrases = [{
    name: "melody",
    time_range: { start: { bar: 5, beat: 1, subdivision: 0 }, end: { bar: 9, beat: 1, subdivision: 0 } },
    events: [
      makeNote({ start_beat: 0 }),
      makeNote({ start_beat: 1, pitch: { note: "E", accidental: "Natural", octave: 4 } }),
      makeNote({ start_beat: 2, pitch: { note: "G", accidental: "Natural", octave: 4 } }),
    ],
  }];
  track.clips = [{
    pattern: "beat",
    time_range: { start: { bar: 5, beat: 1, subdivision: 0 }, end: { bar: 9, beat: 1, subdivision: 0 } },
  }];
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    return makeSnapshot({
      patterns: [{ ...makePattern("beat"), hits: [], events: [makeNote()] }],
      tracks: [track],
    });
  });
  await store.initProject();
  store.setSelectedTrackIndex(0);
  store.setSelectedPatternName(null);
});

function scrollport(container: HTMLElement): HTMLDivElement {
  const el = container.querySelector("[data-pianoroll-grid]") as HTMLDivElement;
  Object.defineProperty(el, "clientHeight", { configurable: true, value: 400 });
  Object.defineProperty(el, "clientWidth", { configurable: true, value: 900 });
  return el;
}

function noteLefts(container: HTMLElement): number[] {
  return [...container.querySelectorAll(".pianoroll-note:not(.note-ghost)")]
    .map((note) => parseFloat((note as HTMLElement).style.left));
}

function marks(container: HTMLElement): HTMLElement[] {
  return [...container.querySelectorAll(".pianoroll-ruler-mark")] as HTMLElement[];
}

function canvas(container: HTMLElement): HTMLElement {
  return container.querySelector("[data-note-canvas]") as HTMLElement;
}

function contentWidth(container: HTMLElement): number {
  const content = container.querySelector(".pianoroll-scroll-content") as HTMLElement;
  return parseFloat(content.style.width);
}

describe("PianoRoll clip origin", () => {
  it("makes the opened clip's span the roll's whole range", async () => {
    store.setEditorSelection({ trackIndex: 0, startBeat: CLIP_START_BEATS, patternName: null });
    const { container } = render(() => <PianoRoll />);
    const scroll = scrollport(container);
    const el = canvas(container);

    await vi.waitFor(() => {
      expect(el.getAttribute("data-origin-beat")).toBe(String(CLIP_START_BEATS));
      expect(el.getAttribute("data-view-end-beat")).toBe("32");
      expect(el.getAttribute("data-view-span-beats")).toBe("16");
    });
    expect(scroll.scrollLeft).toBe(0);

    // The clip's first note is at x = 0, and later notes keep their local distance.
    expect(noteLefts(container)).toEqual([0, DEFAULT_PX_PER_BEAT, 2 * DEFAULT_PX_PER_BEAT]);

    // Bar numbers stay truthful: real bar 5 sits on the left edge, and the
    // clip's exclusive end (bar 9) is not drawn as another playable bar.
    const barTexts = marks(container).map((mark) => mark.textContent);
    expect(barTexts).toEqual(["5", "6", "7", "8"]);
    expect(marks(container)[0].style.left).toBe("0px");
    expect(marks(container).every((mark) => parseFloat(mark.style.left) >= 0)).toBe(true);

    // The canvas is exactly the clip's span, so no beat before it (or after it)
    // is scrollable.
    expect(contentWidth(container)).toBe(48 + 16 * DEFAULT_PX_PER_BEAT);

    // The clip bounds are explicit lines at x = 0 and x = span.
    const startEdge = container.querySelector(".pianoroll-ruler .pianoroll-clip-edge.start") as HTMLElement;
    const endEdge = container.querySelector(".pianoroll-ruler .pianoroll-clip-edge.end") as HTMLElement;
    expect(startEdge.style.left).toBe("0px");
    expect(endEdge.style.left).toBe(`${16 * DEFAULT_PX_PER_BEAT - 1}px`);
  });

  it("opens a clip starting at bar 9 with no region before bar 9", async () => {
    const track = makeTrack("lead_wt", 0);
    track.phrases = [{
      name: "melody",
      time_range: { start: { bar: 9, beat: 1, subdivision: 0 }, end: { bar: 13, beat: 1, subdivision: 0 } },
      events: [makeNote({ start_beat: 0 }), makeNote({ start_beat: 1 })],
    }];
    track.clips = [{
      pattern: "beat",
      time_range: { start: { bar: 9, beat: 1, subdivision: 0 }, end: { bar: 13, beat: 1, subdivision: 0 } },
    }];
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      if (cmd === "transport_mute" || cmd === "transport_unmute") return undefined;
      if (cmd === "transport_seek") {
        const beat = (args as { beat: number }).beat;
        return { playing: false, position_beats: beat, position_seconds: beat / 2, duration_seconds: 96 };
      }
      return makeSnapshot({ tracks: [track] });
    });
    await store.initProject();
    store.setSelectedTrackIndex(0);
    store.setSelectedPatternName(null);
    // Bar 9 = beat 32 in 4/4.
    store.setEditorSelection({ trackIndex: 0, startBeat: 32, patternName: null });

    const { container } = render(() => <><Toolbar /><PianoRoll /></>);
    const scroll = scrollport(container);
    const el = canvas(container);

    await vi.waitFor(() => {
      expect(el.getAttribute("data-origin-beat")).toBe("32");
      expect(el.getAttribute("data-view-end-beat")).toBe("48");
      expect(el.getAttribute("data-view-span-beats")).toBe("16");
    });
    expect(scroll.scrollLeft).toBe(0);

    // The first grid line is the clip origin, labelled with the real bar 9.
    const rulerMarks = marks(container);
    expect(rulerMarks[0].textContent).toBe("9");
    expect(rulerMarks[0].style.left).toBe("0px");
    expect(rulerMarks.map((mark) => mark.textContent)).not.toContain("8");
    expect(rulerMarks.every((mark) => parseFloat(mark.style.left) >= 0)).toBe(true);

    // The note at the clip's own beat 0 sits on the left edge.
    expect(noteLefts(container)).toEqual([0, DEFAULT_PX_PER_BEAT]);
    expect(contentWidth(container)).toBe(48 + 16 * DEFAULT_PX_PER_BEAT);

    // A pointer left of the canvas cannot address a beat before bar 9, and a
    // seek from the ruler lands on absolute project time.
    const ruler = container.querySelector(".pianoroll-ruler") as HTMLElement;
    const playhead = () => container.querySelector(".playback-cursor") as HTMLElement;
    fireEvent.pointerDown(ruler, { button: 0, clientX: -400, pointerId: 7 });
    expect(playhead().style.left).toBe("0px");
    expect(container.querySelector(".position-display")!.textContent).toBe("9:1:0");
    fireEvent.pointerUp(document, { button: 0, clientX: -400, pointerId: 7 });
    await vi.waitFor(() => expect(getLastCallArgs("transport_seek")).toMatchObject({ beat: 32 }));
  });

  it("draws a mid-bar clip's partial bar at the left edge", async () => {
    const track = makeTrack("lead_wt", 0);
    track.phrases = [{
      name: "melody",
      time_range: { start: { bar: 3, beat: 3, subdivision: 0 }, end: { bar: 5, beat: 3, subdivision: 0 } },
      events: [makeNote({ start_beat: 0 }), makeNote({ start_beat: 1 })],
    }];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({ tracks: [track] });
    });
    await store.initProject();
    store.setSelectedTrackIndex(0);
    store.setSelectedPatternName(null);
    store.setEditorSelection({ trackIndex: 0, startBeat: 10, patternName: null }); // bar 3 beat 3

    const { container } = render(() => <PianoRoll />);
    const scroll = scrollport(container);
    const el = canvas(container);

    await vi.waitFor(() => {
      expect(el.getAttribute("data-origin-beat")).toBe("10");
      expect(el.getAttribute("data-view-end-beat")).toBe("18");
      expect(el.getAttribute("data-view-span-beats")).toBe("8");
      expect(scroll.scrollLeft).toBe(0);
    });
    expect(noteLefts(container)).toEqual([0, DEFAULT_PX_PER_BEAT]);

    // The bar containing the origin is partial and has no line of its own; the
    // next truthful bar line (bar 4 at quarter 12) appears 2 beats in.
    const rulerMarks = marks(container);
    expect(rulerMarks[0].textContent).toBe("4");
    expect(rulerMarks[0].style.left).toBe(`${2 * DEFAULT_PX_PER_BEAT}px`);
    expect(rulerMarks.map((mark) => mark.textContent)).not.toContain("3");

    // The canvas does not reserve the beats before the clip origin.
    expect(contentWidth(container)).toBe(48 + 8 * DEFAULT_PX_PER_BEAT);
  });

  it("round-trips x to beat and beat to x from the clip origin", () => {
    const origin = CLIP_START_BEATS;
    const ppb = DEFAULT_PX_PER_BEAT;
    for (const beat of [origin, origin + 0.25, origin + 1, origin + 5.5]) {
      expect(beatAtX(origin, ppb, xForBeat(origin, ppb, beat))).toBeCloseTo(beat);
      expect(xForBeat(origin, ppb, beatAtX(origin, ppb, xForBeat(origin, ppb, beat)))).toBeCloseTo(
        xForBeat(origin, ppb, beat),
      );
    }
    // x = 0 is the origin, and no x maps to a beat before it.
    expect(beatAtX(origin, ppb, 0)).toBe(origin);
    expect(beatAtX(origin, ppb, -200)).toBe(origin);
    expect(beatAtX(origin, ppb, 2 * ppb)).toBe(origin + 2);
  });

  it("keeps note drags sample-accurate under the clip origin", async () => {
    store.setEditorSelection({ trackIndex: 0, startBeat: CLIP_START_BEATS, patternName: null });
    const { container } = render(() => <PianoRoll />);
    scrollport(container);
    fireEvent.click(container.querySelector(".pianoroll-toolbar button") as HTMLElement);
    const notes = container.querySelectorAll(".pianoroll-note");
    expect(noteLefts(container)[0]).toBe(0);

    // 30 px = 0.75 beats at 40 px/beat; the origin must not leak into the delta.
    fireEvent.mouseDown(notes[0], { button: 0, clientX: 10, clientY: 100 });
    fireEvent.mouseUp(window, { button: 0, clientX: 40, clientY: 100 });
    expect(getLastCallArgs("move_note")).toMatchObject({ trackIndex: 0, phraseIndex: 0, noteIndex: 0, newStartBeat: 0.75 });
  });

  it("clamps a note's start to the clip and never writes past its end", async () => {
    store.setEditorSelection({ trackIndex: 0, startBeat: CLIP_START_BEATS, patternName: null });
    const { container } = render(() => <PianoRoll />);
    scrollport(container);
    fireEvent.click(container.querySelector(".pianoroll-toolbar button") as HTMLElement);

    // Dragging far right stops at the last grid step inside the 16-beat clip.
    fireEvent.mouseDown(container.querySelector(".pianoroll-note")!, { button: 0, clientX: 0, clientY: 100 });
    fireEvent.mouseUp(window, { button: 0, clientX: 2000, clientY: 100 });
    expect(getLastCallArgs("move_note")).toMatchObject({ newStartBeat: 15.75 });

    // Resizing far right stops at the clip end.
    const handle = container.querySelector(".note-resize-handle")!;
    fireEvent.mouseDown(handle, { button: 0, clientX: 0, clientY: 100 });
    fireEvent.mouseUp(window, { button: 0, clientX: 2000, clientY: 100 });
    expect(getLastCallArgs("set_note_duration")).toMatchObject({ durationBeats: 16 });
  });

  it("draws only inside the clip span", async () => {
    store.setEditorSelection({ trackIndex: 0, startBeat: CLIP_START_BEATS, patternName: null });
    const { container } = render(() => <PianoRoll />);
    scrollport(container);
    const el = canvas(container);

    // A click left of the canvas starts at the origin.
    fireEvent.mouseDown(el, { button: 0, clientX: -400, clientY: 420 });
    fireEvent.mouseUp(window, { button: 0, clientX: -400, clientY: 420 });
    expect(getLastCallArgs("add_note")).toMatchObject({ startBeat: 0, durationBeats: 1 / 4 });

    // A click past the canvas stops at the last grid step before the clip end.
    fireEvent.mouseDown(el, { button: 0, clientX: 1040, clientY: 420 });
    fireEvent.mouseUp(window, { button: 0, clientX: 1040, clientY: 420 });
    expect(getLastCallArgs("add_note")).toMatchObject({ startBeat: 15.75, durationBeats: 1 / 4 });
  });

  it("starts from the opened clip when switching between clip and pattern views", async () => {
    store.setEditorSelection({ trackIndex: 0, startBeat: CLIP_START_BEATS, patternName: null });
    const { container } = render(() => <PianoRoll />);
    const scroll = scrollport(container);
    const el = canvas(container);

    await vi.waitFor(() => {
      expect(el.getAttribute("data-origin-beat")).toBe(String(CLIP_START_BEATS));
      expect(scroll.scrollLeft).toBe(0);
    });

    store.setSelectedPatternName("beat");
    await vi.waitFor(() => {
      // Pattern notes are clip-local, but the ruler still uses the clip's real bar.
      expect(el.getAttribute("data-origin-beat")).toBe(String(CLIP_START_BEATS));
      expect(scroll.scrollLeft).toBe(0);
      expect(noteLefts(container)).toEqual([0]);
    });

    store.setSelectedPatternName(null);
    await vi.waitFor(() => {
      expect(el.getAttribute("data-origin-beat")).toBe(String(CLIP_START_BEATS));
      expect(noteLefts(container)).toEqual([0, DEFAULT_PX_PER_BEAT, 2 * DEFAULT_PX_PER_BEAT]);
    });
  });

  it("opens an inline pattern block at its placed start", async () => {
    const track = makeTrack("lead_wt", 0);
    track.inline_patterns = [{
      ...makePattern("beat"),
      events: [makeNote({ start_beat: 0 }), makeNote({ start_beat: 1 })],
      hits: [],
    }];
    track.patterns = [{
      name: "beat",
      time_range: { start: { bar: 5, beat: 1, subdivision: 0 }, end: { bar: 9, beat: 1, subdivision: 0 } },
      repeat: 1,
      hits: [],
    }];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({ tracks: [track] });
    });
    await store.initProject();
    store.setSelectedTrackIndex(0);
    store.setSelectedPatternName("beat");
    store.setEditorSelection({ trackIndex: 0, startBeat: CLIP_START_BEATS, patternName: null });

    const { container } = render(() => <PianoRoll />);
    const scroll = scrollport(container);
    const el = canvas(container);

    await vi.waitFor(() => {
      expect(el.getAttribute("data-origin-beat")).toBe(String(CLIP_START_BEATS));
      expect(el.getAttribute("data-view-end-beat")).toBe("32");
      expect(scroll.scrollLeft).toBe(0);
    });
    expect(noteLefts(container)).toEqual([0, DEFAULT_PX_PER_BEAT]);
    const rulerMarks = marks(container);
    expect(rulerMarks[0].textContent).toBe("5");
    expect(rulerMarks[0].style.left).toBe("0px");
    expect(contentWidth(container)).toBe(48 + 16 * DEFAULT_PX_PER_BEAT);
  });

  it("re-pins the clip start at other grid and zoom settings", async () => {
    store.setEditorSelection({ trackIndex: 0, startBeat: CLIP_START_BEATS, patternName: null });
    const { container, getByLabelText } = render(() => <PianoRoll />);
    scrollport(container);
    const el = canvas(container);

    await vi.waitFor(() => expect(el.getAttribute("data-origin-beat")).toBe("16"));

    fireEvent.change(container.querySelector(".pianoroll-toolbar select")!, { target: { value: "8" } });
    expect(noteLefts(container)[0]).toBe(0);

    fireEvent.click(getByLabelText("Zoom out"));
    await vi.waitFor(() => expect(noteLefts(container)[0]).toBe(0));
    fireEvent.click(getByLabelText("Zoom out"));
    await vi.waitFor(() => expect(noteLefts(container)[0]).toBe(0));
  });

  it("opens the selected later phrase, not the track's earliest phrase", async () => {
    const track = makeTrack("lead_wt", 0);
    track.phrases = [
      {
        name: "intro",
        time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 2, beat: 1, subdivision: 0 } },
        events: [makeNote({ start_beat: 0 })],
      },
      {
        name: "melody",
        time_range: { start: { bar: 5, beat: 1, subdivision: 0 }, end: { bar: 9, beat: 1, subdivision: 0 } },
        events: [makeNote({ start_beat: 0 }), makeNote({ start_beat: 1, pitch: { note: "E", accidental: "Natural", octave: 4 } })],
      },
    ];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({ tracks: [track] });
    });
    await store.initProject();
    store.setSelectedTrackIndex(0);
    store.setSelectedPatternName(null);
    store.setEditorSelection({ trackIndex: 0, startBeat: CLIP_START_BEATS, patternName: null });

    const { container } = render(() => <PianoRoll />);
    scrollport(container);
    const el = canvas(container);

    await vi.waitFor(() => expect(el.getAttribute("data-origin-beat")).toBe("16"));
    // Notes before the clip origin are not rendered.
    expect(noteLefts(container)).toEqual([0, DEFAULT_PX_PER_BEAT]);

    store.setEditorSelection({ trackIndex: 0, startBeat: 0, patternName: null });
    await vi.waitFor(() => {
      expect(el.getAttribute("data-origin-beat")).toBe("0");
      expect(el.getAttribute("data-view-end-beat")).toBe("4");
      // The earlier phrase is its own editor: only its one note remains.
      expect(noteLefts(container)).toEqual([0]);
    });
  });

  it("keeps a short or empty clip a usable, non-zero grid", async () => {
    const track = makeTrack("lead_wt", 0);
    track.phrases = [{
      name: "stab",
      time_range: { start: { bar: 9, beat: 1, subdivision: 0 }, end: { bar: 9, beat: 2, subdivision: 0 } },
      events: [],
    }];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({ tracks: [track] });
    });
    await store.initProject();
    store.setSelectedTrackIndex(0);
    store.setSelectedPatternName(null);
    store.setEditorSelection({ trackIndex: 0, startBeat: 32, patternName: null });

    const { container } = render(() => <PianoRoll />);
    scrollport(container);
    const el = canvas(container);

    await vi.waitFor(() => {
      expect(el.getAttribute("data-origin-beat")).toBe("32");
      // A one-beat clip is exactly one beat wide, not padded with extra bars.
      expect(el.getAttribute("data-view-span-beats")).toBe("1");
      expect(el.getAttribute("data-view-end-beat")).toBe("33");
    });
    expect(parseFloat(el.style.width)).toBe(DEFAULT_PX_PER_BEAT);
    expect(parseFloat(el.style.height)).toBeGreaterThan(0);
    expect(marks(container).map((mark) => mark.textContent)).toEqual(["9"]);
  });

  it("keeps a zero-length range from collapsing the grid", async () => {
    const track = makeTrack("lead_wt", 0);
    track.phrases = [{
      name: "empty",
      time_range: { start: { bar: 9, beat: 1, subdivision: 0 }, end: { bar: 9, beat: 1, subdivision: 0 } },
      events: [],
    }];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({ tracks: [track] });
    });
    await store.initProject();
    store.setSelectedTrackIndex(0);
    store.setSelectedPatternName(null);
    store.setEditorSelection({ trackIndex: 0, startBeat: 32, patternName: null });

    const { container } = render(() => <PianoRoll />);
    scrollport(container);
    const el = canvas(container);

    await vi.waitFor(() => {
      expect(el.getAttribute("data-origin-beat")).toBe("32");
      expect(el.getAttribute("data-view-span-beats")).toBe("0.25");
    });
    expect(parseFloat(el.style.width)).toBeGreaterThan(0);
    expect(parseFloat(el.style.height)).toBeGreaterThan(0);
    expect(marks(container)[0].textContent).toBe("9");
  });
});

describe("piano roll preview playhead", () => {
  it("draws the pattern preview's own position inside the roll", async () => {
    vi.useFakeTimers();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "preview_pattern") return undefined;
      if (cmd === "get_transport_state") {
        return {
          playing: false,
          position_beats: 16,
          position_seconds: 8,
          duration_seconds: 20,
          audition_active: true,
          audition: {
            paused: false,
            finished: false,
            // One second into the preview: pattern-local time, not the parked
            // arrangement position.
            position_seconds: 1.0,
            preset: null,
            status: "ready" as const,
            ready_ms: 10,
            instances: 1,
            loads: 1,
            error: null,
          },
        };
      }
      return makeSnapshot({
        patterns: [{ ...makePattern("beat"), hits: [], events: [makeNote()] }],
        tracks: [makeTrack("lead_wt", 0)],
      });
    });
    const { container } = render(() => <PianoRoll />);
    scrollport(container);
    store.setEditorSelection({ trackIndex: 0, startBeat: 0, patternName: "beat" });
    const parked = store.transport().position_beats;
    await store.previewPattern("beat");

    const beats = projectClock(store.project.properties).quarterAtSeconds(1);
    await vi.waitFor(() => {
      const line = container.querySelector(".playback-cursor.preview-cursor") as HTMLElement;
      expect(line).toBeTruthy();
      expect(parseFloat(line.style.left)).toBeCloseTo(beats * DEFAULT_PX_PER_BEAT, 3);
    });
    // The arrangement playhead never moved.
    expect(store.transport().position_beats).toBe(parked);
  });
});
