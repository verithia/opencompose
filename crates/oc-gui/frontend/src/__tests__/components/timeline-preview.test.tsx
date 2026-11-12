/**
 * Timeline regressions: clip thumbnails ignore rests, track preview paints its
 * own playhead, and the clip menu offers a transport-free audition.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import { readFileSync } from "node:fs";
import {
  TRANSPORT_PLAYING,
  TRANSPORT_STOPPED,
  getLastCallArgs,
  invokeMock,
  makeNote,
  makePattern,
  makeSnapshot,
  makeTrack,
} from "../setup";
import { store } from "../../stores/project";
import { Timeline } from "../../components/Timeline";
import { projectClock } from "../../utils/projectClock";
import { ContextMenuLayer, closeContextMenu } from "../../components/ContextMenu";
import type { Accidental, Duration, NoteName, Pitch } from "../../types/project";

function pitch(note: NoteName, accidental: Accidental, octave: number): Pitch {
  return { note, accidental, octave };
}

const QUARTER: Duration = { base: "Quarter", dots: 0, triplet: false, tuplet: null, ties: [] };
const HALF: Duration = { base: "Half", dots: 0, triplet: false, tuplet: null, ties: [] };

/** The seven parsed events of `ours_plain_p`: six notes plus one `rest q`. */
function plainPhraseEvents() {
  return [
    makeNote({ start_beat: 0, pitch: pitch("C", "Natural", 3), duration: QUARTER }),
    makeNote({ start_beat: 1, pitch: pitch("E", "Flat", 3), duration: QUARTER }),
    makeNote({ start_beat: 2, pitch: pitch("G", "Natural", 3), duration: QUARTER }),
    makeNote({ start_beat: 3, pitch: pitch("B", "Flat", 3), duration: QUARTER }),
    makeNote({ start_beat: 4, pitch: pitch("C", "Natural", 4), duration: HALF }),
    // Parsed rests are C0 events carrying the `rest` modifier.
    makeNote({
      start_beat: 6,
      pitch: pitch("C", "Natural", 0),
      duration: QUARTER,
      modifiers: [["rest", { Bool: true }]],
    }),
    makeNote({ start_beat: 7, pitch: pitch("B", "Flat", 3), duration: QUARTER }),
  ];
}

function phraseTrack() {
  const track = makeTrack("Lead", 0);
  track.phrases = [{
    name: "ours_plain_p",
    time_range: {
      start: { bar: 1, beat: 1, subdivision: 0 },
      end: { bar: 3, beat: 1, subdivision: 0 },
    },
    events: plainPhraseEvents(),
  }];
  return track;
}

function notePattern(name: string) {
  const pattern = makePattern(name);
  pattern.events = plainPhraseEvents();
  pattern.hits = [];
  return pattern;
}

function thumbTops(container: HTMLElement): number[] {
  return [...container.querySelectorAll<HTMLElement>(".clip-note-thumb")]
    .map((thumb) => parseFloat(thumb.style.top));
}

/** Piano roll tops are linear at NOTE_HEIGHT px per semitone; normalize the same range. */
function pianoRollRelative(midi: number, min: number, max: number): number {
  return (midi - min) / (max - min);
}

const PLAIN_MIDIS = [48, 51, 55, 58, 60, 58];

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    return makeSnapshot({ tracks: [phraseTrack()], patterns: [notePattern("lead")] });
  });
  await store.initProject();
  closeContextMenu();
});

afterEach(() => {
  vi.useRealTimers();
  closeContextMenu();
});

