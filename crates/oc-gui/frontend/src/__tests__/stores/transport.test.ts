/**
 * Transport polling and cursor logic tests.
 * Uses fake timers to test interval behavior and cursor visibility rules.
 */
import { describe, it, expect, beforeEach, afterEach, vi } from "vitest";
import { store } from "../../stores/project";
import { invokeMock, makeSnapshot, TRANSPORT_STOPPED, TRANSPORT_PLAYING } from "../setup";

describe("transport polling", () => {
  beforeEach(async () => {
    vi.useFakeTimers();
    invokeMock.mockClear();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_project") return makeSnapshot();
      if (cmd === "get_transport_state") return TRANSPORT_PLAYING();
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      return makeSnapshot();
    });
    await store.initProject();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("resumes the same preset button after button and Space pauses without retriggering", async () => {
    await store.stop();
    let paused = true;
    const snapshot = () => ({ ...TRANSPORT_STOPPED(), audition_active: !paused,
      audition: { preset: "Sustained", status: "ready", ready_ms: 0.2,
        instances: 1, loads: 1, error: null, paused, position_seconds: 0.125 } });
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "preview_preset") return undefined;
      if (cmd === "pause_audition") { paused = true; return snapshot(); }
      if (cmd === "resume_audition") { paused = false; return snapshot(); }
      if (cmd === "get_transport_state") return snapshot();
      return TRANSPORT_STOPPED();
    });
    invokeMock.mockClear();
    await store.previewPreset("Sustained");
    expect(store.playbackPlaying()).toBe(true);
    await store.previewPreset("Sustained");
    expect(store.audition()?.paused).toBe(true);
    await vi.advanceTimersByTimeAsync(400);
    await store.previewPreset("Sustained");
    expect(store.playbackPlaying()).toBe(true);
    expect(store.auditionPositionSeconds()).toBe(0.125);
    await store.togglePlayPause();
    expect(store.audition()?.paused).toBe(true);
    await vi.advanceTimersByTimeAsync(400);
    await store.previewPreset("Sustained");
    expect(store.playbackPlaying()).toBe(true);
    expect(store.auditionPositionSeconds()).toBe(0.125);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "preview_preset")).toHaveLength(1);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "pause_audition")).toHaveLength(2);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "resume_audition")).toHaveLength(3);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "transport_play")).toBe(false);
    await store.stop();
  });

  it.each(["pause", "resume", "space"] as const)("retires completed preset on %s without reviving the arrangement", async action => {
    await store.stop();
    let paused = false;
    let finished = false;
    const snapshot = () => ({ ...TRANSPORT_STOPPED(), audition_active: !paused && !finished,
      audition: { preset: "Finite", status: "ready", ready_ms: 0.2,
        instances: 1, loads: 1, error: null, paused, finished, position_seconds: finished ? 6 : 0.1 } });
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "preview_preset") { finished = false; paused = true; return undefined; }
      if (cmd === "pause_audition") { paused = !finished; return snapshot(); }
      if (cmd === "resume_audition") { paused = false; return snapshot(); }
      if (cmd === "get_transport_state") return snapshot();
      return TRANSPORT_STOPPED();
    });
    await store.play();
    await store.previewPreset("Finite");
    if (action !== "pause") await store.previewPreset("Finite");
    finished = true;
    const plays = invokeMock.mock.calls.filter(([cmd]) => cmd === "transport_play").length;
    if (action === "space") await store.togglePlayPause();
    else await store.previewPreset("Finite");
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "transport_play")).toHaveLength(plays);
    if (action === "resume") {
      expect(store.auditionActive()).toBe(true);
      expect(store.playbackPlaying()).toBe(true);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "preview_preset")).toHaveLength(2);
    } else {
      expect(store.auditionActive()).toBe(false);
      await store.previewPreset("Finite");
      expect(store.playbackPlaying()).toBe(true);
      expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "preview_preset")).toHaveLength(2);
    }
    await store.stop();
  });

  it("honors same-button resume coalesced behind a terminal pause response", async () => {
    await store.stop();
    let paused = true;
    let resolvePause!: (value: any) => void;
    const snapshot = (finished = false) => ({ ...TRANSPORT_STOPPED(), audition_active: !paused && !finished,
      audition: { preset: "Finite", status: "ready", ready_ms: 0.2, instances: 1,
        loads: 1, error: null, paused, finished, position_seconds: finished ? 6 : 0.1 } });
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "preview_preset") { paused = true; return undefined; }
      if (cmd === "resume_audition") { paused = false; return snapshot(); }
      if (cmd === "pause_audition") return new Promise(resolve => { resolvePause = resolve; });
      if (cmd === "get_transport_state") return snapshot();
      return TRANSPORT_STOPPED();
    });
    await store.play();
    await store.previewPreset("Finite");
    const plays = invokeMock.mock.calls.filter(([cmd]) => cmd === "transport_play").length;
    const pause = store.previewPreset("Finite");
    expect(resolvePause).toBeTypeOf("function");
    await store.previewPreset("Finite");
    resolvePause(snapshot(true));
    await pause;
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "preview_preset")).toHaveLength(2);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "pause_audition")).toHaveLength(1);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "transport_play")).toHaveLength(plays);
    expect(store.auditionActive()).toBe(true);
    expect(store.playbackPlaying()).toBe(true);
    await store.stop();
  });

  it("retires finished polling before a stale paused flag can retain the preset", async () => {
    await store.stop();
    let finished = false;
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "preview_preset") return undefined;
      if (cmd === "get_transport_state" || cmd === "resume_audition") return {
        ...TRANSPORT_STOPPED(), audition_active: !finished,
        audition: { preset: "Finite", status: "ready", ready_ms: 0.2, instances: 1,
          loads: 1, error: null, paused: finished, finished, position_seconds: 6 },
      };
      return TRANSPORT_STOPPED();
    });
    await store.previewPreset("Finite");
    finished = true;
    await vi.advanceTimersByTimeAsync(100);
    expect(store.auditionActive()).toBe(false);
    await store.stop();
  });

  it("keeps polling after a seek is refused by the recording backend", async () => {
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "transport_seek") throw new Error("Stop recording before moving the playhead.");
      if (cmd === "transport_play" || cmd === "get_transport_state") return TRANSPORT_PLAYING();
      return makeSnapshot();
    });
    await store.play();
    await store.seek(4);
    expect(store.error()).toContain("Stop recording");
    await vi.advanceTimersByTimeAsync(60);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "get_transport_state")).toBe(true);
    expect(store.transport().playing).toBe(true);
  });

  it("adopts a recovered take from the transport poll", async () => {
    const project = makeSnapshot({ name: "Recovered take" });
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "get_transport_state") return { ...TRANSPORT_STOPPED(), project };
      return makeSnapshot();
    });
    await store.play();
    await vi.advanceTimersByTimeAsync(60);
    expect(store.project.name).toBe("Recovered take");
  });

  it.each(["solo", "private"])("project replacement fences a pending %s start", async kind => {
    await store.stop();
    let finish!: (value: any) => void;
    invokeMock.mockImplementation(async cmd => {
      if (cmd === (kind === "solo" ? "preview_track" : "preview_preset")) return await new Promise(resolve => { finish = resolve; });
      if (cmd === "open_file") return { ...makeSnapshot({ name: "Replacement" }), duration_seconds: 12 };
      return TRANSPORT_STOPPED();
    });
    const pending = kind === "solo" ? store.previewTrack(0) : store.previewPreset("Old");
    await Promise.resolve();
    await store.openFile("/other/replacement.ocp");
    finish(kind === "solo" ? TRANSPORT_PLAYING() : undefined);
    await pending;
    await vi.advanceTimersByTimeAsync(200);
    expect(store.project.name).toBe("Replacement");
    expect(store.auditionActive()).toBe(false);
    expect(store.previewTrackIndex()).toBeNull();
    expect(store.transport().playing).toBe(false);
    expect(store.transport().position_seconds).toBe(0);
    expect(store.transport().duration_seconds).toBe(12);
  });

  it("shows an aggregated render warning with its count in the status bar", async () => {
    const line =
      "track 'drums': drum hit 'crash' plays General MIDI note 49, which no pad of this kit has; the hit is silent (256 occurrences)";
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "transport_play") return { ...TRANSPORT_PLAYING(), warnings: [line] };
      if (cmd === "get_transport_state") return TRANSPORT_PLAYING();
      return makeSnapshot();
    });
    await store.play();
    expect(store.error()).toBe(line);
  });

  it("failed file loading leaves cancelled preview stopped and reports the load error", async () => {
    await store.stop();
    let failPreview!: (error: Error) => void;
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "preview_preset") return await new Promise((_resolve, reject) => { failPreview = reject; });
      if (cmd === "open_file") throw new Error("File unavailable");
      return TRANSPORT_STOPPED();
    });
    const pending = store.previewPreset("Old");
    await Promise.resolve();
    await store.openFile("/missing.ocp");
    failPreview(new Error("Obsolete preview error"));
    await pending;
    expect(store.error()).toBe("Error: File unavailable");
    expect(store.auditionActive()).toBe(false);
    expect(store.transport().playing).toBe(false);
    expect(store.loading()).toBe(false);
  });

  it("new project fences an already dispatched private poll error", async () => {
    await store.stop();
    let finish!: (value: any) => void;
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "get_transport_state") return await new Promise(resolve => { finish = resolve; });
      if (cmd === "new_project") return makeSnapshot({ name: "Fresh" });
      return TRANSPORT_STOPPED();
    });
    await store.previewPreset("Old");
    await vi.advanceTimersByTimeAsync(50);
    await store.newProject();
    finish({ ...TRANSPORT_STOPPED(), audition: { preset: "Old", status: "error", error: "Old error" } });
    await Promise.resolve();
    expect(store.project.name).toBe("Fresh");
    expect(store.auditionActive()).toBe(false);
    expect(store.audition()).toBeNull();
    expect(store.error()).not.toBe("Old error");
  });

  it("natural private completion fences older outstanding playing polls", async () => {
    await store.stop();
    const polls: Array<(value: any) => void> = [];
    invokeMock.mockImplementation(async cmd => cmd === "get_transport_state"
      ? await new Promise(resolve => polls.push(resolve)) : TRANSPORT_STOPPED());
    await store.previewPreset("Ending");
    await vi.advanceTimersByTimeAsync(100);
    expect(polls).toHaveLength(2);
    const audition = { preset: "Ending", status: "ready", paused: false, ready_ms: 0, instances: 1, loads: 1, error: null };
    polls[1]({ ...TRANSPORT_STOPPED(), audition_active: false, audition });
    await Promise.resolve(); await Promise.resolve();
    polls[0]({ ...TRANSPORT_STOPPED(), audition_active: true, audition });
    await Promise.resolve(); await Promise.resolve();
    expect(store.auditionActive()).toBe(false);
    expect(store.audition()).toBeNull();
  });

  it("an old project read cannot overwrite a later file load", async () => {
    await store.stop();
    let finish!: (value: any) => void;
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "get_project") return await new Promise(resolve => { finish = resolve; });
      if (cmd === "open_file") return makeSnapshot({ name: "Newer" });
      return TRANSPORT_STOPPED();
    });
    const read = store.initProject();
    await store.openFile("/newer.ocp");
    finish(makeSnapshot({ name: "Stale" }));
    await read;
    expect(store.project.name).toBe("Newer");
  });

  it("applies the parity of Space presses after a pending seek", async () => {
    await store.stop();
    let finish!: (value: ReturnType<typeof TRANSPORT_STOPPED>) => void;
    invokeMock.mockImplementation(async cmd => cmd === "transport_seek"
      ? new Promise(resolve => { finish = resolve; })
      : cmd === "transport_play" ? { ...TRANSPORT_PLAYING(), position_beats: 4 } : TRANSPORT_STOPPED());
    invokeMock.mockClear();
    const seek = store.seek(4);
    const first = store.togglePlayPause();
    const second = store.togglePlayPause();
    finish({ ...TRANSPORT_STOPPED(), position_beats: 4 });
    await Promise.all([seek, first, second]);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "transport_play")).toBe(false);
    expect(store.transport().playing).toBe(false);
    expect(store.transport().position_beats).toBe(4);
    await store.togglePlayPause();
    expect(store.transport().playing).toBe(true);
    expect(store.transport().position_beats).toBe(4);
    await store.stop();
  });

  it("Stop cancels a Space action queued behind an outstanding seek", async () => {
    await store.stop();
    let finish!: (value: any) => void;
    invokeMock.mockImplementation(async cmd => cmd === "transport_seek"
      ? await new Promise(resolve => { finish = resolve; }) : TRANSPORT_STOPPED());
    const seeking = store.seek(0);
    const space = store.togglePlayPause();
    await store.stop();
    finish({ ...TRANSPORT_STOPPED(), position_beats: 0 });
    await seeking; await space;
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "transport_play")).toBe(false);
    expect(store.transport().playing).toBe(false);
  });

  it("restores arrangement extent after solo Stop, replacement and repeated Stop", async () => {
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "get_project") return { ...makeSnapshot(), duration_seconds: 210.3 };
      if (cmd === "preview_track") return { ...TRANSPORT_PLAYING(), duration_seconds: 190.6 };
      return { ...TRANSPORT_STOPPED(), duration_seconds: 190.6 };
    });
    await store.stop();
    await store.initProject();
    await store.previewTrack(0);
    expect(store.transport().duration_seconds).toBe(190.6);
    await store.previewTrack(1);
    await store.stop();
    expect(store.transport().duration_seconds).toBe(210.3);
    expect(store.transport().position_seconds).toBe(0);
    await store.stop();
    expect(store.transport().duration_seconds).toBe(210.3);
    await store.previewTrack(0);
    await store.stopAudition();
    expect(store.transport().duration_seconds).toBe(210.3);
  });

  it("Stop wins over a late timeline start reply", async () => {
    let finish!: (value: ReturnType<typeof TRANSPORT_PLAYING>) => void;
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "transport_play") return await new Promise(resolve => { finish = resolve; });
      return TRANSPORT_STOPPED();
    });
    const pending = store.play();
    await store.stop();
    finish(TRANSPORT_PLAYING());
    await pending;
    expect(store.transport().playing).toBe(false);
    expect(store.cursorActive()).toBe(false);
    expect(store.loading()).toBe(false);
  });

  it("Stop wins over a late solo preview reply", async () => {
    let finish!: (value: ReturnType<typeof TRANSPORT_PLAYING>) => void;
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "preview_track") return await new Promise(resolve => { finish = resolve; });
      return TRANSPORT_STOPPED();
    });
    const pending = store.previewTrack(0);
    await Promise.resolve();
    await store.stop();
    finish(TRANSPORT_PLAYING());
    await pending;
    expect(store.transport().playing).toBe(false);
    expect(store.previewTrackIndex()).toBeNull();
    expect(store.previewCursorBeats()).toBeNull();
  });

  it.each(["pause", "resume"])("retains a track preview toggle during a pending %s", async action => {
    const stopped = { ...TRANSPORT_STOPPED(), position_beats: 4 };
    const playing = { ...TRANSPORT_PLAYING(), position_beats: 12 };
    const paused = { ...playing, playing: false };
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "transport_stop") return stopped;
      return cmd === "transport_pause" ? paused : playing;
    });
    await store.stop();
    await store.previewTrack(0);
    if (action === "resume") await store.togglePlayPause();

    const firstCommand = action === "pause" ? "transport_pause" : "resume_track_preview";
    const secondCommand = action === "pause" ? "resume_track_preview" : "transport_pause";
    let finish!: (snapshot: typeof playing) => void;
    invokeMock.mockClear();
    invokeMock.mockImplementation(async cmd => cmd === firstCommand
      ? new Promise(resolve => { finish = resolve; })
      : cmd === "transport_pause" ? paused : playing);

    const firstToggle = store.togglePlayPause();
    await store.previewTrack(0);
    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).toEqual([firstCommand]);
    finish(action === "pause" ? paused : playing);
    await firstToggle;

    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).toEqual([firstCommand, secondCommand]);
    expect(store.previewTrackIndex()).toBe(0);
    expect(store.transport().playing).toBe(action === "pause");
    expect(store.previewCursorBeats()).toBe(12);
    expect(store.transport().position_beats).toBe(4);
    await store.stop();
  });

  it("parks the arrangement anchor before a dry key replaces a track preview", async () => {
    const parked = { ...TRANSPORT_STOPPED(), position_beats: 4, position_seconds: 2, duration_seconds: 32 };
    const solo = { ...TRANSPORT_PLAYING(), position_beats: 12 };
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "get_project") return {
        ...makeSnapshot({ presets: [{
          name: "Lead", source: "builtin", plugin: "builtin.open_synth",
          program: null, patch: null, file: null, root_note: null, extends: null,
          params: [], effects: [],
        }] }),
        duration_seconds: 32,
      };
      if (cmd === "preview_track" || cmd === "resume_track_preview") return solo;
      if (cmd === "scan_plugins") return [];
      return parked;
    });
    await store.initProject();
    await store.seek(4);
    await store.previewTrack(0);
    expect(store.previewCursorBeats()).toBe(12);

    let finishPause!: (snapshot: typeof parked) => void;
    invokeMock.mockClear();
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "transport_pause") return new Promise(resolve => { finishPause = resolve; });
      if (cmd === "open_synth_note_on") return { ...parked, duration_seconds: 8 };
      return parked;
    });
    const note = store.openSynthNoteOn("Lead", 60);
    const release = store.openSynthNoteOff("Lead", 60);
    const other = store.openSynthNoteOn("Lead", 64);
    const otherRelease = store.openSynthNoteOff("Lead", 64);
    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).toEqual(["transport_pause"]);
    finishPause(parked);
    await Promise.all([note, release, other, otherRelease]);

    expect(invokeMock.mock.calls.map(([cmd]) => cmd)).toEqual([
      "transport_pause", "transport_seek", "open_synth_note_on", "open_synth_note_on",
      "open_synth_note_off", "open_synth_note_off",
    ]);
    expect(invokeMock).toHaveBeenCalledWith("transport_seek", { beat: 4 });
    expect(store.transport().position_beats).toBe(4);
    expect(store.transport().playing).toBe(false);
    expect(store.transport().duration_seconds).toBe(32);
    expect(store.previewTrackIndex()).toBeNull();
    await store.togglePlayPause();
    expect(invokeMock.mock.calls.at(-1)?.[0]).toBe("transport_play");
  });

  it("Stop drops a track preview resume queued behind Pause", async () => {
    await store.stop();
    invokeMock.mockImplementation(async cmd => cmd === "preview_track" || cmd === "resume_track_preview"
      ? TRANSPORT_PLAYING() : TRANSPORT_STOPPED());
    await store.previewTrack(0);

    let finishPause!: (snapshot: ReturnType<typeof TRANSPORT_STOPPED>) => void;
    invokeMock.mockClear();
    invokeMock.mockImplementation(async cmd => cmd === "transport_pause"
      ? new Promise(resolve => { finishPause = resolve; }) : TRANSPORT_STOPPED());
    const pause = store.togglePlayPause();
    await store.togglePlayPause();
    await store.stop();
    finishPause(TRANSPORT_STOPPED());
    await pause;

    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "resume_track_preview")).toBe(false);
    expect(store.previewTrackIndex()).toBeNull();
    expect(store.transport().playing).toBe(false);
  });

  it("replacing a solo preview preserves the interrupted arrangement resume intent", async () => {
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "transport_play" || cmd === "preview_track") return TRANSPORT_PLAYING();
      return TRANSPORT_STOPPED();
    });
    await store.stop();
    await store.play();
    await store.previewTrack(0);
    await store.previewTrack(1);
    await store.stopAudition();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "transport_play")).toHaveLength(2);
    expect(store.transport().playing).toBe(true);
    await store.stop();
  });

  it("a late preset failure cannot clear a newer audition or resume the arrangement", async () => {
    let fail!: (reason: Error) => void;
    invokeMock.mockImplementation(async (cmd, args: any) => {
      if (cmd === "preview_preset" && args.presetName === "old") return await new Promise((_, reject) => { fail = reject; });
      return TRANSPORT_STOPPED();
    });
    await store.stop();
    const old = store.previewPreset("old");
    await Promise.resolve();
    await store.previewPreset("new");
    fail(new Error("old failed"));
    await old;
    expect(store.audition()?.preset).toBe("new");
    expect(store.auditionActive()).toBe(true);
    expect(store.error() ?? "").not.toContain("old failed");
    await store.stop();
  });

  it("a transport poll that resolves after Stop cannot revive playback", async () => {
    let poll!: (value: ReturnType<typeof TRANSPORT_PLAYING>) => void;
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "get_transport_state") return await new Promise(resolve => { poll = resolve; });
      return TRANSPORT_STOPPED();
    });
    await store.play();
    await vi.advanceTimersByTimeAsync(50);
    await store.stop();
    poll(TRANSPORT_PLAYING());
    await Promise.resolve();
    expect(store.transport().playing).toBe(false);
  });

  it("starts polling after play", async () => {
    await store.play();
    expect(store.transport().playing).toBe(true);

    // Advance timer — poll should fire at 50ms intervals
    const callsBefore = invokeMock.mock.calls.filter(([c]) => c === "get_transport_state").length;
    await vi.advanceTimersByTimeAsync(100);
    const callsAfter = invokeMock.mock.calls.filter(([c]) => c === "get_transport_state").length;
    expect(callsAfter).toBeGreaterThan(callsBefore);
  });

  it("keeps background warm-up progress from transport polling", async () => {
    await store.play();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") {
        return { ...TRANSPORT_PLAYING(), warming: true, prepared: false };
      }
      return makeSnapshot();
    });
    await vi.advanceTimersByTimeAsync(60);
    expect(store.transport().warming).toBe(true);
    expect(store.transport().prepared).toBe(false);
  });

  it("keeps the loading chip until the worker has audio", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_play") {
        return { ...TRANSPORT_PLAYING(), warming: true, prepared: false, starting: true };
      }
      if (cmd === "get_transport_state") {
        return { ...TRANSPORT_PLAYING(), warming: true, prepared: false, starting: true };
      }
      return makeSnapshot();
    });
    await store.play();
    expect(store.loading()).toBe(true);

    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") {
        return { ...TRANSPORT_PLAYING(), warming: false, prepared: true, starting: false };
      }
      return makeSnapshot();
    });
    await vi.advanceTimersByTimeAsync(60);
    expect(store.loading()).toBe(false);
  });

  it("honors pause while the first playback request is pending", async () => {
    let complete!: (value: ReturnType<typeof TRANSPORT_PLAYING>) => void;
    invokeMock.mockImplementation((cmd: string) => {
      if (cmd === "transport_play") return new Promise(resolve => { complete = resolve; });
      if (cmd === "get_transport_state") return Promise.resolve(TRANSPORT_PLAYING());
      return Promise.resolve(makeSnapshot());
    });
    const first = store.play();
    await store.togglePlayPause();
    expect(invokeMock.mock.calls.filter(([c]) => c === "transport_play")).toHaveLength(1);
    expect(invokeMock.mock.calls.filter(([c]) => c === "transport_pause")).toHaveLength(1);
    complete(TRANSPORT_PLAYING());
    await first;
    expect(store.transport().playing).not.toBe(true);
  });

  it("clears optimistic playback after a failed start and permits retry", async () => {
    invokeMock.mockRejectedValueOnce(new Error("render failed"));
    await store.play();
    expect(store.transport().playing).toBe(false);
    expect(store.cursorActive()).toBe(false);
    await store.togglePlayPause();
    expect(store.transport().playing).toBe(true);
  });

  it("stops polling after pause", async () => {
    await store.play();

    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_pause") return TRANSPORT_STOPPED();
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });
    await store.pause();

    // Clear call history and advance — no more polling should happen
    invokeMock.mockClear();
    await vi.advanceTimersByTimeAsync(200);
    const pollCalls = invokeMock.mock.calls.filter(([c]) => c === "get_transport_state").length;
    expect(pollCalls).toBe(0);
  });

  it.each([false, true])("keeps draining an active take after playback stops (explicit pause %s)", async (pause) => {
    const stopped = TRANSPORT_STOPPED();
    const record = {
      armed: "vox", recording: true, seconds: 1, report: null,
      disabled_reason: null, count_in_remaining_frames: 0,
    };
    let recording = true;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "transport_pause" || cmd === "get_transport_state") {
        return { ...stopped, record: { ...record, recording } };
      }
      return makeSnapshot();
    });
    await store.play();
    if (pause) await store.pause();
    await vi.advanceTimersByTimeAsync(250);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "get_transport_state")).toHaveLength(5);

    recording = false;
    await vi.advanceTimersByTimeAsync(200);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "get_transport_state")).toHaveLength(8);
  });

  it("stops polling after stop", async () => {
    await store.play();

    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_stop") return TRANSPORT_STOPPED();
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });
    await store.stop();

    invokeMock.mockClear();
    await vi.advanceTimersByTimeAsync(200);
    const pollCalls = invokeMock.mock.calls.filter(([c]) => c === "get_transport_state").length;
    expect(pollCalls).toBe(0);
  });

  it("auto-stops polling when playback finishes (after 3 consecutive stopped polls)", async () => {
    await store.play();

    // Simulate playback finishing: poll returns playing=false
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });

    // Advance past 3 poll cycles (3 * 50ms) so consecutive-stop threshold is met
    await vi.advanceTimersByTimeAsync(180);

    // Transport should now show not playing (auto-stopped)
    expect(store.transport().playing).toBe(false);

    // Further polling should stop
    invokeMock.mockClear();
    await vi.advanceTimersByTimeAsync(200);
    const pollCalls = invokeMock.mock.calls.filter(([c]) => c === "get_transport_state").length;
    expect(pollCalls).toBe(0);
  });

  it("cursor stays visible: transport.playing remains true during playback", async () => {
    // After play() resolves, transport should be playing
    await store.play();
    expect(store.transport().playing).toBe(true);
    expect(store.smoothPositionBeats()).toBeGreaterThanOrEqual(0);

    // After several poll cycles, should still be playing
    await vi.advanceTimersByTimeAsync(200);
    expect(store.transport().playing).toBe(true);
    // smoothPositionBeats should advance (not stuck at 0)
    // Note: position_beats from mock is 4.5
    expect(store.transport().position_beats).toBe(4.5);
  });

  it("cursor visible: smoothPositionBeats updates while playing", async () => {
    await store.play();
    const initial = store.smoothPositionBeats();

    // The smooth position should be set to the backend's reported position
    // after play resolves (4.5 from TRANSPORT_PLAYING mock)
    expect(initial).toBe(4.5);
  });

  it("cursorActive stays true after play even if transport stops", async () => {
    // Reset state first
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_stop") return TRANSPORT_STOPPED();
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      return makeSnapshot();
    });
    await store.stop();
    // cursorActive should be set to true when play() is called
    // and only reset on explicit stop()
    expect(store.cursorActive()).toBe(false);

    await store.play();
    expect(store.cursorActive()).toBe(true);
    expect(store.transport().playing).toBe(true);

    // Pause doesn't deactivate cursor
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_pause") return { playing: false, position_beats: 4.5, position_seconds: 2.1, duration_seconds: 30 };
      if (cmd === "get_transport_state") return { playing: false, position_beats: 4.5, position_seconds: 2.1, duration_seconds: 30 };
      return makeSnapshot();
    });
    await store.pause();
    expect(store.cursorActive()).toBe(true); // cursor stays visible at pause position

    // Only explicit stop deactivates
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_stop") return TRANSPORT_STOPPED();
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });
    await store.stop();
    expect(store.cursorActive()).toBe(false); // now hidden
  });

  it("cursor hidden only when stopped at zero (never played)", async () => {
    // Stop playback first (previous tests may leave it playing)
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_stop") return TRANSPORT_STOPPED();
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });
    await store.stop();

    const t = store.transport();
    expect(t.playing).toBe(false);
    expect(t.position_beats).toBe(0);
    expect(t.position_seconds).toBe(0);
    // This is the condition that hides cursor — returns -1 in cursorX memo
  });
});

