/**
 * Timeline component interaction tests.
 * Tests track selection, clip rendering, cursor visibility, and header interactions.
 * Runs in jsdom — no browser required.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack, makePattern, makeNote, getLastCallArgs, TRANSPORT_PLAYING, TRANSPORT_STOPPED } from "../setup";
import { store } from "../../stores/project";
import { Timeline } from "../../components/Timeline";
import { Toolbar } from "../../components/Toolbar";
import { ContextMenuLayer } from "../../components/ContextMenu";

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
    return makeSnapshot({
      tracks: [makeTrack("Piano", 0), makeTrack("Drums", 1)],
      patterns: [makePattern("beat")],
    });
  });
  await store.initProject();
});

describe("Timeline", () => {
  it("shares one vertical scroll container between track headers and lanes", () => {
    const { container } = render(() => <Timeline />);
    const lanes = container.querySelector(".timeline-lanes") as HTMLDivElement;
    const headers = container.querySelector(".track-headers") as HTMLDivElement;
    const content = container.querySelector(".timeline-scroll-content") as HTMLDivElement;
    const laneColumn = container.querySelector(".timeline-lane-column") as HTMLDivElement;

    // Both columns live inside the same scroller, so their vertical offset is
    // structurally identical and no JS sync (and therefore no lag) is needed.
    expect(content.parentElement).toBe(lanes);
    expect(headers.parentElement).toBe(content);
    expect(laneColumn.parentElement).toBe(content);
    expect(lanes.contains(headers)).toBe(true);

    lanes.scrollLeft = 300;
    lanes.scrollTop = 145.5;
    fireEvent.scroll(lanes);
    expect(lanes.scrollTop).toBe(145.5);
    expect(lanes.scrollLeft).toBe(300);
  });
  it("leaves the gutter to the brand block and puts the px/beat pill at the bottom centre", () => {
    const { container } = render(() => <Timeline />);
    const spacer = container.querySelector(".timeline-ruler-spacer") as HTMLElement;
    expect(spacer.querySelector(".brand-name")).toBeFalsy();
    expect(spacer.querySelector(".zoom-label")).toBeFalsy();

    const pill = container.querySelector(".timeline-zoom-center") as HTMLElement;
    const label = pill.querySelector(".zoom-label") as HTMLElement;
    expect(label.textContent).toBe("24px/beat");
    const marks = container.querySelectorAll<HTMLElement>(".timeline-ruler-mark");
    expect(marks[1].style.left).toBe("96px");

    const zoomIn = pill.querySelector('[aria-label="Zoom in"]') as HTMLButtonElement;
    fireEvent.click(zoomIn);
    expect(label.textContent).toBe("28px/beat");
    // Bar 2 sits four beats in, so its tick must follow the new scale.
    expect(marks[1].style.left).toBe("112px");

    const zoomOut = pill.querySelector('[aria-label="Zoom out"]') as HTMLButtonElement;
    fireEvent.click(zoomOut);
    expect(label.textContent).toBe("24px/beat");
  });
  it("anchors the zoom bar to the timeline panel, not the scroller", () => {
    const { container } = render(() => <Timeline />);
    const panel = container.querySelector(".timeline-panel") as HTMLElement;
    const lanes = container.querySelector(".timeline-lanes") as HTMLElement;
    const zoom = container.querySelector(".timeline-zoom-bar") as HTMLElement;

    expect(zoom).toBeTruthy();
    expect(lanes.contains(zoom)).toBe(false);
    expect(zoom.parentElement).toBe(panel);
  });
  it("follows on play, detaches on manual scrolling, and reattaches on next play", async () => {
    let beat = 40;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_play" || cmd === "get_transport_state" || cmd === "transport_seek")
        return { ...TRANSPORT_PLAYING(), position_beats: beat };
      if (cmd === "transport_stop") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });
    const { container } = render(() => <Timeline />);
    const lanes = container.querySelector(".timeline-lanes") as HTMLDivElement;
    Object.defineProperty(lanes, "clientWidth", { value: 400 });
    await store.play();
    expect(lanes.scrollLeft).toBeGreaterThan(0);
    fireEvent.scroll(lanes); // programmatic follow must not detach
    fireEvent.wheel(lanes, { deltaX: -100 });
    lanes.scrollLeft = 0;
    fireEvent.scroll(lanes);
    beat = 50;
    await store.seek(beat);
    expect(lanes.scrollLeft).toBe(0);
    await store.stop();
    await store.play();
    expect(lanes.scrollLeft).toBeGreaterThan(0);
    await store.stop();
  });
  it("exposes section endings beyond all clips and shrinks after their removal", async () => {
    const sections = [{
      name: "Silent ending", repeat: null, label: null,
      time_range: { start: { bar: 33, beat: 1, subdivision: 0 }, end: { bar: 49, beat: 1, subdivision: 0 } },
    }];
    invokeMock.mockImplementation(async () => makeSnapshot({ tracks: [makeTrack("Piano", 0)], sections }));
    await store.initProject();
    const { container } = render(() => <Timeline />);
    expect(container.querySelectorAll(".timeline-ruler-mark").length).toBe(53);
    invokeMock.mockImplementation(async () => makeSnapshot({ tracks: [makeTrack("Piano", 0)], sections: [] }));
    await store.initProject();
    expect(container.querySelectorAll(".timeline-ruler-mark").length).toBe(20);
  });

  it("renders without crashing", () => {
    const { container } = render(() => <Timeline />);
    expect(container.querySelector(".timeline-panel")).toBeTruthy();
  });

  it("renders track headers for all tracks", () => {
    const { container } = render(() => <Timeline />);
    const headers = container.querySelectorAll(".track-header");
    expect(headers.length).toBe(2);
  });

  it("displays track names", () => {
    const { container } = render(() => <Timeline />);
    const text = container.textContent;
    expect(text).toContain("Piano");
    expect(text).toContain("Drums");
  });

  it("selects track on header click", () => {
    const { container } = render(() => <Timeline />);
    const headers = container.querySelectorAll(".track-header");
    expect(headers.length).toBe(2);

    fireEvent.click(headers[1]);
    expect(store.selectedTrackIndex()).toBe(1);
  });

  it("renders track lanes", () => {
    const { container } = render(() => <Timeline />);
    const lanes = container.querySelectorAll(".track-lane");
    expect(lanes.length).toBe(2);
  });

  it("renders phrase blocks for tracks with phrases", () => {
    const { container } = render(() => <Timeline />);
    const phrases = container.querySelectorAll(".phrase-block");
    // Each track has 1 phrase in makeTrack
    expect(phrases.length).toBeGreaterThanOrEqual(2);
  });

  it("renders inline phrase note thumbnails", () => {
    const { container } = render(() => <Timeline />);
    const phrase = container.querySelector(".phrase-note-block") as HTMLElement;

    expect(phrase).toBeTruthy();
    expect(phrase.querySelector(".clip-label-band")).toBeFalsy();
    expect(phrase.querySelector(".clip-label-chip")?.textContent).toContain("melody");
    expect(phrase.querySelectorAll(".clip-note-thumb").length).toBeGreaterThan(0);
    expect(phrase.querySelector(".clip-color-rail")).toBeFalsy();
    expect(phrase.querySelector(".clip-accent-flag")).toBeTruthy();
  });

  it("renders clip accent flags, labels, and note thumbnails", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        patterns: [{
          ...makePattern("lead"),
          events: [
            makeNote({ start_beat: 0, pitch: { note: "C", accidental: "Natural", octave: 4 } }),
            makeNote({ start_beat: 1.5, pitch: { note: "G", accidental: "Natural", octave: 4 } }),
          ],
          hits: [],
        }],
        tracks: [{
          ...makeTrack("Lead", 0),
          clips: [{ pattern: "lead", time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 3, beat: 1, subdivision: 0 } } }],
        }],
      });
    });
    await store.initProject();

    const { container } = render(() => <Timeline />);
    const clip = container.querySelector(".clip-block") as HTMLElement;

    expect(clip).toBeTruthy();
    expect(clip.querySelector(".clip-color-rail")).toBeFalsy();
    expect(clip.querySelector(".clip-accent-flag")).toBeTruthy();
    expect(clip.querySelector(".clip-label-band")).toBeFalsy();
    const chip = clip.querySelector(".clip-label-chip");
    expect(chip).toBeTruthy();
    expect(chip?.textContent).toContain("lead");
    expect(chip?.textContent).toContain("2n");
    expect(clip.querySelectorAll(".clip-note-thumb").length).toBe(2);
  });

  it("maps thumbnail x positions to the full clip width", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0 };
      return makeSnapshot({
        patterns: [{
          ...makePattern("lead"),
          events: [
            makeNote({ start_beat: 0, pitch: { note: "C", accidental: "Natural", octave: 4 } }),
            makeNote({ start_beat: 4, pitch: { note: "G", accidental: "Natural", octave: 4 } }),
          ],
          hits: [],
        }],
        tracks: [{
          ...makeTrack("Lead", 0),
          clips: [{ pattern: "lead", time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 3, beat: 1, subdivision: 0 } } }],
        }],
      });
    });
    await store.initProject();

    const { container } = render(() => <Timeline />);
    const clip = container.querySelector(".clip-block") as HTMLElement;
    const thumbnail = clip.querySelector(".clip-thumbnail") as HTMLElement;
    expect(thumbnail).toBeTruthy();
    // Position comes from CSS only — no inline horizontal offset on the container.
    expect(thumbnail.style.left).toBe("");
    expect(thumbnail.style.right).toBe("");

    const notes = clip.querySelectorAll<HTMLElement>(".clip-note-thumb");
    expect(notes.length).toBe(2);
    expect(notes[0].style.left).toBe("0%");
    expect(notes[1].style.left).toBe("50%");
  });

  it("scrolls the lanes to keep the dragged playhead visible", () => {
    const { container } = render(() => <><Toolbar /><Timeline /></>);
    const lanes = container.querySelector(".timeline-lanes") as HTMLDivElement;
    Object.defineProperty(lanes, "clientWidth", { configurable: true, value: 400 });
    const ruler = container.querySelector(".timeline-ruler") as HTMLElement;
    const playhead = () => container.querySelector(".playback-cursor") as HTMLElement;

    fireEvent.pointerDown(ruler, { button: 0, clientX: 48, pointerId: 11 });
    fireEvent.pointerMove(document, { clientX: 960, pointerId: 11, buttons: 1 });

    expect(playhead().style.left).toBe("960px");
    expect(lanes.scrollLeft).toBeGreaterThan(0);
    fireEvent.pointerCancel(document, { pointerId: 11 });
  });

  it("tracks a ruler drag every move and seeks once on release", async () => {
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "transport_mute" || cmd === "transport_unmute") return undefined;
      if (cmd === "transport_seek") return { ...TRANSPORT_STOPPED(), position_beats: (args as { beat: number }).beat };
      return makeSnapshot({
        tracks: [makeTrack("Piano", 0), makeTrack("Drums", 1)],
        patterns: [makePattern("beat")],
      });
    });
    await store.initProject();
    const { container } = render(() => <><Toolbar /><Timeline /></>);
    const ruler = container.querySelector(".timeline-ruler") as HTMLElement;
    const readout = () => container.querySelector(".position-display") as HTMLElement;
    const playhead = () => container.querySelector(".playback-cursor") as HTMLElement;
    const seeks = () => invokeMock.mock.calls.filter(([cmd]) => cmd === "transport_seek");

    fireEvent.pointerDown(ruler, { button: 0, clientX: 48, pointerId: 1 });
    expect(readout().textContent).toBe("1:3:0");
    expect(playhead().style.left).toBe("48px");

    fireEvent.pointerMove(document, { clientX: 120, pointerId: 1, buttons: 1 });
    expect(readout().textContent).toBe("2:2:0");
    expect(playhead().style.left).toBe("120px");

    fireEvent.pointerMove(document, { clientX: 192, pointerId: 1, buttons: 1 });
    expect(readout().textContent).toBe("3:1:0");
    expect(playhead().style.left).toBe("192px");
    expect(seeks()).toHaveLength(0);

    fireEvent.pointerUp(document, { clientX: 192, pointerId: 1, button: 0 });
    await vi.waitFor(() => expect(seeks()).toHaveLength(1));
    expect(getLastCallArgs("transport_seek")).toMatchObject({ beat: 8 });
    await vi.waitFor(() => expect(readout().textContent).toBe("3:1:0"));
  });

  it("shows the ruler", () => {
    const { container } = render(() => <Timeline />);
    const ruler = container.querySelector(".timeline-ruler");
    expect(ruler).toBeTruthy();
  });

  it("renders ruler", () => {
    const { container } = render(() => <Timeline />);
    const ruler = container.querySelector(".timeline-ruler");
    expect(ruler).toBeTruthy();
  });

  it("starts track rename on double-click", () => {
    const { container } = render(() => <Timeline />);
    const headers = container.querySelectorAll(".track-header");
    const nameEl = headers[0].querySelector(".track-name");
    if (!nameEl) return;

    fireEvent.dblClick(nameEl);
    // After double-click, an input should appear
    const input = headers[0].querySelector(".track-name-input") as HTMLInputElement;
    expect(input).toBeTruthy();
  });

  it("applies track rename on Enter key", async () => {
    const { container } = render(() => <Timeline />);
    const headers = container.querySelectorAll(".track-header");
    const nameEl = headers[0].querySelector(".track-name");
    if (!nameEl) return;

    fireEvent.dblClick(nameEl);
    const input = headers[0].querySelector(".track-name-input") as HTMLInputElement;
    if (!input) return;

    fireEvent.input(input, { target: { value: "New Piano Name" } });
    invokeMock.mockResolvedValueOnce(makeSnapshot({
      tracks: [{ ...makeTrack("New Piano Name", 0) }, makeTrack("Drums", 1)],
      patterns: [makePattern("beat")],
    }));
    fireEvent.keyDown(input, { key: "Enter" });

    // After Enter, rename should invoke the backend
    await vi.waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([c]) => c === "rename_track");
      expect(calls.length).toBeGreaterThan(0);
    });
  });

  it("cancels rename on Escape", () => {
    const { container } = render(() => <Timeline />);
    const headers = container.querySelectorAll(".track-header");
    const nameEl = headers[0].querySelector(".track-name");
    if (!nameEl) return;

    fireEvent.dblClick(nameEl);
    const input = headers[0].querySelector(".track-name-input") as HTMLInputElement;
    if (!input) return;

    fireEvent.keyDown(input, { key: "Escape" });
    // After Escape, input should be gone (back to display mode)
    const inputAfter = headers[0].querySelector(".track-name-input");
    expect(inputAfter).toBeFalsy();
  });
});

describe("Timeline clip editing", () => {
  type MockArgs = {
    patternName?: string; trackIndex?: number; clipIndex?: number;
    startBar?: number; startBeat?: number; startSub?: number;
    endBar?: number; endBeat?: number; endSub?: number; beat?: number;
  };

  function installClipEditorMock() {
    let clips = [{
      pattern: "lead",
      time_range: {
        start: { bar: 1, beat: 1, subdivision: 0 },
        end: { bar: 3, beat: 1, subdivision: 0 },
      },
    }];
    const patterns = [{
      ...makePattern("lead"),
      events: [makeNote({ start_beat: 0 }), makeNote({ start_beat: 4 })],
      hits: [],
    }];
    const snapshot = () => makeSnapshot({
      patterns,
      tracks: [{ ...makeTrack("Lead", 0), clips }],
    });
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      const a = (args ?? {}) as MockArgs;
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "transport_stop") return TRANSPORT_STOPPED();
      if (cmd === "transport_seek") return { ...TRANSPORT_STOPPED(), position_beats: a.beat ?? 0 };
      if (cmd === "add_clip") {
        clips = [...clips, {
          pattern: a.patternName ?? "lead",
          time_range: {
            start: { bar: a.startBar ?? 1, beat: a.startBeat ?? 1, subdivision: a.startSub ?? 0 },
            end: { bar: a.endBar ?? 1, beat: a.endBeat ?? 1, subdivision: a.endSub ?? 0 },
          },
        }];
        return snapshot();
      }
      if (cmd === "remove_clip") {
        clips = clips.filter((_, index) => index !== a.clipIndex);
        return snapshot();
      }
      return snapshot();
    });
    const commandCount = (name: string) => invokeMock.mock.calls.filter(([cmd]) => cmd === name).length;
    return { snapshot, commandCount };
  }

  /** Open every menu group, so a row one level down can be found. */
  function expandMenuGroups(): void {
    for (const group of document.querySelectorAll<HTMLElement>(".context-menu-group")) {
      if (group.getAttribute("aria-expanded") !== "true") fireEvent.click(group);
    }
  }

  function menuItem(label: string): HTMLElement | undefined {
    const find = () => [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
      .find((item) => item.querySelector(".context-menu-label")?.textContent === label);
    const first = find();
    if (first) return first;
    expandMenuGroups();
    return find();
  }

  async function renderTimeline() {
    const view = render(() => <><Timeline /><ContextMenuLayer /></>);
    const panel = view.container.querySelector(".timeline-panel") as HTMLElement;
    // jsdom has no layout: give the lanes the geometry a drag target reads.
    Array.from(view.container.querySelectorAll<HTMLElement>(".track-lane"))
      .forEach((lane, index) => {
        const top = 22 + index * 64;
        lane.getBoundingClientRect = () => ({
          left: 0, top, right: 2000, bottom: top + 64, width: 2000, height: 64,
          x: 0, y: top, toJSON: () => ({}),
        }) as DOMRect;
      });
    const clip = view.container.querySelector(".clip-block") as HTMLElement;
    return { ...view, panel, clip };
  }

  it("selects a clip on click and clears selection on empty lane click", async () => {
    installClipEditorMock();
    await store.initProject();
    const { container, clip } = await renderTimeline();

    fireEvent.click(clip);
    expect(clip.classList.contains("selected")).toBe(true);
    expect(store.selectedTrackIndex()).toBe(0);
    expect(store.selectedPatternName()).toBe("lead");
    expect(store.editorSelection()).toEqual({ trackIndex: 0, startBeat: 0, patternName: "lead" });

    const lane = container.querySelector(".track-lane") as HTMLElement;
    fireEvent.click(lane);
    expect(container.querySelector(".clip-block.selected")).toBeFalsy();
    expect(store.editorSelection()).toBe(null);
  });

  it("records the opened phrase's own start for the piano roll", async () => {
    const track = makeTrack("Lead", 0);
    track.phrases = [{
      name: "melody",
      time_range: { start: { bar: 5, beat: 1, subdivision: 0 }, end: { bar: 9, beat: 1, subdivision: 0 } },
      events: [makeNote()],
    }];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot({ tracks: [track] });
    });
    await store.initProject();
    const { container } = render(() => <Timeline />);
    const phrase = container.querySelector(".phrase-note-block") as HTMLElement;

    fireEvent.click(phrase);
    // Bar 5 in 4/4 is quarter beat 16.
    expect(store.editorSelection()).toEqual({ trackIndex: 0, startBeat: 16, patternName: null });
  });

  it("selects a clip on pointer down and focuses the timeline", async () => {
    installClipEditorMock();
    await store.initProject();
    const { panel, clip } = await renderTimeline();

    fireEvent.pointerDown(clip, { button: 0, clientX: 50, pointerId: 1 });
    fireEvent.pointerUp(document, { clientX: 50, pointerId: 1, button: 0 });

    expect(clip.classList.contains("selected")).toBe(true);
    expect(store.selectedTrackIndex()).toBe(0);
    expect(document.activeElement).toBe(panel);
  });

  it("moves a clip when dragged after selecting on pointer down", async () => {
    const mock = installClipEditorMock();
    await store.initProject();
    const { clip } = await renderTimeline();

    fireEvent.pointerDown(clip, { button: 0, clientX: 50, clientY: 32, pointerId: 1 });
    fireEvent.pointerMove(document, { clientX: 146, clientY: 32, pointerId: 1, buttons: 1 });
    fireEvent.pointerUp(document, { clientX: 146, clientY: 32, pointerId: 1, button: 0 });

    await vi.waitFor(() => expect(mock.commandCount("move_clip")).toBe(1));
    expect(getLastCallArgs("move_clip")).toMatchObject({
      trackIndex: 0, clipIndex: 0, startBar: 2, startBeat: 1, startSub: 0,
    });
    expect(clip.classList.contains("selected")).toBe(true);
  });

  it("copies and pastes when focus is outside the timeline", async () => {
    const mock = installClipEditorMock();
    await store.initProject();
    await store.seek(4);
    const { clip } = await renderTimeline();

    fireEvent.pointerDown(clip, { button: 0, clientX: 50, pointerId: 1 });
    (document.activeElement as HTMLElement).blur();

    fireEvent.keyDown(document.body, { key: "c", metaKey: true });
    fireEvent.keyDown(document.body, { key: "v", metaKey: true });

    await vi.waitFor(() => expect(mock.commandCount("add_clip")).toBe(1));
    expect(getLastCallArgs("add_clip")).toMatchObject({
      trackIndex: 0, patternName: "lead",
      startBar: 2, startBeat: 1, endBar: 4, endBeat: 1,
    });
  });

  it("ignores clip shortcuts while typing in an input", async () => {
    const mock = installClipEditorMock();
    await store.initProject();
    const { clip } = await renderTimeline();

    fireEvent.pointerDown(clip, { button: 0, clientX: 50, pointerId: 1 });
    fireEvent.pointerUp(document, { clientX: 50, pointerId: 1, button: 0 });

    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();
    fireEvent.keyDown(input, { key: "c", metaKey: true });
    fireEvent.keyDown(input, { key: "v", metaKey: true });
    fireEvent.keyDown(input, { key: "Delete" });
    await new Promise((resolve) => setTimeout(resolve, 20));

    expect(mock.commandCount("add_clip")).toBe(0);
    expect(mock.commandCount("remove_clip")).toBe(0);
    input.remove();
  });

  it("copies and pastes through the clip context menu without the outside handler closing it", async () => {
    const mock = installClipEditorMock();
    await store.initProject();
    await store.seek(4);
    const { clip } = await renderTimeline();

    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });
    const menu = document.querySelector(".clip-context-menu") as HTMLElement;
    expect(menu).toBeTruthy();
    // Pointer down on the menu itself must not dismiss it.
    fireEvent.pointerDown(menu);
    expect(document.querySelector(".clip-context-menu")).toBeTruthy();
    // Pointer down outside does dismiss it.
    fireEvent.pointerDown(document.body);
    expect(document.querySelector(".clip-context-menu")).toBeFalsy();

    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });
    fireEvent.click(menuItem("Copy")!);
    expect(document.querySelector(".clip-context-menu")).toBeFalsy();

    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });
    fireEvent.click(menuItem("Paste at playhead")!);

    await vi.waitFor(() => expect(mock.commandCount("add_clip")).toBe(1));
    expect(getLastCallArgs("add_clip")).toMatchObject({
      trackIndex: 0, patternName: "lead", startBar: 2, startBeat: 1, endBar: 4, endBeat: 1,
    });
  });

  it("copies and pastes a clip at the playhead preserving its duration", async () => {
    const mock = installClipEditorMock();
    await store.initProject();
    await store.seek(4);
    const { container, panel, clip } = await renderTimeline();

    fireEvent.click(clip);
    expect(clip.classList.contains("selected")).toBe(true);
    fireEvent.keyDown(panel, { key: "c", metaKey: true });
    fireEvent.keyDown(panel, { key: "v", metaKey: true });

    await vi.waitFor(() => expect(mock.commandCount("add_clip")).toBe(1));
    expect(getLastCallArgs("add_clip")).toMatchObject({
      trackIndex: 0, patternName: "lead",
      startBar: 2, startBeat: 1, startSub: 0,
      endBar: 4, endBeat: 1, endSub: 0,
    });
    await vi.waitFor(() => expect(container.querySelectorAll(".clip-block").length).toBe(2));
    await vi.waitFor(() => expect(container.querySelectorAll(".clip-block.selected").length).toBe(1));
  });

  it("duplicates the selected clip immediately after its end", async () => {
    const mock = installClipEditorMock();
    await store.initProject();
    const { panel, clip } = await renderTimeline();

    fireEvent.click(clip);
    fireEvent.keyDown(panel, { key: "d", ctrlKey: true });

    await vi.waitFor(() => expect(mock.commandCount("add_clip")).toBe(1));
    expect(getLastCallArgs("add_clip")).toMatchObject({
      trackIndex: 0, patternName: "lead",
      startBar: 3, startBeat: 1, startSub: 0,
      endBar: 5, endBeat: 1, endSub: 0,
    });
  });

  it("cuts the selected clip and pastes it at the playhead", async () => {
    const mock = installClipEditorMock();
    await store.initProject();
    const { panel, clip } = await renderTimeline();

    fireEvent.click(clip);
    fireEvent.keyDown(panel, { key: "x", metaKey: true });
    await vi.waitFor(() => expect(mock.commandCount("remove_clip")).toBe(1));
    await vi.waitFor(() => expect(mock.commandCount("add_clip")).toBe(0));

    await store.seek(4);
    fireEvent.keyDown(panel, { key: "v", metaKey: true });
    await vi.waitFor(() => expect(mock.commandCount("add_clip")).toBe(1));
    expect(getLastCallArgs("add_clip")).toMatchObject({
      patternName: "lead",
      startBar: 2, startBeat: 1, endBar: 4, endBeat: 1,
    });
  });

  it("deletes the selected clip with Delete", async () => {
    const mock = installClipEditorMock();
    await store.initProject();
    const { container, panel, clip } = await renderTimeline();

    fireEvent.click(clip);
    fireEvent.keyDown(panel, { key: "Delete" });

    await vi.waitFor(() => expect(mock.commandCount("remove_clip")).toBe(1));
    expect(getLastCallArgs("remove_clip")).toMatchObject({ trackIndex: 0, clipIndex: 0 });
    await vi.waitFor(() => expect(container.querySelector(".clip-block")).toBeFalsy());
  });

  it("opens a clip context menu without deleting and deletes only from the menu item", async () => {
    const mock = installClipEditorMock();
    await store.initProject();
    const { clip } = await renderTimeline();

    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });
    const menu = document.querySelector(".clip-context-menu") as HTMLElement;
    expect(menu).toBeTruthy();
    expect(menu.textContent).toContain("Open editor");
    expect(menu.textContent).toContain("Rename pattern");
    expect(menu.textContent).toContain("Clip content");
    expect(menu.textContent).toContain("Delete");
    expect(mock.commandCount("remove_blocks")).toBe(0);

    fireEvent.click(menuItem("Delete")!);
    // The menu deletes the clip selection in one command, like the keyboard.
    await vi.waitFor(() => expect(mock.commandCount("remove_blocks")).toBe(1));
  });
});

