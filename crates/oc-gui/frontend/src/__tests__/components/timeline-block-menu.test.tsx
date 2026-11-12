/**
 * Every lane block kind answers a right-click with its own menu: named pattern
 * clips, inline phrases, inline drum patterns and audio clips. Empty lane area
 * keeps the track menu, and a secondary press never selects or drags a block.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import {
  TRANSPORT_STOPPED,
  getLastCallArgs,
  invokeMock,
  makeAudioClip,
  makeNote,
  makePattern,
  makeSnapshot,
  makeTrack,
} from "../setup";
import { store } from "../../stores/project";
import { panelStore } from "../../stores/panels";
import { i18n } from "../../stores/i18n";
import { Timeline } from "../../components/Timeline";
import { closeContextMenu, ContextMenuLayer } from "../../components/ContextMenu";
import { GainEnvelopeWindow } from "../../components/GainEnvelopeWindow";

function range(start: [number, number, number], end: [number, number, number]) {
  return {
    start: { bar: start[0], beat: start[1], subdivision: start[2] },
    end: { bar: end[0], beat: end[1], subdivision: end[2] },
  };
}

function snapshot() {
  const track: any = {
    ...makeTrack("Drums", 0),
    clips: [{ pattern: "beat", time_range: range([1, 1, 0], [3, 1, 0]) }],
    phrases: [{ name: "melody", time_range: range([1, 1, 0], [5, 1, 0]), events: [makeNote()] }],
    patterns: [{
      name: "main_beat",
      time_range: range([1, 1, 0], [2, 1, 0]),
      repeat: 4,
      hits: [{ drum: "kick", position: { beat: 1, subdivision: 0 }, dynamics: "MF", articulation: null, modifiers: [] }],
    }],
  };
  return makeSnapshot({
    patterns: [makePattern("beat")],
    tracks: [track],
    properties: {
      tempo: 120,
      time_signature: { numerator: 4, denominator: 4 },
      key: null,
      swing: null,
      tempo_map: [],
      time_sig_map: [],
      key_map: [],
      audio_clips: [makeAudioClip({ track: "Drums" })],
    } as any,
  });
}

async function mount(prepare?: (project: ReturnType<typeof snapshot>) => void) {
  const project = snapshot();
  prepare?.(project);
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "audio_clip_peaks") return "";
    if (cmd === "preview_block") return [];
    return project;
  });
  await store.initProject();
  const view = render(() => <><Timeline /><GainEnvelopeWindow /><ContextMenuLayer /></>);
  const container = view.container as HTMLElement;
  return {
    ...view,
    clip: container.querySelector(".clip-block:not(.audio-clip-block)") as HTMLElement,
    phrase: container.querySelector(".phrase-note-block") as HTMLElement,
    pattern: container.querySelector(".pattern-block") as HTMLElement,
    audio: container.querySelector(".audio-clip-block") as HTMLElement,
    lane: container.querySelector(".track-lane") as HTMLElement,
  };
}

function menuLabels(): string[] {
  return [...document.querySelectorAll(".clip-context-menu .context-menu-item .context-menu-label")]
    .map((label) => label.textContent ?? "");
}

function menuItem(label: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll<HTMLButtonElement>(".clip-context-menu .context-menu-item")]
    .find((item) => item.querySelector(".context-menu-label")?.textContent === label);
}

/** Open a menu group before reading the rows inside it. */
function openGroup(label: string): void {
  const group = [...document.querySelectorAll<HTMLElement>(".clip-context-menu .context-menu-group")]
    .find((item) => item.querySelector(".context-menu-label")?.textContent === label);
  if (group && group.getAttribute("aria-expanded") !== "true") fireEvent.click(group);
}

function commandCount(name: string): number {
  return invokeMock.mock.calls.filter(([cmd]) => cmd === name).length;
}

const ORIGINAL_USER_AGENT = navigator.userAgent;

beforeEach(async () => {
  invokeMock.mockClear();
  closeContextMenu();
  panelStore.closeBottom();
  i18n.setLocale("en");
  await store.initProject();
});

