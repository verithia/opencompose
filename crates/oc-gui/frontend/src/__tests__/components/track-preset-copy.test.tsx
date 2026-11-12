/**
 * A duplicated track must own its preset, and "save patch as new preset" must
 * fork the patch for one track only.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import {
  TRANSPORT_STOPPED,
  getLastCallArgs,
  invokeMock,
  makeSnapshot,
  makeTrack,
  makePattern,
  askDialogMock,
} from "../setup";
import { store } from "../../stores/project";
import type { ParamValue } from "../../types/project";
import { Timeline } from "../../components/Timeline";
import { openTrackContextMenu } from "../../components/trackMenu";
import { ContextMenuLayer } from "../../components/ContextMenu";

const PARAMS = [
  ["filter.cutoff", { Frequency: 4200 }],
  ["osc.a.level", { Number: 0.5 }],
] as [string, ParamValue][];

function sourceSnapshot() {
  return makeSnapshot({
    presets: [{
      name: "Piano 1",
      source: "builtin",
      plugin: "builtin.open_synth",
      program: null,
      patch: "Glass Pad",
      file: null,
      root_note: null,
      extends: null,
      params: PARAMS,
      effects: [],
    }],
    tracks: [makeTrack("Piano", 0), makeTrack("Drums", 1)],
  });
}

/** What the backend returns: a second track plus its own preset block. */
function duplicatedSnapshot() {
  const snap = sourceSnapshot();
  const original = snap.project.presets[0];
  snap.project.presets = [...snap.project.presets, { ...original, name: "Piano 1 copy" }];
  snap.project.tracks = [
    ...snap.project.tracks,
    { ...makeTrack("Piano 2", 0), preset: "Piano 1 copy" },
  ];
  return snap;
}

/** Save-as-new returns the same track, moved to the forked preset. */
function forkedSnapshot() {
  const snap = sourceSnapshot();
  const original = snap.project.presets[0];
  snap.project.presets = [...snap.project.presets, { ...original, name: "Piano 1 copy" }];
  snap.project.tracks[0] = { ...snap.project.tracks[0], preset: "Piano 1 copy" };
  return snap;
}

function menuItem(label: string): HTMLButtonElement | undefined {
  return [...document.querySelectorAll<HTMLButtonElement>(".context-menu-item")]
    .find((item) => item.querySelector(".context-menu-label")?.textContent === label);
}

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "get_undo_history") return [];
    if (cmd === "duplicate_track") return duplicatedSnapshot();
    if (cmd === "save_track_preset_as_new") return forkedSnapshot();
    return sourceSnapshot();
  });
  await store.initProject();
});

describe("duplicating a track", () => {
  it("issues one command and applies the copy's own preset", async () => {
    const index = await store.duplicateTrack(0);

    expect(index).toBe(2);
    expect(getLastCallArgs("duplicate_track")).toEqual({ trackIndex: 0, name: "Piano 2" });
    const commands = invokeMock.mock.calls.map(([cmd]) => cmd);
    expect(commands).not.toContain("add_track");
    expect(commands).not.toContain("set_track_preset");

    expect(store.project.tracks).toHaveLength(3);
    expect(store.project.tracks[0].preset).toBe("Piano 1");
    expect(store.project.tracks[2].preset).toBe("Piano 1 copy");
    const original = store.project.presets.find((preset) => preset.name === "Piano 1")!;
    const copy = store.project.presets.find((preset) => preset.name === "Piano 1 copy")!;
    expect(copy.name).not.toBe(original.name);
    expect(copy.params).toEqual(original.params);
    expect(store.selectedTrackIndex()).toBe(2);
  });

  it("duplicates a track without a preset through the same command", async () => {
    const index = await store.duplicateTrack(1);

    expect(index).toBe(2);
    expect(getLastCallArgs("duplicate_track")).toEqual({ trackIndex: 1, name: "Drums 2" });
    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).not.toContain("set_track_preset");
  });
});