describe("clip thumbnails", () => {
  it("draws only the six pitched notes of a phrase containing a rest", () => {
    const { container } = render(() => <Timeline />);
    const block = container.querySelector(".phrase-note-block") as HTMLElement;
    const thumbs = container.querySelectorAll(".clip-note-thumb");

    expect(thumbs.length).toBe(6);
    // The 7th parsed event is the rest; it must not inflate the note count.
    expect(block.querySelector(".clip-meta")?.textContent).toBe("6n");
    const tops = thumbTops(container);
    // Pitch range comes from the drawn notes only: C3 sits on the bottom edge,
    // C4 on the top edge. A C0 rest would have pushed C3 into the middle.
    expect(tops[0]).toBeCloseTo(88, 3);
    expect(tops[4]).toBeCloseTo(8, 3);
  });

  it("gives equal pitches equal heights and matches piano-roll geometry", () => {
    const { container } = render(() => <Timeline />);
    const tops = thumbTops(container);
    const min = Math.min(...PLAIN_MIDIS);
    const max = Math.max(...PLAIN_MIDIS);

    expect(tops.length).toBe(PLAIN_MIDIS.length);
    expect(tops[3]).toBeCloseTo(tops[5], 6);
    for (let index = 0; index < PLAIN_MIDIS.length; index++) {
      const relative = (88 - tops[index]) / 80;
      expect(relative).toBeCloseTo(pianoRollRelative(PLAIN_MIDIS[index], min, max), 6);
    }
  });

  it("applies the same rest filter to a named pattern clip", async () => {
    const track = makeTrack("Piano", 0);
    track.phrases = [];
    track.clips = [{
      pattern: "lead",
      time_range: {
        start: { bar: 1, beat: 1, subdivision: 0 },
        end: { bar: 3, beat: 1, subdivision: 0 },
      },
    }];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot({ tracks: [track], patterns: [notePattern("lead")] });
    });
    await store.initProject();
    const { container } = render(() => <Timeline />);
    expect(container.querySelectorAll(".clip-note-thumb").length).toBe(6);
    expect(container.querySelector(".clip-block .clip-meta")?.textContent).toBe("6n 0h");
  });
});

describe("inline drum thumbnails", () => {
  it("shows repeated hits in stable drum rows", async () => {
    const track = makeTrack("Drums", 0);
    track.phrases = [];
    track.patterns = [{ name: "intro", repeat: 4,
      time_range: { start: { bar: 2, beat: 2, subdivision: 1 }, end: { bar: 3, beat: 2, subdivision: 1 } },
      hits: ["kick", "snare", "kick"].map((drum, i) => ({ drum, position: { beat: i + 1, subdivision: 0 }, dynamics: null, articulation: null, modifiers: [] })),
    }];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot({ tracks: [track] });
    });
    await store.initProject();
    const { container } = render(() => <Timeline />);
    const block = container.querySelector<HTMLElement>(".pattern-block")!;
    const hits = [...block.querySelectorAll<HTMLElement>(".clip-hit-thumb")];
    expect(hits).toHaveLength(12);
    expect(hits.map(hit => parseFloat(hit.style.left))).toEqual([0, 6.25, 12.5, 25, 31.25, 37.5, 50, 56.25, 62.5, 75, 81.25, 87.5]);
    expect(hits[0].style.top).toBe(hits[2].style.top);
    expect(hits[0].style.top).not.toBe(hits[1].style.top);
    expect(block.style.left).toBe("126px");
    expect(block.style.width).toBe("384px");
    expect(block.querySelector(".clip-thumbnail")?.getAttribute("aria-hidden")).toBe("true");
    expect(block.textContent).toContain("intro x4");
  });
});

describe("track preview playhead", () => {
  beforeEach(() => {
    vi.useFakeTimers();
  });

  it("follows the polled preview position, then clears without moving the cursor", async () => {
    let previewBeat = 17.5;
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "preview_track") {
        return {
          playing: true,
          position_beats: 16,
          position_seconds: 8,
          duration_seconds: 20,
          did_render: true,
          warnings: [],
        };
      }
      if (cmd === "get_transport_state") {
        return { ...TRANSPORT_PLAYING(), position_beats: previewBeat };
      }
      if (cmd === "transport_pause") return TRANSPORT_STOPPED();
      if (cmd === "transport_seek") {
        return { ...TRANSPORT_STOPPED(), position_beats: (args as { beat: number }).beat };
      }
      return makeSnapshot({ tracks: [phraseTrack()] });
    });
    await store.initProject();
    const { container } = render(() => <Timeline />);

    await store.previewTrack(0);
    expect(store.transport().position_beats).toBe(0);
    expect(store.previewCursorBeats()).toBe(16);
    const previewLine = container.querySelector(".playback-cursor.preview-cursor") as HTMLElement;
    expect(previewLine).toBeTruthy();
    expect(previewLine.style.left).toBe(`${16 * 24}px`);

    await vi.advanceTimersByTimeAsync(60);
    expect(store.previewCursorBeats()).toBe(17.5);
    expect(container.querySelector(".playback-cursor.preview-cursor")?.getAttribute("style"))
      .toContain(`${17.5 * 24}px`);

    // The backend reports the pass over; the preview cursor must disappear.
    previewBeat = 20;
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "transport_pause") return TRANSPORT_STOPPED();
      if (cmd === "transport_seek") {
        return { ...TRANSPORT_STOPPED(), position_beats: (args as { beat: number }).beat };
      }
      return makeSnapshot({ tracks: [phraseTrack()] });
    });
    await vi.advanceTimersByTimeAsync(250);
    expect(store.previewCursorBeats()).toBeNull();
    expect(store.transport().position_beats).toBe(0);
    expect(container.querySelector(".playback-cursor.preview-cursor")).toBeFalsy();
  });
});