afterEach(() => {
  closeContextMenu();
  vi.useRealTimers();
  Object.defineProperty(navigator, "userAgent", { value: ORIGINAL_USER_AGENT, configurable: true });
});

describe("lane block context menus", () => {
  it("opens the full clip menu on a named pattern clip", async () => {
    const { clip } = await mount();
    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });

    const menu = document.querySelector(".clip-context-menu") as HTMLElement;
    expect(menu).toBeTruthy();
    expect(menu.querySelector(".context-menu-heading")?.textContent).toBe("beat");
    expect(menuLabels()).toEqual([
      "Inspect clip",
      "Preview clip",
      "Open editor",
      "Rename pattern",
      "Edit",
      "Clip content",
      "Selection",
      "Delete",
    ]);
    // Acting on the clip must never fall through to the lane's track menu.
    expect(document.querySelector(".track-context-menu")).toBeNull();
    // A right-click never selects the block.
    expect(clip.classList.contains("selected")).toBe(false);
    expect(store.editorSelection()).toBeNull();
  });

  it("double-clicks a pattern clip into the piano roll, not the inspector", async () => {
    const { clip } = await mount();
    fireEvent.click(clip, { detail: 2 });
    expect(panelStore.bottomMode()).toBe("pianoroll");
    expect(panelStore.bottomPanel()).toBe("pianoroll");
  });

  it("double-clicks an inline pattern block into the piano roll", async () => {
    const { pattern } = await mount();
    fireEvent.click(pattern, { detail: 2 });
    expect(panelStore.bottomMode()).toBe("pianoroll");
  });

  it("opens a menu on an inline phrase block", async () => {
    const { phrase } = await mount();
    fireEvent.contextMenu(phrase, { clientX: 120, clientY: 90 });

    const menu = document.querySelector(".clip-context-menu") as HTMLElement;
    expect(menu).toBeTruthy();
    expect(menu.querySelector(".context-menu-heading")?.textContent).toBe("melody");
    expect(menuLabels()).toEqual(["Inspect clip", "Preview clip", "Open editor", "Copy", "Paste at playhead"]);
    expect(phrase.classList.contains("selected")).toBe(false);
    expect(document.querySelector(".track-context-menu")).toBeNull();
  });

  it("inspects an inline phrase with placement and sound source", async () => {
    const { phrase } = await mount();
    fireEvent.contextMenu(phrase, { clientX: 120, clientY: 90 });
    // The menu is actions only: the read-only rows live in the hover tooltip.
    expect(document.querySelector(".clip-context-menu .context-menu-info")).toBeNull();
    const inspect = [...document.querySelectorAll(".context-menu-item")].find(item => item.textContent === "Inspect clip")!;
    fireEvent.click(inspect);
    expect(panelStore.bottomMode()).toBe("detail");
    expect(store.editorSelection()?.trackIndex).toBe(0);
  });

  it("opens a menu on an inline drum pattern block", async () => {
    const { pattern } = await mount();
    fireEvent.contextMenu(pattern, { clientX: 120, clientY: 90 });

    const menu = document.querySelector(".clip-context-menu") as HTMLElement;
    expect(menu).toBeTruthy();
    expect(menu.querySelector(".context-menu-heading")?.textContent).toBe("main_beat");
    expect(menuLabels()).toEqual(["Inspect clip", "Preview clip", "Open editor", "Copy", "Paste at playhead"]);
    expect(pattern.classList.contains("selected")).toBe(false);
  });

  it("opens the audio clip menu with its own actions", async () => {
    const { audio } = await mount();
    fireEvent.contextMenu(audio, { clientX: 120, clientY: 90 });

    const menu = document.querySelector(".clip-context-menu") as HTMLElement;
    expect(menu).toBeTruthy();
    expect(menu.querySelector(".context-menu-heading")?.textContent).toBe("hit");
    // The slip item is inert until the clip hides part of its media.
    expect(menuLabels()).toEqual([
      "Inspect clip",
      "Preview clip",
      "Speed and pitch…",
      "Add gain envelope",
      "Clip content",
      "Delete",
    ]);
    expect(audio.classList.contains("selected")).toBe(false);
  });

  it("previews only the named audio clip when its track and range overlap", async () => {
    const { container } = await mount((project) => {
      const original = project.project.properties.audio_clips![0];
      project.project.properties.audio_clips!.push({
        ...original, name: "other hit", file: "media/other.wav",
      });
    });
    const clips = [...container.querySelectorAll<HTMLElement>(".audio-clip-block")];
    const target = clips.find((clip) => clip.getAttribute("aria-label") === "Audio clip other hit")!;
    fireEvent.contextMenu(target, { clientX: 120, clientY: 90 });
    const preview = menuItem("Preview clip")!;
    expect(preview.getAttribute("aria-disabled")).not.toBe("true");
    fireEvent.click(preview);

    await vi.waitFor(() => expect(getLastCallArgs("preview_block")).toMatchObject({
      trackIndex: 0,
      start: { bar: 1, beat: 1, subdivision: 0 },
      end: { bar: 3, beat: 1, subdivision: 0 },
      startPaused: true,
      audioClipName: "other hit",
    }));
    expect(target.classList.contains("selected")).toBe(false);
    await store.stopAudition();
  });

  it("explains why a muted audio clip cannot preview", async () => {
    const { audio } = await mount((project) => {
      project.project.properties.audio_clips![0].muted = true;
    });
    fireEvent.contextMenu(audio, { clientX: 120, clientY: 90 });
    const preview = menuItem("Preview clip")!;
    expect(preview.getAttribute("aria-disabled")).toBe("true");
    expect(preview.querySelector(".context-menu-hint")?.textContent).toBe("Clip is muted");
    fireEvent.click(preview);
    expect(getLastCallArgs("preview_block")).toBeUndefined();
  });

  it("previews a clip over the clip's own range", async () => {
    const { clip } = await mount();
    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });
    menuItem("Preview clip")!.click();

    await vi.waitFor(() => expect(getLastCallArgs("preview_block")).toMatchObject({
      trackIndex: 0,
      start: { bar: 1, beat: 1, subdivision: 0 },
      end: { bar: 3, beat: 1, subdivision: 0 },
    }));
    await store.stopAudition();
  });

  it("previews an inline phrase over the phrase's own range", async () => {
    const { phrase } = await mount();
    fireEvent.contextMenu(phrase, { clientX: 120, clientY: 90 });
    menuItem("Preview clip")!.click();

    await vi.waitFor(() => expect(getLastCallArgs("preview_block")).toMatchObject({
      trackIndex: 0,
      start: { bar: 1, beat: 1, subdivision: 0 },
      end: { bar: 5, beat: 1, subdivision: 0 },
    }));
    await store.stopAudition();
  });

  it("previews an inline pattern over its whole repeat span", async () => {
    const { pattern } = await mount();
    fireEvent.contextMenu(pattern, { clientX: 120, clientY: 90 });
    menuItem("Preview clip")!.click();

    // One bar defined, repeated four times: the block covers bars 1 to 5.
    await vi.waitFor(() => expect(getLastCallArgs("preview_block")).toMatchObject({
      trackIndex: 0,
      start: { bar: 1, beat: 1, subdivision: 0 },
      end: { bar: 5, beat: 1, subdivision: 0 },
    }));
    await store.stopAudition();
  });

  it("disables preview with a reason on a muted track", async () => {
    const { clip } = await mount((project) => {
      project.project.tracks[0].mix.mute = true;
    });
    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });

    const item = menuItem("Preview clip")!;
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(item.querySelector(".context-menu-hint")?.textContent).toBe("Track is muted");
    item.click();
    await vi.waitFor(() => expect(commandCount("preview_block")).toBe(0));
  });

  it("disables preview with a reason when the clip has nothing to sound", async () => {
    const { clip } = await mount((project) => {
      project.project.patterns = [{ ...makePattern("beat"), hits: [] }];
    });
    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });

    const item = menuItem("Preview clip")!;
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(item.querySelector(".context-menu-hint")?.textContent).toBe("Nothing to preview");
    item.click();
    await vi.waitFor(() => expect(commandCount("preview_block")).toBe(0));
  });

  it("disables preview with a reason for an empty pattern block", async () => {
    const { pattern } = await mount((project) => {
      project.project.tracks[0].patterns[0].hits = [];
    });
    fireEvent.contextMenu(pattern, { clientX: 120, clientY: 90 });

    const item = menuItem("Preview clip")!;
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(item.querySelector(".context-menu-hint")?.textContent).toBe("Nothing to preview");
  });

  it("keeps the track menu on empty lane area", async () => {
    const { lane } = await mount();
    fireEvent.contextMenu(lane, { clientX: 320, clientY: 20 });

    expect(document.querySelector(".track-context-menu")).toBeTruthy();
    expect(document.querySelector(".clip-context-menu")).toBeNull();
  });

  it("opens the phrase editor from its menu", async () => {
    const { phrase } = await mount();
    fireEvent.contextMenu(phrase, { clientX: 120, clientY: 90 });
    menuItem("Open editor")!.click();

    expect(store.editorSelection()).toMatchObject({ trackIndex: 0, startBeat: 0, patternName: null });
    expect(panelStore.bottomPanel()).toBe("pianoroll");
  });

  it("copies and pastes an inline phrase from its menu", async () => {
    const { phrase } = await mount();
    fireEvent.contextMenu(phrase, { clientX: 120, clientY: 90 });
    menuItem("Copy")!.click();

    fireEvent.contextMenu(phrase, { clientX: 120, clientY: 90 });
    menuItem("Paste at playhead")!.click();

    await vi.waitFor(() => expect(commandCount("insert_phrase")).toBe(1));
    const args = getLastCallArgs("insert_phrase") as any;
    expect(args).toMatchObject({ trackIndex: 0, startBar: 1, startBeat: 1, startSub: 0 });
    expect(args.phrase.name).toBe("melody");
  });

  it("never selects or drags a block on a macOS ctrl+click", async () => {
    Object.defineProperty(navigator, "userAgent", {
      value: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15",
      configurable: true,
    });
    const { clip } = await mount();

    // macOS reports a secondary click as the primary button plus ctrlKey.
    fireEvent.pointerDown(clip, { button: 0, ctrlKey: true, pointerId: 1, clientX: 10, clientY: 10 });
    fireEvent.contextMenu(clip, { button: 0, ctrlKey: true, clientX: 10, clientY: 10 });

    expect(document.querySelector(".clip-context-menu")).toBeTruthy();
    expect(clip.classList.contains("selected")).toBe(false);
    expect(store.editorSelection()).toBeNull();

    fireEvent.pointerMove(document, { pointerId: 1, buttons: 1, ctrlKey: true, clientX: 120, clientY: 10 });
    fireEvent.pointerUp(document, { pointerId: 1, button: 0, clientX: 120, clientY: 10 });
    expect(commandCount("move_clip")).toBe(0);
  });

  it("never selects or trims an audio clip on a macOS ctrl+click", async () => {
    Object.defineProperty(navigator, "userAgent", {
      value: "Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/605.1.15",
      configurable: true,
    });
    const { audio } = await mount();

    fireEvent.pointerDown(audio, { button: 0, ctrlKey: true, pointerId: 1, clientX: 10, clientY: 10 });
    fireEvent.contextMenu(audio, { button: 0, ctrlKey: true, clientX: 10, clientY: 10 });

    expect(document.querySelector(".clip-context-menu")).toBeTruthy();
    expect(audio.classList.contains("selected")).toBe(false);
    fireEvent.pointerMove(document, { pointerId: 1, buttons: 1, ctrlKey: true, clientX: 120, clientY: 10 });
    fireEvent.pointerUp(document, { pointerId: 1, button: 0, clientX: 120, clientY: 10 });
    expect(commandCount("trim_audio_clip")).toBe(0);
    expect(commandCount("move_audio_clip")).toBe(0);
  });

  it("never drags a block on a secondary-button press", async () => {
    const { clip } = await mount();

    fireEvent.pointerDown(clip, { button: 2, buttons: 2, pointerId: 1, clientX: 10, clientY: 10 });
    fireEvent.pointerMove(document, { pointerId: 1, buttons: 2, clientX: 120, clientY: 10 });
    fireEvent.pointerUp(document, { pointerId: 1, button: 2, clientX: 120, clientY: 10 });
    fireEvent.contextMenu(clip, { button: 2, clientX: 10, clientY: 10 });

    expect(document.querySelector(".clip-context-menu")).toBeTruthy();
    expect(clip.classList.contains("selected")).toBe(false);
    expect(commandCount("move_clip")).toBe(0);
  });

  it("shows Edit when the clip already has a gain envelope", async () => {
    const { audio } = await mount((project) => {
      project.project.properties.audio_clips![0].gain_envelope = [
        { position: 0, gain_db: 0 },
        { position: 1, gain_db: 0 },
      ];
    });
    fireEvent.contextMenu(audio, { clientX: 120, clientY: 90 });
    expect(menuItem("Edit gain envelope")).toBeTruthy();
  });

  it("offers clip gain and opens the gain envelope window", async () => {
    const { audio } = await mount();

    fireEvent.contextMenu(audio, { clientX: 120, clientY: 90 });
    const slider = document.querySelector<HTMLInputElement>(".clip-context-menu .context-menu-slider input");
    expect(slider).toBeTruthy();
    fireEvent.input(slider!, { target: { value: "-6" } });
    fireEvent.change(slider!, { target: { value: "-6" } });
    await vi.waitFor(() =>
      expect(getLastCallArgs("set_audio_clip_envelope")).toMatchObject({ gainDb: -6 }),
    );

    fireEvent.contextMenu(audio, { clientX: 120, clientY: 90 });
    const add = menuItem("Add gain envelope");
    expect(add).toBeTruthy();
    fireEvent.click(add!);
    await vi.waitFor(() => expect(document.querySelector(".gain-envelope-window")).toBeTruthy());
  });

  it("does not reopen the gain editor if the project changes while adding an envelope", async () => {
    const { audio } = await mount();
    const original = snapshot();
    original.revision = store.projectRevision;
    const replacement = snapshot();
    replacement.revision = original.revision + 1;
    replacement.file_path = "/another/project.ocp";
    let completeEdit!: (value: typeof original) => void;
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "set_audio_clip_envelope") return new Promise<typeof original>((resolve) => { completeEdit = resolve; });
      if (command === "get_transport_state") return TRANSPORT_STOPPED();
      return replacement;
    });

    fireEvent.contextMenu(audio, { clientX: 120, clientY: 90 });
    fireEvent.click(menuItem("Add gain envelope")!);
    expect(completeEdit).toBeTypeOf("function");
    await store.openFile(replacement.file_path);
    completeEdit(original);
    await vi.waitFor(() => expect(commandCount("end_undo_gesture")).toBe(1));

    expect(document.querySelector(".gain-envelope-window")).toBeNull();
  });
});

