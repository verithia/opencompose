/**
 * Store tests — project state mutations, undo/redo, transport.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { store } from "../../stores/project";
import { log } from "../../stores/log";
import type { Project } from "../../types/project";
import {
  invokeMock, makeSnapshot, makeTrack, makePattern, makeNote, makeEffect, createOwnedUndoFixture,
  TRANSPORT_STOPPED, TRANSPORT_PLAYING,
  expectInvoked, getLastCallArgs,
} from "../setup";

// The store module is imported once with its singleton state.
// We clear call history before each test but preserve the mock implementation.
beforeEach(() => {
  invokeMock.mockClear();
  // Re-set the default fallback for any unmatched calls
  invokeMock.mockImplementation(async (cmd: string, _args?: unknown) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_project") return makeSnapshot();
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    return makeSnapshot();
  });
});

// ── Init / New / Error ─────────────────────────────────────────────────────

describe("initProject", () => {
  it("opens the startup path after initialization", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "take_startup_project") return "/music/my song.ocp";
      return makeSnapshot({ name: cmd === "open_file" ? "Startup song" : "Empty" });
    });
    await store.initStartupProject();
    expectInvoked("open_file", { path: "/music/my song.ocp" });
    expect(store.project.name).toBe("Startup song");
  });

  it("keeps the default project without a startup path", async () => {
    invokeMock.mockImplementation(async (cmd: string) => cmd === "take_startup_project" ? null : makeSnapshot());
    await store.initStartupProject();
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "open_file")).toBe(false);
  });

  it("surfaces a failed startup open", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "take_startup_project") return "/missing.ocp";
      if (cmd === "open_file") throw new Error("Failed to read file");
      return makeSnapshot();
    });
    await store.initStartupProject();
    expect(store.error()).toContain("Failed to read file");
    expect(store.loading()).toBe(false);
  });
  it("loads project from backend", async () => {
    invokeMock.mockResolvedValueOnce(makeSnapshot({ name: "Loaded" }));
    await store.initProject();
    expect(store.project.name).toBe("Loaded");
    expect(store.dslSource()).toBeTruthy();
    expect(store.loading()).toBe(false);
    expectInvoked("get_project");
  });

  it("sets error on backend failure", async () => {
    invokeMock.mockRejectedValueOnce(new Error("boom"));
    await store.initProject();
    expect(store.error()).toContain("boom");
  });
});

describe("track preview lifecycle", () => {
  it("retains Space while parking the arrangement before a track preview starts", async () => {
    await store.stop();
    let park!: (value: ReturnType<typeof TRANSPORT_STOPPED>) => void;
    let nativePlaying = false;
    invokeMock.mockImplementation((cmd: string, args?: any) => {
      if (cmd === "transport_play" || cmd === "resume_track_preview") {
        nativePlaying = true;
        return Promise.resolve(TRANSPORT_PLAYING());
      }
      if (cmd === "transport_pause") return new Promise(resolve => { park = resolve; });
      if (cmd === "preview_track") {
        nativePlaying = args.startPaused !== true;
        return Promise.resolve({ ...TRANSPORT_PLAYING(), playing: nativePlaying, did_render: true });
      }
      return Promise.resolve(TRANSPORT_STOPPED());
    });
    await store.play();
    invokeMock.mockClear();
    const pending = store.previewTrack(0);
    await store.togglePlayPause();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "transport_pause")).toHaveLength(1);
    nativePlaying = false;
    park(TRANSPORT_STOPPED());
    await pending;
    expectInvoked("preview_track", { trackIndex: 0, startPaused: true });
    expect(nativePlaying).toBe(false);
    expect(store.transport().playing).toBe(false);
    expect(store.previewTrackIndex()).toBe(0);
    await store.togglePlayPause();
    expectInvoked("resume_track_preview", { trackIndex: 0 });
    expect(nativePlaying).toBe(true);
    expect(store.transport().playing).toBe(true);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "transport_play")).toBe(false);
    await store.stop();
  });

  it("keeps a device-suspended solo paused without restarting the arrangement", async () => {
    await store.stop();
    vi.useFakeTimers();
    try {
      invokeMock.mockImplementation(async (cmd: string) => {
        if (cmd === "transport_play" || cmd === "preview_track" || cmd === "resume_track_preview") {
          return TRANSPORT_PLAYING();
        }
        if (cmd === "get_transport_state") {
          return { ...TRANSPORT_STOPPED(), position_beats: 16, track_preview_rebuild: true };
        }
        return TRANSPORT_STOPPED();
      });
      await store.play();
      await store.previewTrack(0);
      await vi.advanceTimersByTimeAsync(300);
      expect(store.transport().playing).toBe(false);
      expect(store.previewTrackIndex()).toBe(0);
      expect(store.previewCursorBeats()).toBe(16);
      expect(store.transport().position_beats).toBe(TRANSPORT_STOPPED().position_beats);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "transport_play")).toHaveLength(1);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "transport_seek")).toHaveLength(0);
      await store.togglePlayPause();
      expectInvoked("resume_track_preview", { trackIndex: 0 });
      expect(store.transport().playing).toBe(true);
      await store.stop();
    } finally {
      vi.useRealTimers();
    }
  });

  it("a newer pending target wins over an older completion", async () => {
    await store.stop();
    const finish: Array<(value: ReturnType<typeof TRANSPORT_PLAYING>) => void> = [];
    invokeMock.mockImplementation((cmd: string) => cmd === "preview_track"
      ? new Promise(resolve => { finish.push(resolve); }) : Promise.resolve(TRANSPORT_STOPPED()));
    const first = store.previewTrack(0);
    await Promise.resolve();
    const second = store.previewTrack(1);
    for (let i = 0; i < 8; i++) await Promise.resolve();
    expect(finish).toHaveLength(2);
    finish[1](TRANSPORT_PLAYING());
    await second;
    finish[0](TRANSPORT_PLAYING());
    await first;
    expect(store.previewTrackIndex()).toBe(1);
    await store.stop();
  });

  it("reconciles stopped transport after audition failure and allows retry", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "preview_track") throw new Error("preview failed");
      return TRANSPORT_STOPPED();
    });
    await store.previewTrack(0);
    expect(store.error()).toContain("preview failed");
    expect(store.transport().playing).toBe(false);
    expect(store.loading()).toBe(false);
    // The parked cursor is restored instead of adopting the failed preview.
    expectInvoked("transport_seek");
    invokeMock.mockImplementation(async () => TRANSPORT_STOPPED());
    await store.previewTrack(1);
    expectInvoked("preview_track", { trackIndex: 1 });
  });

  it("keeps the arrangement playhead while a track preview plays", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_seek") return { ...TRANSPORT_STOPPED(), position_beats: 8, position_seconds: 4 };
      // A preview is a fresh pass from the start of the solo arrangement.
      if (cmd === "preview_track") return { ...TRANSPORT_PLAYING(), position_beats: 0, position_seconds: 0 };
      if (cmd === "get_transport_state") return { ...TRANSPORT_PLAYING(), position_beats: 0, position_seconds: 0 };
      return TRANSPORT_STOPPED();
    });
    await store.seek(8);
    await store.previewTrack(0);
    // The preview's own frames must not drag the arrangement cursor back to 0.
    expect(store.transport().position_beats).toBe(8);
    expect(store.transport().position_seconds).toBe(4);
    expect(store.smoothPositionBeats()).toBe(8);
    await store.stop();
    expect(store.transport().position_beats).toBe(0);
  });
});

describe("saveFile", () => {
  it("reports failure without clearing dirty state or changing the resource root", async () => {
    const snap = { ...makeSnapshot(), file_path: "/old/project.ocp", dirty: true };
    invokeMock.mockResolvedValueOnce(snap);
    await store.initProject();
    invokeMock.mockRejectedValueOnce(new Error("Save As resource collision"));
    expect(await store.saveFile("/new/project.ocp")).toBe(false);
    expect(store.filePath()).toBe("/old/project.ocp");
    expect(store.dirty()).toBe(true);
    expect(store.error()).toContain("resource collision");
    invokeMock.mockResolvedValueOnce({ path: "/new/project.ocp", snapshot: null });
    expect(await store.saveFile("/new/project.ocp")).toBe(true);
    expect(store.filePath()).toBe("/new/project.ocp");
    expect(store.dirty()).toBe(false);
    expect(store.error()).toBe(null);
  });
});

describe("document identity", () => {
  /** A two-track document at `path`, so a replacement has a track 0 to fall back to. */
  const documentAt = (path: string | null, dirty = false) => {
    const snap = makeSnapshot({ tracks: [makeTrack("Drums", 0), makeTrack("Bass", 1)] });
    snap.file_path = path;
    snap.dirty = dirty;
    return snap;
  };

  /** The picks a loaded document carries; a replacement must drop all of them. */
  const selectPicks = () => {
    store.setSelectedTrackIndex(1);
    store.setEditorSelection({ trackIndex: 1, startBeat: 8, patternName: null });
    store.setBlockSelection([{ kind: "clip", trackIndex: 0, index: 0 }]);
  };

  it("keeps the picks and the generation when a save moves the document's path", async () => {
    const opened = documentAt("/a.ocp");
    const afterSaveAs = documentAt("/b.ocp", true);
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "open_file") return opened;
      if (cmd === "save_file") return { path: "/b.ocp", snapshot: null };
      if (cmd === "update_track_mix") return afterSaveAs;
      return TRANSPORT_STOPPED();
    });

    await store.openFile("/a.ocp");
    const generation = store.documentIdentity().generation;
    selectPicks();

    expect(await store.saveFile("/b.ocp")).toBe(true);
    expect(store.documentIdentity().path).toBe("/b.ocp");

    // Save As keeps the document open, so the next edit is not a replacement.
    await store.updateTrackMix(1, -6);

    expect(store.documentIdentity().generation).toBe(generation);
    expect(store.selectedTrackIndex()).toBe(1);
    expect(store.editorSelection()).toEqual({ trackIndex: 1, startBeat: 8, patternName: null });
    expect(store.blockSelection()).toEqual([{ kind: "clip", trackIndex: 0, index: 0 }]);
  });

  it("treats the edit after a refused open as an edit", async () => {
    const opened = documentAt("/a.ocp");
    const edited = documentAt("/a.ocp", true);
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "open_file") {
        if ((args as { path: string }).path === "/missing.ocp") {
          throw new Error("Failed to read file");
        }
        return opened;
      }
      if (cmd === "update_track_mix") return edited;
      return TRANSPORT_STOPPED();
    });

    await store.openFile("/a.ocp");
    const generation = store.documentIdentity().generation;
    selectPicks();

    await store.openFile("/missing.ocp");
    expect(store.error()).toContain("Failed to read file");

    await store.updateTrackMix(1, -6);

    expect(store.documentIdentity().generation).toBe(generation);
    expect(store.selectedTrackIndex()).toBe(1);
    expect(store.editorSelection()).toEqual({ trackIndex: 1, startBeat: 8, patternName: null });
    expect(store.blockSelection()).toEqual([{ kind: "clip", trackIndex: 0, index: 0 }]);
  });

  it("treats the edit after a superseded open as an edit", async () => {
    const opened = documentAt("/a.ocp");
    const replacement = documentAt("/b.ocp");
    let failSlow!: (error: Error) => void;
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "open_file") {
        const path = (args as { path: string }).path;
        if (path === "/slow.ocp") {
          return new Promise((_resolve, reject) => { failSlow = reject; });
        }
        return path === "/a.ocp" ? opened : replacement;
      }
      if (cmd === "update_track_mix") return { ...replacement, dirty: true };
      return TRANSPORT_STOPPED();
    });

    await store.openFile("/a.ocp");
    const slow = store.openFile("/slow.ocp");
    for (let i = 0; i < 4 && !failSlow; i++) await Promise.resolve();
    await store.openFile("/b.ocp");
    const generation = store.documentIdentity().generation;

    failSlow(new Error("late failure"));
    await slow;

    // The newer load owns the identity; the older failure must not leak into it.
    expect(store.error()).toBe(null);
    await store.updateTrackMix(0, -3);
    expect(store.documentIdentity().generation).toBe(generation);
    expect(store.documentIdentity().path).toBe("/b.ocp");
  });

  it("drops the previous document's picks when a real open lands", async () => {
    const first = documentAt("/a.ocp");
    const second = documentAt("/b.ocp");
    invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
      if (cmd === "open_file") {
        return (args as { path: string }).path === "/a.ocp" ? first : second;
      }
      return TRANSPORT_STOPPED();
    });

    await store.openFile("/a.ocp");
    const generation = store.documentIdentity().generation;
    selectPicks();

    await store.openFile("/b.ocp");

    expect(store.documentIdentity().generation).toBe(generation + 1);
    expect(store.documentIdentity().path).toBe("/b.ocp");
    expect(store.selectedTrackIndex()).toBe(0);
    expect(store.editorSelection()).toBe(null);
    expect(store.blockSelection()).toEqual([]);
  });

  it("adopts the collected patch paths returned by Save As", async () => {
    const opened = documentAt("/a.ocp");
    const snapshot = {
      ...documentAt("/b.ocp"),
      revision: opened.revision + 1,
      dsl_source: "@project \"Portable\"\n// Collected patch path",
    };
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "open_file") return opened;
      if (cmd === "save_file") return { path: "/b.ocp", snapshot };
      return TRANSPORT_STOPPED();
    });
    await store.openFile("/a.ocp");
    const generation = store.documentIdentity().generation;
    selectPicks();
    expect(await store.saveFile("/b.ocp")).toBe(true);
    expect(store.dslSource()).toBe(snapshot.dsl_source);
    expect(store.documentIdentity().generation).toBe(generation);
    expect(store.selectedTrackIndex()).toBe(1);
    expect(store.dirty()).toBe(false);
  });

  it("keeps dirty when an edit lands while the save is written", async () => {
    const opened = documentAt("/a.ocp");
    const edited = documentAt("/a.ocp", true);
    let finishSave!: (saved: { path: string; snapshot: null }) => void;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "open_file") return opened;
      if (cmd === "save_file") return new Promise(resolve => { finishSave = resolve; });
      if (cmd === "update_track_mix") return edited;
      return TRANSPORT_STOPPED();
    });

    await store.openFile("/a.ocp");
    const generation = store.documentIdentity().generation;
    const saving = store.saveFile("/b.ocp");

    // The newer edit adopts its own snapshot while the write is in flight.
    await store.updateTrackMix(1, -6);
    expect(store.dirty()).toBe(true);

    finishSave({ path: "/b.ocp", snapshot: null });
    expect(await saving).toBe(true);

    // The write covers the older state, so the newer edit is still unsaved.
    expect(store.dirty()).toBe(true);
    expect(store.filePath()).toBe("/b.ocp");
    expect(store.documentIdentity().generation).toBe(generation);
    expect(store.documentIdentity().path).toBe("/b.ocp");
  });
});

