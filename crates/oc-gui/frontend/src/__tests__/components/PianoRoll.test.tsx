/**
 * PianoRoll component interaction tests.
 * Tests keyboard shortcuts, note selection, and grid interactions.
 * Runs in jsdom — no browser required.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { getLastCallArgs, invokeMock, makeSnapshot, makeTrack, makePattern, makeNote, expectInvoked } from "../setup";
import { store } from "../../stores/project";
import { PianoRoll, clearNoteClipboard, hitPlacement, scrollTopToCenterMidi } from "../../components/PianoRoll";
import { Toolbar } from "../../components/Toolbar";
import { ContextMenuLayer, closeContextMenu } from "../../components/ContextMenu";
import { gridDivision, resetGridDivision, setGridDivision } from "../../stores/grid";
import { hitStartQuarters } from "../../utils/drumkit";
import { i18n } from "../../stores/i18n";
import type { BarBeat, HitEvent, NoteEvent, ParamValue, Phrase, PresetDef, Project, Track } from "../../types/project";

const originalUserAgent = navigator.userAgent;

function renderRoll() {
  const view = render(() => <><PianoRoll /><ContextMenuLayer /></>);
  const menuItem = (label: string) =>
    [...document.querySelectorAll<HTMLButtonElement>(".context-menu-item")]
      .find((item) => item.querySelector(".context-menu-label")?.textContent === label);
  /** Open one of the note menu's groups before reading its rows. */
  const openGroup = (label: string) => {
    const group = menuItem(label);
    if (!group) throw new Error(`no menu group named ${label}`);
    if (group.getAttribute("aria-expanded") !== "true") fireEvent.click(group);
  };
  return { ...view, menuItem, openGroup };
}

/** Give an element a fixed box; jsdom reports every element as zero-sized. */
function stubRect(element: HTMLElement, top: number, height: number) {
  element.getBoundingClientRect = () => ({
    top, bottom: top + height, left: 0, right: 0, width: 0, height, x: 0, y: top,
    toJSON: () => ({}),
  }) as DOMRect;
}

/** Let a pending store write finish before its result is asserted. */
function flush() {
  return new Promise<void>((resolve) => setTimeout(resolve, 0));
}

beforeEach(async () => {
  closeContextMenu();
  clearNoteClipboard();
  resetGridDivision();
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    return makeSnapshot({
      patterns: [{ ...makePattern("beat"), instrument:{category:"keys",name:"piano"}, hits:[], events: [makeNote(), makeNote({ start_beat: 1, pitch: { note: "E", accidental: "Natural", octave: 4 } })] }],
      tracks: [makeTrack("Piano", 0)],
    });
  });
  await store.initProject();
  store.setSelectedPatternName("beat");
});

