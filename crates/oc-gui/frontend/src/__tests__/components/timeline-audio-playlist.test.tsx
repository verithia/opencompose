import { fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { expect, it, vi } from "vitest";
import { closeContextMenu, ContextMenuLayer } from "../../components/ContextMenu";
import { panelStore } from "../../stores/panels";
import { gainAutomationUi } from "../../stores/gainAutomationUi";
import { Timeline } from "../../components/Timeline";
import { i18n } from "../../stores/i18n";
import { automationHandleSubset } from "../../utils/automationRow";
import { store } from "../../stores/project";
import type { AutomationLane } from "../../types/project";
import { getLastCallArgs, invokeMock, makeAudioClip, makeNote, makeSnapshot, makeTrack, TRANSPORT_STOPPED } from "../setup";

it("spaces dense anchors without changing curve data or hiding a selected point", () => {
  const points = Array.from({ length: 23 }, (_, index) => ({ index, at: { x: index, y: 8 } }));
  expect(automationHandleSubset(points).map((point) => point.index)).toEqual([0, 22]);
  expect(automationHandleSubset(points, 11).map((point) => point.index)).toEqual([11]);
  const spread = points.map(({ index }) => ({ index, at: { x: index * 14, y: 8 } }));
  expect(automationHandleSubset(spread)).toEqual(spread);
});

it("fits the mixed-meter timeline below six pixels per quarter and spaces the ruler labels", () => {
  const track = makeTrack("Meter changes", 0);
  track.phrases = [];
  const snapshot = makeSnapshot({ tracks: [track] });
  snapshot.project.properties.time_signature = { numerator: 5, denominator: 4 };
  snapshot.project.properties.time_sig_map = [
    { position: { bar: 3, beat: 1, subdivision: 0 }, numerator: 3, denominator: 8 },
  ];
  store.applyAiSnapshot(snapshot);
  const view = render(() => <Timeline />);
  const scroller = view.container.querySelector<HTMLElement>(".timeline-lanes")!;
  Object.defineProperty(scroller, "clientWidth", { value: 110 });
  fireEvent.click(view.getByRole("button", { name: i18n.t("command.view.zoomToFit") }));
  const width = view.container.querySelector<HTMLElement>(".timeline-lane-column")!.style.width;
  expect(parseFloat(width)).toBeCloseTo(110);
  const marks = [...view.container.querySelectorAll<HTMLElement>(".timeline-ruler-mark")];
  expect(marks[0].textContent).toBe("1");
  expect(marks.length).toBeGreaterThan(1);
  expect(marks.slice(1).every((mark, index) =>
    parseFloat(mark.style.left) - parseFloat(marks[index].style.left) >= 34,
  )).toBe(true);
});

it("draws recorded controls without exposing capture samples as editable points", () => {
  const start = { bar: 1, beat: 1, subdivision: 0 };
  const end = { ...start, bar: 2 };
  store.applyAiSnapshot(makeSnapshot({ buses: [{
    name: "Captured gain", routes: ["master"], effects: [{ effect_type: "Gain", params: [] }],
    automation: [{
      param: "fx1.factor", mode: "recorded", playlist_lane: 1, time_range: { start, end },
      points: [{ position: start, value: { Number: 1 } }, { position: end, value: { Number: 0.5 } }],
    }],
  }] }));
  const view = render(() => <Timeline />);
  const row = view.container.querySelector("[data-playlist-lane='1']")!;
  expect(row.querySelector(".automation-polyline")?.getAttribute("points")).toMatch(/0\.00,/);
  expect(row.querySelectorAll(".automation-point")).toHaveLength(0);
  expect(row.textContent).toContain(i18n.t("device.automation.mode.recorded"));
});

it("mounts detailed audio controls only when a short overview clip is selected", () => {
  const track = makeTrack("Short shots", 0);
  const snapshot = makeSnapshot({ tracks: [track] });
  snapshot.project.properties.audio_clips = [makeAudioClip({
    name: "Short shot", track: track.name, playlist_lane: 1,
    time_range: {
      start: { bar: 1, beat: 1, subdivision: 0 },
      end: { bar: 1, beat: 1, subdivision: 1 },
    },
  })];
  store.applyAiSnapshot(snapshot);
  const view = render(() => <Timeline />);
  const row = view.container.querySelector<HTMLElement>("[data-playlist-lane='1']")!;
  expect(row.querySelector(".audio-clip-waveform")).toBeNull();
  const label = i18n.t("clip.audioLabel", { name: "Short shot" });
  fireEvent.click(within(row).getByRole("button", { name: label }));
  expect(row.querySelector(".playlist-mini-audio")).toBeNull();
  expect(row.querySelector(".audio-clip-block.selected")).toHaveAttribute("aria-label", label);
});

it("groups real audio clips by playlist row without changing their mix owners", async () => {
  const snapshot = makeSnapshot({ tracks: [makeTrack("Mixer A", 0), makeTrack("Mixer B", 1)] });
  snapshot.project.properties.playlist_rows = { 2: "Original drums" };
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
    expect(view.getByText("Original drums")).toBeVisible();
    expect(view.getByText(i18n.t("timeline.playlistRow", { lane: 8 }))).toBeVisible();
    const rows = [...view.container.querySelectorAll<HTMLElement>("[data-playlist-lane]")];
    expect(rows.map((row) => row.dataset.playlistLane)).toEqual(["2", "8", "unassigned"]);
    expect(rows[0].querySelectorAll(".audio-clip-block")).toHaveLength(2);
    expect(within(rows[0]).getByText("Second source")).toBeVisible();
    expect(rows[0].querySelectorAll(".playlist-band")).toHaveLength(1);
    expect([...rows[0].querySelectorAll<HTMLElement>(".playlist-item")].map((item) => item.style.left))
      .toEqual(["0px", "0px"]);
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
    expect(curves[0].closest(".playlist-band")).toBe(curves[1].closest(".playlist-band"));
    expect(curves[2].closest(".playlist-band")).toBe(curves[0].closest(".playlist-band"));
    expect(curves[2].closest(".playlist-item")).toHaveClass("playlist-overlap");
    const span = curves[2].querySelector<HTMLElement>(".automation-span")!;
    expect(span.style.left).toBe("0px");
    expect(curves[2].style.width).toBe("96px");
    vi.spyOn(curves[2], "getBoundingClientRect").mockReturnValue(new DOMRect(280, 0, 96, 38));
    fireEvent.dblClick(span, { clientX: 328, clientY: 19 });
    await waitFor(() => expect(getLastCallArgs("add_automation_point")).toMatchObject({
      target: "bus:Wide Dry", laneParam: "fx1.factor", laneIndex: 0, positionBeats: 6,
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


it("uses one compact strip for consecutive samples, notes and automation", async () => {
  const at = (bar: number) => ({ bar, beat: 1, subdivision: 0 });
  const range = (start: number, end: number) => ({ start: at(start), end: at(end) });
  const track = makeTrack("Original piano", 0);
  track.phrases = [{ name: "Piano intro", playlist_lane: 1, time_range: range(2, 3), events: [makeNote()] }];
  const snapshot = makeSnapshot({ tracks: [track], buses: [{
    name: "Piano mix", routes: ["master"], effects: [{ effect_type: "Gain", params: [] }],
    automation: [{
      param: "fx1.factor", playlist_lane: 1, time_range: range(3, 4),
      points: [2, 3, 4, 5].map((bar) => ({ position: at(bar), value: { Number: 0.7 } })),
    }],
  }] });
  snapshot.project.properties.audio_clips = [
    makeAudioClip({ name: "Opening texture", track: track.name, playlist_lane: 1, time_range: range(1, 2) }),
  ];
  const fallback = invokeMock.getMockImplementation();
  invokeMock.mockImplementation(async (cmd, args) => {
    if (cmd === "get_project") return snapshot;
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "audio_clip_peaks") return "";
    return fallback?.(cmd, args);
  });
  try {
    await store.initProject();
    const view = render(() => <Timeline />);
    const row = view.container.querySelector<HTMLElement>("[data-playlist-lane='1']")!;
    const bands = row.querySelectorAll(".playlist-band");
    expect(bands).toHaveLength(1);
    expect(row.style.height).toBe(view.container.querySelector<HTMLElement>(".playlist-header")?.style.height);
    expect(bands[0].querySelector(".audio-clip-block")).toBeTruthy();
    expect(bands[0].querySelector(".playlist-note-thumbnail path")?.getAttribute("d")).toMatch(/^M/);
    expect(bands[0].querySelectorAll(".automation-point")).toHaveLength(2);
    expect(bands[0].querySelector(".automation-polyline")?.getAttribute("clip-path")).toMatch(/^url\(#/);
  } finally {
    if (fallback) invokeMock.mockImplementation(fallback);
  }
});


it("virtualizes offscreen clips horizontally and keeps a dragged clip mounted until release", async () => {
  let observerCallback: IntersectionObserverCallback | undefined;
  const observed = new Set<Element>();
  const observer = {
    observe: (element: Element) => observed.add(element),
    unobserve: (element: Element) => observed.delete(element),
    disconnect: () => observed.clear(),
  } as unknown as IntersectionObserver;
  const intersect = (target: Element, isIntersecting: boolean) => {
    expect(observed.has(target)).toBe(true);
    observerCallback!([{ target, isIntersecting } as IntersectionObserverEntry], observer);
  };
  vi.stubGlobal("IntersectionObserver", function (
    callback: IntersectionObserverCallback,
    options: IntersectionObserverInit,
  ) {
    expect(options.rootMargin).toBe("256px 384px");
    observerCallback = callback;
    return observer;
  });

  const track = makeTrack("Sampler", 0);
  const at = (bar: number) => ({ bar, beat: 1, subdivision: 0 });
  const snapshot = makeSnapshot({ tracks: [track] });
  snapshot.project.properties.audio_clips = [1, 50].map((bar) =>
    makeAudioClip({ name: `Sample at ${bar}`, track: track.name, playlist_lane: 1,
      time_range: { start: at(bar), end: at(bar + 1) } }),
  );
  const fallback = invokeMock.getMockImplementation();
  invokeMock.mockImplementation(async (cmd, args) => {
    if (cmd === "get_project") return snapshot;
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "audio_clip_peaks") return "";
    return fallback?.(cmd, args);
  });
  try {
    await store.initProject();
    const view = render(() => <Timeline />);
    const row = view.container.querySelector("[data-playlist-lane='1']")!;
    intersect(row, true);
    const [first, far] = row.querySelectorAll(".playlist-item");
    expect(first.getAttribute("data-playlist-label")).toBe("Sample at 1");
    expect(far.getAttribute("data-playlist-label")).toBe("Sample at 50");
    expect(row.querySelectorAll(".audio-clip-block")).toHaveLength(0);
    intersect(first, true);
    const clip = first.querySelector(".audio-clip-block")!;
    expect(clip).toBeTruthy();
    expect(far.querySelector(".audio-clip-block")).toBeNull();
    fireEvent.pointerDown(clip, { pointerId: 12, button: 0, clientX: 10, clientY: 10 });
    intersect(first, false);
    intersect(far, true);
    expect(first.querySelector(".audio-clip-block")).toBe(clip);
    expect(far.querySelector(".audio-clip-block")).toBeTruthy();
    fireEvent.pointerUp(document, { pointerId: 12, clientX: 10, clientY: 10 });
    expect(first.querySelector(".audio-clip-block")).toBe(clip);
    (clip as HTMLElement).blur();
    expect(first.querySelector(".audio-clip-block")).toBeNull();
    expect(store.project.properties.audio_clips).toHaveLength(2);
    const scroller = view.container.querySelector<HTMLElement>(".timeline-lanes")!;
    scroller.scrollLeft = 200;
    fireEvent.scroll(scroller);
    intersect(first, true);
    intersect(far, false);
    expect(first.querySelector(".audio-clip-block")).toBeNull();
    expect(far.querySelector(".audio-clip-block")).toBeTruthy();
    await waitFor(() => expect(first.querySelector(".audio-clip-block")).toBeTruthy());
    await waitFor(() => expect(far.querySelector(".audio-clip-block")).toBeNull());
    scroller.scrollLeft = 224;
    fireEvent.scroll(scroller);
    intersect(far, true);
    expect(far.querySelector(".audio-clip-block")).toBeNull();
    for (let step = 1; step <= 3; step += 1) {
      await new Promise<void>((resolve) => requestAnimationFrame(() => {
        scroller.scrollLeft = 224 + step * 24;
        fireEvent.scroll(scroller);
        resolve();
      }));
    }
    expect(far.querySelector(".audio-clip-block")).toBeTruthy();
    view.unmount();
  } finally {
    vi.unstubAllGlobals();
    if (fallback) invokeMock.mockImplementation(fallback);
  }
});