describe("newProject", () => {
  it("resets to empty project and clears undo", async () => {
    const snap = makeSnapshot({ name: "New" });
    invokeMock.mockResolvedValueOnce(snap);
    await store.newProject();
    expect(store.project.name).toBe("New");
    expect(store.selectedTrackIndex()).toBe(null);
    expectInvoked("new_project");
  });
});

// ── Track mutations ────────────────────────────────────────────────────────

describe("addTrack / removeTrack", () => {
  beforeEach(async () => {
    invokeMock.mockResolvedValueOnce(makeSnapshot());
    await store.initProject();
  });

  it("adds a track and selects it", async () => {
    const snap = makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
    invokeMock.mockResolvedValueOnce(snap);
    const idx = await store.addTrack("Piano", "piano", "acoustic_grand");
    expect(idx).toBe(0);
    expect(store.project.tracks.length).toBe(1);
    expect(store.selectedTrackIndex()).toBe(0);
    expectInvoked("add_track", { name: "Piano", instrumentCategory: "piano", instrumentName: "acoustic_grand" });
  });

  it("removes a track and clears selection if empty", async () => {
    const snapA = makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
    invokeMock.mockResolvedValueOnce(snapA);
    await store.addTrack("Piano", "piano", "acoustic_grand");
    const snapB = makeSnapshot({ tracks: [] });
    invokeMock.mockResolvedValueOnce(snapB);
    await store.removeTrack(0);
    expect(store.project.tracks.length).toBe(0);
    expect(store.selectedTrackIndex()).toBe(null);
    expectInvoked("remove_track", { trackIndex: 0 });
  });

  it("adjusts selection when removing last track", async () => {
    const snap2 = makeSnapshot({ tracks: [makeTrack("A", 0), makeTrack("B", 1)] });
    invokeMock.mockResolvedValueOnce(snap2);
    await store.initProject();
    store.setSelectedTrackIndex(1);
    const snap1 = makeSnapshot({ tracks: [makeTrack("A", 0)] });
    invokeMock.mockResolvedValueOnce(snap1);
    await store.removeTrack(1);
    expect(store.selectedTrackIndex()).toBe(0);
  });

  it("keeps the selection on the same track when an earlier one is removed", async () => {
    const three = makeSnapshot({
      tracks: [makeTrack("A", 0), makeTrack("B", 1), makeTrack("C", 2)],
    });
    invokeMock.mockResolvedValueOnce(three);
    await store.initProject();
    store.setSelectedTrackIndex(2);

    const two = makeSnapshot({ tracks: [makeTrack("B", 0), makeTrack("C", 1)] });
    invokeMock.mockResolvedValueOnce(two);
    await store.removeTrack(0);

    // C moved down one lane; the selection follows it, not the old index.
    expect(store.selectedTrackIndex()).toBe(1);
    expect(store.selectedTrack()?.name).toBe("C");
  });

  it("selects the neighbour when the selected track is deleted", async () => {
    const three = makeSnapshot({
      tracks: [makeTrack("A", 0), makeTrack("B", 1), makeTrack("C", 2)],
    });
    invokeMock.mockResolvedValueOnce(three);
    await store.initProject();
    store.setSelectedTrackIndex(1);

    const two = makeSnapshot({ tracks: [makeTrack("A", 0), makeTrack("C", 1)] });
    invokeMock.mockResolvedValueOnce(two);
    await store.removeTrack(1);

    expect(store.selectedTrackIndex()).toBe(1);
    expect(store.selectedTrack()?.name).toBe("C");
  });

  it("drops the block and editor picks that named the removed track", async () => {
    const three = makeSnapshot({
      tracks: [makeTrack("A", 0), makeTrack("B", 1), makeTrack("C", 2)],
    });
    invokeMock.mockResolvedValueOnce(three);
    await store.initProject();
    store.setBlockSelection([
      { kind: "clip", trackIndex: 0, index: 0 },
      { kind: "clip", trackIndex: 1, index: 0 },
    ]);
    store.setEditorSelection({ trackIndex: 2, startBeat: 4, patternName: null });

    const two = makeSnapshot({ tracks: [makeTrack("A", 0), makeTrack("C", 1)] });
    invokeMock.mockResolvedValueOnce(two);
    await store.removeTrack(1);

    expect(store.blockSelection()).toEqual([{ kind: "clip", trackIndex: 0, index: 0 }]);
    // C is lane 1 now, and the editor followed it.
    expect(store.editorSelection()).toEqual({ trackIndex: 1, startBeat: 4, patternName: null });
  });

  it("clamps a selection the document no longer has", async () => {
    const three = makeSnapshot({
      tracks: [makeTrack("A", 0), makeTrack("B", 1), makeTrack("C", 2)],
    });
    invokeMock.mockResolvedValueOnce(three);
    await store.initProject();
    store.setSelectedTrackIndex(2);

    // Any snapshot can shrink the list, an undo of an added track included.
    const two = makeSnapshot({ tracks: [makeTrack("A", 0), makeTrack("B", 1)] });
    invokeMock.mockResolvedValueOnce(two);
    await store.renameTrack(0, "A");

    expect(store.selectedTrackIndex()).toBe(1);
  });

  it("renames a track", async () => {
    const snapA = makeSnapshot({ tracks: [makeTrack("Old", 0)] });
    invokeMock.mockResolvedValueOnce(snapA);
    await store.addTrack("Old", "piano", "acoustic_grand");
    const snapB = makeSnapshot({
      tracks: [{ ...makeTrack("New Name", 0), name: "New Name" }],
    });
    invokeMock.mockResolvedValueOnce(snapB);
    await store.renameTrack(0, "New Name");
    expect(store.project.tracks[0].name).toBe("New Name");
    expectInvoked("rename_track", { trackIndex: 0, newName: "New Name" });
  });
});