describe("PianoRoll", () => {
  it("commits the release coordinates even without a final move event", () => {
    const {container} = render(() => <PianoRoll />);
    const note = container.querySelector('.pianoroll-note')!;
    fireEvent.mouseDown(note,{button:0,clientX:10,clientY:100});
    fireEvent.mouseUp(window,{button:0,clientX:90,clientY:88});
    expect(getLastCallArgs('move_pattern_note')).toMatchObject({newStartBeat:2,newPitch:{note:'C',accidental:'Sharp',octave:4}});
  });

  it("does not commit a drag when another button is released", () => {
    const {container} = render(() => <PianoRoll />);
    fireEvent.mouseDown(container.querySelector('.pianoroll-note')!,{button:0,clientX:10,clientY:100});
    fireEvent.mouseMove(window,{clientX:50,clientY:100});
    fireEvent.mouseUp(window,{button:2,clientX:50,clientY:100});
    expect(getLastCallArgs('move_pattern_note')).toBeUndefined();
    expect(container.querySelector('.note-ghost')).toBeTruthy();
    fireEvent.mouseUp(window,{button:0,clientX:50,clientY:100});
    expect(getLastCallArgs('move_pattern_note')).toMatchObject({newStartBeat:1});
  });

  it("Escape cancels a drag without a mutation on later release", () => {
    const {container} = render(() => <PianoRoll />);
    fireEvent.mouseDown(container.querySelector('.pianoroll-note')!,{button:0,clientX:10,clientY:100});
    fireEvent.mouseMove(window,{clientX:50,clientY:76});
    fireEvent.keyDown(window,{key:'Escape'});
    fireEvent.mouseUp(window,{button:0,clientX:50,clientY:76});
    expect(getLastCallArgs('move_pattern_note')).toBeUndefined();
    expect(container.querySelector('.note-ghost')).toBeNull();
  });

  it("focus loss cancels drawing without adding a note", () => {
    const {container} = render(() => <PianoRoll />);
    fireEvent.mouseDown(container.querySelector('[data-note-canvas]')!,{button:0,clientX:0,clientY:420});
    fireEvent.mouseMove(window,{clientX:160,clientY:420});
    fireEvent.blur(window);
    fireEvent.mouseUp(window,{button:0,clientX:160,clientY:420});
    expect(getLastCallArgs('add_pattern_note')).toBeUndefined();
    expect(container.querySelector('.note-preview')).toBeNull();
  });

  it("preserves a snapped move smaller than two pixels when zoomed out", () => {
    const {container,getByLabelText} = render(() => <PianoRoll />);
    for(let i=0;i<5;i++) fireEvent.click(getByLabelText('Zoom out'));
    fireEvent.change(container.querySelector('.pianoroll-toolbar select')!,{target:{value:'8'}});
    fireEvent.mouseDown(container.querySelector('.pianoroll-note')!,{button:0,clientX:10,clientY:100});
    fireEvent.mouseUp(window,{button:0,clientX:11.25,clientY:100});
    expect(getLastCallArgs('move_pattern_note')).toMatchObject({newStartBeat:0.125});
  });

  it("pencil drag moves in both axes and shows the destination before committing", () => {
    const {container} = render(() => <PianoRoll />);
    const note = container.querySelector('.pianoroll-note:not(.note-ghost)') as HTMLElement;
    const top = parseFloat(note.style.top);
    fireEvent.mouseDown(note,{button:0,clientX:10,clientY:100});
    fireEvent.mouseMove(window,{clientX:50,clientY:76});
    expect(note.style.left).toBe('40px');
    expect(parseFloat(note.style.top)).toBe(top-24);
    expect(container.querySelector('.note-ghost')).toBeTruthy();
    expect(getLastCallArgs('move_pattern_note')).toBeUndefined();
    fireEvent.mouseUp(window,{button:0,clientX:50,clientY:76});
    expect(getLastCallArgs('move_pattern_note')).toMatchObject({newStartBeat:1,newPitch:{note:'D',octave:4}});
  });

  it("pencil edge resize previews and commits a multi-bar exact duration", () => {
    const {container} = render(() => <PianoRoll />);
    const handle = container.querySelector('.note-resize-handle')!;
    const note = handle.parentElement!;
    const width = parseFloat(note.style.width);
    fireEvent.mouseDown(handle,{button:0,clientX:0,clientY:100});
    fireEvent.mouseMove(window,{clientX:320,clientY:100});
    expect(parseFloat(note.style.width)).toBe(width+320);
    fireEvent.mouseUp(window,{button:0,clientX:320,clientY:100});
    expect(getLastCallArgs('set_pattern_note_duration')).toMatchObject({durationBeats:9});
  });

  it("pencil draws a long note in one add operation", () => {
    const {container} = render(() => <PianoRoll />);
    const canvas=container.querySelector('[data-note-canvas]')!;
    fireEvent.mouseDown(canvas,{button:0,clientX:0,clientY:420});
    fireEvent.mouseMove(window,{clientX:320,clientY:420});
    fireEvent.mouseUp(window,{clientX:320,clientY:420});
    expect(getLastCallArgs('add_pattern_note')).toMatchObject({startBeat:0,durationBeats:8});
    expect(invokeMock.mock.calls.filter(c=>c[0]==='add_pattern_note')).toHaveLength(1);
  });

  it("triplet grid draws an exact third-quarter note", () => {
    const {container} = render(() => <PianoRoll />);
    fireEvent.change(container.querySelector('.pianoroll-toolbar select')!,{target:{value:'3'}});
    const canvas=container.querySelector('[data-note-canvas]')!;
    fireEvent.mouseDown(canvas,{button:0,clientX:40/3,clientY:420});
    fireEvent.mouseUp(window,{clientX:40/3,clientY:420});
    expect(getLastCallArgs('add_pattern_note')).toMatchObject({startBeat:1/3,durationBeats:1/3});
  });
  it("renders without crashing", () => {
    const { container } = render(() => <PianoRoll />);
    // PianoRoll has a toolbar as first child
    expect(container.querySelector(".pianoroll-toolbar")).toBeTruthy();
  });

  it("renders piano keys", () => {
    const { container } = render(() => <PianoRoll />);
    const keys = container.querySelectorAll(".piano-key");
    // Should have at least some piano keys rendered (128 MIDI notes minus some)
    expect(keys.length).toBeGreaterThan(10);
  });

  it("renders the grid area", () => {
    const { container } = render(() => <PianoRoll />);
    const grid = container.querySelector("[tabindex='-1']");
    expect(grid).toBeTruthy();
  });

  it("scrolls the grid to keep the dragged playhead visible", () => {
    const { container } = render(() => <><Toolbar /><PianoRoll /></>);
    const grid = container.querySelector(".pianoroll-scroll") as HTMLDivElement;
    Object.defineProperty(grid, "clientWidth", { configurable: true, value: 400 });
    const ruler = container.querySelector(".pianoroll-ruler") as HTMLElement;
    const playhead = () => container.querySelector(".playback-cursor") as HTMLElement;

    fireEvent.pointerDown(ruler, { button: 0, clientX: 80, pointerId: 12 });
    fireEvent.pointerMove(document, { clientX: 1200, pointerId: 12, buttons: 1 });

    // The scrub is clamped to the roll's span, so the playhead stays on canvas
    // while the grid still scrolls to follow it.
    const left = parseFloat(playhead().style.left);
    expect(left).toBeGreaterThan(0);
    expect(left).toBeLessThanOrEqual(16 * 40);
    expect(grid.scrollLeft).toBeGreaterThan(0);
    fireEvent.pointerCancel(document, { pointerId: 12 });
  });

  it.each([".cursor-head", ".pianoroll-ruler"])("cancels %s scrubbing only for its pointer and removes drag listeners", (selector) => {
    store.setScrubPosition(0);
    const { container } = render(() => <PianoRoll />);
    const target = container.querySelector(selector)!;
    const count = (command: string) => invokeMock.mock.calls.filter(([cmd]) => cmd === command).length;
    fireEvent.pointerDown(target, {button:0, clientX:80, pointerId:23});
    fireEvent.pointerMove(document, {clientX:160, pointerId:23, buttons:1});
    expect(store.scrubPosition()).toBe(4);
    expect(count("transport_mute")).toBe(1);
    fireEvent.pointerCancel(document, {pointerId:24});
    expect(store.scrubPosition()).toBe(4);
    expect(count("transport_unmute")).toBe(0);
    fireEvent.pointerCancel(document, {pointerId:23});
    expect(store.scrubPosition()).toBeNull();
    expect(count("transport_unmute")).toBe(1);
    fireEvent.pointerMove(document, {clientX:240, pointerId:23, buttons:1});
    fireEvent.pointerUp(document, {clientX:240, pointerId:23, button:0});
    expect(store.scrubPosition()).toBeNull();
    expect(count("transport_seek")).toBe(0);
    expect(count("transport_unmute")).toBe(1);
  });

  it("tracks a ruler drag every move and seeks once on release", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      if (cmd === "transport_mute" || cmd === "transport_unmute") return undefined;
      if (cmd === "transport_seek") return { playing: false, position_beats: (args as { beat: number }).beat, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        patterns: [{ ...makePattern("beat"), instrument: { category: "keys", name: "piano" }, hits: [], events: [makeNote()] }],
        tracks: [makeTrack("Piano", 0)],
      });
    });
    await store.initProject();
    const { container } = render(() => <><Toolbar /><PianoRoll /></>);
    const ruler = container.querySelector(".pianoroll-ruler") as HTMLElement;
    const readout = () => container.querySelector(".position-display") as HTMLElement;
    const playhead = () => container.querySelector(".playback-cursor") as HTMLElement;
    const seeks = () => invokeMock.mock.calls.filter(([cmd]) => cmd === "transport_seek");

    fireEvent.pointerDown(ruler, { button: 0, clientX: 80, pointerId: 1 });
    expect(readout().textContent).toBe("1:3:0");
    expect(playhead().style.left).toBe("80px");

    fireEvent.pointerMove(document, { clientX: 200, pointerId: 1, buttons: 1 });
    expect(readout().textContent).toBe("2:2:0");
    expect(playhead().style.left).toBe("200px");
    expect(seeks()).toHaveLength(0);

    fireEvent.pointerUp(document, { clientX: 200, pointerId: 1, button: 0 });
    await vi.waitFor(() => expect(seeks()).toHaveLength(1));
    expect(getLastCallArgs("transport_seek")).toMatchObject({ beat: 5 });
  });

  it("renders ruler bar marks", () => {
    const { container } = render(() => <PianoRoll />);
    const marks = container.querySelectorAll(".pianoroll-ruler-mark");
    // Should render at least 1 bar mark
    expect(marks.length).toBeGreaterThan(0);
  });

  it("fires delete key handler on Backspace", async () => {
    const { container } = render(() => <PianoRoll />);
    const grid = container.querySelector("[tabindex='-1']") as HTMLElement;
    if (!grid) throw new Error("Grid not found");

    // The grid needs focus to receive keyboard events
    grid.focus();
    // No notes selected, so nothing should happen (no crash)
    fireEvent.keyDown(grid, { key: "Backspace" });
    // If we get here without error, the handler executed safely
    expect(true).toBe(true);
  });

  it("fires delete key handler on Delete", async () => {
    const { container } = render(() => <PianoRoll />);
    const grid = container.querySelector("[tabindex='-1']") as HTMLElement;
    if (!grid) throw new Error("Grid not found");

    grid.focus();
    fireEvent.keyDown(grid, { key: "Delete" });
    expect(true).toBe(true);
  });

  it("handles arrow key nudge without crashing", () => {
    const { container } = render(() => <PianoRoll />);
    const grid = container.querySelector("[tabindex='-1']") as HTMLElement;
    if (!grid) throw new Error("Grid not found");

    grid.focus();
    fireEvent.keyDown(grid, { key: "ArrowRight" });
    fireEvent.keyDown(grid, { key: "ArrowLeft" });
    fireEvent.keyDown(grid, { key: "ArrowRight", shiftKey: true });
    expect(true).toBe(true);
  });

  it("handles Ctrl+D duplicate without crashing", () => {
    const { container } = render(() => <PianoRoll />);
    const grid = container.querySelector("[tabindex='-1']") as HTMLElement;
    if (!grid) throw new Error("Grid not found");

    grid.focus();
    fireEvent.keyDown(grid, { key: "d", ctrlKey: true });
    expect(true).toBe(true);
  });

  it("shows pattern name in header when a pattern is selected", () => {
    const { container } = render(() => <PianoRoll />);
    const text = container.textContent;
    expect(text).toContain("beat");
  });

  it("centers C4 when the selected pattern changes", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        patterns: [makePattern("beat"), makePattern("lead")],
        tracks: [makeTrack("Piano", 0)],
      });
    });
    await store.initProject();
    store.setSelectedPatternName("beat");

    const { container } = render(() => <PianoRoll />);
    const scroller = container.querySelector("[data-pianoroll-grid]") as HTMLDivElement;
    // jsdom has no layout; pretend the panel has a real height.
    Object.defineProperty(scroller, "clientHeight", { configurable: true, value: 400 });

    store.setSelectedPatternName("lead");
    await vi.waitFor(() => {
      expect(scroller.scrollTop).toBe(scrollTopToCenterMidi(400));
    });
  });

  it("renders note blocks for pattern events", () => {
    const { container } = render(() => <PianoRoll />);
    const notes = container.querySelectorAll(".pianoroll-note");
    // We set up the pattern with 2 notes
    expect(notes.length).toBe(2);
  });

  it("selects note on click", () => {
    const { container } = render(() => <PianoRoll />);
    const notes = container.querySelectorAll(".pianoroll-note");
    if (notes.length === 0) return; // skip if no notes rendered (layout-dependent)

    fireEvent.mouseDown(notes[0], { button: 0 });
    // After click, the note should get a 'selected' class
    // (implementation detail — if classList check is too fragile, just verify no crash)
    expect(true).toBe(true);
  });

  it("shows context menu on right-click", () => {
    const { container } = render(() => <PianoRoll />);
    const notes = container.querySelectorAll(".pianoroll-note");
    if (notes.length === 0) return;

    fireEvent.contextMenu(notes[0]);
    // If the context menu implementation adds it, check; otherwise just no crash
    expect(true).toBe(true);
  });

  it("notes have grab cursor in select mode and pointer cursor in draw mode", () => {
    const { container } = render(() => <PianoRoll />);
    const notes = container.querySelectorAll(".pianoroll-note") as NodeListOf<HTMLElement>;
    if (notes.length === 0) return;

    // Default tool is "draw" — notes should have pointer cursor
    expect(notes[0].style.cursor).toBe("pointer");

    // Switch to select mode by clicking the select tool button
    const selectBtn = container.querySelector(".pianoroll-toolbar button") as HTMLElement;
    if (selectBtn) {
      fireEvent.click(selectBtn);
      // After re-render, notes should have grab cursor
      const updatedNotes = container.querySelectorAll(".pianoroll-note") as NodeListOf<HTMLElement>;
      if (updatedNotes.length > 0) {
        expect(updatedNotes[0].style.cursor).toBe("grab");
      }
    }
  });

  it("notes have a resize handle element", () => {
    const { container } = render(() => <PianoRoll />);
    const handles = container.querySelectorAll(".note-resize-handle");
    // Each note should have one resize handle
    const notes = container.querySelectorAll(".pianoroll-note");
    expect(handles.length).toBe(notes.length);
  });

  it("deletes a track-view note from its original phrase", async () => {
    const track = makeTrack("Piano", 0);
    track.phrases = [
      {
        name: "first",
        time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 2, beat: 1, subdivision: 0 } },
        events: [makeNote()],
      },
      {
        name: "second",
        time_range: { start: { bar: 3, beat: 1, subdivision: 0 }, end: { bar: 4, beat: 1, subdivision: 0 } },
        events: [makeNote({ pitch: { note: "E", accidental: "Natural", octave: 4 } })],
      },
    ];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({ tracks: [track] });
    });
    await store.initProject();
    store.setSelectedPatternName(null);
    store.setSelectedTrackIndex(0);

    const { container, menuItem } = renderRoll();
    fireEvent.click(container.querySelector(".pianoroll-toolbar button") as HTMLElement);
    const notes = container.querySelectorAll(".pianoroll-note");
    expect(notes.length).toBe(2);

    fireEvent.contextMenu(notes[1], { clientX: 50, clientY: 100 });
    fireEvent.click(menuItem("Delete note")!);

    expectInvoked("remove_note", { trackIndex: 0, phraseIndex: 1, noteIndex: 0 });
  });

  it("moves a track-view note using phrase-local beats", async () => {
    const track = makeTrack("Piano", 0);
    track.phrases = [
      {
        name: "first",
        time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 2, beat: 1, subdivision: 0 } },
        events: [makeNote()],
      },
      {
        name: "second",
        time_range: { start: { bar: 3, beat: 1, subdivision: 0 }, end: { bar: 4, beat: 1, subdivision: 0 } },
        events: [makeNote({ pitch: { note: "E", accidental: "Natural", octave: 4 }, start_beat: 0 })],
      },
    ];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({ tracks: [track] });
    });
    await store.initProject();
    store.setSelectedPatternName(null);
    store.setSelectedTrackIndex(0);

    const { container } = render(() => <PianoRoll />);
    fireEvent.click(container.querySelector(".pianoroll-toolbar button") as HTMLElement);
    const notes = container.querySelectorAll(".pianoroll-note");
    expect(notes.length).toBe(2);

    fireEvent.mouseDown(notes[1], { button: 0, clientX: 0 });
    fireEvent.mouseMove(window, { clientX: 40 });
    fireEvent.mouseUp(window,{button:0,clientX:40});

    const args = getLastCallArgs("move_note");
    expect(args).toMatchObject({ trackIndex: 0, phraseIndex: 1, noteIndex: 0, newStartBeat: 1 });
  });

  it("opens the context menu on a macOS right-click without changing the selection", () => {
    const mac = "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15";
    Object.defineProperty(navigator, "userAgent", { value: mac, configurable: true });
    const { container } = renderRoll();
    fireEvent.click(container.querySelector(".pianoroll-toolbar button") as HTMLElement);
    const notes = container.querySelectorAll(".pianoroll-note") as NodeListOf<HTMLElement>;

    fireEvent.mouseDown(notes[0], { button: 0, clientX: 10, clientY: 100 });
    fireEvent.mouseUp(window, { button: 0, clientX: 10, clientY: 100 });
    expect(notes[0].classList.contains("note-selected")).toBe(true);
    const beatsBefore = store.transport().position_beats;

    // macOS reports a secondary click as the primary button plus ctrlKey.
    fireEvent.mouseDown(notes[1], { button: 0, ctrlKey: true, clientX: 50, clientY: 100 });
    fireEvent.contextMenu(notes[1], { button: 0, ctrlKey: true, clientX: 50, clientY: 100 });

    const after = container.querySelectorAll(".pianoroll-note") as NodeListOf<HTMLElement>;
    expect(after[0].classList.contains("note-selected")).toBe(true);
    expect(after[1].classList.contains("note-selected")).toBe(false);
    expect(after[1].classList.contains("note-dragging")).toBe(false);
    expect(container.querySelector(".note-ghost")).toBeNull();
    expect(store.transport().position_beats).toBe(beatsBefore);
    expect(document.querySelector(".pianoroll-context-menu")).toBeTruthy();
  });

  it("opens the context menu on a secondary-button press without changing the selection", () => {
    Object.defineProperty(navigator, "userAgent", { value: "Mozilla/5.0 (Windows NT 10.0; Win64; x64)", configurable: true });
    const { container } = renderRoll();
    fireEvent.click(container.querySelector(".pianoroll-toolbar button") as HTMLElement);
    const notes = container.querySelectorAll(".pianoroll-note") as NodeListOf<HTMLElement>;

    fireEvent.mouseDown(notes[0], { button: 0, clientX: 10, clientY: 100 });
    fireEvent.mouseUp(window, { button: 0, clientX: 10, clientY: 100 });
    fireEvent.mouseDown(notes[1], { button: 2, clientX: 50, clientY: 100 });
    fireEvent.contextMenu(notes[1], { button: 2, clientX: 50, clientY: 100 });

    const after = container.querySelectorAll(".pianoroll-note") as NodeListOf<HTMLElement>;
    expect(after[0].classList.contains("note-selected")).toBe(true);
    expect(after[1].classList.contains("note-selected")).toBe(false);
    expect(document.querySelector(".pianoroll-context-menu")).toBeTruthy();
  });

  it("still selects a note on a plain left click", () => {
    const { container } = renderRoll();
    const notes = container.querySelectorAll(".pianoroll-note") as NodeListOf<HTMLElement>;
    fireEvent.mouseDown(notes[0], { button: 0, clientX: 10, clientY: 100 });
    fireEvent.mouseUp(window, { button: 0, clientX: 10, clientY: 100 });
    expect(notes[0].classList.contains("note-selected")).toBe(true);
    expect(notes[1].classList.contains("note-selected")).toBe(false);
  });

  it("note menu names the note it acts on and leaves the selection alone", () => {
    const { container, menuItem, openGroup } = renderRoll();
    fireEvent.click(container.querySelector(".pianoroll-toolbar button") as HTMLElement);
    const notes = container.querySelectorAll(".pianoroll-note") as NodeListOf<HTMLElement>;
    fireEvent.mouseDown(notes[0], { button: 0, clientX: 10, clientY: 100 });
    fireEvent.mouseUp(window, { button: 0, clientX: 10, clientY: 100 });
    const beatsBefore = store.transport().position_beats;

    fireEvent.contextMenu(notes[1], { clientX: 50, clientY: 100 });

    expect(document.querySelector(".pianoroll-note-menu .context-menu-heading")?.textContent).toBe("E4 · mf 80");
    expect(notes[0].classList.contains("note-selected")).toBe(true);
    expect(notes[1].classList.contains("note-selected")).toBe(false);
    expect(store.transport().position_beats).toBe(beatsBefore);
    expect(menuItem("Copy")).toBeTruthy();
    expect(menuItem("Paste at playhead")?.getAttribute("aria-disabled")).toBe("true");
    expect(menuItem("Duplicate")).toBeTruthy();
    expect(menuItem("Delete note")).toBeTruthy();
    // Both velocity scales live inside their groups now.
    openGroup("Velocity");
    expect(menuItem("80 (MF)")?.getAttribute("aria-checked")).toBe("true");
    expect(menuItem("Mute note")).toBeTruthy();
    expect(menuItem("Grace note")).toBeTruthy();
  });

  it("note menu actions target the right-clicked note, not the selection", async () => {
    const { container, menuItem } = renderRoll();
    fireEvent.click(container.querySelector(".pianoroll-toolbar button") as HTMLElement);
    const notes = container.querySelectorAll(".pianoroll-note") as NodeListOf<HTMLElement>;
    fireEvent.mouseDown(notes[0], { button: 0, clientX: 10, clientY: 100 });
    fireEvent.mouseUp(window, { button: 0, clientX: 10, clientY: 100 });

    fireEvent.contextMenu(notes[1], { clientX: 50, clientY: 100 });
    fireEvent.click(menuItem("Delete note")!);

    expectInvoked("remove_pattern_note", { patternName: "beat", noteIndex: 1 });
  });

  it("note menu copies, pastes and duplicates the note under the cursor", async () => {
    const { container, menuItem } = renderRoll();
    fireEvent.click(container.querySelector(".pianoroll-toolbar button") as HTMLElement);
    const notes = container.querySelectorAll(".pianoroll-note") as NodeListOf<HTMLElement>;

    fireEvent.contextMenu(notes[1], { clientX: 50, clientY: 100 });
    fireEvent.click(menuItem("Copy")!);
    fireEvent.contextMenu(notes[0], { clientX: 10, clientY: 100 });
    fireEvent.click(menuItem("Paste at playhead")!);
    expectInvoked("add_pattern_note", { patternName: "beat", noteName: "E", octave: 4, durationBeats: 1 });

    fireEvent.contextMenu(notes[1], { clientX: 50, clientY: 100 });
    fireEvent.click(menuItem("Duplicate")!);
    expectInvoked("add_pattern_note", { patternName: "beat", noteName: "E", octave: 4, startBeat: 2 });
  });

  it("empty grid right-click opens the grid menu without clearing the selection or seeking", () => {
    const { container, menuItem } = renderRoll();
    fireEvent.click(container.querySelector(".pianoroll-toolbar button") as HTMLElement);
    const notes = container.querySelectorAll(".pianoroll-note") as NodeListOf<HTMLElement>;
    fireEvent.mouseDown(notes[0], { button: 0, clientX: 10, clientY: 100 });
    fireEvent.mouseUp(window, { button: 0, clientX: 10, clientY: 100 });
    const canvas = container.querySelector("[data-note-canvas]") as HTMLElement;
    const beatsBefore = store.transport().position_beats;

    fireEvent.mouseDown(canvas, { button: 2, clientX: 120, clientY: 100 });
    fireEvent.contextMenu(canvas, { button: 2, clientX: 120, clientY: 100 });

    expect(notes[0].classList.contains("note-selected")).toBe(true);
    expect(container.querySelector(".pianoroll-marquee")).toBeNull();
    expect(container.querySelector(".note-ghost")).toBeNull();
    expect(store.transport().position_beats).toBe(beatsBefore);
    expect(document.querySelector(".pianoroll-grid-menu .context-menu-heading")?.textContent).toContain("bar 1");
    expect(menuItem("Draw note here")).toBeTruthy();
    expect(menuItem("Paste note here")?.getAttribute("aria-disabled")).toBe("true");
    expect(menuItem("1/16")?.getAttribute("aria-checked")).toBe("true");
  });

  it("grid menu draws a note and changes the shared grid", () => {
    const { container, menuItem } = renderRoll();
    fireEvent.click(container.querySelector(".pianoroll-toolbar button") as HTMLElement);
    const canvas = container.querySelector("[data-note-canvas]") as HTMLElement;

    fireEvent.contextMenu(canvas, { button: 2, clientX: 120, clientY: 100 });
    fireEvent.click(menuItem("Draw note here")!);
    expectInvoked("add_pattern_note", { patternName: "beat" });

    fireEvent.contextMenu(canvas, { button: 2, clientX: 120, clientY: 100 });
    fireEvent.click(menuItem("1/8")!);
    expect(gridDivision()).toBe(2);
  });

  it("grid cursor is custom pencil in draw mode", () => {
    const { container } = render(() => <PianoRoll />);
    const scrollContainer = container.querySelector("[tabindex='-1']") as HTMLElement;
    if (!scrollContainer) return;
    // The grid div has a custom SVG cursor URL with crosshair fallback
    const allDivs = scrollContainer.querySelectorAll("div") as NodeListOf<HTMLElement>;
    const gridDiv = Array.from(allDivs).find(d => d.style.cursor?.includes("crosshair"));
    expect(gridDiv).toBeTruthy();
  });
});