describe("block preview menu", () => {
  it("auditions the clip's own range without selecting it or touching the transport", async () => {
    const track = makeTrack("Lead", 0);
    track.phrases = [];
    track.clips = [{
      pattern: "lead",
      time_range: {
        start: { bar: 1, beat: 1, subdivision: 0 },
        end: { bar: 3, beat: 1, subdivision: 0 },
      },
    }];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "preview_block") return [];
      if (cmd === "get_transport_state") {
        return { ...TRANSPORT_STOPPED(), audition_active: true };
      }
      return makeSnapshot({ tracks: [track], patterns: [notePattern("lead")] });
    });
    await store.initProject();
    // Opening a document names its first track; the preview must not move that
    // pick, so the assertion below compares against what the load left.
    const trackBeforePreview = store.selectedTrackIndex();
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);

    fireEvent.contextMenu(container.querySelector(".clip-block") as HTMLElement, {
      clientX: 120,
      clientY: 90,
    });
    const item = [...document.querySelectorAll(".context-menu-item")]
      .find((entry) => entry.querySelector(".context-menu-label")?.textContent === "Preview clip");
    expect(item).toBeTruthy();
    fireEvent.click(item as HTMLElement);

    await vi.waitFor(() => expect(getLastCallArgs("preview_block")).toMatchObject({
      trackIndex: 0,
      start: { bar: 1, beat: 1, subdivision: 0 },
      end: { bar: 3, beat: 1, subdivision: 0 },
    }));
    const commands = invokeMock.mock.calls.map(([cmd]) => cmd);
    expect(commands).not.toContain("transport_play");
    expect(commands).not.toContain("transport_seek");
    expect(commands).not.toContain("transport_pause");
    expect(store.selectedTrackIndex()).toBe(trackBeforePreview);
    expect(store.selectedPatternName()).toBeNull();
    expect(store.editorSelection()).toBeNull();

    await store.stopAudition();
  });
});

it("disables named drum clip audition when the only hit shifts to its exclusive end", async () => {
  const track = makeTrack("Drums", 0);
  track.phrases = [];
  track.clips = [{
    pattern: "late",
    time_range: {
      start: { bar: 1, beat: 1, subdivision: 0 },
      end: { bar: 1, beat: 1, subdivision: 3 },
    },
  }];
  const pattern = {
    ...notePattern("late"),
    events: [],
    swing: 75,
    hits: [{
      drum: "hihat",
      position: { beat: 1, subdivision: 2 },
      dynamics: null,
      articulation: null,
      modifiers: [],
    }],
  };
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    return makeSnapshot({ tracks: [track], patterns: [pattern] });
  });
  await store.initProject();
  const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);

  expect(container.querySelectorAll(".clip-hit-thumb")).toHaveLength(0);
  fireEvent.contextMenu(container.querySelector(".clip-block") as HTMLElement);
  const item = [...document.querySelectorAll(".context-menu-item")]
    .find((entry) => entry.querySelector(".context-menu-label")?.textContent === "Preview clip");
  expect(item?.getAttribute("aria-disabled")).toBe("true");
  fireEvent.click(item as HTMLElement);
  expect(getLastCallArgs("preview_block")).toBeUndefined();
});