// ── Mix mutations ────────────────────────────────────────────────────────

describe("mix controls", () => {
  beforeEach(async () => {
    const snap = makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
    invokeMock.mockResolvedValueOnce(snap);
    await store.initProject();
  });

  it("toggles mute", async () => {
    const muted = makeSnapshot({
      tracks: [{ ...makeTrack("Piano", 0), mix: { volume_db: 0, pan: 0, sends: [], mute: true, solo: false } }],
    });
    invokeMock.mockResolvedValueOnce(muted);
    await store.toggleMute(0);
    expect(store.project.tracks[0].mix.mute).toBe(true);
    expectInvoked("set_track_mute", { trackIndex: 0, mute: true });
  });

  it("toggles solo", async () => {
    const soloed = makeSnapshot({
      tracks: [{ ...makeTrack("Piano", 0), mix: { volume_db: 0, pan: 0, sends: [], mute: false, solo: true } }],
    });
    invokeMock.mockResolvedValueOnce(soloed);
    await store.toggleSolo(0);
    expect(store.project.tracks[0].mix.solo).toBe(true);
    expectInvoked("set_track_solo", { trackIndex: 0, solo: true });
  });

  it("updates volume and pan", async () => {
    const volSnap = makeSnapshot({
      tracks: [{ ...makeTrack("Piano", 0), mix: { volume_db: 6, pan: -0.5, sends: [], mute: false, solo: false } }],
    });
    invokeMock.mockResolvedValueOnce(volSnap);
    await store.updateTrackMix(0, 6, -0.5);
    expect(store.project.tracks[0].mix.volume_db).toBe(6);
    expect(store.project.tracks[0].mix.pan).toBe(-0.5);
    expectInvoked("update_track_mix", { trackIndex: 0, volumeDb: 6, pan: -0.5 });
  });

  it("skips toggle if track index is out of range", async () => {
    // Track doesn't exist at index 5, should not invoke
    await store.toggleMute(5);
    // The store checks `project.tracks[trackIndex]` before invoking
    // Since track 5 doesn't exist, it returns early
  });
});

// ── Note editing ───────────────────────────────────────────────────────────

describe("note editing", () => {
  beforeEach(async () => {
    const snap = makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
    invokeMock.mockResolvedValueOnce(snap);
    await store.initProject();
  });

  it("adds a note with correct args", async () => {
    const snap = makeSnapshot({
      tracks: [{
        ...makeTrack("Piano", 0),
        phrases: [{
          name: "melody",
          time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 4, beat: 4, subdivision: 0 } },
          events: [makeNote({ pitch: { note: "D", accidental: "Natural", octave: 4 }, duration: { base: "Eighth", dots: 0, triplet: false, tuplet: null, ties: [] } })],
        }],
      }],
    });
    invokeMock.mockResolvedValueOnce(snap);
    await store.addNote(0, 0, "D", "", 4, "Eighth", "MF", 0);
    expect(store.project.tracks[0].phrases[0].events.length).toBe(1);
    expect(store.project.tracks[0].phrases[0].events[0].pitch.note).toBe("D");
    expectInvoked("add_note", { trackIndex: 0, phraseIndex: 0, noteName: "D", octave: 4, baseDuration: "Eighth" });
  });

  it("removes a note", async () => {
    const empty = makeSnapshot({
      tracks: [{ ...makeTrack("Piano", 0), phrases: [{ name: "melody", time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 4, beat: 4, subdivision: 0 } }, events: [] }] }],
    });
    invokeMock.mockResolvedValueOnce(empty);
    await store.removeNote(0, 0, 0);
    expect(store.project.tracks[0].phrases[0].events.length).toBe(0);
    expectInvoked("remove_note", { trackIndex: 0, phraseIndex: 0, noteIndex: 0 });
  });

  it("moves a note", async () => {
    const moved = makeSnapshot({
      tracks: [{
        ...makeTrack("Piano", 0),
        phrases: [{
          name: "melody",
          time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 4, beat: 4, subdivision: 0 } },
          events: [makeNote({ start_beat: 2 })],
        }],
      }],
    });
    invokeMock.mockResolvedValueOnce(moved);
    await store.moveNote(0, 0, 0, 2);
    expect(store.project.tracks[0].phrases[0].events[0].start_beat).toBe(2);
    expectInvoked("move_note", { trackIndex: 0, phraseIndex: 0, noteIndex: 0, newStartBeat: 2 });
  });

  it("sets note velocity", async () => {
    const snap = makeSnapshot({
      tracks: [{
        ...makeTrack("Piano", 0),
        phrases: [{
          name: "melody",
          time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 4, beat: 4, subdivision: 0 } },
          events: [makeNote({ velocity: 100 })],
        }],
      }],
    });
    invokeMock.mockResolvedValueOnce(snap);
    await store.setNoteVelocity(0, 0, 0, 100);
    expect(store.project.tracks[0].phrases[0].events[0].velocity).toBe(100);
    expectInvoked("set_note_velocity", { trackIndex: 0, phraseIndex: 0, noteIndex: 0, velocity: 100 });
  });

  it("sets a note's dynamics token and an articulation", async () => {
    const snap = makeSnapshot({
      tracks: [{
        ...makeTrack("Piano", 0),
        phrases: [{
          name: "melody",
          time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 4, beat: 4, subdivision: 0 } },
          events: [makeNote({ dynamics: "PP", articulation: "Staccato" })],
        }],
      }],
    });
    invokeMock.mockResolvedValueOnce(snap);
    await store.setNoteDynamics(0, 0, 0, "pp");
    expect(store.project.tracks[0].phrases[0].events[0].dynamics).toBe("PP");
    expectInvoked("set_note_dynamics", { trackIndex: 0, phraseIndex: 0, noteIndex: 0, dynamics: "pp" });

    invokeMock.mockResolvedValueOnce(snap);
    await store.setNoteArticulation(0, 0, 0, "staccato");
    expect(store.project.tracks[0].phrases[0].events[0].articulation).toBe("Staccato");
    expectInvoked("set_note_articulation", { trackIndex: 0, phraseIndex: 0, noteIndex: 0, articulation: "staccato" });
  });

  it("mutes a note", async () => {
    const snap = makeSnapshot({
      tracks: [{
        ...makeTrack("Piano", 0),
        phrases: [{
          name: "melody",
          time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 4, beat: 4, subdivision: 0 } },
          events: [makeNote({ muted: true })],
        }],
      }],
    });
    invokeMock.mockResolvedValueOnce(snap);
    await store.setNoteMuted(0, 0, 0, true);
    expect(store.project.tracks[0].phrases[0].events[0].muted).toBe(true);
  });

  it("sets note as grace", async () => {
    const snap = makeSnapshot({
      tracks: [{
        ...makeTrack("Piano", 0),
        phrases: [{
          name: "melody",
          time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 4, beat: 4, subdivision: 0 } },
          events: [makeNote({ grace: true })],
        }],
      }],
    });
    invokeMock.mockResolvedValueOnce(snap);
    await store.setNoteGrace(0, 0, 0, true);
    expect(store.project.tracks[0].phrases[0].events[0].grace).toBe(true);
  });
});

// ── Pattern editing ────────────────────────────────────────────────────────