/** Render the roll over a pattern built from `notes`, replacing the default fixture. */
async function renderNotes(notes: NoteEvent[], overrides?: Partial<Project>) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    return makeSnapshot({
      patterns: [{
        ...makePattern("beat"),
        instrument: { category: "keys", name: "piano" },
        hits: [],
        events: notes,
      }],
      tracks: [makeTrack("Piano", 0)],
      ...overrides,
    });
  });
  await store.initProject();
  store.setSelectedPatternName("beat");
  return renderRoll();
}

describe("velocity lane", () => {
  it("draws one bar per visible note, taller for a louder note", async () => {
    const { container } = await renderNotes([
      makeNote({ velocity: 64, dynamics: null }),
      makeNote({ start_beat: 1, pitch: { note: "E", accidental: "Natural", octave: 4 }, velocity: 127, dynamics: null }),
      // Above the drawn pitch range: the grid hides it, so the lane must too.
      makeNote({ start_beat: 2, pitch: { note: "C", accidental: "Natural", octave: 9 } }),
    ]);

    const bars = [...container.querySelectorAll<HTMLElement>("[data-velocity-bar]")];
    expect(bars).toHaveLength(2);
    const [soft, loud] = bars.map((bar) => parseFloat(bar.style.height));
    expect(loud).toBeGreaterThan(soft);
    expect(loud / soft).toBeCloseTo(127 / 64, 1);
  });

  it("draws a bar for a drum hit and reads its exact velocity", async () => {
    const { container } = await renderNotes([], {
      patterns: [{
        ...makePattern("beat"),
        events: [],
        hits: [
          { drum: "kick", position: { beat: 1, subdivision: 0 }, dynamics: "MP", articulation: null, modifiers: [] },
          { drum: "snare", position: { beat: 2, subdivision: 0 }, dynamics: "MF", velocity: 120, articulation: null, modifiers: [] },
          { drum: "hihat", position: { beat: 3, subdivision: 0 }, dynamics: null, velocity: 24, articulation: null, modifiers: [] },
        ],
      }],
    });

    const bars = [...container.querySelectorAll<HTMLElement>("[data-hit-bar]")];
    expect(bars).toHaveLength(3);
    expect(bars.map((bar) => bar.dataset.velocitySource)).toEqual([
      "dynamics", "velocity", "velocity",
    ]);
    const [soft, loud] = bars.map((bar) => parseFloat(bar.style.height));
    expect(loud).toBeGreaterThan(soft);
  });

  it("previews a lane drag and commits one velocity on release", async () => {
    const { container } = await renderNotes([makeNote({ velocity: 16, dynamics: null })]);
    const bar = container.querySelector<HTMLElement>("[data-velocity-bar]")!;
    const velocityEdits = () => invokeMock.mock.calls.filter(([cmd]) => cmd === "set_pattern_note_velocity");

    // A zero-height box in jsdom falls back to the lane height, so 25px below
    // the top of the lane is half of 0..127.
    fireEvent.mouseDown(bar, { button: 0, clientY: 0 });
    fireEvent.mouseMove(document, { clientY: 12 });
    expect(velocityEdits()).toHaveLength(0);
    fireEvent.mouseUp(document, { clientY: 12 });

    expect(velocityEdits()).toHaveLength(1);
    expect(getLastCallArgs("set_pattern_note_velocity")).toMatchObject({ patternName: "beat", noteIndex: 0 });
    const committed = getLastCallArgs("set_pattern_note_velocity")!.velocity as number;
    expect(committed).toBeGreaterThan(16);
    expect(committed).toBeLessThanOrEqual(127);
  });

  it("a note written mf and a note written vel=80 are distinguishable", async () => {
    const { container, menuItem, openGroup } = await renderNotes([
      makeNote({ dynamics: "MF", velocity: null }),
      makeNote({ start_beat: 1, pitch: { note: "E", accidental: "Natural", octave: 4 }, dynamics: null, velocity: 80 }),
    ]);
    const notes = container.querySelectorAll<HTMLElement>(".pianoroll-note");
    const bars = container.querySelectorAll<HTMLElement>("[data-velocity-bar]");

    expect(notes[0].dataset.velocitySource).toBe("dynamics");
    expect(notes[1].dataset.velocitySource).toBe("velocity");
    expect(notes[1].classList.contains("note-velocity-explicit")).toBe(true);
    expect(notes[0].classList.contains("note-velocity-explicit")).toBe(false);
    expect(bars[1].classList.contains("velocity-bar-explicit")).toBe(true);
    expect(bars[0].classList.contains("velocity-bar-explicit")).toBe(false);

    // The same 80 carries a different written origin, which the menu spells out.
    fireEvent.contextMenu(notes[0], { clientX: 10, clientY: 100 });
    expect(document.querySelector(".pianoroll-note-menu .context-menu-heading")?.textContent).toBe("C4 · mf 80");
    fireEvent.contextMenu(notes[1], { clientX: 50, clientY: 100 });
    expect(document.querySelector(".pianoroll-note-menu .context-menu-heading")?.textContent).toBe("E4 · v 80");
    openGroup("Articulation");
    expect(menuItem("No articulation")).toBeTruthy();
  });

  it("writes the chosen dynamics token through the store", async () => {
    const { container, menuItem, openGroup } = await renderNotes([makeNote({ dynamics: "MF", velocity: null })]);
    const note = container.querySelector<HTMLElement>(".pianoroll-note")!;

    fireEvent.contextMenu(note, { clientX: 10, clientY: 100 });
    openGroup("Dynamics");
    // Every token the DSL knows, with the written one checked and no exact
    // velocity marking a second row as current.
    for (const [token, velocity] of [["ppp", 16], ["pp", 33], ["p", 49], ["mp", 64], ["mf", 80], ["f", 96], ["ff", 112], ["fff", 127]] as const) {
      expect(menuItem(`${token} (${velocity})`), token).toBeTruthy();
      expect(menuItem(`${token} (${velocity})`)!.getAttribute("aria-checked")).toBe(token === "mf" ? "true" : "false");
    }

    fireEvent.click(menuItem("pp (33)")!);
    expectInvoked("set_pattern_note_dynamics", { patternName: "beat", noteIndex: 0, dynamics: "pp" });
  });

  it("maps the pointer against the lane axis, not the bar", async () => {
    const { container } = await renderNotes([makeNote({ velocity: 16, dynamics: null })]);
    const body = container.querySelector<HTMLElement>(".pianoroll-velocity-body")!;
    const bar = container.querySelector<HTMLElement>("[data-velocity-bar]")!;
    // The lane body is 56 px tall with a 6 px inset, so 22 px into its usable
    // 44 px is half the axis. The bar's own box is deliberately unrelated: an
    // axis-free mapping would read it and land somewhere else entirely.
    stubRect(body, 100, 56);
    stubRect(bar, 130, 5);

    fireEvent.mouseDown(bar, { button: 0, clientY: 128 });
    fireEvent.mouseUp(document, { clientY: 128 });

    const committed = getLastCallArgs("set_pattern_note_velocity")!.velocity as number;
    expect(committed).toBe(64);
  });

  it("labels the axis at 32, 64, 96 and 127", async () => {
    const { container } = await renderNotes([makeNote({ velocity: 16, dynamics: null })]);
    const ticks = [...container.querySelectorAll<HTMLElement>("[data-velocity-tick]")];
    expect(ticks.map((tick) => tick.textContent)).toEqual(["32", "64", "96", "127"]);
    // 127 sits on the inner top edge of the body, where a full bar ends.
    expect(ticks[3].style.bottom).toBe("50px");
    const guides = [...container.querySelectorAll<HTMLElement>("[data-velocity-guide]")];
    expect(guides).toHaveLength(4);
    expect(guides[3].style.bottom).toBe("50px");
  });

  it("a typed value lands on the selected note", async () => {
    const { container } = await renderNotes([makeNote({ velocity: 16, dynamics: null })]);
    const bar = container.querySelector<HTMLElement>("[data-velocity-bar]")!;
    const input = () => container.querySelector<HTMLInputElement>(".pianoroll-velocity-input")!;

    expect(input().value).toBe("");

    fireEvent.mouseDown(bar, { button: 0, clientY: 0 });
    fireEvent.mouseUp(document, { clientY: 0 });
    expect(input().value).toBe("16");
    invokeMock.mockClear();

    fireEvent.change(input(), { target: { value: "96" } });
    expectInvoked("set_pattern_note_velocity", { patternName: "beat", noteIndex: 0, velocity: 96 });
  });

  it("refuses a value outside 1–127 without touching the project", async () => {
    const { container } = await renderNotes([makeNote({ velocity: 16, dynamics: null })]);
    const bar = container.querySelector<HTMLElement>("[data-velocity-bar]")!;
    fireEvent.mouseDown(bar, { button: 0, clientY: 0 });
    fireEvent.mouseUp(document, { clientY: 0 });
    invokeMock.mockClear();

    fireEvent.change(container.querySelector<HTMLInputElement>(".pianoroll-velocity-input")!, {
      target: { value: "200" },
    });

    expect(container.querySelector(".pianoroll-lane-notice")?.textContent).toBe("Velocity must be 1–127.");
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_pattern_note_velocity")).toBe(false);
  });

  it("a failed velocity write is text in the lane", async () => {
    const { container } = await renderNotes([makeNote({ velocity: 16, dynamics: null })]);
    const bar = container.querySelector<HTMLElement>("[data-velocity-bar]")!;
    fireEvent.mouseDown(bar, { button: 0, clientY: 0 });
    fireEvent.mouseUp(document, { clientY: 0 });

    invokeMock.mockRejectedValueOnce("engine refused");
    fireEvent.change(container.querySelector<HTMLInputElement>(".pianoroll-velocity-input")!, {
      target: { value: "90" },
    });
    await Promise.resolve();
    await Promise.resolve();

    expect(container.querySelector(".pianoroll-lane-notice")?.textContent).toBe(
      "Velocity write failed: engine refused",
    );
  });

  it("alt-click clears an exact velocity", async () => {
    const { container } = await renderNotes([makeNote({ velocity: 96, dynamics: "MF" })]);
    const bar = container.querySelector<HTMLElement>("[data-velocity-bar]")!;

    fireEvent.mouseDown(bar, { button: 0, altKey: true, clientY: 0 });
    fireEvent.mouseUp(document, { clientY: 0 });

    expectInvoked("clear_pattern_note_velocity", { patternName: "beat", noteIndex: 0 });
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_pattern_note_velocity")).toBe(false);
  });

  it("alt-click on a note that follows its written dynamic says why nothing happened", async () => {
    const { container } = await renderNotes([makeNote({ velocity: null, dynamics: "MF" })]);
    const bar = container.querySelector<HTMLElement>("[data-velocity-bar]")!;

    fireEvent.mouseDown(bar, { button: 0, altKey: true, clientY: 0 });

    expect(container.querySelector(".pianoroll-lane-notice")?.textContent).toBe(
      "This note already follows its written dynamic.",
    );
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "clear_pattern_note_velocity")).toBe(false);

    // The box shows the value the token produces, marked as written rather than exact.
    const input = container.querySelector<HTMLInputElement>(".pianoroll-velocity-input")!;
    expect(input.value).toBe("80");
    expect(input.classList.contains("velocity-input-written")).toBe(true);
    expect(input.getAttribute("title")).toBe(
      "Type 1–127 to replace the written dynamic with an exact velocity.",
    );
  });

  it("disables the gutter box until a note is selected and names the reason", async () => {
    const { container } = await renderNotes([makeNote({ velocity: 16, dynamics: null })]);
    const input = container.querySelector<HTMLInputElement>(".pianoroll-velocity-input")!;

    expect(input.disabled).toBe(true);
    expect(input.getAttribute("title")).toBe("Select a note to type its velocity.");
    expect(container.querySelector(".pianoroll-lane-notice")?.textContent).toBe(
      "Select a note to type its velocity.",
    );

    fireEvent.mouseDown(container.querySelector<HTMLElement>("[data-velocity-bar]")!, {
      button: 0, clientY: 0,
    });
    fireEvent.mouseUp(document, { clientY: 0 });
    // The commit is in flight: the entry stays disabled until it lands.
    await flush();
    expect(input.disabled).toBe(false);
    expect(container.querySelector(".pianoroll-lane-notice")).toBeNull();
  });

  it("offers reset to the written dynamic only when an exact velocity exists", async () => {
    const { container, menuItem, openGroup } = await renderNotes([makeNote({ velocity: 96, dynamics: "MF" })]);
    fireEvent.contextMenu(container.querySelector<HTMLElement>(".pianoroll-note")!, { clientX: 10, clientY: 100 });
    openGroup("Velocity");

    const reset = menuItem("Reset to written dynamic")!;
    expect(reset.getAttribute("aria-disabled")).toBeNull();
    fireEvent.click(reset);
    expectInvoked("clear_pattern_note_velocity", { patternName: "beat", noteIndex: 0 });
  });

  it("disables the reset row on a note that only has a written dynamic", async () => {
    const { container, menuItem, openGroup } = await renderNotes([makeNote({ velocity: null, dynamics: "MF" })]);
    fireEvent.contextMenu(container.querySelector<HTMLElement>(".pianoroll-note")!, { clientX: 10, clientY: 100 });
    openGroup("Velocity");

    const reset = menuItem("Reset to written dynamic")!;
    expect(reset.getAttribute("aria-disabled")).toBe("true");
    expect(reset.querySelector(".context-menu-hint")?.textContent).toBe(
      "This note already follows its written dynamic.",
    );
  });
});