describe("seek and position", () => {
  beforeEach(async () => {
    vi.useFakeTimers();
    invokeMock.mockClear();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_project") return makeSnapshot();
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });
    await store.initProject();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("seek updates position_beats immediately", async () => {
    const seekPos = 12.5;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_seek") return {
        playing: false, position_beats: seekPos, position_seconds: 6.82, duration_seconds: 30
      };
      return makeSnapshot();
    });

    await store.seek(seekPos);
    expect(store.transport().position_beats).toBe(seekPos);
  });

  it("seek activates cursor (cursorActive becomes true)", async () => {
    // Initially stopped — cursorActive may be false
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_stop") return TRANSPORT_STOPPED();
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });
    await store.stop();
    expect(store.cursorActive()).toBe(false);

    // Seek should NOT activate cursor (only play does)
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_seek") return {
        playing: false, position_beats: 8.0, position_seconds: 4.36, duration_seconds: 30
      };
      return makeSnapshot();
    });
    await store.seek(8.0);
    // After seek to a non-zero position, cursorActive is still false
    // (cursor visibility in the component uses cursorActive || playing)
    expect(store.cursorActive()).toBe(false);
    // But position_beats is updated
    expect(store.transport().position_beats).toBe(8.0);
  });

  it("play after seek starts from seeked position", async () => {
    // Seek to beat 8
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_seek") return {
        playing: false, position_beats: 8.0, position_seconds: 4.36, duration_seconds: 30
      };
      if (cmd === "transport_play") return {
        playing: true, position_beats: 8.0, position_seconds: 4.36, duration_seconds: 30, did_render: true, warnings: []
      };
      if (cmd === "get_transport_state") return {
        playing: true, position_beats: 8.5, position_seconds: 4.64, duration_seconds: 30
      };
      return makeSnapshot();
    });

    await store.seek(8.0);
    await store.play();

    expect(store.transport().playing).toBe(true);
    expect(store.transport().position_beats).toBe(8.0);
    expect(store.cursorActive()).toBe(true);
  });

  it("play sets cursorActive and transport.playing", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_play") return {
        playing: true, position_beats: 0, position_seconds: 0, duration_seconds: 30, did_render: true, warnings: []
      };
      if (cmd === "get_transport_state") return {
        playing: true, position_beats: 0.5, position_seconds: 0.27, duration_seconds: 30
      };
      return makeSnapshot();
    });

    await store.play();
    expect(store.transport().playing).toBe(true);
    expect(store.cursorActive()).toBe(true);
    // position_beats should match what transportPlay returned
    expect(store.transport().position_beats).toBe(0);
  });
});