describe("pattern editing", () => {
  beforeEach(async () => {
    const snap = makeSnapshot({ patterns: [makePattern("beat")] });
    invokeMock.mockResolvedValueOnce(snap);
    await store.initProject();
  });

  it("adds a pattern note", async () => {
    const snap = makeSnapshot({
      patterns: [{
        ...makePattern("beat"),
        events: [makeNote()],
      }],
    });
    invokeMock.mockResolvedValueOnce(snap);
    await store.addPatternNote("beat", "C", "", 4, "Quarter", 0);
    expect(store.project.patterns[0].events.length).toBe(1);
    expectInvoked("add_pattern_note", { patternName: "beat", noteName: "C", octave: 4, startBeat: 0 });
  });

  it("removes a pattern note", async () => {
    const emptyPattern = makeSnapshot({ patterns: [{ ...makePattern("beat"), events: [] }] });
    invokeMock.mockResolvedValueOnce(emptyPattern);
    await store.removePatternNote("beat", 0);
    expect(store.project.patterns[0].events.length).toBe(0);
    expectInvoked("remove_pattern_note", { patternName: "beat", noteIndex: 0 });
  });

  it("moves a pattern note", async () => {
    const snap = makeSnapshot({
      patterns: [{ ...makePattern("beat"), events: [makeNote({ start_beat: 2 })] }],
    });
    invokeMock.mockResolvedValueOnce(snap);
    await store.movePatternNote("beat", 0, 2);
    expect(store.project.patterns[0].events[0].start_beat).toBe(2);
    expectInvoked("move_pattern_note", { patternName: "beat", noteIndex: 0, newStartBeat: 2 });
  });

  it("adds a pattern hit", async () => {
    const snap = makeSnapshot({
      patterns: [{
        ...makePattern("beat"),
        hits: [
          { drum: "kick", position: { beat: 1, subdivision: 0 }, dynamics: "MF", articulation: null, modifiers: [] },
          { drum: "snare", position: { beat: 3, subdivision: 0 }, dynamics: "F", articulation: null, modifiers: [] },
          { drum: "hihat", position: { beat: 2, subdivision: 0 }, dynamics: "F", articulation: null, modifiers: [] },
        ],
      }],
    });
    invokeMock.mockResolvedValueOnce(snap);
    await store.addPatternHit("beat", "hihat", 2, 0, "F");
    expect(store.project.patterns[0].hits.length).toBe(3);
    expectInvoked("add_pattern_hit", { patternName: "beat", drum: "hihat", beat: 2, subdivision: 0 });
  });

  it("removes a pattern hit", async () => {
    const snap = makeSnapshot({
      patterns: [{ ...makePattern("beat"), hits: [{ drum: "kick", position: { beat: 1, subdivision: 0 }, dynamics: "MF", articulation: null, modifiers: [] }] }],
    });
    invokeMock.mockResolvedValueOnce(snap);
    await store.removePatternHit("beat", 1);
    expect(store.project.patterns[0].hits.length).toBe(1);
    expectInvoked("remove_pattern_hit", { patternName: "beat", hitIndex: 1 });
  });

  it("adds a new pattern", async () => {
    const snap = makeSnapshot({
      patterns: [makePattern("beat"), makePattern("fill")],
    });
    invokeMock.mockResolvedValueOnce(snap);
    await store.addPattern("fill", "drums", "acoustic_kit");
    expect(store.project.patterns.length).toBe(2);
    expect(store.selectedPatternName()).toBe("fill");
    expectInvoked("add_pattern", { name: "fill", instrumentCategory: "drums", instrumentName: "acoustic_kit" });
  });

  it("removes a pattern and clears selection", async () => {
    store.setSelectedPatternName("beat");
    const snap = makeSnapshot({ patterns: [] });
    invokeMock.mockResolvedValueOnce(snap);
    await store.removePattern("beat");
    expect(store.project.patterns.length).toBe(0);
    expect(store.selectedPatternName()).toBe(null);
    expectInvoked("remove_pattern", { patternName: "beat" });
  });

  it("sets pattern properties", async () => {
    const snap = makeSnapshot({
      patterns: [{ ...makePattern("beat"), transpose: 2, swing: 60, color: "#ff0000" }],
    });
    invokeMock.mockResolvedValueOnce(snap);
    await store.setPatternTranspose("beat", 2);
    expectInvoked("set_pattern_transpose", { patternName: "beat", transpose: 2 });

    invokeMock.mockResolvedValueOnce(snap);
    await store.setPatternSwing("beat", 60);
    expectInvoked("set_pattern_swing", { patternName: "beat", swing: 60 });

    invokeMock.mockResolvedValueOnce(snap);
    await store.setPatternColor("beat", "#ff0000");
    expectInvoked("set_pattern_color", { patternName: "beat", color: "#ff0000" });
  });
});

// ── Clip editing ───────────────────────────────────────────────────────────

describe("clip editing", () => {
  beforeEach(async () => {
    const snap = makeSnapshot({ tracks: [makeTrack("Drums", 0)], patterns: [makePattern("beat")] });
    invokeMock.mockResolvedValueOnce(snap);
    await store.initProject();
  });

  it("adds a clip", async () => {
    const snap = makeSnapshot({
      tracks: [{
        ...makeTrack("Drums", 0),
        clips: [{ pattern: "beat", time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 3, beat: 1, subdivision: 0 } } }],
      }],
      patterns: [makePattern("beat")],
    });
    invokeMock.mockResolvedValueOnce(snap);
    await store.addClip(0, "beat", 1, 1, 0, 3, 1, 0);
    expect(store.project.tracks[0].clips.length).toBe(1);
    expect(store.project.tracks[0].clips[0].pattern).toBe("beat");
    expectInvoked("add_clip", { trackIndex: 0, patternName: "beat", startBar: 1, startBeat: 1 });
  });

  it("removes a clip", async () => {
    const snapA = makeSnapshot({
      tracks: [{
        ...makeTrack("Drums", 0),
        clips: [{ pattern: "beat", time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 3, beat: 1, subdivision: 0 } } }],
      }],
      patterns: [makePattern("beat")],
    });
    invokeMock.mockResolvedValueOnce(snapA);
    await store.addClip(0, "beat", 1, 1, 0, 3, 1, 0);
    const snapB = makeSnapshot({ tracks: [{ ...makeTrack("Drums", 0), clips: [] }], patterns: [makePattern("beat")] });
    invokeMock.mockResolvedValueOnce(snapB);
    await store.removeClip(0, 0);
    expect(store.project.tracks[0].clips.length).toBe(0);
    expectInvoked("remove_clip", { trackIndex: 0, clipIndex: 0 });
  });

  it("moves a clip", async () => {
    const snapA = makeSnapshot({
      tracks: [{
        ...makeTrack("Drums", 0),
        clips: [{ pattern: "beat", time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 3, beat: 1, subdivision: 0 } } }],
      }],
      patterns: [makePattern("beat")],
    });
    invokeMock.mockResolvedValueOnce(snapA);
    await store.addClip(0, "beat", 1, 1, 0, 3, 1, 0);

    const snapB = makeSnapshot({
      tracks: [{
        ...makeTrack("Drums", 0),
        clips: [{ pattern: "beat", time_range: { start: { bar: 2, beat: 1, subdivision: 0 }, end: { bar: 4, beat: 1, subdivision: 0 } } }],
      }],
      patterns: [makePattern("beat")],
    });
    invokeMock.mockResolvedValueOnce(snapB);
    await store.moveClip(0, 0, 2, 1, 0);
    expect(store.project.tracks[0].clips[0].time_range.start.bar).toBe(2);
    expectInvoked("move_clip", { trackIndex: 0, clipIndex: 0, startBar: 2, startBeat: 1, startSub: 0 });
  });
});

// ── Effects / Buses ────────────────────────────────────────────────────────

describe("effects and buses", () => {
  beforeEach(async () => {
    const snap = makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
    invokeMock.mockResolvedValueOnce(snap);
    await store.initProject();
  });

  it("adds an effect to a track", async () => {
    const snap = makeSnapshot({
      tracks: [{ ...makeTrack("Piano", 0), effects: [makeEffect("Reverb")] }],
    });
    invokeMock.mockResolvedValueOnce(snap);
    await store.addEffect("track:0", "reverb");
    expect(store.project.tracks[0].effects.length).toBe(1);
    expectInvoked("add_effect", { target: "track:0", effectType: "reverb" });
  });

  it("removes an effect", async () => {
    const snap = makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
    invokeMock.mockResolvedValueOnce(snap);
    await store.removeEffect("track:0", 0);
    expectInvoked("remove_effect", { target: "track:0", effectIndex: 0 });
  });

  it("sets an effect parameter", async () => {
    const snap = makeSnapshot({
      tracks: [{ ...makeTrack("Piano", 0), effects: [makeEffect("Reverb", [["mix", { Number: 0.5 }]])] }],
    });
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "begin_parameter_gesture") return "effect-test-owner";
      if (command === "end_owned_undo_gesture") return true;
      return snap;
    });
    await store.setEffectParam("track:0", 0, "mix", "0.5");
    expectInvoked("set_effect_param_owned", { token: "effect-test-owner", target: "track:0", effectIndex: 0, paramName: "mix", paramValue: "0.5" });
  });

  it("adds and removes a bus", async () => {
    const snapAdd = makeSnapshot({
      buses: [{ name: "reverb_bus", routes: ["master"], effects: [] }],
    });
    invokeMock.mockResolvedValueOnce(snapAdd);
    await store.addBus("reverb_bus", ["master"]);
    expectInvoked("add_bus", { name: "reverb_bus" });

    const snapDel = makeSnapshot({ buses: [] });
    invokeMock.mockResolvedValueOnce(snapDel);
    await store.removeBus("reverb_bus");
    expectInvoked("remove_bus", { name: "reverb_bus" });
  });
});

// ── Preset management ──────────────────────────────────────────────────────