describe("save patch as new preset", () => {
  it("is in the track menu and forks only that track's preset", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const header = container.querySelector(".track-header") as HTMLElement;

    fireEvent.contextMenu(header, { clientX: 100, clientY: 60 });
    const item = menuItem("Save patch as new preset");
    expect(item).toBeTruthy();
    expect(item!.getAttribute("aria-disabled")).toBeNull();

    fireEvent.click(item!);
    await vi.waitFor(() => expect(getLastCallArgs("save_track_preset_as_new")).toEqual({ trackIndex: 0 }));
    await vi.waitFor(() => expect(store.project.tracks[0].preset).toBe("Piano 1 copy"));
    expect(store.project.tracks[1].preset).toBeNull();
  });

  it("is unavailable on a track that has no patch", async () => {
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);
    const headers = container.querySelectorAll(".track-header");
    fireEvent.contextMenu(headers[1] as HTMLElement, { clientX: 100, clientY: 60 });

    const item = menuItem("Save patch as new preset");
    expect(item).toBeTruthy();
    expect(item!.getAttribute("aria-disabled")).toBe("true");
    fireEvent.click(item!);
    expect(getLastCallArgs("save_track_preset_as_new")).toBeUndefined();
  });
});

describe("track action scope", () => {
  it("names append and sound-only copying without implying selected-track shortcuts", () => {
    render(() => <ContextMenuLayer />);
    openTrackContextMenu({ x: 20, y: 20, trackIndex: 0, onRename: () => {}, includeAddTrack: true });
    // Adding a track sits in the Track tools group.
    const group = [...document.querySelectorAll<HTMLElement>(".context-menu-group")]
      .find((item) => item.querySelector(".context-menu-label")?.textContent === "Track tools")!;
    fireEvent.click(group);
    expect(menuItem("Add track at end")).toBeTruthy();
    const copy = menuItem("New track with this sound");
    expect(copy).toBeTruthy();
    expect(copy!.querySelector(".context-menu-shortcut")).toBeNull();
    expect(menuItem("Delete track")!.querySelector(".context-menu-shortcut")).toBeNull();
  });
});

describe("deleting a track that holds clips", () => {
  it("asks before it takes the clips with it", async () => {
    const track = makeTrack("Piano", 0);
    track.clips = [{
      pattern: "lead",
      time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 3, beat: 1, subdivision: 0 } },
    }];
    invokeMock.mockImplementation(async (cmd: string) =>
      cmd === "scan_plugins" ? [] : makeSnapshot({ tracks: [track], patterns: [makePattern("lead")] }));
    await store.initProject();
    const { container } = render(() => <><Timeline /><ContextMenuLayer /></>);

    fireEvent.contextMenu(container.querySelector(".track-lane") as HTMLElement, { clientX: 10, clientY: 10 });
    const item = [...document.querySelectorAll<HTMLElement>(".context-menu-item")]
      .find((entry) => entry.querySelector(".context-menu-label")?.textContent === "Delete track")!;
    askDialogMock.mockResolvedValueOnce(false);
    fireEvent.click(item);
    await new Promise((resolve) => setTimeout(resolve, 10));
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "remove_track")).toBe(false);
    expect(askDialogMock).toHaveBeenCalled();
  });
});

describe("track tools group", () => {
  it("keeps MIDI, add-track and mixer behind one group", () => {
    render(() => <ContextMenuLayer />);
    openTrackContextMenu({
      x: 10, y: 10, trackIndex: 0, onRename: () => {},
      includeMidi: true, includeAddTrack: true, includeOpenMixer: true,
    });
    const labels = () => [...document.querySelectorAll(".context-menu-label")]
      .map((node) => node.textContent);
    expect(labels()).toContain("Track tools");
    expect(labels()).not.toContain("Import MIDI…");
    const group = [...document.querySelectorAll<HTMLElement>(".context-menu-group")]
      .find((item) => item.querySelector(".context-menu-label")?.textContent === "Track tools")!;
    fireEvent.click(group);
    expect(labels()).toContain("Import MIDI…");
    expect(labels()).toContain("Open mixer");
  });
});