describe("Timeline cursor", () => {
  beforeEach(async () => {
    vi.useFakeTimers();
    invokeMock.mockClear();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "transport_stop") return TRANSPORT_STOPPED();
      return makeSnapshot({
        tracks: [makeTrack("Piano", 0)],
        patterns: [makePattern("beat")],
      });
    });
    await store.initProject();
    // Ensure clean state
    await store.stop();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("cursor is hidden when never played", () => {
    const { container } = render(() => <Timeline />);
    const cursor = container.querySelector(".playback-cursor");
    expect(cursor).toBeFalsy();
  });

  it("cursor appears after play", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "get_transport_state") return TRANSPORT_PLAYING();
      return makeSnapshot({ tracks: [makeTrack("Piano", 0)], patterns: [makePattern("beat")] });
    });

    const { container } = render(() => <Timeline />);
    await store.play();

    const cursor = container.querySelector(".playback-cursor");
    expect(cursor).toBeTruthy();
  });

  it("cursor position reflects transport position_beats", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_play") return {
        playing: true, position_beats: 4.0, position_seconds: 2.18, duration_seconds: 30, did_render: true, warnings: []
      };
      if (cmd === "get_transport_state") return {
        playing: true, position_beats: 4.0, position_seconds: 2.18, duration_seconds: 30
      };
      return makeSnapshot({ tracks: [makeTrack("Piano", 0)], patterns: [makePattern("beat")] });
    });

    const { container } = render(() => <Timeline />);
    await store.play();

    const cursor = container.querySelector(".playback-cursor") as HTMLElement;
    expect(cursor).toBeTruthy();
    // Default zoom is 24px/beat, so position 4.0 beats = 96px
    const left = parseFloat(cursor.style.left);
    expect(left).toBeCloseTo(4.0 * 24, 0); // 96px
  });

  it("cursor remains visible after pause (cursorActive stays true)", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "transport_pause") return {
        playing: false, position_beats: 4.5, position_seconds: 2.45, duration_seconds: 30
      };
      if (cmd === "get_transport_state") return {
        playing: false, position_beats: 4.5, position_seconds: 2.45, duration_seconds: 30
      };
      return makeSnapshot({ tracks: [makeTrack("Piano", 0)], patterns: [makePattern("beat")] });
    });

    const { container } = render(() => <Timeline />);
    await store.play();
    await store.pause();

    const cursor = container.querySelector(".playback-cursor") as HTMLElement;
    expect(cursor).toBeTruthy(); // still visible after pause
    const left = parseFloat(cursor.style.left);
    expect(left).toBeCloseTo(4.5 * 24, 0); // 108px
  });

  it("cursor disappears after explicit stop", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "transport_stop") return TRANSPORT_STOPPED();
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot({ tracks: [makeTrack("Piano", 0)], patterns: [makePattern("beat")] });
    });

    const { container } = render(() => <Timeline />);
    await store.play();

    // Cursor should be visible
    expect(container.querySelector(".playback-cursor")).toBeTruthy();

    await store.stop();

    // After stop, cursor should be hidden
    const cursor = container.querySelector(".playback-cursor");
    expect(cursor).toBeFalsy();
  });

  it("cursor head is rendered on ruler when cursor is visible", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "get_transport_state") return TRANSPORT_PLAYING();
      return makeSnapshot({ tracks: [makeTrack("Piano", 0)], patterns: [makePattern("beat")] });
    });

    const { container } = render(() => <Timeline />);
    await store.play();

    const cursorHead = container.querySelector(".cursor-head");
    expect(cursorHead).toBeTruthy();
  });
});