describe("named clip loop", () => {
  it("loops a named clip from its menu, once", async () => {
    const { clip } = await mount();
    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });
    openGroup("Clip content");
    const item = menuItem("Repeat to fill")!;
    expect(item).toBeTruthy();
    expect(item.getAttribute("aria-disabled")).not.toBe("true");
    fireEvent.click(item);

    await vi.waitFor(() => expect(commandCount("set_pattern_clip_loop")).toBe(1));
    // The pattern spans two beats and the clip eight, so two beats is the loop.
    expect(getLastCallArgs("set_pattern_clip_loop")).toMatchObject({
      trackIndex: 0, clipIndex: 0, loopQuarters: 2,
    });
  });

  it("uses the complete declared cycle even when its tail is silent", async () => {
    const { clip } = await mount((snapshot) => {
      const pattern = snapshot.project.patterns[0];
      pattern.bars = 1.5;
      pattern.events = [];
      pattern.hits = [1, 4, 6].map((beat) => ({
        drum: "tom_low",
        position: { beat, subdivision: 2 },
        dynamics: "MF",
        articulation: null,
        modifiers: [],
      }));
    });
    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });
    openGroup("Clip content");
    const item = menuItem("Repeat to fill")!;
    expect(item.getAttribute("aria-disabled")).not.toBe("true");
    fireEvent.click(item);

    await vi.waitFor(() => expect(commandCount("set_pattern_clip_loop")).toBe(1));
    expect(getLastCallArgs("set_pattern_clip_loop")).toMatchObject({
      trackIndex: 0, clipIndex: 0, loopQuarters: 6,
    });
  });

  it("draws a seam per repeat and offers Stop repeating", async () => {
    const { clip } = await mount((snapshot) => {
      snapshot.project.tracks[0].clips[0].loop_quarters = 2;
    });
    expect(clip.querySelectorAll(".clip-loop-seam").length).toBe(3);
    expect(clip.textContent).toContain("loop 2 q");

    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });
    openGroup("Clip content");
    const clear = menuItem("Stop repeating")!;
    expect(clear).toBeTruthy();
    expect(menuItem("Repeat to fill")).toBeUndefined();
    fireEvent.click(clear);

    await vi.waitFor(() => expect(commandCount("set_pattern_clip_loop")).toBe(1));
    expect(getLastCallArgs("set_pattern_clip_loop")!.loopQuarters).toBeNull();
  });

  it("refuses a clip whose pattern has no room to repeat", async () => {
    const { clip } = await mount((snapshot) => {
      const project = snapshot.project;
      // A 16-beat pattern in an eight-beat clip spans the whole block.
      project.patterns[0] = {
        ...project.patterns[0],
        hits: [
          { drum: "kick", position: { beat: 1, subdivision: 0 }, dynamics: "MF", articulation: null, modifiers: [] },
          { drum: "snare", position: { beat: 4, subdivision: 3 }, dynamics: "MF", articulation: null, modifiers: [] },
        ],
        events: [],
      };
      project.tracks[0].clips[0] = { ...project.tracks[0].clips[0], offset_quarters: 8 };
    });
    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });
    openGroup("Clip content");
    const item = menuItem("Repeat to fill")!;
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(item.querySelector(".context-menu-hint")?.textContent).toBe(
      "The loop must end inside the pattern's content.",
    );
  });
});