describe("presets", () => {
  beforeEach(async () => {
    invokeMock.mockResolvedValueOnce(makeSnapshot());
    await store.initProject();
  });

  it("adds a preset", async () => {
    const snap = makeSnapshot({
      presets: [
        { name: "Piano 1", source: "soundfont", plugin: null, program: 0, patch: null, file: null, root_note: null, extends: null, params: [], effects: [] },
        { name: "Synth Lead", source: "clap", plugin: "com.surge.synth", program: null, patch: null, file: null, root_note: null, extends: null, params: [], effects: [] },
      ],
    });
    invokeMock.mockResolvedValueOnce(snap);
    await store.addPreset("Synth Lead", "clap", { plugin: "com.surge.synth" });
    expect(store.project.presets.length).toBe(2);
    expectInvoked("add_preset", { name: "Synth Lead", source: "clap" });
  });

  it("removes a preset", async () => {
    const snap = makeSnapshot({ presets: [] });
    invokeMock.mockResolvedValueOnce(snap);
    await store.removePreset("Piano 1");
    expect(store.project.presets.length).toBe(0);
    expectInvoked("remove_preset", { name: "Piano 1" });
  });

  it("sets track preset", async () => {
    const snap = makeSnapshot({ tracks: [{ ...makeTrack("Piano", 0), preset: "Synth Lead" }] });
    invokeMock.mockResolvedValueOnce(snap);
    await store.setTrackPreset(0, "Synth Lead");
    expectInvoked("set_track_preset", { trackIndex: 0, presetName: "Synth Lead" });
  });

  it("sets preset params in one batch", async () => {
    const snap = makeSnapshot({
      presets: [{
        name: "Piano 1",
        source: "soundfont",
        plugin: null,
        program: 0,
        patch: null,
        file: null,
        root_note: null,
        extends: null,
        params: [
          ["scala_file", { String: "/tmp/tuning.scl" }],
          ["scala_root_note", { String: "C4" }],
        ],
        effects: [],
      }],
    });
    const ownership = createOwnedUndoFixture();
    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
      const admission = ownership.route(command, args);
      if (admission) return admission.value;
      if (ownership.mutation(command, args) === "set_preset_params") return snap;
      if (command === "get_undo_history") return [];
      throw new Error(`Unexpected preset batch IPC ${command}`);
    });

    await store.setPresetParams("Piano 1", [["scala_file", "/tmp/tuning.scl"], ["scala_root_note", "C4"]]);

    const args = getLastCallArgs("set_preset_params_owned");
    expect(args?.name).toBe("Piano 1");
    expect(args?.params).toEqual([["scala_file", "/tmp/tuning.scl"], ["scala_root_note", "C4"]]);
    expect(store.project.presets[0].params).toHaveLength(2);
  });

  it("returns a rejected preset batch so the caller can show it", async () => {
    const ownership = createOwnedUndoFixture();
    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
      const admission = ownership.route(command, args);
      if (admission) return admission.value;
      if (ownership.mutation(command, args) === "set_preset_params") throw new Error("Unknown OpenSynth parameter(s): osc.bogus");
      throw new Error(`Unexpected preset batch IPC ${command}`);
    });

    const error = await store.setPresetParams("Piano 1", [["osc.bogus", "1"]]);

    expect(error).toContain("osc.bogus");
    expect(store.error()).toContain("osc.bogus");
  });
});

// ── Tempo ──────────────────────────────────────────────────────────────────

describe("project properties", () => {
  beforeEach(async () => {
    invokeMock.mockResolvedValueOnce(makeSnapshot());
    await store.initProject();
  });

  it("sets tempo", async () => {
    const snap = makeSnapshot();
    snap.project.properties.tempo = 140;
    invokeMock.mockResolvedValueOnce(snap);
    await store.setTempo(140);
    expect(store.project.properties.tempo).toBe(140);
    expectInvoked("set_tempo", { tempo: 140 });
  });
});

// ── Undo / Redo ────────────────────────────────────────────────────────────

describe("undo / redo", () => {
  beforeEach(async () => {
    invokeMock.mockResolvedValueOnce(makeSnapshot());
    await store.initProject();
    // Clear undo to start fresh (newProject/initProject now clears it)
    store.clearUndoHistory();
  });

  it("undos an add-note and redos it", async () => {
    const afterAdd = makeSnapshot({
      tracks: [{
        ...makeTrack("Piano", 0),
        phrases: [{
          name: "melody",
          time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 4, beat: 4, subdivision: 0 } },
          events: [makeNote({ pitch: { note: "E", accidental: "Natural", octave: 4 } })],
        }],
      }],
    });
    invokeMock.mockResolvedValueOnce(afterAdd);
    await store.addNote(0, 0, "E", "", 4, "Quarter", "MF", 0);
    expect(store.project.tracks[0].phrases[0].events.length).toBe(1);

    // Undo — restores empty project
    invokeMock.mockResolvedValueOnce(makeSnapshot());
    await store.undo();
    expect(store.project.tracks.length).toBe(0);

    // Redo — back to 1 note
    invokeMock.mockResolvedValueOnce(afterAdd);
    await store.redo();
    expect(store.project.tracks[0].phrases[0].events.length).toBe(1);
  });

  it("multiple undos work", async () => {
    // Note 1
    const snap1 = makeSnapshot({
      tracks: [{ ...makeTrack("Piano", 0), phrases: [{ name: "melody", time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 4, beat: 4, subdivision: 0 } }, events: [makeNote()] }] }],
    });
    invokeMock.mockResolvedValueOnce(snap1);
    await store.addNote(0, 0, "C", "", 4, "Quarter", "MF", 0);

    // Note 2
    const snap2 = makeSnapshot({
      tracks: [{ ...makeTrack("Piano", 0), phrases: [{ name: "melody", time_range: { start: { bar: 1, beat: 1, subdivision: 0 }, end: { bar: 4, beat: 4, subdivision: 0 } }, events: [makeNote(), makeNote({ start_beat: 1, pitch: { note: "D", accidental: "Natural", octave: 4 } })] }] }],
    });
    invokeMock.mockResolvedValueOnce(snap2);
    await store.addNote(0, 0, "D", "", 4, "Quarter", "MF", 1);

    // Undo: back to snap1
    invokeMock.mockResolvedValueOnce(snap1);
    await store.undo();
    expect(store.project.tracks[0].phrases[0].events.length).toBe(1);

    // Undo: back to empty
    invokeMock.mockResolvedValueOnce(makeSnapshot());
    await store.undo();
    expect(store.project.tracks.length).toBe(0);
  });

  it("undo does nothing when history is empty", async () => {
    const before = store.project.name;
    await store.undo();
    expect(store.project.name).toBe(before);
  });

  it("redo does nothing when redo stack is empty", async () => {
    const before = store.project.name;
    await store.redo();
    expect(store.project.name).toBe(before);
  });
});

// ── Selected track / pattern ───────────────────────────────────────────────

describe("selection", () => {
  beforeEach(async () => {
    const snap = makeSnapshot({
      tracks: [makeTrack("Piano", 0), makeTrack("Drums", 1)],
      patterns: [makePattern("beat")],
    });
    invokeMock.mockResolvedValueOnce(snap);
    await store.initProject();
  });

  it("selectedTrack returns correct track", () => {
    store.setSelectedTrackIndex(1);
    expect(store.selectedTrack()?.name).toBe("Drums");
  });

  it("selectedTrack returns null when no selection", () => {
    store.setSelectedTrackIndex(null);
    expect(store.selectedTrack()).toBe(null);
  });

  it("selectedPattern finds pattern by name", () => {
    store.setSelectedPatternName("beat");
    const p = store.selectedPattern();
    expect(p?.name).toBe("beat");
    expect(p?.hits.length).toBe(2);
  });

  it("selectedPattern returns null for unknown name", () => {
    store.setSelectedPatternName("nonexistent");
    expect(store.selectedPattern()).toBe(null);
  });
});

// ── Transport ──────────────────────────────────────────────────────────────

describe("transport", () => {
  beforeEach(async () => {
    invokeMock.mockResolvedValueOnce(makeSnapshot());
    await store.initProject();
  });

  it("starts playing", async () => {
    invokeMock.mockResolvedValueOnce(TRANSPORT_PLAYING());
    await store.play();
    expect(store.transport().playing).toBe(true);
    expect(store.transport().position_beats).toBeGreaterThan(0);
    expectInvoked("transport_play");
  });

  it("pauses", async () => {
    invokeMock.mockResolvedValueOnce(TRANSPORT_PLAYING());
    await store.play();
    invokeMock.mockResolvedValueOnce({ ...TRANSPORT_STOPPED(), playing: false });
    await store.pause();
    expect(store.transport().playing).toBe(false);
    expectInvoked("transport_pause");
  });

  it("stops and resets position", async () => {
    invokeMock.mockResolvedValueOnce(TRANSPORT_PLAYING());
    await store.play();
    invokeMock.mockResolvedValueOnce(TRANSPORT_STOPPED());
    await store.stop();
    expect(store.transport().position_beats).toBe(0);
    expectInvoked("transport_stop");
  });

  it("seeks to a beat", async () => {
    const seeked = { ...TRANSPORT_STOPPED(), position_beats: 8 };
    invokeMock.mockResolvedValueOnce(seeked);
    await store.seek(8);
    expect(store.transport().position_beats).toBe(8);
    expectInvoked("transport_seek", { beat: 8 });
  });

  it("togglePlayPause starts when stopped", async () => {
    invokeMock.mockResolvedValueOnce(TRANSPORT_PLAYING());
    await store.togglePlayPause();
    expectInvoked("transport_play");
  });

  it("togglePlayPause pauses when playing", async () => {
    // First start playing
    invokeMock.mockResolvedValueOnce(TRANSPORT_PLAYING());
    await store.play();
    // Then toggle should pause
    invokeMock.mockResolvedValueOnce(TRANSPORT_STOPPED());
    await store.togglePlayPause();
    expectInvoked("transport_pause");
  });
});

// ── Error handling ─────────────────────────────────────────────────────────

describe("error handling", () => {
  beforeEach(async () => {
    invokeMock.mockResolvedValueOnce(makeSnapshot());
    await store.initProject();
  });

  it("sets error on addTrack failure", async () => {
    invokeMock.mockRejectedValueOnce(new Error("Track limit reached"));
    await store.addTrack("Extra", "piano", "grand");
    expect(store.error()).toContain("Track limit reached");
  });

  it("sets error on play failure", async () => {
    invokeMock.mockRejectedValueOnce(new Error("No SoundFont loaded"));
    await store.play();
    expect(store.error()).toContain("No SoundFont loaded");
  });

  it("clears error on successful operation", async () => {
    // First cause an error
    invokeMock.mockRejectedValueOnce(new Error("oops"));
    await store.addTrack("X", "piano", "grand");
    expect(store.error()).toBeTruthy();

    // Then succeed — error is cleared via applySnapshot
    const snap = makeSnapshot({ tracks: [makeTrack("Y", 0)] });
    invokeMock.mockResolvedValueOnce(snap);
    await store.addTrack("Y", "piano", "grand");
    expect(store.error()).toBe(null);
  });
});