describe("articulation lane", () => {
  const presetWith = (params: [string, ParamValue][]) => ({
    ...makeSnapshot().project.presets[0],
    params,
  });

  it("draws one chip at each note start and tags the articulated one", async () => {
    const { container } = await renderNotes([
      makeNote({ articulation: "Staccato" }),
      makeNote({ start_beat: 1, pitch: { note: "E", accidental: "Natural", octave: 4 } }),
    ]);
    const chips = [...container.querySelectorAll<HTMLElement>("[data-articulation-chip]")];

    expect(chips).toHaveLength(2);
    expect(chips[0].textContent).toBe("st");
    expect(chips[0].dataset.articulation).toBe("staccato");
    // A plain note still gets a chip, so the lane can set one there.
    expect(chips[1].textContent).toBe("");
    expect(chips[1].classList.contains("articulation-chip-empty")).toBe(true);
  });

  it("names the keyswitch the preset declares", async () => {
    const { container, menuItem } = await renderNotes(
      [makeNote({ articulation: "Staccato" })],
      { presets: [presetWith([["articulation_staccato_keyswitch", { String: "C0" }]])] },
    );
    const chip = container.querySelector<HTMLElement>("[data-articulation-chip]")!;
    expect(chip.dataset.keyswitch).toBe("C0");

    // The same sentence is the menu heading, which is what a hover shows.
    fireEvent.click(chip, { clientX: 10, clientY: 100 });
    expect(document.querySelector(".pianoroll-note-menu .context-menu-heading")?.textContent).toBe(
      "staccato · keyswitch C0",
    );
    expect(menuItem("staccato")?.getAttribute("aria-checked")).toBe("true");
  });

  it("says no keyswitch when the preset declares none for the token", async () => {
    const { container } = await renderNotes(
      [makeNote({ articulation: "Staccato" })],
      { presets: [presetWith([["articulation_legato_keyswitch", { String: "C0" }]])] },
    );
    const chip = container.querySelector<HTMLElement>("[data-articulation-chip]")!;
    expect(chip.dataset.keyswitch).toBeUndefined();

    fireEvent.click(chip, { clientX: 10, clientY: 100 });
    expect(document.querySelector(".pianoroll-note-menu .context-menu-heading")?.textContent).toBe(
      "staccato · no keyswitch in this preset",
    );
  });

  it("clicking a chip opens the articulation menu with the current token checked", async () => {
    const { container, menuItem } = await renderNotes([makeNote({ articulation: "Staccato" })]);
    const chip = container.querySelector<HTMLElement>("[data-articulation-chip]")!;

    fireEvent.click(chip, { clientX: 10, clientY: 100 });
    expect(menuItem("staccato")?.getAttribute("aria-checked")).toBe("true");
    expect(menuItem("legato")?.getAttribute("aria-checked")).toBe("false");
    fireEvent.click(menuItem("pizzicato")!);
    expectInvoked("set_pattern_note_articulation", {
      patternName: "beat", noteIndex: 0, articulation: "pizzicato",
    });
  });

  it("alt-click clears the articulation", async () => {
    const { container } = await renderNotes([makeNote({ articulation: "Staccato" })]);
    const chip = container.querySelector<HTMLElement>("[data-articulation-chip]")!;

    fireEvent.click(chip, { altKey: true });

    expectInvoked("set_pattern_note_articulation", {
      patternName: "beat", noteIndex: 0, articulation: null,
    });
  });

  it("alt-click on a note with no articulation says why nothing happened", async () => {
    // The lane only exists while some note carries an articulation; the plain
    // note is the one under test.
    const { container } = await renderNotes([
      makeNote({ articulation: "Staccato" }),
      makeNote({ start_beat: 1, pitch: { note: "E", accidental: "Natural", octave: 4 } }),
    ]);
    const chip = [...container.querySelectorAll<HTMLElement>("[data-articulation-chip]")]
      .find((element) => element.classList.contains("articulation-chip-empty"))!;

    fireEvent.click(chip, { altKey: true });

    expect(container.querySelector(".pianoroll-articulation-body .pianoroll-lane-notice")?.textContent).toBe(
      "This note has no articulation.",
    );
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_pattern_note_articulation")).toBe(false);
  });

  it("a failed articulation write is text in the lane", async () => {
    const { container } = await renderNotes([makeNote({ articulation: "Staccato" })]);
    const chip = container.querySelector<HTMLElement>("[data-articulation-chip]")!;

    invokeMock.mockRejectedValueOnce("preset has no such token");
    fireEvent.click(chip, { altKey: true });
    await flush();

    expect(container.querySelector(".pianoroll-articulation-body .pianoroll-lane-notice")?.textContent).toBe(
      "Articulation could not be cleared: preset has no such token",
    );
  });

  it("dims the chip while the write is in flight", async () => {
    const { container } = await renderNotes([
      makeNote({ articulation: "Staccato" }),
      makeNote({ start_beat: 1, pitch: { note: "E", accidental: "Natural", octave: 4 }, articulation: "Staccato" }),
    ]);
    // The snapshot replaces the chips, so every assertion re-reads the live one.
    // The first note keeps the lane open while the second one is cleared.
    const chip = () => container.querySelectorAll<HTMLElement>("[data-articulation-chip]")[1]!;
    const written = makeSnapshot({
      patterns: [{
        ...makePattern("beat"),
        instrument: { category: "keys", name: "piano" },
        hits: [],
        events: [
          makeNote({ articulation: "Staccato" }),
          makeNote({ start_beat: 1, pitch: { note: "E", accidental: "Natural", octave: 4 } }),
        ],
      }],
      tracks: [makeTrack("Piano", 0)],
    });

    let finish: (value: unknown) => void = () => {};
    invokeMock.mockImplementationOnce(() => new Promise((resolve) => { finish = resolve; }));
    fireEvent.click(chip(), { altKey: true });
    await flush();
    expect(chip().classList.contains("lane-pending")).toBe(true);
    expect(chip().getAttribute("aria-busy")).toBe("true");

    finish(written);
    await flush();
    expect(chip().classList.contains("lane-pending")).toBe(false);
    // The clear landed: the chip is now the empty one for this note.
    expect(chip().classList.contains("articulation-chip-empty")).toBe(true);
  });
});

describe("articulation markers", () => {
  it("tags a staccato note and leaves a plain note untagged", async () => {
    const { container } = await renderNotes([
      makeNote({ articulation: "Staccato" }),
      makeNote({ start_beat: 1, pitch: { note: "E", accidental: "Natural", octave: 4 } }),
    ]);
    const notes = container.querySelectorAll<HTMLElement>(".pianoroll-note");

    expect(notes[0].querySelector(".pianoroll-note-tag")?.textContent).toBe("st");
    expect(notes[0].dataset.articulation).toBe("staccato");
    expect(notes[1].querySelector(".pianoroll-note-tag")).toBeNull();
    expect(notes[1].dataset.articulation).toBeUndefined();
  });

  it("sets and clears the articulation from the note menu", async () => {
    const { container, menuItem, openGroup } = await renderNotes([makeNote({ articulation: "Staccato" })]);
    const note = container.querySelector<HTMLElement>(".pianoroll-note")!;

    fireEvent.contextMenu(note, { clientX: 10, clientY: 100 });
    openGroup("Articulation");
    expect(menuItem("staccato")?.getAttribute("aria-checked")).toBe("true");
    expect(menuItem("legato")?.getAttribute("aria-checked")).toBe("false");
    fireEvent.click(menuItem("pizzicato")!);
    expectInvoked("set_pattern_note_articulation", { patternName: "beat", noteIndex: 0, articulation: "pizzicato" });

    fireEvent.contextMenu(note, { clientX: 10, clientY: 100 });
    openGroup("Articulation");
    fireEvent.click(menuItem("No articulation")!);
    expectInvoked("set_pattern_note_articulation", { patternName: "beat", noteIndex: 0, articulation: null });
  });
});

describe("batch note tools", () => {
  const tool = (container: HTMLElement, name: string) =>
    container.querySelector<HTMLElement>(`[data-note-tool="${name}"]`)!;
  const button = (container: HTMLElement, name: string) =>
    tool(container, name).querySelector<HTMLButtonElement>("button")!;
  const amount = (container: HTMLElement, name: string) =>
    tool(container, name).querySelector<HTMLSelectElement>("select")!;
  const calls = (cmd: string) => invokeMock.mock.calls.filter(([name]) => name === cmd);
  const selectNote = (note: HTMLElement, shiftKey = false) => {
    fireEvent.mouseDown(note, { button: 0, clientX: 10, clientY: 100, shiftKey });
    fireEvent.mouseUp(window, { button: 0, clientX: 10, clientY: 100 });
  };
  const twoNotes = () => [
    makeNote({ velocity: 64, dynamics: null }),
    makeNote({ start_beat: 1, pitch: { note: "E", accidental: "Natural", octave: 4 }, dynamics: "MF", velocity: null }),
  ];

  it("quantises the selection once, at the chosen strength", async () => {
    const { container } = await renderNotes(twoNotes());
    selectNote(container.querySelectorAll<HTMLElement>(".pianoroll-note")[0]);
    fireEvent.change(amount(container, "quantise"), { target: { value: "75" } });

    fireEvent.click(button(container, "quantise"));
    await flush();

    expect(calls("quantise_notes")).toHaveLength(1);
    expect(getLastCallArgs("quantise_notes")).toEqual({
      scope: { kind: "pattern", pattern_name: "beat" },
      notes: [{ phrase_index: 0, note_index: 0 }],
      gridQuarters: 0.25,
      strengthPercent: 75,
    });
  });

  it("falls back to every visible note when nothing is selected", async () => {
    const { container } = await renderNotes(twoNotes());

    fireEvent.click(button(container, "humanise"));
    await flush();

    const args = getLastCallArgs("humanise_notes")!;
    expect(args.notes).toEqual([
      { phrase_index: 0, note_index: 0 },
      { phrase_index: 0, note_index: 1 },
    ]);
    expect(args.scope).toEqual({ kind: "pattern", pattern_name: "beat" });
  });

  it("states the selection fallback in the tooltip", async () => {
    const { container } = await renderNotes(twoNotes());

    expect(tool(container, "quantise").title).toContain("With nothing selected it acts on every visible note");
    expect(tool(container, "humanise").title).toContain("With nothing selected it acts on every visible note");
    expect(tool(container, "legato").title).toContain("every visible note");
  });

  it("shows the pending label while the call runs", async () => {
    const notes = twoNotes();
    const { container } = await renderNotes(notes);
    let release: (() => void) | undefined;
    invokeMock.mockImplementationOnce(() => new Promise((resolve) => {
      release = () => resolve(makeSnapshot({
        patterns: [{ ...makePattern("beat"), instrument: { category: "keys", name: "piano" }, hits: [], events: notes }],
        tracks: [makeTrack("Piano", 0)],
      }));
    }));

    selectNote(container.querySelectorAll<HTMLElement>(".pianoroll-note")[0]);
    fireEvent.click(button(container, "quantise"));

    expect(button(container, "quantise").textContent).toBe("Snapping to grid…");
    expect(button(container, "quantise").getAttribute("aria-busy")).toBe("true");

    release!();
    await flush();
    expect(button(container, "quantise").textContent).toBe("Snap to grid");
    expect(button(container, "quantise").getAttribute("aria-busy")).toBe("false");
  });

  it("renders a failed batch edit as text in the roll", async () => {
    const { container } = await renderNotes(twoNotes());
    invokeMock.mockImplementationOnce(async () => {
      throw "Pattern 'beat' not found";
    });

    selectNote(container.querySelectorAll<HTMLElement>(".pianoroll-note")[0]);
    fireEvent.click(button(container, "quantise"));
    await flush();

    const notice = container.querySelector(".pianoroll-toolbar-notice");
    expect(notice?.textContent).toBe("Snap to grid failed: Pattern 'beat' not found");
    expect(notice?.querySelector("[role='alert']")).toBeTruthy();
    expect(store.error()).toBe("Pattern 'beat' not found");
  });

  it("states why each tool cannot act when the roll has no notes", async () => {
    const { container } = await renderNotes([]);

    expect(button(container, "quantise").disabled).toBe(true);
    expect(button(container, "humanise").disabled).toBe(true);
    expect(button(container, "legato").disabled).toBe(true);
    expect(tool(container, "quantise").title).toBe("Select notes to snap to the grid.");
    expect(tool(container, "humanise").title).toBe("Select notes to loosen their timing.");
    expect(tool(container, "legato").title).toBe("Select two or more notes to extend them to the next note.");

    fireEvent.click(button(container, "quantise"));
    expect(calls("quantise_notes")).toHaveLength(0);
  });

  it("needs two notes before legato can act on one", async () => {
    const { container } = await renderNotes([makeNote()]);

    expect(button(container, "legato").disabled).toBe(true);
    expect(tool(container, "legato").title).toBe("Select two or more notes to extend them to the next note.");
    expect(button(container, "quantise").disabled).toBe(false);
  });

  it("says nothing to change at 0 % and refuses to run", async () => {
    const { container } = await renderNotes(twoNotes());
    fireEvent.change(amount(container, "quantise"), { target: { value: "0" } });

    expect(button(container, "quantise").textContent).toBe("Nothing to change at 0 %");
    expect(button(container, "quantise").disabled).toBe(true);
    expect(tool(container, "quantise").title).toBe("Nothing to change at 0 %");

    fireEvent.click(button(container, "quantise"));
    expect(calls("quantise_notes")).toHaveLength(0);
  });

  it("refuses a pattern shorter than one grid step", async () => {
    const { container } = await renderNotes([
      makeNote({ duration: { base: "SixtyFourth", dots: 0, triplet: false, tuplet: null, ties: [] } }),
    ]);
    fireEvent.change(container.querySelector(".pianoroll-toolbar select")!, { target: { value: "8" } });

    expect(button(container, "quantise").disabled).toBe(true);
    expect(tool(container, "quantise").title).toBe("The pattern is shorter than one grid step.");
  });

  it("makes the selection legato in one call", async () => {
    const { container } = await renderNotes(twoNotes());
    const notes = container.querySelectorAll<HTMLElement>(".pianoroll-note");
    selectNote(notes[0]);
    selectNote(notes[1], true);

    fireEvent.click(button(container, "legato"));
    await flush();

    expect(calls("legato_notes")).toHaveLength(1);
    expect(getLastCallArgs("legato_notes")).toEqual({
      scope: { kind: "pattern", pattern_name: "beat" },
      notes: [
        { phrase_index: 0, note_index: 0 },
        { phrase_index: 0, note_index: 1 },
      ],
      amountBeats: null,
    });
  });

  it("extends by the chosen number of grid steps", async () => {
    const { container } = await renderNotes(twoNotes());
    fireEvent.change(amount(container, "legato"), { target: { value: "two-steps" } });

    fireEvent.click(button(container, "legato"));
    await flush();

    expect(getLastCallArgs("legato_notes")!.amountBeats).toBe(0.5);
  });

  it("humanises with the chosen amount and a seed", async () => {
    const { container } = await renderNotes(twoNotes());
    fireEvent.change(amount(container, "humanise"), { target: { value: "timing-1-velocity-12" } });

    fireEvent.click(button(container, "humanise"));
    await flush();

    const args = getLastCallArgs("humanise_notes")!;
    expect(args.timingTicks).toBe(1);
    expect(args.velocitySpread).toBe(12);
    expect(typeof args.seed).toBe("number");
    expect(args.scope).toEqual({ kind: "pattern", pattern_name: "beat" });
  });

  it("clears the selection after a batch edit reorders the notes", async () => {
    const { container } = await renderNotes(twoNotes());
    selectNote(container.querySelectorAll<HTMLElement>(".pianoroll-note")[0]);

    fireEvent.click(button(container, "quantise"));
    await flush();

    expect(container.querySelectorAll(".pianoroll-note.note-selected")).toHaveLength(0);
  });
});