describe("previewTrack transport", () => {
  beforeEach(async () => {
    vi.useFakeTimers();
    invokeMock.mockClear();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_project") return makeSnapshot();
      if (cmd === "get_transport_state" || cmd === "transport_stop") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });
    await store.stop();
    await store.initProject();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("previewTrack sets transport to playing", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "preview_track") return {
        playing: true, position_beats: 0, position_seconds: 0, duration_seconds: 10
      };
      if (cmd === "get_transport_state") return {
        playing: true, position_beats: 0.5, position_seconds: 0.27, duration_seconds: 10
      };
      return makeSnapshot();
    });

    await store.previewTrack(0);
    expect(store.transport().playing).toBe(true);
    expect(store.transport().duration_seconds).toBe(10);
  });

  it("previewTrack does not start poll if not playing", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "preview_track") return {
        playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 0
      };
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });

    await store.previewTrack(0);
    expect(store.transport().playing).toBe(false);

    // No polling should happen
    invokeMock.mockClear();
    await vi.advanceTimersByTimeAsync(200);
    const pollCalls = invokeMock.mock.calls.filter(([c]) => c === "get_transport_state").length;
    expect(pollCalls).toBe(0);
  });
});

describe("transport robustness", () => {
  beforeEach(async () => {
    vi.useFakeTimers();
    invokeMock.mockClear();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_project") return makeSnapshot();
      if (cmd === "get_transport_state") return TRANSPORT_PLAYING();
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "transport_stop") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });
    await store.initProject();
    // Reset transport state
    await store.stop();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it("does not auto-stop on a single transient playing=false poll", async () => {
    // Re-mock for play
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_PLAYING();
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      return makeSnapshot();
    });
    await store.play();

    // Simulate a single transient playing=false (e.g., during seek)
    let pollCount = 0;
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") {
        pollCount++;
        // Only first poll returns false, rest return true
        if (pollCount === 1) return TRANSPORT_STOPPED();
        return TRANSPORT_PLAYING();
      }
      return makeSnapshot();
    });

    // Advance past 3 poll cycles (150ms) — transient blip should reset
    await vi.advanceTimersByTimeAsync(150);
    // Transport should be playing again (second poll returned true)
    expect(store.transport().playing).toBe(true);

    // Polling should still be active
    invokeMock.mockClear();
    await vi.advanceTimersByTimeAsync(100);
    const pollCalls = invokeMock.mock.calls.filter(([c]) => c === "get_transport_state").length;
    expect(pollCalls).toBeGreaterThan(0);
  });

  it("auto-stops only after 3 consecutive not-playing polls", async () => {
    // Re-mock for play
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_PLAYING();
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      return makeSnapshot();
    });
    await store.play();

    // All polls now return not-playing
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });

    // After 3+ consecutive stopped polls (3 * 50ms = 150ms), auto-stop triggers
    await vi.advanceTimersByTimeAsync(200);

    // Polling should now be stopped
    invokeMock.mockClear();
    await vi.advanceTimersByTimeAsync(200);
    const lateCalls = invokeMock.mock.calls.filter(([c]) => c === "get_transport_state").length;
    expect(lateCalls).toBe(0);
  });

  it("previewTrack sets cursorActive so cursor is visible", async () => {
    // After stop(), cursorActive should be false
    expect(store.cursorActive()).toBe(false);

    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "preview_track") return TRANSPORT_PLAYING();
      if (cmd === "get_transport_state") return TRANSPORT_PLAYING();
      return makeSnapshot();
    });

    await store.previewTrack(0);
    expect(store.cursorActive()).toBe(true);
  });

  it("seek does not preserve playing state (no false-positive togglePlayPause)", async () => {
    // Re-mock for play
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_PLAYING();
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      return makeSnapshot();
    });
    await store.play();
    expect(store.transport().playing).toBe(true);

    // Seek -- backend returns playing=false (stopped for seek)
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_seek") return TRANSPORT_STOPPED();
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });

    await store.seek(10);
    // Transport should reflect backend truth: not playing
    expect(store.transport().playing).toBe(false);
  });

  it("togglePlayPause calls play when not playing", async () => {
    // Start in stopped state (from beforeEach stop())
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "get_transport_state") return TRANSPORT_PLAYING();
      return makeSnapshot();
    });

    expect(store.transport().playing).toBe(false);
    await store.togglePlayPause();
    expect(store.transport().playing).toBe(true);
  });

  it("togglePlayPause calls pause when playing", async () => {
    // Re-mock for play
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_PLAYING();
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      return makeSnapshot();
    });
    await store.play();
    expect(store.transport().playing).toBe(true);

    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "transport_pause") return TRANSPORT_STOPPED();
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });

    await store.togglePlayPause();
    expect(store.transport().playing).toBe(false);
  });
});

