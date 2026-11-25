import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { expect, it, vi } from "vitest";
import { closeContextMenu, ContextMenuLayer } from "../../components/ContextMenu";
import { panelStore } from "../../stores/panels";
import { gainAutomationUi } from "../../stores/gainAutomationUi";
import { Timeline } from "../../components/Timeline";
import { i18n } from "../../stores/i18n";
import { store } from "../../stores/project";
import type { AutomationLane } from "../../types/project";
import { getLastCallArgs, invokeMock, makeAudioClip, makeNote, makeSnapshot, makeTrack, TRANSPORT_STOPPED } from "../setup";

it("groups real audio clips by playlist row without changing their mix owners", async () => {
  const snapshot = makeSnapshot({ tracks: [makeTrack("Mixer A", 0), makeTrack("Mixer B", 1)] });
  snapshot.project.properties.audio_clips = [
    makeAudioClip({ name: "Later row", track: "Mixer A", playlist_lane: 8 }),
    makeAudioClip({ name: "First source", track: "Mixer A", playlist_lane: 2 }),
    makeAudioClip({ name: "Second source", track: "Mixer B", playlist_lane: 2 }),
    makeAudioClip({ name: "New audio", track: "Mixer B" }),
  ];
  const fallback = invokeMock.getMockImplementation();
  invokeMock.mockImplementation(async (cmd, args) => {
    if (cmd === "get_project") return snapshot;
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "audio_clip_peaks") return "";
    return fallback?.(cmd, args);
  });
  try {
    i18n.setLocale("en");
    await store.initProject();
    const view = render(() => <Timeline />);
    const toggle = view.getByRole("button", { name: "Playlist" });
    expect(toggle.getAttribute("aria-pressed")).toBe("true");
    const rows = [...view.container.querySelectorAll<HTMLElement>("[data-playlist-lane]")];
    expect(rows.map((row) => row.dataset.playlistLane)).toEqual(["2", "8", "unassigned"]);
    expect(rows[0].querySelectorAll(".audio-clip-block")).toHaveLength(2);
    const second = within(rows[0]).getByRole("button", {
      name: i18n.t("clip.audioLabel", { name: "Second source" }),
    });
    fireEvent.click(second);
    expect(store.selectedTrackIndex()).toBe(1);
    expect(store.project.properties.audio_clips?.map((clip) => clip.track))
      .toEqual(["Mixer A", "Mixer A", "Mixer B", "Mixer B"]);
    vi.spyOn(rows[0], "getBoundingClientRect").mockReturnValue(new DOMRect(0, 20, 1000, 50));
    fireEvent.pointerDown(second, { button: 0, pointerId: 1, clientX: 4, clientY: 32 });
    fireEvent.pointerMove(document, { pointerId: 1, clientX: 130, clientY: 95 });
    fireEvent.pointerUp(document, { pointerId: 1, clientX: 130, clientY: 95 });
    expect(getLastCallArgs("move_audio_clip")).toBeUndefined();
    fireEvent.pointerDown(second, { button: 0, pointerId: 2, clientX: 4, clientY: 32 });
    fireEvent.pointerMove(document, { pointerId: 2, clientX: 130, clientY: 32 });
    fireEvent.pointerUp(document, { pointerId: 2, clientX: 130, clientY: 32 });
    await waitFor(() => expect(getLastCallArgs("move_audio_clip"))
      .toMatchObject({ clipIndex: 2, track: "Mixer B" }));
    // The drag consumes the synthetic click in the same event turn.
    await new Promise((resolve) => setTimeout(resolve, 0));
    fireEvent.click(toggle);
    expect(view.container.querySelector("[data-playlist-lane]")).toBeNull();
    fireEvent.click(toggle);
    expect(view.container.querySelectorAll("[data-playlist-lane]")).toHaveLength(3);
  } finally {
    if (fallback) invokeMock.mockImplementation(fallback);
  }
});

it("opens one original instrument part from a grouped playlist phrase", async () => {
  const tracks = [makeTrack("Pluck part", 0), makeTrack("Bass part", 1)];
  for (const track of tracks) {
    track.phrases = [{
      name: "Theme",
      playlist_lane: 4,
      time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 2, beat: 1, subdivision: 0 } },
      events: [makeNote()],
    }];
  }
  tracks[1].phrases.push({ ...tracks[1].phrases[0], name: "New bass notes", playlist_lane: undefined });
  const snapshot = makeSnapshot({ tracks });
  const fallback = invokeMock.getMockImplementation();
  invokeMock.mockImplementation(async (cmd, args) => {
    if (cmd === "get_project") return snapshot;
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    return fallback?.(cmd, args);
  });
  try {
    i18n.setLocale("en");
    await store.initProject();
    const view = render(() => <><Timeline /><ContextMenuLayer /></>);
    expect(view.getByRole("button", { name: "Playlist" }).getAttribute("aria-pressed")).toBe("true");
    expect(view.container.querySelector("[data-playlist-lane=unassigned]")?.textContent)
      .toContain("New bass notes");
    const group = view.getByRole("button", { name: "Theme, 2 instrument parts" });
    fireEvent.click(group);
    fireEvent.click(await screen.findByRole("menuitem", { name: "Bass part" }));
    expect(store.blockSelection()).toEqual([{ kind: "phrase", trackIndex: 1, index: 0 }]);
    expect(store.selectedTrackIndex()).toBe(1);
    expect(panelStore.bottomMode()).toBe("pianoroll");
    expect(store.project.tracks.map((track) => track.phrases[0].playlist_lane)).toEqual([4, 4]);
  } finally {
    closeContextMenu();
    panelStore.closeBottom();
    if (fallback) invokeMock.mockImplementation(fallback);
  }
});