describe("Timeline document swap and clip keys", () => {
  const CLIP_RANGE = (startBar: number, endBar: number) => ({
    start: { bar: startBar, beat: 1, subdivision: 0 },
    end: { bar: endBar, beat: 1, subdivision: 0 },
  });

  /** One track with two named clips; `secondStartBar` is what an edit moves. */
  function projectWithClips(secondStartBar: number, revision: number) {
    const track = {
      ...makeTrack("Lead", 0),
      phrases: [],
      clips: [
        { pattern: "a", time_range: CLIP_RANGE(1, 3) },
        { pattern: "b", time_range: CLIP_RANGE(secondStartBar, secondStartBar + 2) },
      ],
    };
    const snapshot = makeSnapshot({
      tracks: [track],
      patterns: [makePattern("a"), makePattern("b")],
    });
    snapshot.revision = revision;
    return snapshot;
  }

  function installProject(snapshot: ReturnType<typeof makeSnapshot>) {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return snapshot;
    });
  }

  it("leaves Mod+C to a text selection outside the timeline", async () => {
    installProject(projectWithClips(5, 710));
    await store.initProject();
    const { container } = render(() => (
      <>
        <div class="ai-transcript" tabindex="0"><p>selected answer text</p></div>
        <Timeline />
      </>
    ));
    // A clip pick exists, so the timeline has something Mod+C could copy.
    const clip = container.querySelector(".clip-block") as HTMLElement;
    fireEvent.pointerDown(clip, { button: 0, pointerId: 11, clientX: 0, clientY: 30 });
    fireEvent.pointerUp(document, { pointerId: 11, clientX: 0, clientY: 30, button: 0 });
    expect(store.blockSelection().length).toBe(1);

    // Focus and select text in the transcript, which lives outside the panel.
    const transcript = container.querySelector(".ai-transcript") as HTMLElement;
    transcript.focus();
    const range = document.createRange();
    range.selectNodeContents(transcript);
    const selection = document.getSelection()!;
    selection.removeAllRanges();
    selection.addRange(range);
    expect(selection.toString()).toContain("selected answer text");

    // The timeline must not claim the key: the webview copies the selection.
    expect(fireEvent.keyDown(transcript, { key: "c", metaKey: true })).toBe(true);
    expect(store.clipClipboard()).toBeNull();
    expect(store.blockSelection().length).toBe(1);

    // Cut would remove the clip; it must pass through as well.
    expect(fireEvent.keyDown(transcript, { key: "x", metaKey: true })).toBe(true);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "remove_clip")).toBe(false);
    selection.removeAllRanges();
  });
});