// ── Derived state ──────────────────────────────────────────────────────────

describe("derived state", () => {
  beforeEach(async () => {
    invokeMock.mockResolvedValueOnce(makeSnapshot({ name: "Derived Project" }));
    await store.initProject();
  });

  it("project name is accessible", () => {
    expect(store.project.name).toBe("Derived Project");
  });

  it("dslSource has content", () => {
    expect(store.dslSource()).toContain("@project");
  });

  it("loading resets after init", () => {
    expect(store.loading()).toBe(false);
  });

  it("dirty is false after init", () => {
    expect(store.dirty()).toBe(false);
  });
});

// ── Project duration ───────────────────────────────────────────────────────

describe("project duration", () => {
  const withDuration = (duration: number, filePath: string | null = null) => ({
    ...makeSnapshot(),
    duration_seconds: duration,
    file_path: filePath,
  });

  it("is known from the opening snapshot before any playback", async () => {
    invokeMock.mockImplementation(async (cmd: string) =>
      cmd === "get_project" ? withDuration(8) : makeSnapshot());
    await store.initProject();
    expect(store.transport().duration_seconds).toBe(8);
    expect(store.transport().playing).toBe(false);
  });

  it("does not read the transport to learn the duration", async () => {
    invokeMock.mockImplementation(async (cmd: string) =>
      cmd === "get_project" ? withDuration(8) : makeSnapshot());
    await store.initProject();
    const reads = invokeMock.mock.calls.filter(([cmd]) => cmd === "get_transport_state");
    expect(reads).toHaveLength(0);
  });

  it("updates when a project file opens", async () => {
    invokeMock.mockImplementation(async (cmd: string) =>
      cmd === "open_file" ? withDuration(12.5, "/tmp/song.ocp") : makeSnapshot());
    await store.openFile("/tmp/song.ocp");
    expect(store.transport().duration_seconds).toBe(12.5);
  });
});

describe("preset audition", () => {
  afterEach(() => {
    vi.useRealTimers();
  });

  const loading = (preset: string) => ({
    ...TRANSPORT_STOPPED(),
    audition: {
      preset, status: "loading", ready_ms: null, instances: 1, loads: 1, error: null,
    },
  });
  const ready = (preset: string) => ({
    ...TRANSPORT_STOPPED(),
    audition_active: true,
    audition: {
      preset, status: "ready", ready_ms: 0.4, instances: 1, loads: 1, error: null,
    },
  });
  const drained = (preset: string) => ({
    ...TRANSPORT_STOPPED(),
    audition_active: false,
    audition: ready(preset).audition,
  });

  it.each([
    ["preset", () => store.previewPreset("Retained")],
    ["pattern", () => store.previewPattern("Retained")],
    ["clip", () => store.previewBlock(0, { bar: 1, beat: 1, subdivision: 0 }, { bar: 2, beat: 1, subdivision: 0 })],
    ["factory", () => store.auditionFactoryPatch("Retained", [])],
  ] as const)("%s toggles pause/resume at its retained private cursor", async (_kind, start) => {
    vi.useFakeTimers();
    await store.stop();
    let paused = false;
    const state = () => ({ ...ready("Retained"), audition_active: !paused,
      audition: { ...ready("Retained").audition, paused, position_seconds: 1.25 } });
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "pause_audition") { paused = true; return state(); }
      if (cmd === "resume_audition") { paused = false; return state(); }
      if (cmd === "get_transport_state") return state();
      if (cmd === "preview_block") return [];
      return TRANSPORT_STOPPED();
    });
    await start();
    await start();
    expectInvoked("pause_audition");
    await vi.advanceTimersByTimeAsync(400);
    expect(store.auditionActive()).toBe(true);
    expect(store.playbackPlaying()).toBe(false);
    expect(store.auditionPositionSeconds()).toBe(1.25);
    await store.togglePlayPause();
    expectInvoked("resume_audition");
    expect(store.playbackPlaying()).toBe(true);
    expect(store.auditionPositionSeconds()).toBe(1.25);
    await store.stop();
    expect(store.auditionActive()).toBe(false);
  });

  it("drum strikes own temporary playback and retrigger instead of toggling", async () => {
    await store.stop();
    invokeMock.mockImplementation(async (cmd: string) => cmd === "pause_audition"
      ? { ...ready("Kit"), audition: { ...ready("Kit").audition, paused: true, position_seconds: 0.1 } }
      : TRANSPORT_STOPPED());
    await store.previewDrumPad("Kit", 36, 100);
    await store.previewDrumPad("Kit", 36, 100);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "preview_drum_pad")).toHaveLength(2);
    expect(store.auditionActive()).toBe(true);
    await store.togglePlayPause();
    expectInvoked("pause_audition");
    expect(store.playbackPlaying()).toBe(false);
    await store.stop();
    expect(store.auditionActive()).toBe(false);
  });

  it("keeps a pad strike paused when Space arrives while parking the arrangement", async () => {
    await store.stop();
    let park!: (state: ReturnType<typeof TRANSPORT_STOPPED>) => void;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "transport_pause") {
        return new Promise<ReturnType<typeof TRANSPORT_STOPPED>>(resolve => { park = resolve; });
      }
      if (cmd === "resume_audition") return ready("Kit");
      return TRANSPORT_STOPPED();
    });
    await store.play();
    const strike = store.previewDrumPad("Kit", 36, 100);
    await store.togglePlayPause();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "transport_pause")).toHaveLength(1);
    park(TRANSPORT_STOPPED());
    await strike;
    expect(invokeMock).toHaveBeenCalledWith("preview_drum_pad", {
      presetName: "Kit", note: 36, velocity: 100, startPaused: true,
    });
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "resume_audition")).toHaveLength(0);
    expect(store.playbackPlaying()).toBe(false);
    await store.togglePlayPause();
    expectInvoked("resume_audition");
    expect(store.playbackPlaying()).toBe(true);
    await store.stop();
  });

  it("keeps the latest rapid private pause/resume intent", async () => {
    await store.stop();
    let finish!: (value: any) => void;
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "pause_audition") return new Promise(resolve => { finish = resolve; });
      if (cmd === "resume_audition") return Promise.resolve({ ...ready("Rapid"), audition: { ...ready("Rapid").audition, paused: false } });
      return Promise.resolve(TRANSPORT_STOPPED());
    });
    await store.previewPreset("Rapid");
    const first = store.togglePlayPause();
    await store.togglePlayPause();
    finish({ ...ready("Rapid"), audition: { ...ready("Rapid").audition, paused: true } });
    await first;
    expectInvoked("resume_audition");
    expect(store.playbackPlaying()).toBe(true);
    await store.stop();
  });

  it("an explicit arrangement play takes ownership from a private preview", async () => {
    await store.stop();
    invokeMock.mockImplementation(async (cmd: string) => cmd === "transport_play" ? TRANSPORT_PLAYING() : TRANSPORT_STOPPED());
    await store.previewPreset("Takeover");
    await store.play();
    expectInvoked("stop_audition");
    expectInvoked("transport_play");
    expect(store.auditionActive()).toBe(false);
    expect(store.transport().playing).toBe(true);
    await store.stop();
  });

  const previewStart = { bar: 3, beat: 1, subdivision: 0 };
  const previewEnd = { bar: 4, beat: 1, subdivision: 0 };

  it.each([
    ["preview_block", () => store.previewBlock(0, previewStart, previewEnd)],
    ["preview_pattern", () => store.previewPattern("Pattern")],
    ["preview_preset", () => store.previewPreset("Preset")],
    ["audition_factory_patch", () => store.auditionFactoryPatch("Factory", [])],
  ] as const)("keeps Space bound to %s while parking and loading", async (command, preview) => {
    vi.useFakeTimers();
    await store.stop();
    let park!: (snapshot: ReturnType<typeof TRANSPORT_STOPPED>) => void;
    let install!: (warnings: string[]) => void;
    let paused = true;
    const auditionState = () => ({
      ...TRANSPORT_STOPPED(),
      audition_active: !paused,
      audition: { ...ready("Track 1").audition, status: "idle", preset: null, paused, position_seconds: 0 },
    });
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "transport_play") return Promise.resolve(TRANSPORT_PLAYING());
      if (cmd === "transport_pause") return new Promise(resolve => { park = resolve; });
      if (cmd === command) return new Promise(resolve => { install = resolve; });
      if (cmd === "resume_audition") { paused = false; return Promise.resolve(auditionState()); }
      if (cmd === "get_transport_state") return Promise.resolve(auditionState());
      return Promise.resolve(TRANSPORT_STOPPED());
    });
    await store.play();
    invokeMock.mockClear();
    const operation = preview();
    await store.togglePlayPause();
    expect(store.playbackPlaying()).toBe(false);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "transport_pause")).toHaveLength(1);

    park(TRANSPORT_STOPPED());
    await vi.waitFor(() => expectInvoked(command, { startPaused: true }));
    await vi.advanceTimersByTimeAsync(500);
    expect(store.auditionActive()).toBe(true);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "resume_audition" || cmd === "pause_audition")).toBe(false);

    install([]);
    await operation;
    await vi.advanceTimersByTimeAsync(100);
    expect(store.auditionActive()).toBe(true);
    expect(store.playbackPlaying()).toBe(false);
    expect(store.audition()?.paused).toBe(true);

    await store.togglePlayPause();
    expectInvoked("resume_audition");
    expect(store.playbackPlaying()).toBe(true);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "transport_play")).toBe(false);
    await store.stop();
  });

  it("an arrangement seek cancels a preview before it finishes parking", async () => {
    await store.stop();
    let park!: (snapshot: ReturnType<typeof TRANSPORT_STOPPED>) => void;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "transport_pause") return new Promise(resolve => { park = resolve; });
      if (cmd === "transport_seek") return { ...TRANSPORT_STOPPED(), position_beats: 0 };
      return TRANSPORT_STOPPED();
    });
    await store.play();
    invokeMock.mockClear();
    const preview = store.previewPattern("Pending");
    expect(store.audition()?.status).toBe("loading");
    await store.seek(0);
    park(TRANSPORT_STOPPED());
    await preview;
    expect(store.audition()).toBeNull();
    expect(store.auditionActive()).toBe(false);
    expect(store.transport().position_beats).toBe(0);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "preview_pattern")).toBe(false);
    await store.stop();
  });

  it("shows loading as soon as the click is accepted, before the invoke resolves", async () => {
    let finish!: () => void;
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "preview_preset") return new Promise<void>(resolve => { finish = resolve; });
      return Promise.resolve(TRANSPORT_STOPPED());
    });
    const pending = store.previewPreset("Lead Warm");
    expect(store.audition()?.status).toBe("loading");
    expect(store.audition()?.preset).toBe("Lead Warm");
    await vi.waitFor(() => expect(finish).toBeTypeOf("function"));
    finish();
    await pending;
    await new Promise(resolve => setTimeout(resolve, 50));
    await store.stopAudition();
  });

  it("reports readiness through the transport state poll", async () => {
    let current: ReturnType<typeof ready> | ReturnType<typeof drained> = ready("Lead Warm");
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "preview_preset") return undefined;
      if (cmd === "stop_audition") return undefined;
      if (cmd === "get_transport_state") return current;
      return makeSnapshot();
    });
    await store.previewPreset("Lead");
    expect(store.audition()?.status).toBe("loading");
    await new Promise(resolve => setTimeout(resolve, 60));
    expect(store.audition()?.status).toBe("ready");
    expect(store.audition()?.instances).toBe(1);
    // When the audition stops sounding the poll clears the status.
    current = drained("Lead Warm");
    await new Promise(resolve => setTimeout(resolve, 80));
    expect(store.audition()).toBeNull();
    expect(store.auditionActive()).toBe(false);
  });

  it("keeps the newest click loading while an older audition is in flight", async () => {
    vi.useFakeTimers();
    let current: any = loading("Bass");
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "preview_preset") return undefined;
      if (cmd === "get_transport_state") return current;
      return makeSnapshot();
    });
    await store.previewPreset("Lead");
    await store.previewPreset("Bass");
    await vi.advanceTimersByTimeAsync(40);
    expect(store.audition()?.preset).toBe("Bass");
    expect(store.audition()?.status).toBe("loading");
    current = ready("Bass");
    await vi.advanceTimersByTimeAsync(40);
    expect(store.audition()?.status).toBe("ready");
    current = drained("Bass Sub");
    await vi.advanceTimersByTimeAsync(60);
    expect(store.audition()).toBeNull();
  });
});

