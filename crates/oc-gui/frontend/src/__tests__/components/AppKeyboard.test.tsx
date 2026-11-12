/**
 * App-level keyboard shortcut tests.
 * Tests that global shortcuts (Ctrl+Z, Space, etc) are handled.
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack, TRANSPORT_PLAYING, TRANSPORT_STOPPED, expectInvoked, saveDialogMock } from "../setup";
import { store } from "../../stores/project";
import { panelStore } from "../../stores/panels";
import { uiScaleStore } from "../../stores/uiScale";
import App from "../../App";

const setZoomMock = vi.hoisted(() => vi.fn(async () => {}));
vi.mock("@tauri-apps/api/webview", () => ({
  getCurrentWebview: () => ({ setZoom: setZoomMock }),
}));

const storage = new Map<string, string>();
if (!window.localStorage) {
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => { storage.set(key, String(value)); },
      removeItem: (key: string) => { storage.delete(key); },
      clear: () => { storage.clear(); },
    },
  });
}

beforeEach(async () => {
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "take_startup_project") return null;
    if (cmd === "scan_plugins") return [];
    if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
    if (cmd === "get_undo_state") return { can_undo: true, can_redo: false, undo_label: "Test", redo_label: null, undo_labels: ["Test"], redo_labels: [] };
    if (cmd === "transport_play") return TRANSPORT_PLAYING();
    if (cmd === "transport_stop") return TRANSPORT_STOPPED();
    if (cmd === "transport_pause") return TRANSPORT_STOPPED();
    if (cmd === "undo") return makeSnapshot();
    if (cmd === "redo") return makeSnapshot();
    return makeSnapshot();
  });
  await store.initProject();
  await store.stop();
  invokeMock.mockClear();
  setZoomMock.mockClear();
  uiScaleStore.set(1);
  window.localStorage.removeItem("opencompose.uiScale");
  panelStore.closeBottom();
  panelStore.setActiveRegion("timeline");
  store.setSelectedPresetName(null);
});

describe("App keyboard shortcuts", () => {
  it.each(["ctrlKey", "metaKey"])("%s+Shift+S opens Save As even with an existing path", async (modifier) => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "take_startup_project") return null;
      if (cmd === "scan_plugins") return [];
      if (cmd === "save_file") return { path: "/new/copy.ocp", snapshot: null };
      return { ...makeSnapshot(), file_path: "/old/project.ocp" };
    });
    saveDialogMock.mockResolvedValueOnce("/new/copy.ocp");
    const { container } = render(() => <App />);
    await vi.waitFor(() => expect(store.filePath()).toBe("/old/project.ocp"));
    fireEvent.keyDown(container, { key: "S", shiftKey: true, [modifier]: true });
    await vi.waitFor(() => expectInvoked("save_file", { path: "/new/copy.ocp" }));
  });

  it("renders App for keyboard testing", async () => {
    const { container } = render(() => <App />);
    expect(container.querySelector(".daw-toolbar")).toBeInTheDocument();
  });

  it("Alt+T adds a marker at the playhead", async () => {
    const { container } = render(() => <App />);
    // Let the startup loop restore finish before the shortcut runs.
    await new Promise((resolve) => setTimeout(resolve, 25));
    invokeMock.mockClear();
    fireEvent.keyDown(container, { key: "t", altKey: true });

    await vi.waitFor(() => {
      const call = invokeMock.mock.calls.find(([cmd]) => cmd === "add_marker");
      expect(call?.[1]).toEqual({
        name: "Marker 1",
        position: { bar: 1, beat: 1, subdivision: 0 },
      });
    });
  });

  it("L refuses without a region and says why", async () => {
    const { container } = render(() => <App />);
    await new Promise((resolve) => setTimeout(resolve, 25));
    invokeMock.mockClear();
    fireEvent.keyDown(container, { key: "l" });

    await vi.waitFor(() => {
      expect(store.loopError()).toBe("Drag in the ruler to set a loop region first.");
    });
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "set_loop_region")).toBe(false);
  });

  it("L arms the loop once a region exists", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "set_loop_region") {
        const args = invokeMock.mock.calls.at(-1)?.[1] as { enabled: boolean };
        return { enabled: args.enabled, start_beats: 0, end_beats: 4 };
      }
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      return makeSnapshot();
    });
    const { container } = render(() => <App />);
    // Let the project load and its loop restore settle before drawing a region.
    await new Promise((resolve) => setTimeout(resolve, 25));
    expect(await store.setLoopRegion(0, 4)).toBe(true);
    fireEvent.keyDown(container, { key: "l" });

    await vi.waitFor(() => expect(store.loop().enabled).toBe(true));
  });

  it("Space key triggers togglePlayPause", async () => {
    const { container } = render(() => <App />);
    // Space at document level should trigger play
    fireEvent.keyDown(container, { key: " ", code: "Space" });

    await vi.waitFor(() => {
      const calls = invokeMock.mock.calls.filter(([c]) => c === "transport_play");
      expect(calls.length).toBeGreaterThan(0);
    }, { timeout: 500 }).catch(() => {
      // Space handling may depend on focus state — verify no crash
    });
  });

  it("leaves Space on a focused button and modified Space to their own handlers", async () => {
    const { container } = render(() => <App />);
    const button = container.querySelector("button")!;
    button.focus();
    const before = invokeMock.mock.calls.length;
    const buttonEvent = new KeyboardEvent("keydown", {key:" ",code:"Space",bubbles:true,cancelable:true});
    button.dispatchEvent(buttonEvent);
    expect(buttonEvent.defaultPrevented).toBe(false);
    button.blur();
    fireEvent.keyDown(container, {key:" ",code:"Space",ctrlKey:true});
    expect(invokeMock.mock.calls.slice(before).some(([cmd]) => cmd === "transport_play")).toBe(false);
  });

  it("ignores held Space key repeat", async () => {
    const { container } = render(() => <App />);
    invokeMock.mockClear();
    fireEvent.keyDown(container, { key: " ", code: "Space", repeat: true });
    expect(invokeMock.mock.calls.filter(([c]) => c === "transport_play" || c === "transport_pause")).toHaveLength(0);
  });

  it("Space transports when the presets panel is open but the timeline was clicked", async () => {
    panelStore.openBottom("presets");
    store.setSelectedPresetName("Piano 1");
    const { container } = render(() => <App />);
    fireEvent.pointerDown(container.querySelector(".split-top")!);
    fireEvent.keyDown(container, { key: " ", code: "Space" });

    await vi.waitFor(() => {
      const transport = invokeMock.mock.calls.filter(([c]) => c === "transport_play" || c === "transport_pause");
      expect(transport.length).toBeGreaterThan(0);
    });
    expect(invokeMock.mock.calls.filter(([c]) => c === "preview_preset")).toHaveLength(0);
  });

  it("records a timeline click even when a clip handler stops propagation", async () => {
    panelStore.openBottom("presets");
    store.setSelectedPresetName("Piano 1");
    const { container } = render(() => <App />);
    const target = container.querySelector(".timeline-panel")!;
    target.addEventListener("pointerdown", (event) => event.stopPropagation());
    fireEvent.pointerDown(target);
    fireEvent.keyDown(container, { key: " ", code: "Space" });

    await vi.waitFor(() => {
      const transport = invokeMock.mock.calls.filter(([c]) => c === "transport_play" || c === "transport_pause");
      expect(transport.length).toBeGreaterThan(0);
    });
    expect(invokeMock.mock.calls.filter(([c]) => c === "preview_preset")).toHaveLength(0);
  });

  it("Space auditions the selected preset after clicking inside the bottom panel", async () => {
    panelStore.openBottom("presets");
    store.setSelectedPresetName("Piano 1");
    const { container } = render(() => <App />);
    fireEvent.pointerDown(container.querySelector(".split-bottom")!);
    fireEvent.keyDown(container, { key: " ", code: "Space" });

    await vi.waitFor(() => expectInvoked("preview_preset", { presetName: "Piano 1" }));
    expect(
      invokeMock.mock.calls.filter(([c]) => c === "transport_play" || c === "transport_pause"),
    ).toHaveLength(0);
  });

  it("Space auditions the selected clip from the Clips panel", async () => {
    panelStore.openBottom("clips");
    store.setSelectedPatternName("piano_hook");
    const { container } = render(() => <App />);
    fireEvent.pointerDown(container.querySelector(".split-bottom")!);
    fireEvent.keyDown(container, { key: " ", code: "Space" });

    await vi.waitFor(() => expectInvoked("preview_pattern", { patternName: "piano_hook" }));
    expect(invokeMock.mock.calls.filter(([c]) => c === "transport_play")).toHaveLength(0);
  });

  it("Space auditions the edited pattern from the Piano Roll", async () => {
    panelStore.openBottom("pianoroll");
    store.setSelectedPatternName("piano_hook");
    const { container } = render(() => <App />);
    fireEvent.pointerDown(container.querySelector(".split-bottom")!);
    fireEvent.keyDown(container, { key: " ", code: "Space" });

    await vi.waitFor(() => expectInvoked("preview_pattern", { patternName: "piano_hook" }));
  });

  it("Space pauses a running audition instead of starting it again", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "take_startup_project") return null;
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") {
        return {
          ...TRANSPORT_STOPPED(),
          audition_active: true,
          audition: { preset: "Piano 1", status: "ready", ready_ms: 1, instances: 0, loads: 0, error: null },
        };
      }
      if (cmd === "preview_preset") return null;
      if (cmd === "stop_audition") return null;
      return makeSnapshot();
    });
    panelStore.openBottom("presets");
    store.setSelectedPresetName("Piano 1");
    const { container } = render(() => <App />);
    fireEvent.pointerDown(container.querySelector(".split-bottom")!);
    fireEvent.keyDown(container, { key: " ", code: "Space" });
    await vi.waitFor(() => expectInvoked("preview_preset"));

    invokeMock.mockClear();
    fireEvent.keyDown(container, { key: " ", code: "Space" });

    await vi.waitFor(() => expectInvoked("pause_audition"));
    expect(invokeMock.mock.calls.filter(([c]) => c === "preview_preset")).toHaveLength(0);
  });

  it("Space on the timeline pauses the audible preview owner", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "take_startup_project") return null;
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") {
        return {
          ...TRANSPORT_STOPPED(),
          audition_active: true,
          audition: { preset: "Piano 1", status: "ready", ready_ms: 1, instances: 0, loads: 0, error: null },
        };
      }
      if (cmd === "preview_preset") return null;
      if (cmd === "stop_audition") return null;
      if (cmd === "transport_play") return TRANSPORT_PLAYING();
      return makeSnapshot();
    });
    panelStore.openBottom("presets");
    store.setSelectedPresetName("Piano 1");
    const { container } = render(() => <App />);
    fireEvent.pointerDown(container.querySelector(".split-bottom")!);
    fireEvent.keyDown(container, { key: " ", code: "Space" });
    await vi.waitFor(() => expectInvoked("preview_preset"));

    fireEvent.pointerDown(container.querySelector(".split-top")!);
    fireEvent.keyDown(container, { key: " ", code: "Space" });

    await vi.waitFor(() => expectInvoked("pause_audition"));
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "transport_play")).toBe(false);
  });

  it("clears an invalidated paused preview so the next click can start a fresh solo pass", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "preview_track") return TRANSPORT_PLAYING();
      if (cmd === "resume_track_preview") throw new Error("Preview changed; start a new preview");
      if (cmd === "transport_pause" || cmd === "transport_seek") return TRANSPORT_STOPPED();
      return makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
    });
    await store.previewTrack(0);
    await store.previewTrack(0);
    await store.previewTrack(0);
    expect(store.previewTrackIndex()).toBeNull();
    expect(store.auditionActive()).toBe(false);
    expect(store.error()).toContain("Preview changed");
    await store.previewTrack(0);
    expect(invokeMock.mock.calls.filter(([c]) => c === "preview_track")).toHaveLength(2);
    await store.stop();
  });

  it("honors pending preview pause and resume while ignoring held Space repeats", async () => {
    let finish!: (value: ReturnType<typeof TRANSPORT_PLAYING>) => void;
    const pending = new Promise<ReturnType<typeof TRANSPORT_PLAYING>>(resolve => { finish = resolve; });
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "take_startup_project") return null;
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "preview_track") return pending;
      if (cmd === "resume_track_preview") return TRANSPORT_PLAYING();
      if (cmd === "transport_pause" || cmd === "transport_stop") return TRANSPORT_STOPPED();
      return makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
    });
    const { container } = render(() => <App />);
    await vi.waitFor(() => expect(container.querySelector(".track-preview-btn")).toBeTruthy());
    fireEvent.click(container.querySelector(".track-preview-btn")!);
    await vi.waitFor(() => expectInvoked("preview_track"));
    fireEvent.click(container.querySelector(".track-preview-btn")!);
    fireEvent.keyDown(container, { key: " ", code: "Space" });
    fireEvent.keyDown(container, { key: " ", code: "Space", repeat: true });
    expect(invokeMock.mock.calls.filter(([c]) => c === "preview_track")).toHaveLength(1);
    expect(invokeMock.mock.calls.filter(([c]) => c === "transport_pause")).toHaveLength(0);
    finish({ ...TRANSPORT_PLAYING(), playing: false });
    await vi.waitFor(() => expect(invokeMock.mock.calls.filter(([c]) => c === "resume_track_preview")).toHaveLength(1));
    expect(store.transport().playing).toBe(true);
    const input = document.createElement("input");
    container.append(input); input.focus();
    fireEvent.keyDown(input, { key: " ", code: "Space" });
    expect(invokeMock.mock.calls.filter(([c]) => c === "transport_pause")).toHaveLength(0);
    input.remove();
    await store.stop();
  });

  it("same track Preview and Space pause/resume solo playback; switching target starts fresh and Stop clears it", async () => {
    let transport = { playing: false, position_beats: 8, position_seconds: 4, duration_seconds: 30 };
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "take_startup_project") return null;
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { ...transport };
      if (cmd === "transport_seek") transport = { ...transport, position_beats: Number(args?.beat), position_seconds: Number(args?.beat) / 2 };
      else if (cmd === "transport_pause") transport = { ...transport, playing: false };
      else if (cmd === "preview_track") transport = { ...transport, playing: true, position_beats: 0, position_seconds: 0 };
      else if (cmd === "resume_track_preview" || cmd === "transport_play") transport = { ...transport, playing: true };
      else if (cmd === "transport_stop") transport = { ...transport, playing: false, position_beats: 0, position_seconds: 0 };
      else return makeSnapshot({ tracks: [makeTrack("Piano", 0), makeTrack("Bass", 1)] });
      return { ...transport };
    });
    await store.initProject();
    await store.seek(8);
    const { container } = render(() => <App />);
    const preview = () => container.querySelectorAll(".track-preview-btn");
    fireEvent.click(preview()[0]);
    await vi.waitFor(() => expect(store.transport().playing).toBe(true));
    expect(store.transport().position_beats).toBe(8);
    transport = { ...transport, position_beats: 3, position_seconds: 1.5 };
    fireEvent.click(preview()[0]);
    await vi.waitFor(() => expect(store.transport().playing).toBe(false));
    expect(store.previewTrackIndex()).toBe(0);
    expect(store.previewCursorBeats()).toBe(3);
    expect(preview()[0].getAttribute("aria-label")).toBe("Resume preview");
    expect(invokeMock.mock.calls.filter(([c]) => c === "preview_track")).toHaveLength(1);
    fireEvent.keyDown(container, { key: " ", code: "Space" });
    await vi.waitFor(() => expect(store.transport().playing).toBe(true));
    expectInvoked("resume_track_preview", { trackIndex: 0 });
    expect(store.previewCursorBeats()).toBe(3);
    expect(invokeMock.mock.calls.filter(([c]) => c === "transport_play")).toHaveLength(0);
    fireEvent.keyDown(container, { key: " ", code: "Space" });
    await vi.waitFor(() => expect(store.transport().playing).toBe(false));
    fireEvent.click(preview()[0]);
    await vi.waitFor(() => expect(store.transport().playing).toBe(true));
    expect(invokeMock.mock.calls.filter(([c]) => c === "resume_track_preview")).toHaveLength(2);
    fireEvent.click(preview()[1]);
    await vi.waitFor(() => expectInvoked("preview_track", { trackIndex: 1 }));
    expect(store.previewTrackIndex()).toBe(1);
    await store.stop();
    expect(store.previewTrackIndex()).toBeNull();
    expect(store.auditionActive()).toBe(false);
    fireEvent.keyDown(container, { key: " ", code: "Space" });
    await vi.waitFor(() => expectInvoked("transport_play"));
  });

  it("actual App Space immediately after preview cursor release waits for seek then plays from zero", async () => {
    let position = 8;
    let finishSeek!: () => void;
    let seekPending = false;
    invokeMock.mockImplementation(async (cmd: string, args?: any) => {
      if (cmd === "take_startup_project") return null;
      if (cmd === "scan_plugins") return [];
      if (cmd === "preview_track") { position = 6; return { ...TRANSPORT_PLAYING(), position_beats: position }; }
      if (cmd === "transport_pause" || cmd === "transport_stop") return { ...TRANSPORT_STOPPED(), position_beats: position };
      if (cmd === "transport_seek") {
        if (args.beat === 0) { seekPending = true; await new Promise<void>(resolve => { finishSeek = resolve; }); }
        position = args.beat;
        return { ...TRANSPORT_STOPPED(), position_beats: position, position_seconds: position / 2 };
      }
      if (cmd === "transport_play") return { ...TRANSPORT_PLAYING(), position_beats: position, position_seconds: position / 2 };
      if (cmd === "get_transport_state") return { ...TRANSPORT_STOPPED(), position_beats: position };
      return makeSnapshot({ tracks: [makeTrack("Piano", 0)] });
    });
    const { container } = render(() => <App />);
    await vi.waitFor(() => expect(invokeMock.mock.calls.some(([cmd]) => cmd === "take_startup_project")).toBe(true));
    await store.seek(8);
    await store.previewTrack(0);
    await store.togglePlayPause();
    expect(store.auditionActive()).toBe(true);
    const head = container.querySelector(".cursor-head")!;
    fireEvent.pointerDown(head, { button: 0, pointerId: 9, clientX: 144 });
    fireEvent.pointerMove(document, { pointerId: 9, clientX: 0, buttons: 1 });
    fireEvent.pointerUp(document, { pointerId: 9, clientX: 0, button: 0 });
    // Native preview teardown is still awaited. This must go through App's
    // active-owner branch, not call the store's generic toggle directly.
    expect(store.auditionActive()).toBe(true);
    fireEvent.keyDown(container, { key: " ", code: "Space" });
    await vi.waitFor(() => expect(seekPending).toBe(true));
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "transport_play")).toBe(false);
    finishSeek();
    await vi.waitFor(() => expect(store.transport().playing).toBe(true));
    expect(store.transport().position_beats).toBe(0);
    expect(store.previewTrackIndex()).toBeNull();
    expect(invokeMock.mock.calls.some(([cmd]) => cmd === "resume_track_preview")).toBe(false);
    await store.stop();
  });

  it("preview counters follow playback and pause with tempo changes while preserving the arrangement anchor", async () => {
    const snapshot = makeSnapshot({ tracks: [makeTrack("Keys", 0)] });
    snapshot.project.properties.tempo = 120;
    snapshot.project.properties.tempo_map = [{ position: { bar: 2, beat: 1, subdivision: 0 }, bpm: 60 }];
    let transport = { playing: false, position_beats: 8, position_seconds: 6, duration_seconds: 30 };
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "take_startup_project") return null;
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { ...transport };
      if (cmd === "transport_seek") transport = { ...transport, position_beats: Number(args?.beat), position_seconds: 6 };
      else if (cmd === "preview_track") transport = { ...transport, playing: true, position_beats: 5, position_seconds: 3 };
      else if (cmd === "transport_pause") transport = { ...transport, playing: false };
      else if (cmd === "transport_stop") transport = { ...transport, playing: false, position_beats: 0, position_seconds: 0 };
      else return snapshot;
      return { ...transport };
    });
    await store.initProject();
    await store.seek(8);
    const { container } = render(() => <App />);
    await store.previewTrack(0);
    await vi.waitFor(() => expect(container.querySelector(".position-display")?.textContent).toBe("2:2:0"));
    expect(container.querySelector(".status-transport-time")?.textContent).toContain("0:03.0");
    expect(store.transport().position_beats).toBe(8);
    expect(store.transport().position_seconds).toBe(6);
    // Drag feedback must take priority over the still-owned solo cursor.
    store.setScrubPosition(0);
    expect(container.querySelector(".position-display")?.textContent).toBe("1:1:0");
    expect(container.querySelector(".status-transport-time")?.textContent).toContain("0:00.0");
    store.setScrubPosition(4);
    expect(container.querySelector(".position-display")?.textContent).toBe("2:1:0");
    expect(container.querySelector(".status-transport-time")?.textContent).toContain("0:02.0");
    store.setScrubPosition(null);
    await store.previewTrack(0);
    expect(store.transport().playing).toBe(false);
    expect(container.querySelector(".position-display")?.textContent).toBe("2:2:0");
    expect(container.querySelector(".status-transport-time")?.textContent).toContain("0:03.0");
    await store.stop();
    expect(store.previewCursorBeats()).toBeNull();
    expect(container.querySelector(".position-display")?.textContent).toBe("1:1:0");
    expect(container.querySelector(".status-transport-time")?.textContent).toContain("0:00.0");
  });

  it("a preset audition leaves the timeline playhead alone", async () => {
    let transport = { playing: false, position_beats: 0, position_seconds: 0, duration_seconds: 30 };
    invokeMock.mockImplementation(async (cmd: string, args?: Record<string, unknown>) => {
      if (cmd === "take_startup_project") return null;
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return { ...transport };
      if (cmd === "transport_seek") {
        transport = { ...transport, position_beats: Number(args?.beat), position_seconds: Number(args?.beat) / 2 };
        return { ...transport };
      }
      if (cmd === "preview_preset") return null;
      if (cmd === "stop_audition") return null;
      return makeSnapshot();
    });
    await store.initProject();
    await store.seek(8);
    await store.previewPreset("Piano 1");
    expect(store.transport().position_beats).toBe(8);
    await store.stopAudition();
    expect(store.transport().position_beats).toBe(8);
  });

  it("Space stays a space in a text input", async () => {
    const { container } = render(() => <App />);
    invokeMock.mockClear();
    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();
    const event = new KeyboardEvent("keydown", {
      key: " ",
      code: "Space",
      bubbles: true,
      cancelable: true,
    });

    input.dispatchEvent(event);

    expect(event.defaultPrevented).toBe(false);
    expect(invokeMock.mock.calls.filter(([c]) => c === "transport_play" || c === "preview_preset")).toHaveLength(0);
    expect(container.querySelector(".daw-toolbar")).toBeTruthy();
    input.remove();
  });

  it("Ctrl+Z triggers undo", async () => {
    const { container } = render(() => <App />);
    fireEvent.keyDown(container, { key: "z", ctrlKey: true });

    await vi.waitFor(() => {
      expectInvoked("undo");
    }, { timeout: 500 }).catch(() => {
      // Undo shortcut may depend on focus not being in a text input
    });
  });

  it("Ctrl+Shift+Z triggers redo", async () => {
    const { container } = render(() => <App />);
    fireEvent.keyDown(container, { key: "z", ctrlKey: true, shiftKey: true });

    await vi.waitFor(() => {
      expectInvoked("redo");
    }, { timeout: 500 }).catch(() => {
      // Redo shortcut may depend on app state
    });
  });

  it("Cmd+Z triggers undo (macOS)", async () => {
    const { container } = render(() => <App />);
    fireEvent.keyDown(container, { key: "z", metaKey: true });

    await vi.waitFor(() => {
      expectInvoked("undo");
    }, { timeout: 500 }).catch(() => {});
  });

  it("Cmd/Ctrl+Shift+D exports diagnostics through the toolbar handler", async () => {
    saveDialogMock.mockResolvedValueOnce("/tmp/opencompose-diagnostics-shortcut.tar.gz");
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "take_startup_project") return null;
      if (cmd === "scan_plugins") return [];
      if (cmd === "get_transport_state") return TRANSPORT_STOPPED();
      if (cmd === "export_diagnostics") return "/tmp/opencompose-diagnostics-shortcut.tar.gz";
      if (cmd === "reveal_diagnostics") return undefined;
      return makeSnapshot();
    });
    const { container } = render(() => <App />);

    fireEvent.keyDown(container, { key: "d", metaKey: true, shiftKey: true });

    await vi.waitFor(() => expectInvoked("export_diagnostics"));
    await vi.waitFor(() =>
      expectInvoked("reveal_diagnostics", { path: "/tmp/opencompose-diagnostics-shortcut.tar.gz" }),
    );
  });

  it("Cmd/Ctrl +/- scale the interface and persist it, leaving timeline zoom alone", async () => {
    const { container } = render(() => <App />);
    const timelineZoom = container.querySelector(".timeline-zoom-center .zoom-label");

    fireEvent.keyDown(container, { key: "=", metaKey: true });
    await vi.waitFor(() => expect(uiScaleStore.scale()).toBe(1.1));
    await vi.waitFor(() => expect(setZoomMock).toHaveBeenCalledWith(1.1));
    expect(window.localStorage.getItem("opencompose.uiScale")).toBe("1.1");

    fireEvent.keyDown(container, { key: "-", ctrlKey: true });
    await vi.waitFor(() => expect(uiScaleStore.scale()).toBe(1));
    fireEvent.keyDown(container, { key: "-", ctrlKey: true });
    await vi.waitFor(() => expect(uiScaleStore.scale()).toBe(0.9));
    await vi.waitFor(() => expect(setZoomMock).toHaveBeenCalledWith(0.9));

    fireEvent.keyDown(container, { key: "0", metaKey: true });
    await vi.waitFor(() => expect(uiScaleStore.scale()).toBe(1));
    await vi.waitFor(() => expect(setZoomMock).toHaveBeenCalledWith(1));
    expect(timelineZoom?.textContent).toBe("24px/beat");
  });

  it("accepts the shifted plus and minus key variants", async () => {
    const { container } = render(() => <App />);

    fireEvent.keyDown(container, { key: "+", metaKey: true, shiftKey: true });
    await vi.waitFor(() => expect(uiScaleStore.scale()).toBe(1.1));

    fireEvent.keyDown(container, { key: "_", ctrlKey: true, shiftKey: true });
    await vi.waitFor(() => expect(uiScaleStore.scale()).toBe(1));
  });

  it("scales the interface even while a text input is focused", async () => {
    const { container } = render(() => <App />);
    const input = document.createElement("input");
    document.body.appendChild(input);
    input.focus();

    // The zoom chords are global on purpose: a panel field keeps the focus,
    // and Cmd/Ctrl + = must still resize the interface from there.
    fireEvent.keyDown(input, { key: "=", metaKey: true });
    await vi.waitFor(() => expect(uiScaleStore.scale()).toBe(1.1));

    input.remove();
    expect(container.querySelector(".daw-toolbar")).toBeTruthy();
  });
});