describe("streamed A/B readout", () => {
  const candidate = (label: string) => ({ label, params: [] as [string, string | null][], effects: [] });
  const pair = (pending = true) => ({ status: pending ? "unmeasured" : "matched", measurement_pending: pending,
    active: "b", a_label: "A", b_label: "B", a_lufs: pending ? null : -18, b_raw_lufs: -15,
    b_lufs: -18, raw_delta_lu: 3, applied_gain_db: -3, matched_delta_lu: 0,
    unmeasured: pending ? "both" : null, window_seconds: 4 });
  beforeEach(async () => {
    vi.useFakeTimers();
    invokeMock.mockImplementation(async () => TRANSPORT_STOPPED());
    await store.stop(); store.clearAbPair(); invokeMock.mockClear();
  });
  afterEach(async () => { await store.stop(); store.clearAbPair(); vi.useRealTimers(); });

  it("retains Space while parking the arrangement for a comparison", async () => {
    let finishPark!: (snapshot: ReturnType<typeof TRANSPORT_STOPPED>) => void;
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "transport_pause") {
        return new Promise<ReturnType<typeof TRANSPORT_STOPPED>>(resolve => { finishPark = resolve; });
      }
      if (cmd === "compare_factory_patches") return pair();
      return TRANSPORT_STOPPED();
    });
    await store.play();
    const preparing = store.compareFactoryPatches(candidate("A"), candidate("B"));
    await store.togglePlayPause();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "transport_pause")).toHaveLength(1);
    finishPark(TRANSPORT_STOPPED());
    expect(await preparing).toEqual(pair());
    expect(invokeMock).toHaveBeenCalledWith("compare_factory_patches", expect.objectContaining({ startPaused: true }));
    expect(store.abBusy()).toBe(false);
    expect(store.playbackPlaying()).toBe(false);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "resume_audition")).toHaveLength(0);
    await store.togglePlayPause();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "resume_audition")).toHaveLength(1);
  });

  it("clears an A/B start superseded by Stop while parking the arrangement", async () => {
    let finishPark!: (snapshot: ReturnType<typeof TRANSPORT_STOPPED>) => void;
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      if (cmd === "transport_pause") {
        return new Promise<ReturnType<typeof TRANSPORT_STOPPED>>(resolve => { finishPark = resolve; });
      }
      return TRANSPORT_STOPPED();
    });
    await store.play();
    const preparing = store.compareFactoryPatches(candidate("A"), candidate("B"));
    expect(store.abBusy()).toBe(true);
    await store.stop();
    finishPark(TRANSPORT_STOPPED());
    expect(await preparing).toBeNull();
    expect(store.abBusy()).toBe(false);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "compare_factory_patches")).toBe(false);
  });

  it("stays busy while the native pair installs after leaving a track preview", async () => {
    let finish!: (snapshot: ReturnType<typeof pair>) => void;
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "preview_track") return TRANSPORT_PLAYING();
      if (cmd === "compare_factory_patches") {
        return new Promise<ReturnType<typeof pair>>(resolve => { finish = resolve; });
      }
      return TRANSPORT_STOPPED();
    });
    await store.previewTrack(0);
    const preparing = store.compareFactoryPatches(candidate("A"), candidate("B"));
    for (let i = 0; i < 12; i++) await Promise.resolve();
    expect(finish).toBeTypeOf("function");
    expect(store.abBusy()).toBe(true);
    finish(pair());
    expect(await preparing).toEqual(pair());
    expect(store.abBusy()).toBe(false);
  });

  it("updates current streamed pair when measurement arrives, then stops reading", async () => {
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "compare_factory_patches") return pair();
      if (cmd === "get_ab_audition_state") return pair(false);
      if (cmd === "get_transport_state") return { ...TRANSPORT_STOPPED(), audition_active: true,
        audition: { preset: "B", status: "ready", paused: false } };
      return TRANSPORT_STOPPED();
    });
    await store.compareFactoryPatches(candidate("A"), candidate("B"));
    expect(store.abPair()?.measurement_pending).toBe(true);
    await vi.advanceTimersByTimeAsync(250);
    expect(store.abPair()?.status).toBe("matched");
    expect(store.abPair()?.a_lufs).toBe(-18);
    await vi.advanceTimersByTimeAsync(600);
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "get_ab_audition_state")).toHaveLength(1);
  });

  it("finishes naturally, receives late measurement without owner revival, then replays on switch", async () => {
    let active = false;
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "compare_factory_patches") return pair();
      if (cmd === "get_ab_audition_state") return pair(false);
      if (cmd === "switch_ab") { active = true; return { ...pair(false), active: "a" }; }
      if (cmd === "get_transport_state") return { ...TRANSPORT_STOPPED(), audition_active: active,
        audition: { preset: "B", status: "ready", paused: false, position_seconds: active ? 0.1 : 2 } };
      return TRANSPORT_STOPPED();
    });
    await store.compareFactoryPatches(candidate("A"), candidate("B"));
    await vi.advanceTimersByTimeAsync(100);
    expect(store.auditionActive()).toBe(false);
    expect(store.abPair()?.measurement_pending).toBe(true);
    await vi.advanceTimersByTimeAsync(200);
    expect(store.abPair()?.status).toBe("matched");
    expect(store.auditionActive()).toBe(false);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "stop_audition")).toBe(false);
    await store.switchAb();
    expect(store.auditionActive()).toBe(true);
    await vi.advanceTimersByTimeAsync(50);
    expect(store.auditionPositionSeconds()).toBe(0.1);
    await store.stop();
    expect(store.abPair()).toBeNull();
    const calls = invokeMock.mock.calls.filter(([cmd]) => cmd === "switch_ab").length;
    expect(await store.switchAb()).toBeNull();
    expect(invokeMock.mock.calls.filter(([cmd]) => cmd === "switch_ab")).toHaveLength(calls);
  });

  it("switching a paused A/B pair leaves it paused at its private cursor", async () => {
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "compare_factory_patches" || cmd === "switch_ab") return pair(false);
      if (cmd === "pause_audition" || cmd === "get_transport_state") return { ...TRANSPORT_STOPPED(), audition_active: false,
        audition: { preset: "B", status: "ready", paused: true, position_seconds: 1.25 } };
      return TRANSPORT_STOPPED();
    });
    await store.compareFactoryPatches(candidate("A"), candidate("B"));
    await store.togglePlayPause();
    invokeMock.mockClear();
    await store.switchAb();
    expect(store.auditionActive()).toBe(true);
    expect(store.playbackPlaying()).toBe(false);
    expect(store.auditionPositionSeconds()).toBe(1.25);
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "resume_audition")).toBe(false);
  });

  it.each(["stop", "project", "target"])("ignores an outstanding A/B read after %s takeover", async action => {
    let finish!: (value: any) => void;
    invokeMock.mockImplementation(async cmd => {
      if (cmd === "compare_factory_patches") return pair();
      if (cmd === "get_ab_audition_state") return await new Promise(resolve => { finish = resolve; });
      if (cmd === "new_project") return makeSnapshot();
      if (cmd === "get_transport_state") return { ...TRANSPORT_STOPPED(), audition_active: true,
        audition: { preset: "B", status: "ready", paused: false } };
      return TRANSPORT_STOPPED();
    });
    await store.compareFactoryPatches(candidate("A"), candidate("B"));
    await vi.advanceTimersByTimeAsync(200);
    if (action === "stop") await store.stop();
    else if (action === "project") await store.newProject();
    else await store.previewPreset("Other");
    finish(pair(false));
    await Promise.resolve(); await Promise.resolve();
    expect(store.abPair()?.status).not.toBe("matched");
    if (action !== "target") expect(store.auditionActive()).toBe(false);
  });
});