// ── Mixer routing edits ────────────────────────────────────────────────────

describe("mixer routing edits", () => {
  function baseProject() {
    return makeSnapshot({
      tracks: [{
        ...makeTrack("Keys", 0),
        mix: {
          volume_db: 0, pan: 0, mute: false, solo: false,
          sends: [{ target: "space", amount: 0.5 }],
        },
      }],
      buses: [
        { name: "space", routes: ["master"], effects: [] },
        { name: "glue", routes: ["master"], effects: [] },
      ],
    }).project;
  }

  const before = () => makeSnapshot(baseProject());

  function withSend(target: string, amount = 0.25) {
    const project = baseProject();
    project.tracks[0].mix.sends.push({ target, amount, pre_fader: false, pan: 0 });
    return makeSnapshot(project);
  }

  function withSendMoved(to: string) {
    const project = baseProject();
    project.tracks[0].mix.sends[0].target = to;
    return makeSnapshot(project);
  }

  beforeEach(async () => {
    invokeMock.mockResolvedValueOnce(before());
    await store.initProject();
    store.clearUndoHistory();
    invokeMock.mockClear();
  });

  it("adds a bus, returns its name and selects the applied snapshot", async () => {
    const project = baseProject();
    project.buses.push({ name: "glue_2", routes: ["master"], effects: [] });
    invokeMock.mockResolvedValueOnce(makeSnapshot(project));

    await expect(store.addBus("glue_2", ["master"])).resolves.toBe("glue_2");
    expect(invokeMock.mock.calls).toContainEqual([
      "add_bus", { name: "glue_2", routes: ["master"] },
    ]);
    expect(store.project.buses.map((bus) => bus.name)).toContain("glue_2");
  });

  it("returns null when the backend rejects a bus name", async () => {
    invokeMock.mockRejectedValueOnce(new Error("Bus 'glue_2' already exists"));
    await expect(store.addBus("glue_2", ["master"])).resolves.toBeNull();
    expect(store.error()).toContain("already exists");
    expect(store.project.buses.some((bus) => bus.name === "glue_2")).toBe(false);
  });

  it("re-points a send with one command", async () => {
    invokeMock.mockResolvedValueOnce(withSendMoved("glue"));
    await expect(store.setSendTarget(0, "space", "glue")).resolves.toBe(true);
    expectInvoked("set_send_target", {
      trackIndex: 0, fromBus: "space", toBus: "glue", sourceOwner: null, sourceOutput: null,
    });
    expect(store.project.tracks[0].mix.sends[0].target).toBe("glue");
  });

  it("rejects an in-use bus delete and keeps the project", async () => {
    invokeMock.mockRejectedValueOnce(
      new Error("Bus 'space' is in use (1 send); re-route it explicitly to delete"),
    );
    await expect(store.removeBus("space")).resolves.toBe(false);
    expectInvoked("remove_bus", { name: "space", reroute: false });
    expect(store.error()).toContain("in use");
    expect(store.project.buses.some((bus) => bus.name === "space")).toBe(true);
  });

  it("deletes a bus through the explicit re-route", async () => {
    const project = baseProject();
    project.buses = project.buses.filter((bus) => bus.name !== "space");
    project.tracks[0].mix.sends = [];
    invokeMock.mockResolvedValueOnce(makeSnapshot(project));

    await expect(store.removeBusReroute("space")).resolves.toBe(true);
    expectInvoked("remove_bus", { name: "space", reroute: true });
    expect(store.project.buses.some((bus) => bus.name === "space")).toBe(false);
  });

  it("commits one command per routing gesture", async () => {
    invokeMock.mockResolvedValueOnce(before());
    await store.setSendAmount(0, "space", 0.25);
    invokeMock.mockResolvedValueOnce(before());
    await store.setSendPreFader(0, "space", true);
    invokeMock.mockResolvedValueOnce(withSendMoved("glue"));
    await store.setSendTarget(0, "space", "glue");
    invokeMock.mockResolvedValueOnce(withSend("glue"));
    await store.addSend(0, "glue", 0.25);
    invokeMock.mockResolvedValueOnce(before());
    await store.removeSend(0, "space");

    const commands = invokeMock.mock.calls
      .map(([cmd]) => cmd)
      .filter((cmd) => cmd !== "get_undo_state" && cmd !== "get_undo_history");
    expect(commands).toEqual([
      "set_send", "set_send", "set_send_target", "add_send", "remove_send",
    ]);
  });

  it("undoes and redoes one routing edit at a time", async () => {
    const added = withSend("glue");
    invokeMock.mockResolvedValueOnce(added);
    await store.addSend(0, "glue", 0.25);
    expect(store.project.tracks[0].mix.sends).toHaveLength(2);

    invokeMock.mockResolvedValueOnce(before());
    await store.undo();
    expectInvoked("undo");
    expect(store.project.tracks[0].mix.sends).toHaveLength(1);

    invokeMock.mockResolvedValueOnce(added);
    await store.redo();
    expectInvoked("redo");
    expect(store.project.tracks[0].mix.sends).toHaveLength(2);
  });
});