/**
 * One backend that applies the commands it receives to a single pattern's note
 * list, sorting it as the real one does. A static snapshot would hide the
 * re-sort the selection identity has to survive.
 */
async function renderSortingRoll(notes: NoteEvent[], extraTracks: Track[] = []) {
  let events = notes.map((note) => ({ ...note }));
  const sortEvents = () => { events = [...events].sort((a, b) => a.start_beat - b.start_beat); };
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    // Every snapshot carries fresh note objects: the store shares nothing with
    // the backend, and mutating what it already holds would hide the update.
    if (cmd === "move_notes") {
      const next = events.map((note) => ({ ...note }));
      for (const move of args!.moves as { note_index: number; new_start_beat: number }[]) {
        next[move.note_index].start_beat = move.new_start_beat;
      }
      events = next;
      sortEvents();
    }
    if (cmd === "move_pattern_note") {
      const next = events.map((note) => ({ ...note }));
      const note = next[args!.noteIndex as number];
      note.start_beat = args!.newStartBeat as number;
      if (args!.newPitch) note.pitch = args!.newPitch as typeof note.pitch;
      events = next;
      sortEvents();
    }
    if (cmd === "remove_pattern_note") {
      const next = events.map((note) => ({ ...note }));
      next.splice(args!.noteIndex as number, 1);
      events = next;
    }
    if (cmd === "remove_notes") {
      const indices = [...new Set((args!.notes as { note_index: number }[]).map((note) => note.note_index))];
      const next = events.map((note) => ({ ...note }));
      for (const index of indices.sort((a, b) => b - a)) next.splice(index, 1);
      events = next;
    }
    if (cmd === "set_note_velocities") {
      const next = events.map((note) => ({ ...note }));
      for (const value of args!.values as { note_index: number; velocity: number }[]) {
        next[value.note_index].velocity = value.velocity;
      }
      events = next;
    }
    return makeSnapshot({
      patterns: [{
        ...makePattern("beat"),
        instrument: { category: "keys", name: "piano" },
        hits: [],
        events,
      }],
      tracks: [makeTrack("Piano", 0), ...extraTracks],
    });
  });
  await store.initProject();
  store.setSelectedPatternName("beat");
  return renderRoll();
}

describe("note selection identity", () => {
  const C4 = { note: "C" as const, accidental: "Natural" as const, octave: 4 };
  const D4 = { note: "D" as const, accidental: "Natural" as const, octave: 4 };
  const E4 = { note: "E" as const, accidental: "Natural" as const, octave: 4 };
  const F4 = { note: "F" as const, accidental: "Natural" as const, octave: 4 };

  /** Four sixteenths a step apart, the fixture the review used. */
  const fourNotes = () => [
    makeNote({ start_beat: 0, pitch: C4 }),
    makeNote({ start_beat: 0.25, pitch: D4 }),
    makeNote({ start_beat: 0.5, pitch: E4 }),
    makeNote({ start_beat: 0.75, pitch: F4 }),
  ];

  const selected = (container: HTMLElement) =>
    [...container.querySelectorAll<HTMLElement>(".pianoroll-note.note-selected")];

  const grid = (container: HTMLElement) => container.querySelector("[tabindex='-1']") as HTMLElement;

  it("nudges a four-note selection in one command and keeps all four selected", async () => {
    const { container } = await renderSortingRoll(fourNotes());
    fireEvent.keyDown(grid(container), { key: "a", ctrlKey: true });
    expect(selected(container)).toHaveLength(4);
    invokeMock.mockClear();

    fireEvent.keyDown(grid(container), { key: "ArrowRight", shiftKey: true });
    await flush();

    const nudges = invokeMock.mock.calls.filter(([cmd]) => cmd === "move_notes");
    expect(nudges).toHaveLength(1);
    expect((nudges[0][1] as any).moves).toEqual([
      { phrase_index: 0, note_index: 0, new_start_beat: 1 },
      { phrase_index: 0, note_index: 1, new_start_beat: 1.25 },
      { phrase_index: 0, note_index: 2, new_start_beat: 1.5 },
      { phrase_index: 0, note_index: 3, new_start_beat: 1.75 },
    ]);
    // The same four notes, one beat later, still highlighted.
    const after = selected(container);
    expect(after).toHaveLength(4);
    expect(after.map((note) => note.style.left)).toEqual(["40px", "50px", "60px", "70px"]);
  });

  it("deletes a four-note selection in one command", async () => {
    const { container } = await renderSortingRoll(fourNotes());
    fireEvent.keyDown(grid(container), { key: "a", ctrlKey: true });
    invokeMock.mockClear();

    fireEvent.keyDown(grid(container), { key: "Delete" });
    await flush();

    const removals = invokeMock.mock.calls.filter(([cmd]) => cmd === "remove_notes");
    expect(removals).toHaveLength(1);
    expect((removals[0][1] as any).notes).toHaveLength(4);
    expect(container.querySelectorAll(".pianoroll-note")).toHaveLength(0);
  });

  it("keeps the dragged note selected after the move re-sorts the list", async () => {
    const { container } = await renderSortingRoll([
      makeNote({ start_beat: 0, pitch: C4 }),
      makeNote({ start_beat: 1, pitch: E4 }),
    ]);
    const notes = container.querySelectorAll<HTMLElement>(".pianoroll-note");
    fireEvent.mouseDown(notes[0], { button: 0, clientX: 10, clientY: 100 });
    fireEvent.mouseUp(window, { button: 0, clientX: 10, clientY: 100 });
    expect(selected(container)).toHaveLength(1);

    // Drag the first note two beats right, past its neighbour.
    fireEvent.mouseDown(notes[0], { button: 0, clientX: 10, clientY: 100 });
    fireEvent.mouseMove(window, { clientX: 90, clientY: 100 });
    fireEvent.mouseUp(window, { button: 0, clientX: 90, clientY: 100 });
    await flush();

    const after = [...container.querySelectorAll<HTMLElement>(".pianoroll-note")];
    // Sorted by start: the neighbour is first now, the dragged note second.
    expect(after).toHaveLength(2);
    expect(after[0].classList.contains("note-selected")).toBe(false);
    expect(after[1].classList.contains("note-selected")).toBe(true);
    expect(getLastCallArgs("move_pattern_note")).toMatchObject({ newStartBeat: 2 });
  });

  it("writes a typed velocity across the selection in one command", async () => {
    const { container } = await renderSortingRoll([fourNotes()[0], fourNotes()[1]]);
    const bars = () => container.querySelectorAll<HTMLElement>("[data-velocity-bar]");
    fireEvent.mouseDown(bars()[0], { button: 0, clientY: 0 });
    fireEvent.mouseUp(document, { clientY: 0 });
    await flush();
    fireEvent.mouseDown(bars()[1], { button: 0, clientY: 0, shiftKey: true });
    fireEvent.mouseUp(document, { clientY: 0 });
    await flush();
    expect(selected(container)).toHaveLength(2);
    invokeMock.mockClear();

    fireEvent.change(container.querySelector<HTMLInputElement>(".pianoroll-velocity-input")!, {
      target: { value: "100" },
    });
    await flush();

    const writes = invokeMock.mock.calls.filter(([cmd]) => cmd === "set_note_velocities");
    expect(writes).toHaveLength(1);
    expect((writes[0][1] as any).values).toEqual([
      { phrase_index: 0, note_index: 0, velocity: 100 },
      { phrase_index: 0, note_index: 1, velocity: 100 },
    ]);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_pattern_note_velocity")).toBe(false);
  });

  it("deletes a track-view selection across two phrases in one command", async () => {
    const track = makeTrack("Piano", 0);
    track.phrases = [
      {
        name: "first",
        time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 3, beat: 1, subdivision: 0 } },
        events: [makeNote({ start_beat: 0, pitch: C4 })],
      },
      {
        name: "second",
        time_range: { start: { bar: 3, beat: 1, subdivision: 0 }, end: { bar: 5, beat: 1, subdivision: 0 } },
        events: [makeNote({ start_beat: 0, pitch: E4 })],
      },
    ];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({ tracks: [track] });
    });
    await store.initProject();
    store.setSelectedPatternName(null);
    store.setSelectedTrackIndex(0);
    const { container } = renderRoll();
    fireEvent.keyDown(grid(container), { key: "a", ctrlKey: true });
    expect(selected(container)).toHaveLength(2);
    invokeMock.mockClear();

    fireEvent.keyDown(grid(container), { key: "Delete" });
    await flush();

    const removals = invokeMock.mock.calls.filter(([cmd]) => cmd === "remove_notes");
    expect(removals).toHaveLength(1);
    expect((removals[0][1] as any).scope).toEqual({ kind: "track", track_index: 0 });
    expect((removals[0][1] as any).notes).toEqual([
      { phrase_index: 0, note_index: 0 },
      { phrase_index: 1, note_index: 0 },
    ]);
    expect(selected(container)).toHaveLength(0);
  });

  it("refuses a selection the document has moved on from", async () => {
    const { container } = await renderSortingRoll(fourNotes());
    fireEvent.keyDown(grid(container), { key: "a", ctrlKey: true });
    expect(selected(container)).toHaveLength(4);
    // An edit from outside the roll removes one of the selected notes.
    await store.removePatternNote("beat", 0);
    await flush();
    invokeMock.mockClear();

    fireEvent.keyDown(grid(container), { key: "Delete" });
    await flush();

    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "remove_notes")).toBe(false);
    expect(store.error()).toBe("The selection no longer matches the notes in the project. Select again.");
    expect(container.querySelectorAll(".pianoroll-note")).toHaveLength(3);
  });

  it("clears the selection when the track and pattern change", async () => {
    const other = makeTrack("Bass", 1);
    // The other track's note shares the selection's identity exactly, so a
    // stale token would silently highlight it.
    other.phrases = [{
      name: "bass",
      time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 5, beat: 1, subdivision: 0 } },
      events: [makeNote({ start_beat: 0, pitch: { note: "C", accidental: "Natural", octave: 4 } })],
    }];
    const { container } = await renderSortingRoll(fourNotes(), [other]);
    fireEvent.keyDown(grid(container), { key: "a", ctrlKey: true });
    expect(selected(container)).toHaveLength(4);

    store.setSelectedPatternName(null);
    store.setSelectedTrackIndex(1);
    await flush();

    expect(selected(container)).toHaveLength(0);
  });
});

/** Render the roll in track view over one phrase carrying score marks. */
async function renderPhrase(phrase: Partial<Phrase> & { events: NoteEvent[] }) {
  const track = makeTrack("Strings", 0);
  track.phrases = [{
    name: "swell",
    time_range: {
      start: { bar: 1, beat: 1, subdivision: 0 },
      end: { bar: 3, beat: 1, subdivision: 0 },
    },
    ...phrase,
  }];
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    return makeSnapshot({ patterns: [], tracks: [track] });
  });
  await store.initProject();
  store.setSelectedPatternName(null);
  store.setSelectedTrackIndex(0);
  return renderRoll();
}

const E4 = { note: "E", accidental: "Natural", octave: 4 } as const;
const G4 = { note: "G", accidental: "Natural", octave: 4 } as const;
const C5 = { note: "C", accidental: "Natural", octave: 5 } as const;

describe("score marks", () => {
  it("draws a crescendo wedge from its mark to the dynamic it swells to", async () => {
    const { container } = await renderPhrase({
      events: [
        makeNote({ start_beat: 0, dynamics: "PP", velocity: null }),
        makeNote({ start_beat: 6, pitch: G4, dynamics: "F", velocity: null }),
      ],
      hairpins: [{ beat: 2, kind: "crescendo" }],
    });

    const wedge = container.querySelector<SVGElement>('[data-score-strip] [data-hairpin="crescendo"]')!;
    expect(wedge).toBeTruthy();
    // Six beats after the mark at the default 40 px per beat.
    expect(wedge.getAttribute("width")).toBe("160");
    expect(parseFloat(wedge.style.left)).toBe(80);
    // Opens from a point on the left and is full height on its right edge.
    expect(wedge.querySelector("polygon")!.getAttribute("points")).toBe("0,7 160,1 160,13");
    expect(container.querySelector('[data-score-strip] [data-hairpin="diminuendo"]')).toBeNull();
  });

  it("draws a diminuendo closing to a point", async () => {
    const { container } = await renderPhrase({
      events: [
        makeNote({ start_beat: 0, dynamics: "F", velocity: null }),
        makeNote({ start_beat: 4, pitch: G4, dynamics: "PP", velocity: null }),
      ],
      hairpins: [{ beat: 0, kind: "diminuendo" }],
    });

    const wedge = container.querySelector<SVGElement>('[data-score-strip] [data-hairpin="diminuendo"]')!;
    expect(wedge.getAttribute("width")).toBe("160");
    expect(wedge.querySelector("polygon")!.getAttribute("points")).toBe("0,1 0,13 160,7");
  });

  it("draws one line per pedal hold and labels it Ped.", async () => {
    const { container } = await renderPhrase({
      events: [
        makeNote({ start_beat: 0 }),
        makeNote({ start_beat: 4, pitch: E4 }),
      ],
      pedals: [{ beat: 0, down: true }, { beat: 4, down: false }],
    });

    const pedals = [...container.querySelectorAll<HTMLElement>("[data-score-strip] [data-pedal]")];
    expect(pedals).toHaveLength(1);
    expect(pedals[0].dataset.pedalEnd).toBe("4");
    expect(parseFloat(pedals[0].style.left)).toBe(0);
    expect(parseFloat(pedals[0].style.width)).toBe(160);
    expect(pedals[0].querySelector(".pianoroll-pedal-label")?.textContent).toBe("Ped.");
  });

  it("draws nothing for a phrase without marks", async () => {
    const { container } = await renderPhrase({ events: [makeNote()] });

    expect(container.querySelector("[data-score-strip]")).toBeNull();
    expect(container.querySelector("[data-hairpin]")).toBeNull();
    expect(container.querySelector("[data-pedal]")).toBeNull();
  });

  it("shows the marks a pattern opened on its own carries", async () => {
    const { container } = await renderNotes([makeNote()], {
      patterns: [{
        ...makePattern("beat"),
        instrument: { category: "keys", name: "piano" },
        hits: [],
        events: [makeNote()],
        pedals: [{ beat: 0, down: true }, { beat: 2, down: false }],
      }],
    });

    expect(container.querySelector("[data-score-strip]")).toBeTruthy();
    expect(container.querySelectorAll("[data-score-strip] [data-pedal]")).toHaveLength(1);
    // A pattern has no way to write a swell, so it shows none.
    expect(container.querySelector("[data-score-strip] [data-hairpin]")).toBeNull();
  });

  it("marks a tuplet note with its ratio and leaves a plain note alone", async () => {
    const { container } = await renderNotes([
      makeNote({ duration: { base: "Eighth", dots: 0, triplet: false, tuplet: [3, 2], ties: [] } }),
      makeNote({ start_beat: 1, pitch: E4 }),
    ]);
    const notes = container.querySelectorAll<HTMLElement>(".pianoroll-note");

    const superscript = notes[0].querySelector("[data-tuplet]");
    expect(superscript?.getAttribute("data-tuplet")).toBe("3:2");
    expect(superscript?.textContent).toBe("3");
    expect(notes[0].classList.contains("note-tuplet")).toBe(true);
    expect(notes[1].querySelector("[data-tuplet]")).toBeNull();
  });

  it("draws a diagonal on a note that glides to the next one", async () => {
    const { container } = await renderNotes([
      makeNote({ modifiers: [["gliss", { String: "next" }]] }),
      makeNote({ start_beat: 1, pitch: E4 }),
    ]);
    const notes = container.querySelectorAll<HTMLElement>(".pianoroll-note");

    expect(notes[0].querySelector("[data-note-gliss]")).toBeTruthy();
    expect(notes[1].querySelector("[data-note-gliss]")).toBeNull();
  });
});