describe("F2 rename", () => {
  it("renames the selected clip's pattern, not an older pick", async () => {
    const { clip } = await mount();
    const { store } = await import("../../stores/project");
    const { RENAME_SELECTION_EVENT } = await import("../../stores/appEvents");
    // Select the block the way a click does, then raise the command.
    store.setBlockSelection([{ kind: "clip", trackIndex: 0, index: 0 }]);
    window.dispatchEvent(new Event(RENAME_SELECTION_EVENT));
    const prompt = document.querySelector<HTMLInputElement>(".text-prompt input");
    expect(prompt).toBeTruthy();
    expect(prompt!.value).toBe("beat");
    expect(clip).toBeTruthy();
  });
});

describe("colour groups", () => {
  it("picks a track colour from the palette row", async () => {
    const { container } = await mount();
    const { store } = await import("../../stores/project");
    const { openContextMenu } = await import("../../components/ContextMenu");
    const { openTrackContextMenu } = await import("../../components/trackMenu");
    openContextMenu({ x: 10, y: 10, items: [] });
    openTrackContextMenu({
      trackIndex: 0,
      onRename: () => {},
      onCreatePattern: () => {},
    } as never);
    const palette = document.querySelector(".context-menu-palette");
    expect(palette).toBeTruthy();
    const swatch = [...document.querySelectorAll<HTMLElement>(".context-menu-chip")]
      .find((item) => item.getAttribute("aria-label") === "Mint")!;
    fireEvent.click(swatch);
    await waitFor(() => {
      expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_track_channel_overrides")).toBe(true);
    });
    expect(store.project.tracks[0]).toBeTruthy();
    expect(container).toBeTruthy();
  });

  it("picks a clip colour from the palette row", async () => {
    const { clip } = await mount();
    fireEvent.contextMenu(clip, { clientX: 120, clientY: 90 });
    const swatch = [...document.querySelectorAll<HTMLElement>(".context-menu-chip")]
      .find((item) => item.getAttribute("aria-label") === "Pink");
    expect(swatch).toBeTruthy();
    fireEvent.click(swatch!);
    await waitFor(() => {
      expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_pattern_color")).toBe(true);
    });
  });
});