it("keeps placed patterns in view when only part of a project has playlist rows", async () => {
  const track = makeTrack("Pattern owner", 0);
  track.clips = [{
    pattern: "Reusable pattern",
    time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 2, beat: 1, subdivision: 0 } },
  }];
  const snapshot = makeSnapshot({ tracks: [track] });
  snapshot.project.properties.audio_clips = [
    makeAudioClip({ name: "Imported audio", track: track.name, playlist_lane: 8 }),
  ];
  const fallback = invokeMock.getMockImplementation();
  invokeMock.mockImplementation(async (cmd, args) => {
    if (cmd === "get_project") return snapshot;
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "audio_clip_peaks") return "";
    return fallback?.(cmd, args);
  });
  try {
    i18n.setLocale("en");
    await store.initProject();
    const view = render(() => <Timeline />);
    const toggle = view.getByRole("button", { name: "Playlist" });
    expect(toggle.getAttribute("aria-pressed")).toBe("false");
    expect(view.getByText("Reusable pattern")).toBeTruthy();
    fireEvent.click(toggle);
    expect(view.container.querySelector("[data-playlist-lane='8']")).toBeTruthy();
    fireEvent.click(toggle);
    expect(view.getByText("Reusable pattern")).toBeTruthy();
  } finally {
    if (fallback) invokeMock.mockImplementation(fallback);
  }
});

it("shows automation-only rows and edits the original bus parameter", async () => {
  const at = (bar: number) => ({ bar, beat: 1, subdivision: 0 });
  const lane = (start: number, end: number): AutomationLane => ({
    param: "fx1.factor",
    playlist_lane: 8,
    time_range: { start: at(start), end: at(end) },
    points: [{ position: at(start), value: { Number: 0.8 } }],
  });
  const snapshot = makeSnapshot({ buses: [
    { name: "Wide Wet", effects: [{ effect_type: "Gain", params: [] }], routes: ["master"],
      automation: [lane(1, 3), lane(3, 4)] },
    { name: "Wide Dry", effects: [{ effect_type: "Gain", params: [] }], routes: ["master"],
      automation: [lane(2, 3)] },
  ] });
  const fallback = invokeMock.getMockImplementation();
  invokeMock.mockImplementation(async (cmd, args) => {
    if (cmd === "get_project" || cmd === "add_automation_point") return snapshot;
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    return fallback?.(cmd, args);
  });
  try {
    i18n.setLocale("en");
    await store.initProject();
    const view = render(() => <Timeline />);
    const row = view.container.querySelector("[data-playlist-lane='8']")!;
    const curves = [...row.querySelectorAll<HTMLElement>(".automation-lane")];
    expect(curves.map((curve) => curve.dataset.laneIndex)).toEqual(["0", "1", "0"]);
    expect(curves[0].style.top).toBe(curves[1].style.top);
    expect(curves[2].style.top).not.toBe(curves[0].style.top);
    vi.spyOn(curves[2], "getBoundingClientRect").mockReturnValue(new DOMRect(0, 0, 2000, 38));
    fireEvent.dblClick(curves[2].querySelector(".automation-span")!, { clientX: 120, clientY: 19 });
    await waitFor(() => expect(getLastCallArgs("add_automation_point")).toMatchObject({
      target: "bus:Wide Dry", laneParam: "fx1.factor", laneIndex: 0,
    }));
  } finally {
    if (fallback) invokeMock.mockImplementation(fallback);
  }
});

it("opens post-insert gain automation from its arrangement row", async () => {
  const start = { bar: 1, beat: 1, subdivision: 0 };
  const end = { ...start, bar: 2 };
  const snapshot = makeSnapshot({ buses: [{
    name: "Drum mix", effects: [], routes: ["master"],
    gain_automation: [{
      playlist_lane: 6, interpolation: "Linear", time_range: { start, end },
      points: [{ position: start, value: { Number: 0.25 } }],
    }],
  }] });
  const fallback = invokeMock.getMockImplementation();
  invokeMock.mockImplementation(async (cmd, args) => {
    if (cmd === "get_project") return snapshot;
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    return fallback?.(cmd, args);
  });
  try {
    i18n.setLocale("en");
    await store.initProject();
    const view = render(() => <Timeline />);
    const row = view.container.querySelector("[data-playlist-lane='6']")!;
    const button = within(row as HTMLElement).getByRole("button", { name: i18n.t("gainEditor.edit") });
    fireEvent.click(button);
    expect(gainAutomationUi.request()).toMatchObject({ target: "bus:Drum mix", index: 0 });
    expect(panelStore.bottomMode()).toBe("mixer");
  } finally {
    gainAutomationUi.clear();
    panelStore.closeBottom();
    if (fallback) invokeMock.mockImplementation(fallback);
  }
});