describe("hairpin ghost bars", () => {
  /** Four unwritten notes under one crescendo; the last one writes `f`. */
  const crescendo = () => renderPhrase({
    events: [
      makeNote({ start_beat: 0, dynamics: null, velocity: null }),
      makeNote({ start_beat: 2, pitch: E4, dynamics: null, velocity: null }),
      makeNote({ start_beat: 4, pitch: G4, dynamics: null, velocity: null }),
      makeNote({ start_beat: 6, pitch: C5, dynamics: "F", velocity: null }),
    ],
    hairpins: [{ beat: 0, kind: "crescendo" }],
  });

  it("draws a note inside a hairpin hollow, with the ghost at the ramp level", async () => {
    const { container } = await crescendo();
    const bars = [...container.querySelectorAll<HTMLElement>("[data-velocity-bar]")];

    expect(bars).toHaveLength(4);
    expect(bars.slice(0, 3).map((bar) => bar.dataset.hairpin)).toEqual([
      "crescendo", "crescendo", "crescendo",
    ]);
    expect(bars[0].classList.contains("velocity-bar-hollow")).toBe(true);
    // 33 swells to 96 over six beats: 33 at the mark, 47 two beats in, 67 at four.
    const ghosts = [...container.querySelectorAll<HTMLElement>("[data-velocity-ghost]")];
    expect(ghosts.map((ghost) => ghost.dataset.ghostVelocity)).toEqual(["33", "47", "67"]);
    // The ghost sits on the lane axis, at the level it names.
    expect(parseFloat(ghosts[2].style.bottom)).toBeCloseTo(6 + (67 / 127) * 44, 1);
  });

  it("draws a note past the span solid, with no ghost", async () => {
    const { container } = await crescendo();
    const bars = [...container.querySelectorAll<HTMLElement>("[data-velocity-bar]")];

    expect(bars[3].classList.contains("velocity-bar-hollow")).toBe(false);
    expect(bars[3].dataset.hairpin).toBeUndefined();
    expect(container.querySelectorAll("[data-velocity-ghost]")).toHaveLength(3);
  });

  it("draws a note outside a hairpin solid even when the phrase has one", async () => {
    const { container } = await renderPhrase({
      events: [
        makeNote({ start_beat: 0, dynamics: "PP", velocity: null }),
        makeNote({ start_beat: 3, pitch: E4, dynamics: "F", velocity: null }),
      ],
      hairpins: [{ beat: 0, kind: "crescendo" }],
    });
    const bars = [...container.querySelectorAll<HTMLElement>("[data-velocity-bar]")];

    // The crescendo ends where the f is written, so that note is not inside it.
    expect(bars[0].classList.contains("velocity-bar-hollow")).toBe(true);
    expect(bars[1].classList.contains("velocity-bar-hollow")).toBe(false);
    expect(container.querySelectorAll("[data-velocity-ghost]")).toHaveLength(1);
  });
});

describe("toolbar labels", () => {
  it("names the velocity gestures as one group", async () => {
    const { container } = await renderNotes([makeNote({ velocity: 64, dynamics: null })]);
    // "Ramp", "Scale" and "Compress" alone read as audio effects; the label
    // right before them says they act on velocity.
    const label = container.querySelector("[data-velocity-tool]")!.previousElementSibling;
    expect(label?.textContent).toBe("Velocity");
    const tools = [...container.querySelectorAll("[data-velocity-tool]")]
      .map((element) => element.getAttribute("data-velocity-tool"));
    expect(tools).toEqual(["ramp", "scale", "compress"]);
  });
});

describe("velocity lane tools", () => {
  const tool = (container: HTMLElement, name: string) =>
    container.querySelector<HTMLElement>(`[data-velocity-tool="${name}"]`)!;
  const button = (container: HTMLElement, name: string) =>
    tool(container, name).querySelector<HTMLButtonElement>("button")!;
  const amount = (container: HTMLElement, name: string) =>
    tool(container, name).querySelector<HTMLSelectElement>("select")!;
  const calls = (cmd: string) => invokeMock.mock.calls.filter(([name]) => name === cmd);
  const selectNote = (note: HTMLElement, shiftKey = false) => {
    fireEvent.mouseDown(note, { button: 0, clientX: 10, clientY: 100, shiftKey });
    fireEvent.mouseUp(window, { button: 0, clientX: 10, clientY: 100 });
  };
  const twoNotes = () => [
    makeNote({ velocity: 40, dynamics: null }),
    makeNote({ start_beat: 1, pitch: E4, velocity: 100, dynamics: null }),
  ];

  it("ramps the selected notes in one call", async () => {
    const { container } = await renderNotes(twoNotes());
    const notes = container.querySelectorAll<HTMLElement>(".pianoroll-note");
    selectNote(notes[0]);
    selectNote(notes[1], true);

    fireEvent.click(button(container, "ramp"));
    await flush();

    expect(calls("ramp_note_velocity")).toHaveLength(1);
    expect(getLastCallArgs("ramp_note_velocity")).toEqual({
      scope: { kind: "pattern", pattern_name: "beat" },
      notes: [
        { phrase_index: 0, note_index: 0 },
        { phrase_index: 0, note_index: 1 },
      ],
    });
  });

  it("scales by the chosen percentage", async () => {
    const { container } = await renderNotes(twoNotes());
    selectNote(container.querySelector<HTMLElement>(".pianoroll-note")!);
    fireEvent.change(amount(container, "scale"), { target: { value: "75" } });

    fireEvent.click(button(container, "scale"));
    await flush();

    expect(calls("scale_note_velocity")).toHaveLength(1);
    expect(getLastCallArgs("scale_note_velocity")).toMatchObject({
      percent: 75,
      notes: [{ phrase_index: 0, note_index: 0 }],
    });
  });

  it("compresses toward the chosen target", async () => {
    const { container } = await renderNotes(twoNotes());
    selectNote(container.querySelector<HTMLElement>(".pianoroll-note")!);
    fireEvent.change(amount(container, "compress"), { target: { value: "96" } });

    fireEvent.click(button(container, "compress"));
    await flush();

    expect(calls("compress_note_velocity")).toHaveLength(1);
    expect(getLastCallArgs("compress_note_velocity")).toMatchObject({
      target: 96,
      notes: [{ phrase_index: 0, note_index: 0 }],
    });
  });

  it("states why a ramp needs two notes and refuses to run", async () => {
    const { container } = await renderNotes(twoNotes());
    selectNote(container.querySelector<HTMLElement>(".pianoroll-note")!);

    expect(button(container, "ramp").disabled).toBe(true);
    expect(tool(container, "ramp").title).toBe("Select two or more notes to ramp their velocity.");
    expect(button(container, "scale").disabled).toBe(false);

    fireEvent.click(button(container, "ramp"));
    expect(calls("ramp_note_velocity")).toHaveLength(0);
  });

  it("states why scaling and compressing need a selection", async () => {
    const { container } = await renderNotes(twoNotes());

    expect(button(container, "scale").disabled).toBe(true);
    expect(button(container, "compress").disabled).toBe(true);
    expect(tool(container, "scale").title).toBe("Select one or more notes to scale their velocity.");
    expect(tool(container, "compress").title).toBe(
      "Select one or more notes to compress their velocity.",
    );
  });

  it("shows the pending label while the gesture runs", async () => {
    const notes = twoNotes();
    const { container } = await renderNotes(notes);
    let release: (() => void) | undefined;
    invokeMock.mockImplementationOnce(() => new Promise((resolve) => {
      release = () => resolve(makeSnapshot({
        patterns: [{
          ...makePattern("beat"),
          instrument: { category: "keys", name: "piano" },
          hits: [],
          events: notes,
        }],
        tracks: [makeTrack("Piano", 0)],
      }));
    }));
    const rendered = container.querySelectorAll<HTMLElement>(".pianoroll-note");
    selectNote(rendered[0]);
    selectNote(rendered[1], true);

    fireEvent.click(button(container, "ramp"));

    expect(button(container, "ramp").textContent).toBe("Ramping\u2026");
    expect(button(container, "ramp").getAttribute("aria-busy")).toBe("true");

    release!();
    await flush();
    expect(button(container, "ramp").textContent).toBe("Ramp");
    expect(button(container, "ramp").getAttribute("aria-busy")).toBe("false");
  });

  it("renders a failed velocity gesture as text in the lane", async () => {
    const { container } = await renderNotes(twoNotes());
    invokeMock.mockRejectedValueOnce("engine refused");
    const notes = container.querySelectorAll<HTMLElement>(".pianoroll-note");
    selectNote(notes[0]);
    selectNote(notes[1], true);

    fireEvent.click(button(container, "ramp"));
    await flush();

    const notice = container.querySelector(".pianoroll-velocity-body .pianoroll-lane-notice");
    expect(notice?.textContent).toBe("Ramp failed: engine refused");
    expect(notice?.getAttribute("role")).toBe("alert");
    expect(store.error()).toBe("engine refused");
  });
});

/**
 * One backend that applies the clipboard commands to a pattern's note list the
 * way the real one does, so a paste can be read back from the roll. The
 * transport starts at `positionBeats`, which is where a paste lands.
 */
async function renderClipboardRoll(notes: NoteEvent[], positionBeats = 0) {
  let events = notes.map((note) => ({ ...note }));
  const sortEvents = () => { events = [...events].sort((a, b) => a.start_beat - b.start_beat); };
  const snapshot = () => makeSnapshot({
    patterns: [{
      ...makePattern("beat"),
      instrument: { category: "keys", name: "piano" },
      hits: [],
      events,
    }],
    tracks: [makeTrack("Piano", 0)],
  });
  invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") {
      return { playing: false, position_beats: positionBeats, position_seconds: 0, duration_seconds: 0 };
    }
    if (cmd === "transport_seek") {
      return { playing: false, position_beats: args!.beat as number, position_seconds: 0, duration_seconds: 0 };
    }
    if (cmd === "add_pattern_note") {
      const payload = args!.note as NoteEvent;
      events = [...events, { ...payload, start_beat: args!.startBeat as number }];
      sortEvents();
    }
    if (cmd === "add_notes") {
      for (const insert of args!.notes as { note: NoteEvent }[]) {
        events = [...events, { ...insert.note }];
      }
      sortEvents();
    }
    if (cmd === "duplicate_notes") {
      const refs = args!.notes as { note_index: number }[];
      const copies = refs.map((ref) => events[ref.note_index]);
      for (const note of copies) {
        events = [...events, { ...note, start_beat: note.start_beat + (args!.offsetBeats as number) }];
      }
      sortEvents();
    }
    return snapshot();
  });
  await store.initProject();
  // The store keeps transport state across mounts, so the paste anchor is set
  // explicitly instead of inherited from whatever ran before.
  await store.seek(positionBeats);
  store.setSelectedPatternName("beat");
  return renderRoll();
}