describe("preview cursor drag takeover", () => {
  it.each([[false, 0], [false, 4], [true, 0], [true, 4]] as const)(
    "drag from preview playing=%s to beat%s commits before immediate Space", async (playing, target) => {
      await store.stop();
      let position = 8;
      let finish!: () => void;
      invokeMock.mockImplementation(async (cmd: string, args?: any) => {
        if (cmd === "preview_track") { position = 3; return { ...TRANSPORT_PLAYING(), position_beats: position }; }
        if (cmd === "transport_pause") return { ...TRANSPORT_STOPPED(), position_beats: position };
        if (cmd === "transport_seek") {
          if (args.beat === target) await new Promise<void>(resolve => { finish = resolve; });
          position = args.beat;
          return { ...TRANSPORT_STOPPED(), position_beats: position, position_seconds: position / 2 };
        }
        if (cmd === "transport_play") return { ...TRANSPORT_PLAYING(), position_beats: position, position_seconds: position / 2 };
        if (cmd === "get_transport_state") return { ...TRANSPORT_PLAYING(), position_beats: position };
        return makeSnapshot({ tracks: [phraseTrack()] });
      });
      await store.seek(8);
      await store.previewTrack(0);
      if (!playing) await store.toggleTrackPreview();
      const { container } = render(() => <Timeline />);
      const head = container.querySelector(".cursor-head")!;
      expect(head).toBeTruthy();
      fireEvent.pointerDown(head, { button: 0, pointerId: 7, clientX: 72 });
      fireEvent.pointerMove(document, { pointerId: 7, clientX: target * 24, buttons: 1 });
      expect((container.querySelector(".playback-cursor") as HTMLElement).style.left).toBe(`${target * 24}px`);
      fireEvent.pointerUp(document, { pointerId: 7, clientX: target * 24, button: 0 });
      const space = store.togglePlayPause();
      await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
      expect(invokeMock.mock.calls.some(([cmd]) => cmd === "resume_track_preview" || cmd === "transport_play")).toBe(false);
      finish();
      await space;
      await vi.waitFor(() => expect(store.scrubPosition()).toBeNull());
      expect(store.previewTrackIndex()).toBeNull();
      expect(store.transport().playing).toBe(true);
      expect(store.transport().position_beats).toBe(target);
      expect(getLastCallArgs("transport_seek")).toEqual({ beat: target });
      await store.stop();
    });

  it("pointercancel restores paused preview without committing a seek or retaining mute", async () => {
    await store.stop();
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "preview_track") return { ...TRANSPORT_PLAYING(), position_beats: 3 };
      if (cmd === "transport_pause") return { ...TRANSPORT_STOPPED(), position_beats: 3 };
      return TRANSPORT_STOPPED();
    });
    await store.previewTrack(0);
    await store.toggleTrackPreview();
    invokeMock.mockClear();
    const { container } = render(() => <Timeline />);
    fireEvent.pointerDown(container.querySelector(".cursor-head")!, { button: 0, pointerId: 8, clientX: 72 });
    fireEvent.pointerMove(document, { pointerId: 8, clientX: 0, buttons: 1 });
    fireEvent.pointerCancel(document, { pointerId: 8 });
    fireEvent.pointerUp(document, { pointerId: 8, clientX: 0, button: 0 });
    expect(store.scrubPosition()).toBeNull();
    expect(store.previewTrackIndex()).toBe(0);
    expect(store.transport().playing).toBe(false);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "transport_seek")).toBe(false);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "transport_unmute")).toHaveLength(1);
    await store.stop();
  });
});