describe("snapshot revisions", () => {
  /**
   * Return the module-level revision floor to the fixtures' revision. The store
   * keeps the highest admitted revision for the whole file, so a test that
   * feeds explicit revisions must leave the floor where the shared fixtures are.
   */
  afterEach(async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_project") return makeSnapshot();
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });
    await store.initProject();
    log.clear();
  });

  const atRevision = (revision: number, overrides?: Partial<Project>) => {
    const snap = makeSnapshot(overrides);
    snap.revision = revision;
    return snap;
  };

  it("refuses the older of two out-of-order replies and keeps the screen", () => {
    const tracks = [makeTrack("Drums", 0), makeTrack("Bass", 1)];
    const muted = atRevision(5, { name: "Muted", tracks });
    muted.dirty = true;
    expect(store.applyAiSnapshot(muted)).toBe(true);
    store.setSelectedTrackIndex(1);
    const selection = { trackIndex: 1, startBeat: 4, patternName: null };
    store.setEditorSelection(selection);
    log.clear();

    // The fader's reply was generated before the mute and lands after it.
    const preMute = atRevision(4, { name: "Pre-mute", tracks });
    preMute.dirty = false;
    expect(store.applyAiSnapshot(preMute)).toBe(false);

    expect(store.project.name).toBe("Muted");
    expect(store.dirty()).toBe(true);
    expect(store.selectedTrackIndex()).toBe(1);
    expect(store.editorSelection()).toEqual(selection);
    expect(log.entries()).toContainEqual(
      expect.objectContaining({ level: "warn", source: "project" }),
    );
    expect(log.entries().some((entry) => entry.message.includes("revision 4"))).toBe(true);
  });

  it("applies a repeated revision idempotently", () => {
    const first = atRevision(6, { name: "Saved" });
    first.dirty = true;
    expect(store.applyPreviewSnapshot(first)).toBe(true);
    log.clear();

    // A save's snapshot repeats the revision the store already shows.
    const saved = atRevision(6, { name: "Saved" });
    saved.dirty = false;
    expect(store.applyPreviewSnapshot(saved)).toBe(true);

    expect(store.project.name).toBe("Saved");
    expect(store.dirty()).toBe(false);
    expect(log.entries().some((entry) => entry.level === "warn")).toBe(false);
  });

  it("refuses a reply from the document an open replaced", async () => {
    expect(store.applyAiSnapshot(atRevision(3, { name: "Before open" }))).toBe(true);

    const opened = atRevision(4, { name: "Opened", tracks: [makeTrack("Drums", 0)] });
    opened.file_path = "/b.ocp";
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "open_file") return opened;
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });
    await store.openFile("/b.ocp");
    log.clear();

    // The load bumped the revision, so the replaced document's replies are older.
    expect(store.applyAiSnapshot(atRevision(3, { name: "Before open" }))).toBe(false);

    expect(store.project.name).toBe("Opened");
    expect(store.filePath()).toBe("/b.ocp");
    expect(log.entries().some((entry) => entry.message.includes("revision 3"))).toBe(true);
  });

  it("lets a load install a lower revision and refuses replies that arrive during it", async () => {
    expect(store.applyAiSnapshot(atRevision(9, { name: "Old document" }))).toBe(true);

    let finishOpen!: (snap: ReturnType<typeof makeSnapshot>) => void;
    // A load resets the expected revision, so the new document may start lower.
    const reopened = atRevision(1, { name: "Reopened" });
    reopened.file_path = "/reset.ocp";
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "open_file") return new Promise((resolve) => { finishOpen = resolve; });
      if (cmd === "get_transport_state") return Promise.resolve(TRANSPORT_STOPPED());
      return Promise.resolve(makeSnapshot());
    });
    const opening = store.openFile("/reset.ocp");
    for (let i = 0; i < 8 && !finishOpen; i++) await Promise.resolve();
    expect(finishOpen).toBeTypeOf("function");

    // A reply issued before the open describes the document being replaced.
    expect(store.applyAiSnapshot(atRevision(8, { name: "Stale during load" }))).toBe(false);
    expect(store.project.name).toBe("Old document");

    finishOpen(reopened);
    await opening;

    expect(store.project.name).toBe("Reopened");
    expect(store.filePath()).toBe("/reset.ocp");
  });
});

describe("document swap", () => {
  /** Two named clips; the second one is what an unrelated edit moves. */
  const clipsAt = (secondStartBar: number) => [
    {
      pattern: "a",
      time_range: {
        start: { bar: 1, beat: 1, subdivision: 0 },
        end: { bar: 3, beat: 1, subdivision: 0 },
      },
    },
    {
      pattern: "b",
      time_range: {
        start: { bar: secondStartBar, beat: 1, subdivision: 0 },
        end: { bar: secondStartBar + 2, beat: 1, subdivision: 0 },
      },
    },
  ];

  const snapshotWith = (revision: number, secondStartBar: number) => {
    const snap = makeSnapshot({
      tracks: [{ ...makeTrack("Piano", 0), phrases: [], clips: clipsAt(secondStartBar) }],
      patterns: [makePattern("a"), makePattern("b")],
      markers: [{ name: "verse", position: { bar: 2, beat: 1, subdivision: 0 } }],
    });
    snap.revision = revision;
    return snap;
  };

  it("does not rewrite the snapshot it was handed", () => {
    const first = snapshotWith(2010, 5);
    expect(store.applyAiSnapshot(first)).toBe(true);
    // A later reply that drops a clip must not reach back into the object the
    // caller still holds.
    const second = makeSnapshot({
      tracks: [{ ...makeTrack("Piano", 0), phrases: [], clips: clipsAt(5).slice(0, 1) }],
      markers: [],
    });
    second.revision = 2011;
    expect(store.applyAiSnapshot(second)).toBe(true);

    expect(first.project.tracks[0].clips).toHaveLength(2);
    expect(first.project.markers).toHaveLength(1);
    expect(store.project.tracks[0].clips).toHaveLength(1);
    expect(store.project.markers).toEqual([]);
  });
});

describe("owned effect mutation document admission", () => {
  it("closes a delayed begin token without editing a replacement document", async () => {
    let resolve!: (token: string) => void;
    const begun = new Promise<string>(done => { resolve = done; });
    let document = makeSnapshot({ name: "First" });
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "get_project") return document;
      if (command === "begin_parameter_gesture") return begun;
      if (command === "end_owned_undo_gesture") return true;
      return document;
    });
    await store.initProject();
    const pending = store.setEffectParam("track:0", 0, "mix", "0.5");
    await vi.waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "begin_parameter_gesture")).toBe(true));
    document = makeSnapshot({ name: "Replacement" });
    await store.initProject();
    resolve("stale-effect-owner");
    await pending;
    expect(invokeMock.mock.calls.some(([command]) => command === "set_effect_param_owned")).toBe(false);
    expectInvoked("end_owned_undo_gesture", { token: "stale-effect-owner" });
    expect(store.project.name).toBe("Replacement");
  });

  it("does not publish an old effect snapshot after same-name document replacement", async () => {
    let resolve!: (snapshot: ReturnType<typeof makeSnapshot>) => void;
    const rendered = new Promise<ReturnType<typeof makeSnapshot>>(done => { resolve = done; });
    let document = makeSnapshot({ name: "Same Name" });
    invokeMock.mockImplementation(async (command: string) => {
      if (command === "get_project") return document;
      if (command === "begin_parameter_gesture") return "effect-owner";
      if (command === "set_effect_param_owned") return rendered;
      if (command === "end_owned_undo_gesture") return true;
      return document;
    });
    await store.initProject();
    const pending = store.setEffectParam("track:0", 0, "mix", "0.5");
    await vi.waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "set_effect_param_owned")).toBe(true));
    document = makeSnapshot({ name: "Same Name", properties: { ...document.project.properties, tempo: 155 } });
    await store.initProject();
    resolve({ ...makeSnapshot({ name: "Stale Reply" }), revision: 1000000 });
    await pending;
    expect(store.project.name).toBe("Same Name");
    expect(store.project.properties.tempo).toBe(155);
    expectInvoked("end_owned_undo_gesture", { token: "effect-owner" });
  });
});


describe("parameter target stamp admission", () => {
  it("sends the original captured stamp when a same-type swap precedes deferred begin admission", async () => {
    const original = "target-before-swap" as import("../../types/project").ParameterTargetStamp;
    const replacement = "target-after-swap" as import("../../types/project").ParameterTargetStamp;
    let liveStamp = original;
    let release!: () => void;
    const barrier = new Promise<void>(resolve => { release = resolve; });
    const snapshot = { ...makeSnapshot(), parameter_target_stamp: original };
    invokeMock.mockImplementation(async (command: string, raw?: unknown) => {
      if (command === "get_project") return snapshot;
      if (command === "get_undo_history") return [];
      if (command === "get_transport_state") return TRANSPORT_STOPPED();
      if (command === "begin_parameter_gesture") {
        const args = raw as { expectedStamp: string };
        await barrier;
        if (args.expectedStamp !== liveStamp) throw new Error("Target changed before admission");
        return "unexpected-new-target-owner";
      }
      if (command === "end_owned_undo_gesture") return true;
      throw new Error(`Unexpected parameter admission IPC ${command}`);
    });
    await store.initProject();
    const pending = store.setEffectParam("track:0", 0, "factor", "0.2");
    await vi.waitFor(() => expect(invokeMock).toHaveBeenCalledWith("begin_parameter_gesture", {
      scope: "preset", expectedStamp: original,
    }));
    // A backend same-type move can precede its snapshot reaching the frontend.
    liveStamp = replacement;
    release();
    await pending;
    expect(invokeMock.mock.calls.some(([command]) => command === "set_effect_param_owned")).toBe(false);
    expect(invokeMock.mock.calls.some(([command]) => command === "end_owned_undo_gesture")).toBe(false);
    expect(store.parameterTargetStamp()).toBe(original);
  });

  it("updates the stamp only after snapshot admission and never restores it from an older reply", async () => {
    const original = "accepted-old-target" as import("../../types/project").ParameterTargetStamp;
    const replacement = "accepted-new-target" as import("../../types/project").ParameterTargetStamp;
    const initial = { ...makeSnapshot(), revision: 10, parameter_target_stamp: original };
    invokeMock.mockImplementation(async (command: string) => command === "get_project" ? initial : makeSnapshot());
    await store.initProject();
    const owner = store.parameterRequestOwner();
    store.applyAiSnapshot({ ...initial, revision: 11, parameter_target_stamp: replacement });
    expect(store.parameterTargetStamp()).toBe(replacement);
    expect(owner()).toBe(false);
    store.applyAiSnapshot(initial);
    expect(store.parameterTargetStamp()).toBe(replacement);
  });

  it("refuses parameter editing when old-server snapshots omit target admission", async () => {
    const legacy = makeSnapshot();
    Reflect.deleteProperty(legacy, "parameter_target_stamp");
    invokeMock.mockImplementation(async (command: string) => command === "get_project" ? legacy : makeSnapshot());
    await store.initProject();
    invokeMock.mockClear();
    await store.setEffectParam("track:0", 0, "factor", "0.2");
    expect(store.parameterTargetStamp()).toBeNull();
    expect(store.parameterRequestOwner()()).toBe(false);
    expect(invokeMock.mock.calls.some(([command]) => command === "begin_parameter_gesture" || command === "set_effect_param_owned")).toBe(false);
  });
});