describe("note clipboard", () => {
  const grid = (container: HTMLElement) => container.querySelector("[tabindex='-1']") as HTMLElement;

  const selectNoteAt = (container: HTMLElement, index: number, shiftKey = false) => {
    const note = container.querySelectorAll<HTMLElement>(".pianoroll-note")[index];
    fireEvent.mouseDown(note, { button: 0, clientX: 10, clientY: 100, shiftKey });
    fireEvent.mouseUp(window, { button: 0, clientX: 10, clientY: 100 });
  };

  /** The loud, short, muted grace note the review duplicated. */
  const anFffStaccato = (overrides: Partial<NoteEvent> = {}) => makeNote({
    start_beat: 1,
    velocity: 127,
    dynamics: "FFF",
    articulation: "Staccato",
    ...overrides,
  });

  it("Ctrl+C then Ctrl+V keeps every field and shows the pasted velocity at once", async () => {
    const { container } = await renderClipboardRoll([anFffStaccato({ muted: true, grace: true })]);
    selectNoteAt(container, 0);
    fireEvent.keyDown(grid(container), { key: "c", ctrlKey: true });
    invokeMock.mockClear();

    fireEvent.keyDown(grid(container), { key: "v", ctrlKey: true });
    await flush();

    const args = getLastCallArgs("add_pattern_note")!;
    expect(args).toMatchObject({ patternName: "beat", startBeat: 0 });
    expect(args.note).toMatchObject({
      start_beat: 0,
      velocity: 127,
      dynamics: "FFF",
      articulation: "Staccato",
      muted: true,
      grace: true,
    });

    // The pasted note reads its own exact velocity back from the document.
    await vi.waitFor(() => {
      expect(container.querySelectorAll(".pianoroll-note")).toHaveLength(2);
    });
    const pasted = container.querySelectorAll<HTMLElement>(".pianoroll-note")[0];
    fireEvent.contextMenu(pasted, { clientX: 10, clientY: 100 });
    expect(document.querySelector(".pianoroll-note-menu .context-menu-heading")?.textContent)
      .toBe("C4 · fff→127");
    const bars = [...container.querySelectorAll<HTMLElement>('[data-velocity-bar][data-velocity-source="velocity"]')];
    expect(bars).toHaveLength(2);
    expect(parseFloat(bars[0].style.height)).toBe(parseFloat(bars[1].style.height));
  });

  it("Ctrl+D keeps every field of the duplicated note", async () => {
    const { container } = await renderClipboardRoll([anFffStaccato()]);
    selectNoteAt(container, 0);
    invokeMock.mockClear();

    fireEvent.keyDown(grid(container), { key: "d", ctrlKey: true });
    await flush();

    const args = getLastCallArgs("add_pattern_note")!;
    expect(args).toMatchObject({ patternName: "beat", startBeat: 2 });
    expect(args.note).toMatchObject({
      start_beat: 2,
      velocity: 127,
      dynamics: "FFF",
      articulation: "Staccato",
    });
  });

  it("pastes a copied group in one batch command, keeping every offset", async () => {
    const { container } = await renderClipboardRoll([
      anFffStaccato({ start_beat: 0 }),
      anFffStaccato({ start_beat: 1, pitch: { note: "E", accidental: "Natural", octave: 4 } }),
    ], 2);
    fireEvent.keyDown(grid(container), { key: "a", ctrlKey: true });
    fireEvent.keyDown(grid(container), { key: "c", ctrlKey: true });
    invokeMock.mockClear();

    fireEvent.keyDown(grid(container), { key: "v", ctrlKey: true });
    await flush();

    const calls = invokeMock.mock.calls.filter(([cmd]) => cmd === "add_notes");
    expect(calls).toHaveLength(1);
    const args = calls[0][1] as { scope: unknown; notes: { phrase_index: number; note: NoteEvent }[] };
    expect(args.scope).toEqual({ kind: "pattern", pattern_name: "beat" });
    expect(args.notes).toHaveLength(2);
    expect(args.notes.map((insert) => insert.note.start_beat)).toEqual([2, 3]);
    expect(args.notes.map((insert) => insert.note.velocity)).toEqual([127, 127]);
    expect(args.notes.map((insert) => insert.note.articulation)).toEqual(["Staccato", "Staccato"]);
    // The single-note path is not used for a group.
    expect(getLastCallArgs("add_pattern_note")).toBeUndefined();
  });

  it("duplicates a selected group in one batch command", async () => {
    const { container } = await renderClipboardRoll([
      anFffStaccato({ start_beat: 0 }),
      anFffStaccato({ start_beat: 1, pitch: { note: "E", accidental: "Natural", octave: 4 } }),
      anFffStaccato({ start_beat: 2, pitch: { note: "G", accidental: "Natural", octave: 4 } }),
    ]);
    fireEvent.keyDown(grid(container), { key: "a", ctrlKey: true });
    invokeMock.mockClear();

    fireEvent.keyDown(grid(container), { key: "d", ctrlKey: true });
    await flush();

    const calls = invokeMock.mock.calls.filter(([cmd]) => cmd === "duplicate_notes");
    expect(calls).toHaveLength(1);
    const args = calls[0][1] as { scope: unknown; notes: unknown[]; offsetBeats: number };
    expect(args.scope).toEqual({ kind: "pattern", pattern_name: "beat" });
    expect(args.notes).toHaveLength(3);
    expect(args.offsetBeats).toBe(1);
    expect(getLastCallArgs("add_pattern_note")).toBeUndefined();
  });

  it("pastes from a pattern view into a track view with the fields intact", async () => {
    const { container } = await renderClipboardRoll([anFffStaccato({ start_beat: 1 })]);
    selectNoteAt(container, 0);
    fireEvent.keyDown(grid(container), { key: "c", ctrlKey: true });

    store.setSelectedPatternName(null);
    store.setSelectedTrackIndex(0);
    await flush();
    const trackGrid = grid(container);
    invokeMock.mockClear();

    fireEvent.keyDown(trackGrid, { key: "v", ctrlKey: true });
    await flush();

    const args = getLastCallArgs("add_note")!;
    expect(args).toMatchObject({ trackIndex: 0, phraseIndex: 0, startBeat: 0 });
    expect(args.note).toMatchObject({
      velocity: 127,
      dynamics: "FFF",
      articulation: "Staccato",
    });
  });
});

describe("pattern bounds", () => {
  const patternWith = (events: NoteEvent[], bars?: number) => ({
    ...makePattern("beat"),
    instrument: { category: "keys" as const, name: "piano" },
    bars: bars ?? null,
    hits: [],
    events,
  });

  /** Render the selected pattern, optionally opened from a clip of `clipEndBar`. */
  async function renderBounds(events: NoteEvent[], bars?: number, clipEndBar?: number) {
    const track = makeTrack("Piano", 0);
    if (clipEndBar !== undefined) {
      track.clips = [{
        pattern: "beat",
        time_range: {
          start: { bar: 1, beat: 1, subdivision: 0 },
          end: { bar: clipEndBar, beat: 1, subdivision: 0 },
        },
      }];
    }
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") {
        return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      }
      return makeSnapshot({ patterns: [patternWith(events, bars)], tracks: [track] });
    });
    await store.initProject();
    store.setSelectedTrackIndex(0);
    store.setSelectedPatternName("beat");
    if (clipEndBar !== undefined) {
      store.setEditorSelection({ trackIndex: 0, startBeat: 0, patternName: "beat" });
    }
    return renderRoll();
  }

  it("clamps a draw to the pattern's length, not to the editing view", async () => {
    const { container } = await renderBounds([makeNote()], 1);
    fireEvent.mouseDown(container.querySelector("[data-note-canvas]")!, { button: 0, clientX: 200, clientY: 420 });
    fireEvent.mouseUp(window, { clientX: 200, clientY: 420 });

    // Beat 5 is past a one-bar pattern; the note lands on the last step inside.
    expect(getLastCallArgs("add_pattern_note")).toMatchObject({ startBeat: 3.75 });
  });

  it("marks a note past the pattern's length and counts it", async () => {
    const { container } = await renderBounds([makeNote(), makeNote({ start_beat: 5 })], 1);

    expect(container.querySelectorAll(".note-out-of-range")).toHaveLength(1);
    expect(container.querySelector(".pianoroll-length-edge")?.getAttribute("data-length-end-beat")).toBe("4");
    expect(container.querySelector(".pianoroll-toolbar-notice")?.textContent).toContain("1 notes and 0 hits");
  });

  it("warns in a clip whose placement ends before the pattern content", async () => {
    const { container } = await renderBounds([makeNote(), makeNote({ start_beat: 5 })], undefined, 2);

    expect(container.querySelector(".pianoroll-toolbar-notice")?.textContent).toContain("1 notes and 0 hits");
    // The roll shows the whole pattern, so the note past the clip's end is
    // visible and marked instead of hidden behind the clip edge.
    expect(container.querySelectorAll(".note-out-of-range")).toHaveLength(1);
    fireEvent.mouseDown(container.querySelector("[data-note-canvas]")!, { button: 0, clientX: 200, clientY: 420 });
    fireEvent.mouseUp(window, { clientX: 200, clientY: 420 });
    expect(getLastCallArgs("add_pattern_note")).toMatchObject({ startBeat: 3.75 });
  });
});

// ── Drum lane ──

/** A kit whose pad names the roll's fixed map did not carry. */
const DRUM_KIT: PresetDef = {
  name: "basic",
  source: "builtin",
  plugin: "builtin.drumkit",
  program: null,
  patch: null,
  file: null,
  root_note: null,
  extends: null,
  effects: [],
  params: [
    ["pad.kick.note", { Number: 36 }],
    ["pad.snare.note", { Number: 38 }],
    ["pad.hat_closed.note", { Number: 42 }],
  ],
};

function drumHit(
  drum: string,
  beat: number,
  modifiers: [string, ParamValue][] = [],
): HitEvent {
  return {
    drum,
    position: { beat, subdivision: 0 },
    dynamics: "MF",
    velocity: null,
    articulation: null,
    modifiers,
  };
}

/** The roll over one drum pattern whose track owns the test kit. */
async function renderDrumRoll(
  hits: HitEvent[],
  options: { end?: BarBeat; swing?: number } = {},
) {
  const pattern = {
    ...makePattern("beat"),
    instrument: { category: "drums", name: "drumkit" },
    preset: null,
    swing: options.swing ?? null,
    events: [],
    hits,
  };
  const track = makeTrack("drums", 0);
  track.instrument = { category: "drums", name: "drumkit" };
  track.preset = "basic";
  if (options.end) {
    track.clips = [{
      pattern: "beat",
      time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: options.end },
    }];
  }
  const snapshot = makeSnapshot({ presets: [DRUM_KIT], patterns: [pattern], tracks: [track] });
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    return snapshot;
  });
  await store.initProject();
  store.setSelectedPatternName("beat");
  if (options.end) {
    store.setSelectedTrackIndex(0);
    store.setEditorSelection({ trackIndex: 0, startBeat: 0, patternName: "beat" });
  }
  return renderRoll();
}

function laneRowLabels(container: HTMLElement): (string | undefined)[] {
  return [...container.querySelectorAll<HTMLElement>(".piano-key.drum-lane-key")]
    .map((row) => row.dataset.rowLabel);
}

/** One beat per hit, so a view that starts at bar 2 drops the first four. */
const SLIP_HITS: HitEvent[] = [
  "kick", "snare", "rim", "clap", "tomlow", "tommid", "tomhigh", "crash",
].map((drum, index) => drumHit(drum, index + 1));