describe("clip preview playhead", () => {
  it("paints the preview position at the block's own place on the arrangement", async () => {
    vi.useFakeTimers();
    const track = makeTrack("Lead", 0);
    track.phrases = [];
    track.clips = [{
      pattern: "lead",
      time_range: {
        start: { bar: 2, beat: 1, subdivision: 0 },
        end: { bar: 4, beat: 1, subdivision: 0 },
      },
    }];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "preview_block") return [];
      if (cmd === "get_transport_state") {
        return {
          ...TRANSPORT_STOPPED(),
          audition_active: true,
          audition: {
            paused: false,
            finished: false,
            // Two seconds into the preview at 120 bpm: four beats in.
            position_seconds: 2.0,
            preset: null,
            status: "ready" as const,
            ready_ms: 12,
            instances: 1,
            loads: 1,
            error: null,
          },
        };
      }
      return makeSnapshot({ tracks: [track], patterns: [notePattern("lead")] });
    });
    await store.initProject();
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);

    fireEvent.contextMenu(container.querySelector(".clip-block") as HTMLElement, {
      clientX: 120,
      clientY: 90,
    });
    const item = [...document.querySelectorAll(".context-menu-item")]
      .find((entry) => entry.querySelector(".context-menu-label")?.textContent === "Preview clip");
    fireEvent.click(item as HTMLElement);

    // The clip starts at bar 2 (four beats); the preview plays from there.
    const beats = 4 + projectClock(store.project.properties).quarterAtSeconds(2);
    await vi.waitFor(() => {
      expect(container.querySelector(".playback-cursor.preview-cursor")?.getAttribute("style"))
        .toContain(`${beats * 24}px`);
    });
    // The arrangement playhead stays parked where the user left it.
    expect(store.transport().position_beats).toBe(0);
  });
});

describe("playhead handle", () => {
  it("uses a drag cursor instead of a resize one", () => {
    // vitest runs without CSS, so the rule is read from the stylesheet: a
    // horizontal-resize cursor made the playhead read as a zoom handle.
    const css = readFileSync(`${process.cwd()}/src/styles/daw/pianoroll.css`, "utf8");
    const rule = css.slice(css.indexOf(".cursor-head {"), css.indexOf(".cursor-head:hover"));
    expect(rule).toContain("cursor: grab");
    expect(rule).not.toContain("cursor: ew-resize");
    expect(css).toContain(".cursor-head:active");
  });
});

describe("pattern preview on the arrangement", () => {
  it("paints the preview where the pattern sits, not at the parked playhead", async () => {
    vi.useFakeTimers();
    const track = makeTrack("Lead", 0);
    track.phrases = [];
    track.clips = [{
      pattern: "lead",
      time_range: {
        start: { bar: 2, beat: 1, subdivision: 0 },
        end: { bar: 4, beat: 1, subdivision: 0 },
      },
    }];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "preview_pattern") return [];
      if (cmd === "get_transport_state") {
        return {
          ...TRANSPORT_STOPPED(),
          audition_active: true,
          audition: {
            paused: false,
            finished: false,
            position_seconds: 2.0,
            preset: null,
            status: "ready" as const,
            ready_ms: 12,
            instances: 1,
            loads: 1,
            error: null,
          },
        };
      }
      return makeSnapshot({ tracks: [track], patterns: [notePattern("lead")] });
    });
    await store.initProject();
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);

    // The piano roll previews a pattern, which has no transport position of its
    // own; the timeline still has to show where it plays.
    await store.previewPattern("lead");

    const clock = projectClock(store.project.properties);
    const beats = clock.quarterAt({ bar: 2, beat: 1, subdivision: 0 })
      + clock.quarterAtSeconds(2);
    await vi.waitFor(() => expect(store.auditionArrangementBeats()).toBeCloseTo(beats, 6));
    await vi.waitFor(() => {
      expect(container.querySelector(".playback-cursor.preview-cursor")?.getAttribute("style"))
        .toContain(`${beats * 24}px`);
    });
    expect(container.querySelector(".cursor-head")?.classList.contains("preview-cursor-head")).toBe(true);
  });
});

describe("track colour", () => {
  it("paints the mixer's colour on the header, the clips and the lane", async () => {
    const track = phraseTrack();
    track.mix = { ...track.mix, overrides: { color: "#123456" } };
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot({ tracks: [track], patterns: [notePattern("lead")] });
    });
    await store.initProject();
    const { container } = render(() => <Timeline />);
    // One colour for the strip and the clips it hosts.
    const bar = container.querySelector<HTMLElement>(".track-color-bar")!;
    expect(bar.style.background).toContain("rgb(18, 52, 86)");
    const block = container.querySelector<HTMLElement>(".phrase-note-block")!;
    expect(block.style.getPropertyValue("--clip-accent")).toBe("#123456");
  });
});
