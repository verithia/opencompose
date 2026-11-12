/**
 * Timeline selection and clipboard tests.
 * Covers the frame class for every lane block kind and payload-preserving paste.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import {
  invokeMock,
  makeSnapshot,
  makeTrack,
  makePattern,
  makeNote,
  TRANSPORT_STOPPED,
} from "../setup";
import { store } from "../../stores/project";
import { Timeline } from "../../components/Timeline";
import { parseBinding } from "../../stores/commands";
import { resetAllBindings, setBinding } from "../../stores/keymap";

const PHRASE_RANGE = {
  start: { bar: 1, beat: 1, subdivision: 0 },
  end: { bar: 3, beat: 1, subdivision: 0 },
};

function toQuarter(bb: { bar: number; beat: number; subdivision: number }): number {
  return (bb.bar - 1) * 4 + (bb.beat - 1) + bb.subdivision / 4;
}

function fromQuarter(quarter: number) {
  const bar = Math.floor(quarter / 4) + 1;
  const rest = quarter - (bar - 1) * 4;
  const beat = Math.floor(rest) + 1;
  return { bar, beat, subdivision: Math.round((rest - Math.floor(rest)) * 4) };
}

function retime(block: { time_range: typeof PHRASE_RANGE }, startBar: number, startBeat: number, startSub: number) {
  const duration = toQuarter(block.time_range.end) - toQuarter(block.time_range.start);
  const startQuarter = toQuarter({ bar: startBar, beat: startBeat, subdivision: startSub });
  block.time_range = {
    start: { bar: startBar, beat: startBeat, subdivision: startSub },
    end: fromQuarter(startQuarter + duration),
  };
}

function installBlockMock() {
  let snapshot = makeSnapshot({
    tracks: [{
      ...makeTrack("Piano", 0),
      phrases: [{
        name: "verse",
        time_range: PHRASE_RANGE,
        events: [
          makeNote({ start_beat: 0 }),
          makeNote({ start_beat: 2, pitch: { note: "G", accidental: "Natural", octave: 4 } }),
        ],
      }],
      patterns: [{
        name: "drum_loop",
        time_range: {
          start: { bar: 5, beat: 1, subdivision: 0 },
          end: { bar: 6, beat: 1, subdivision: 0 },
        },
        repeat: 1,
        hits: [
          { drum: "kick", position: { beat: 1, subdivision: 0 }, dynamics: "MF", articulation: null, modifiers: [] },
        ],
      }],
      clips: [{
        pattern: "hook",
        time_range: {
          start: { bar: 9, beat: 1, subdivision: 0 },
          end: { bar: 11, beat: 1, subdivision: 0 },
        },
      }],
    }],
    patterns: [makePattern("hook")],
  });
  const commandCount = (name: string) => invokeMock.mock.calls.filter(([cmd]) => cmd === name).length;

  invokeMock.mockImplementation(async (cmd: string, rawArgs?: unknown) => {
    const args = (rawArgs ?? {}) as Record<string, any>;
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "transport_seek") return { ...TRANSPORT_STOPPED(), position_beats: args.beat ?? 0 };
    if (cmd === "insert_phrase") {
      const copy = structuredClone(args.phrase);
      retime(copy, args.startBar, args.startBeat, args.startSub);
      snapshot.project.tracks[args.trackIndex].phrases.push(copy);
      snapshot = { ...snapshot, dirty: true };
      return structuredClone(snapshot);
    }
    if (cmd === "insert_inline_pattern") {
      const copy = structuredClone(args.pattern);
      retime(copy, args.startBar, args.startBeat, args.startSub);
      snapshot.project.tracks[args.trackIndex].patterns.push(copy);
      snapshot = { ...snapshot, dirty: true };
      return structuredClone(snapshot);
    }
    return structuredClone(snapshot);
  });
  return { commandCount };
}

beforeEach(async () => {
  invokeMock.mockClear();
  installBlockMock();
  await store.initProject();
});

describe("Timeline block selection", () => {
  it("marks a clicked phrase with the selected frame and clears it on the empty lane", () => {
    const { container } = render(() => <Timeline />);
    const phrase = container.querySelector(".phrase-note-block") as HTMLElement;
    const lane = container.querySelector(".track-lane") as HTMLElement;

    fireEvent.click(phrase);
    expect(phrase.classList.contains("selected")).toBe(true);
    expect(container.querySelectorAll(".phrase-block.selected").length).toBe(1);
    expect(store.selectedTrackIndex()).toBe(0);

    fireEvent.click(lane);
    expect(container.querySelector(".phrase-note-block.selected")).toBeFalsy();
    expect(container.querySelectorAll(".phrase-block.selected").length).toBe(0);
  });

  it("marks a clicked inline pattern and moves the frame between block kinds", () => {
    const { container } = render(() => <Timeline />);
    const pattern = container.querySelector(".pattern-block") as HTMLElement;
    const clip = container.querySelector(".clip-block:not(.performance-block)") as HTMLElement;

    fireEvent.click(pattern);
    expect(pattern.classList.contains("selected")).toBe(true);

    fireEvent.click(clip);
    expect(clip.classList.contains("selected")).toBe(true);
    expect(container.querySelector(".pattern-block.selected")).toBeFalsy();
    expect(container.querySelectorAll(".phrase-block.selected").length).toBe(1);
  });
});

describe("Timeline clipboard", () => {
  it("copies a phrase and pastes its notes at the playhead on the same track", async () => {
    const { container } = render(() => <Timeline />);
    const phrase = container.querySelector(".phrase-note-block") as HTMLElement;
    const originalEvents = JSON.parse(JSON.stringify(store.project.tracks[0].phrases[0].events));

    fireEvent.click(phrase);
    await store.seek(8); // bar 3, beat 1
    fireEvent.keyDown(document.body, { key: "c", metaKey: true });
    fireEvent.keyDown(document.body, { key: "v", metaKey: true });

    await vi.waitFor(() => expect(container.querySelectorAll(".phrase-note-block").length).toBe(2));
    const pasted = store.project.tracks[0].phrases[1];
    expect(pasted.events).toEqual(originalEvents);
    expect(pasted.time_range?.start).toEqual({ bar: 3, beat: 1, subdivision: 0 });
    expect(pasted.time_range?.end).toEqual({ bar: 5, beat: 1, subdivision: 0 });
    expect(store.dirty()).toBe(true);
    const pastedElement = container.querySelectorAll(".phrase-note-block")[1] as HTMLElement;
    expect(pastedElement.classList.contains("selected")).toBe(true);
  });

  it("pastes a phrase repeatedly from the same clipboard", async () => {
    const { container } = render(() => <Timeline />);
    fireEvent.click(container.querySelector(".phrase-note-block") as HTMLElement);
    await store.seek(12);

    fireEvent.keyDown(document.body, { key: "c", metaKey: true });
    fireEvent.keyDown(document.body, { key: "v", metaKey: true });
    await vi.waitFor(() => expect(store.project.tracks[0].phrases.length).toBe(2));
    fireEvent.keyDown(document.body, { key: "v", metaKey: true });

    await vi.waitFor(() => expect(store.project.tracks[0].phrases.length).toBe(3));
    expect(store.project.tracks[0].phrases[1].time_range?.start).toEqual({ bar: 4, beat: 1, subdivision: 0 });
    expect(store.project.tracks[0].phrases[2].time_range?.start).toEqual({ bar: 4, beat: 1, subdivision: 0 });
  });

  it("copies an inline pattern and keeps its hits when pasted", async () => {
    const { container } = render(() => <Timeline />);
    const originalHits = JSON.parse(JSON.stringify(store.project.tracks[0].patterns[0].hits));

    fireEvent.click(container.querySelector(".pattern-block") as HTMLElement);
    await store.seek(12); // bar 4, beat 1
    fireEvent.keyDown(document.body, { key: "c", metaKey: true });
    fireEvent.keyDown(document.body, { key: "v", metaKey: true });

    await vi.waitFor(() => expect(store.project.tracks[0].patterns.length).toBe(2));
    const pasted = store.project.tracks[0].patterns[1];
    expect(pasted.hits).toEqual(originalHits);
    expect(pasted.time_range?.start).toEqual({ bar: 4, beat: 1, subdivision: 0 });
    expect(pasted.time_range?.end).toEqual({ bar: 5, beat: 1, subdivision: 0 });
    expect(store.dirty()).toBe(true);
  });

  it("honours a rebound copy key instead of the built-in one", async () => {
    installBlockMock();
    resetAllBindings();
    try {
      const { container } = render(() => <Timeline />);
      fireEvent.click(container.querySelector(".phrase-note-block") as HTMLElement);
      expect(setBinding("edit.copy", parseBinding("Mod+Shift+C")!).ok).toBe(true);

      // The old chord no longer copies, so paste has nothing to place.
      fireEvent.keyDown(document.body, { key: "c", metaKey: true });
      fireEvent.keyDown(document.body, { key: "v", metaKey: true });
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(store.project.tracks[0].phrases.length).toBe(1);

      // The rebound chord reaches the same surface handler.
      fireEvent.keyDown(document.body, { key: "C", metaKey: true, shiftKey: true });
      fireEvent.keyDown(document.body, { key: "v", metaKey: true });
      await vi.waitFor(() => expect(store.project.tracks[0].phrases.length).toBe(2));
    } finally {
      resetAllBindings();
    }
  });

  it("ignores copy and paste while a text input has focus", async () => {
    const mock = installBlockMock();
    const { container } = render(() => <Timeline />);
    fireEvent.click(container.querySelector(".phrase-note-block") as HTMLElement);

    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();
    fireEvent.keyDown(input, { key: "c", metaKey: true });
    fireEvent.keyDown(input, { key: "v", metaKey: true });
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(mock.commandCount("insert_phrase")).toBe(0);
    expect(store.project.tracks[0].phrases.length).toBe(1);
    input.remove();
  });
});