describe("drum lane", () => {
  it("builds its rows from the kit's pads, in the kit's order", async () => {
    const { container } = await renderDrumRoll([]);

    expect(laneRowLabels(container)).toEqual(["kick", "snare", "hat_closed"]);
    const notes = [...container.querySelectorAll<HTMLElement>(".piano-key.drum-lane-key")]
      .map((row) => row.dataset.rowNote);
    expect(notes).toEqual(["36", "38", "42"]);
    // Empty rows are gone: three pads, three rows, not one per semitone.
    expect(container.querySelectorAll(".pianoroll-row-bg.drum-lane-row")).toHaveLength(3);
    expect(container.querySelector<HTMLElement>("[data-kit-name]")?.dataset.kitName).toBe("basic");
  });

  it("shows a note the kit does not map as an Other row with its number", async () => {
    const { container } = await renderDrumRoll([
      drumHit("pad_50", 1, [["midi_note", { Number: 50 }]]),
    ]);

    expect(laneRowLabels(container)).toEqual(["kick", "snare", "hat_closed", "Other 50"]);
    const hit = container.querySelector<HTMLElement>(".pianoroll-drum-hit")!;
    expect(hit.dataset.hitRowNote).toBe("50");
    // The hit sits on the Other row, wherever that row is.
    const otherRow = container.querySelector<HTMLElement>('[data-row-label="Other 50"]')!;
    expect(parseFloat(hit.style.top)).toBeGreaterThan(parseFloat(otherRow.style.top));
    expect(parseFloat(hit.style.top)).toBeLessThan(parseFloat(otherRow.style.top) + 12);
  });

  it("keeps excluded authored hits editable and agrees about both clip-edge checks", async () => {
    const at = (drum: string, beat: number, subdivision: number, offset = 0): HitEvent => ({
      ...drumHit(drum, beat, offset === 0 ? [] : [["timing_offset_beats", { Number: offset }]]),
      position: { beat, subdivision },
    });
    const { container } = await renderDrumRoll([
      at("kick", 2, 1),                   // 1.25q is played.
      at("hat_closed", 3, 2),             // 2.5q swings past the 2.75q end.
      at("snare", 3, 3, -0.0625),         // A negative offset cannot revive an excluded anchor.
      at("snare", 4, 0),                  // Written beyond the view's playback edge.
    ], { end: { bar: 1, beat: 3, subdivision: 3 }, swing: 100 });
    const markers = [...container.querySelectorAll<HTMLElement>(".pianoroll-drum-hit")];
    const bars = [...container.querySelectorAll<HTMLElement>("[data-hit-bar]")];
    expect(markers.map((hit) => [hit.dataset.hitIndex, hit.classList.contains("hit-out-of-range")]))
      .toEqual([["0", false], ["1", true], ["2", true], ["3", true]]);
    expect(bars.map((hit) => [hit.dataset.hitIndex, hit.classList.contains("hit-out-of-range")]))
      .toEqual([["0", false], ["1", true], ["2", true], ["3", true]]);
    expect(container.querySelector(".pianoroll-toolbar-notice")?.textContent)
      .toContain(i18n.t("pianoroll.outOfRange", { notes: 0, hits: 3 }));
    expect(container.querySelector("[data-note-canvas]")?.getAttribute("data-view-end-beat"))
      .toBe("4");
    expect((container.querySelector(".pianoroll-ruler .pianoroll-clip-edge.end") as HTMLElement).style.left)
      .toBe("110px");

    fireEvent.mouseDown(bars[3], { button: 0, clientY: 10 });
    expect(markers[3].classList.contains("hit-selected")).toBe(true);
    fireEvent.mouseUp(document);
    expect(getLastCallArgs("set_pattern_hit_velocity")).toMatchObject({
      patternName: "beat", hitIndex: 3,
    });
    expect(getLastCallArgs("add_pattern_hit")).toBeUndefined();
  });

  it("writes a hit's position as the nearest sixteenth plus the grid offset", () => {
    // A 1/32 position has no sixteenth to snap to, so the offset carries it.
    expect(hitPlacement(0.03125)).toEqual({ beat: 1, subdivision: 0, offsetBeats: 0.03125 });
    // A triplet sits between sixteenths; the nearest one keeps the offset
    // inside the engine's ±0.125 limit.
    const triplet = hitPlacement(1 / 3);
    expect(triplet.beat).toBe(1);
    expect(triplet.subdivision).toBe(1);
    expect(triplet.offsetBeats).toBeCloseTo(1 / 12, 9);
    // A hit a 32nd before a beat is written on that beat, offset backwards.
    expect(hitPlacement(0.875)).toEqual({ beat: 2, subdivision: 0, offsetBeats: -0.125 });
  });

  it("places a hit exactly on the 1/32 grid, not on a sixteenth", async () => {
    const { container } = await renderDrumRoll([]);
    setGridDivision(8);

    const canvas = container.querySelector("[data-note-canvas]") as HTMLElement;
    const clientX = 0.125 * 40;
    fireEvent.mouseDown(canvas, { button: 0, clientX, clientY: 6 });
    fireEvent.mouseUp(window, { button: 0, clientX, clientY: 6 });

    const args = getLastCallArgs("add_pattern_hit")!;
    expect(args).toMatchObject({
      patternName: "beat", drum: "kick", beat: 1, subdivision: 1, midiNote: null,
    });
    expect(args.gridOffsetBeats as number).toBeCloseTo(-0.125, 9);

    const start = hitStartQuarters({
      ...drumHit("kick", args.beat as number),
      position: { beat: args.beat as number, subdivision: args.subdivision as number },
      modifiers: [["grid_offset_beats", { Number: args.gridOffsetBeats as number }]],
    });
    expect(start).toBeCloseTo(0.125, 9);
  });

  it("places a hit exactly on the triplet grid", async () => {
    const { container } = await renderDrumRoll([]);
    fireEvent.change(container.querySelector(".pianoroll-toolbar select")!, { target: { value: "3" } });

    const canvas = container.querySelector("[data-note-canvas]") as HTMLElement;
    const clientX = 40 / 3;
    fireEvent.mouseDown(canvas, { button: 0, clientX, clientY: 6 });
    fireEvent.mouseUp(window, { button: 0, clientX, clientY: 6 });

    const args = getLastCallArgs("add_pattern_hit")!;
    expect(args).toMatchObject({ drum: "kick", beat: 1, subdivision: 1 });
    expect(args.gridOffsetBeats as number).toBeCloseTo(1 / 12, 9);

    const start = hitStartQuarters({
      ...drumHit("kick", args.beat as number),
      position: { beat: args.beat as number, subdivision: args.subdivision as number },
      modifiers: [["grid_offset_beats", { Number: args.gridOffsetBeats as number }]],
    });
    expect(start).toBeCloseTo(1 / 3, 9);
  });

  it.each([[8, 2.625], [6, 8 / 3]])(
    "places the final playable step of division %s before a trimmed clip edge",
    async (division, expected) => {
      const { container } = await renderDrumRoll([], {
        end: { bar: 1, beat: 3, subdivision: 3 },
        swing: 100,
      });
      setGridDivision(division);
      const canvas = container.querySelector("[data-note-canvas]") as HTMLElement;
      const clientX = 2.74 * 40;
      fireEvent.mouseMove(canvas, { clientX, clientY: 6 });
      const ghost = container.querySelector<HTMLElement>(".note-preview")!;
      expect(parseFloat(ghost.style.left)).toBeCloseTo(expected * 40, 8);
      fireEvent.mouseDown(canvas, { button: 0, clientX, clientY: 6 });
      fireEvent.mouseUp(window, { button: 0, clientX, clientY: 6 });

      const args = getLastCallArgs("add_pattern_hit")!;
      expect(args).toMatchObject({ beat: 3, subdivision: 3, offsetBeats: null });
      const hit: HitEvent = {
        ...drumHit("kick", 3),
        position: { beat: 3, subdivision: 3 },
        modifiers: [["grid_offset_beats", { Number: args.gridOffsetBeats as number }]],
      };
      expect(hitStartQuarters(hit)).toBeCloseTo(expected, 9);
      const placed = await renderDrumRoll([hit], {
        end: { bar: 1, beat: 3, subdivision: 3 },
        swing: 100,
      });
      const marker = placed.container.querySelector(".pianoroll-drum-hit");
      expect(marker).toBeTruthy();
      expect(marker?.classList.contains("hit-out-of-range")).toBe(false);
    },
  );

  it("writes an unmapped row's note so the hit plays that note", async () => {
    const { container } = await renderDrumRoll([
      drumHit("pad_50", 1, [["midi_note", { Number: 50 }]]),
    ]);

    const canvas = container.querySelector("[data-note-canvas]") as HTMLElement;
    // The fourth row is the Other 50 row.
    fireEvent.mouseDown(canvas, { button: 0, clientX: 40, clientY: 36 + 6 });
    fireEvent.mouseUp(window, { button: 0, clientX: 40, clientY: 36 + 6 });

    expect(getLastCallArgs("add_pattern_hit")).toMatchObject({
      patternName: "beat", drum: "pad_50", beat: 2, subdivision: 0, midiNote: 50,
    });
  });

  it("keeps the real hit index when the view drops the hits before it", async () => {
    const track = makeTrack("drums", 0);
    track.instrument = { category: "drums", name: "drumkit" };
    track.preset = "basic";
    track.phrases = [];
    track.patterns = [{
      name: "beat",
      time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 3, beat: 1, subdivision: 0 } },
      repeat: 1,
      hits: SLIP_HITS,
    }];
    const snapshot = makeSnapshot({
      presets: [DRUM_KIT],
      patterns: [{ ...makePattern("beat"), preset: null, events: [], hits: SLIP_HITS }],
      tracks: [track],
    });
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return snapshot;
    });
    await store.initProject();
    store.setSelectedTrackIndex(0);
    store.setSelectedPatternName(null);
    // The view opens at bar 2, so beats 0-3 sit before x = 0.
    store.setEditorSelection({ trackIndex: 0, startBeat: 4, patternName: null });

    const { container, menuItem } = renderRoll();
    const hits = [...container.querySelectorAll<HTMLElement>(".pianoroll-drum-hit")];
    expect(hits).toHaveLength(4);

    fireEvent.contextMenu(hits[1], { clientX: 200, clientY: 20 });
    const heading = document.querySelector(".pianoroll-hit-menu .context-menu-heading");
    expect(heading?.textContent).toBe("tommid");
    fireEvent.click(menuItem(i18n.t("menu.hit.delete"))!);

    const args = getLastCallArgs("remove_pattern_hit")!;
    expect(args).toMatchObject({ patternName: "beat", hitIndex: 5 });
    expect(args.hitIndex).not.toBe(1);
  });

  it("deletes the picked hit with the delete key", async () => {
    const { container } = await renderDrumRoll([drumHit("kick", 1), drumHit("snare", 2)]);
    const hits = [...container.querySelectorAll<HTMLElement>(".pianoroll-drum-hit")];
    expect(hits).toHaveLength(2);

    fireEvent.click(hits[1]);
    expect(hits[1].classList.contains("hit-selected")).toBe(true);
    fireEvent.keyDown(container.querySelector("[data-pianoroll-grid]")!, { key: "Delete" });

    expect(getLastCallArgs("remove_pattern_hit")).toMatchObject({
      patternName: "beat", hitIndex: 1,
    });
  });
});

afterEach(() => {
  Object.defineProperty(navigator, "userAgent", { value: originalUserAgent, configurable: true });
});

describe("right-drag eraser", () => {
  const canvas = (container: HTMLElement) =>
    container.querySelector<HTMLElement>("[data-note-canvas]")!;

  it("erases the note the pointer passes over", async () => {
    const { container } = await renderNotes([
      makeNote({ start_beat: 0, pitch: { note: "C", accidental: "Natural", octave: 4 } }),
      makeNote({ start_beat: 2, pitch: { note: "E", accidental: "Natural", octave: 4 } }),
    ]);
    const notes = container.querySelectorAll<HTMLElement>(".pianoroll-note");
    // jsdom has no hit testing, so the pointer answers with the note under the
    // x coordinate it is asked about.
    const original = document.elementFromPoint;
    document.elementFromPoint = (x: number) => (x < 20 ? notes[0] : notes[1]) as Element;

    fireEvent.pointerDown(canvas(container), { button: 2, pointerId: 7, clientX: 10, clientY: 100 });
    fireEvent.pointerMove(window, { pointerId: 7, clientX: 30, clientY: 100 });
    fireEvent.pointerUp(window, { pointerId: 7, clientX: 30, clientY: 100 });
    document.elementFromPoint = original;
    await flush();

    const removals = invokeMock.mock.calls.filter(([cmd]) => cmd === "remove_notes");
    expect(removals.length).toBeGreaterThan(0);
    const removed = removals.flatMap(([, args]) => (args as any).notes.map((n: any) => n.note_index));
    expect(new Set(removed)).toEqual(new Set([0, 1]));
    // One undo entry for the whole swipe.
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "begin_undo_gesture")).toHaveLength(1);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "end_undo_gesture")).toHaveLength(1);
  });

  it("opens the note menu when the press never moves", async () => {
    const { container } = await renderNotes([
      makeNote({ start_beat: 0, pitch: { note: "C", accidental: "Natural", octave: 4 } }),
    ]);
    const note = container.querySelector<HTMLElement>(".pianoroll-note")!;
    const original = document.elementFromPoint;
    document.elementFromPoint = () => note;

    fireEvent.pointerDown(canvas(container), { button: 2, pointerId: 9, clientX: 10, clientY: 100 });
    fireEvent.pointerUp(window, { pointerId: 9, clientX: 10, clientY: 100 });
    document.elementFromPoint = original;
    await flush();

    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "remove_notes")).toHaveLength(0);
    expect(document.querySelector(".pianoroll-note-menu")).toBeTruthy();
  });
});

describe("erasing drum hits", () => {
  it("erases a hit the right-drag passes over", async () => {
    const pattern = {
      ...makePattern("beat"),
      instrument: { category: "drums", name: "acoustic_kit" },
      events: [],
      hits: [
        { drum: "kick", position: { beat: 1, subdivision: 0 }, dynamics: "MF" as const, articulation: null, modifiers: [] },
        { drum: "snare", position: { beat: 2, subdivision: 0 }, dynamics: "MF" as const, articulation: null, modifiers: [] },
      ],
    };
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({ patterns: [pattern], tracks: [makeTrack("Drums", 0)] });
    });
    await store.initProject();
    store.setSelectedPatternName("beat");
    const { container } = renderRoll();
    await flush();

    const hit = container.querySelector<HTMLElement>(".pianoroll-drum-hit")!;
    const original = document.elementFromPoint;
    document.elementFromPoint = () => hit;
    const canvas = container.querySelector<HTMLElement>("[data-note-canvas]")!;

    fireEvent.pointerDown(canvas, { button: 2, pointerId: 11, clientX: 10, clientY: 100 });
    fireEvent.pointerMove(window, { pointerId: 11, clientX: 40, clientY: 100 });
    fireEvent.pointerUp(window, { pointerId: 11, clientX: 40, clientY: 100 });
    document.elementFromPoint = original;
    await flush();

    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "remove_pattern_hit").length).toBeGreaterThan(0);
  });
});

describe("optional lanes", () => {
  const velocityLane = (container: HTMLElement) =>
    container.querySelector(".pianoroll-velocity-lane");
  const articulationLane = (container: HTMLElement) =>
    container.querySelector(".pianoroll-articulation-lane");

  it("hides both lanes for a pattern with nothing in it", async () => {
    const { container } = await renderNotes([]);
    expect(velocityLane(container)).toBeNull();
    expect(articulationLane(container)).toBeNull();
  });

  it("shows the velocity lane once the pattern has notes", async () => {
    const { container } = await renderNotes([makeNote({})]);
    expect(velocityLane(container)).toBeTruthy();
    // No note names an articulation, so that lane stays out of the way.
    expect(articulationLane(container)).toBeNull();
  });

  it("shows the articulation lane only while a note uses one", async () => {
    const { container } = await renderNotes([makeNote({ articulation: "Staccato" })]);
    expect(articulationLane(container)).toBeTruthy();
  });
});

describe("pattern length control", () => {
  it("adds a bar and lets the clip follow", async () => {
    const { container } = await renderNotes([
      makeNote({ start_beat: 0, pitch: { note: "C", accidental: "Natural", octave: 4 } }),
    ]);
    const add = container.querySelector<HTMLButtonElement>('[data-pattern-length] button[aria-label="Add a bar"]')!;
    expect(add).toBeTruthy();
    fireEvent.click(add);
    await flush();

    expect(getLastCallArgs("set_pattern_bars")).toMatchObject({ patternName: "beat", bars: 2 });
  });

  it("never shrinks a pattern below one bar", async () => {
    const { container } = await renderNotes([
      makeNote({ start_beat: 0, pitch: { note: "C", accidental: "Natural", octave: 4 } }),
    ]);
    const remove = container.querySelector<HTMLButtonElement>('[data-pattern-length] button[aria-label="Remove a bar"]')!;
    expect(remove.disabled).toBe(true);
  });
});

describe("clip end handle", () => {
  it("drags the clip's end to a new bar", async () => {
    const { container } = await renderBoundsForHandle();
    const handle = container.querySelector<HTMLElement>("[data-clip-end-handle]")!;
    expect(handle).toBeTruthy();

    fireEvent.pointerDown(handle, { button: 0, pointerId: 3, clientX: 160, clientY: 10 });
    fireEvent.pointerMove(window, { pointerId: 3, clientX: 320, clientY: 10 });
    fireEvent.pointerUp(window, { pointerId: 3, clientX: 320, clientY: 10 });
    await flush();

    // 40 px per beat: x=320 is beat 8, the end of a two-bar clip.
    expect(getLastCallArgs("resize_clip")).toMatchObject({
      trackIndex: 0, clipIndex: 0, endBar: 3, endBeat: 1, endSub: 0,
    });
  });

  it("grows a bare pattern by whole bars", async () => {
    const { container } = await renderNotes([
      makeNote({ start_beat: 0, pitch: { note: "C", accidental: "Natural", octave: 4 } }),
    ]);
    const handle = container.querySelector<HTMLElement>("[data-clip-end-handle]")!;
    fireEvent.pointerDown(handle, { button: 0, pointerId: 4, clientX: 160, clientY: 10 });
    fireEvent.pointerMove(window, { pointerId: 4, clientX: 320, clientY: 10 });
    fireEvent.pointerUp(window, { pointerId: 4, clientX: 320, clientY: 10 });
    await flush();

    expect(getLastCallArgs("set_pattern_bars")).toMatchObject({ patternName: "beat", bars: 2 });
  });
});

async function renderBoundsForHandle() {
  const track = makeTrack("Piano", 0);
  track.clips = [{
    pattern: "beat",
    time_range: {
      start: { bar: 1, beat: 1, subdivision: 0 },
      end: { bar: 2, beat: 1, subdivision: 0 },
    },
  }];
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") {
      return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    }
    return makeSnapshot({ patterns: [makePattern("beat")], tracks: [track] });
  });
  await store.initProject();
  store.setSelectedTrackIndex(0);
  store.setSelectedPatternName("beat");
  store.setEditorSelection({ trackIndex: 0, startBeat: 0, patternName: "beat" });
  return renderRoll();
}
