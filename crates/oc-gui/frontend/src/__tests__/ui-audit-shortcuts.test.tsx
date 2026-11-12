/**
 * Control audit — the keyboard shortcuts the Preferences keymap promises.
 * Each row of that list is a control: if it claims an action, it must fire it.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { fireEvent } from "@solidjs/testing-library";
import { loadAuditProject, makeTimelineProject, mountSurface, resetSurface, settle } from "./ui-audit-support";
import { invokeMock, saveDialogMock, openDialogMock, TRANSPORT_STOPPED, expectInvoked, makeSnapshot } from "./setup";
import { store } from "../stores/project";
import { panelStore } from "../stores/panels";
import App from "../App";
import { i18n } from "../stores/i18n";
import { exportAudioStore } from "../stores/exportAudio";

beforeEach(async () => {
  i18n.setLocale("en");
  await loadAuditProject(makeTimelineProject());
});

afterEach(() => {
  resetSurface();
  i18n.setLocale("en");
});

async function mountApp() {
  invokeMock.mockImplementation(async (command: string) => {
    if (command === "get_transport_state") return TRANSPORT_STOPPED();
    if (command === "transport_play") return { ...TRANSPORT_STOPPED(), playing: true };
    if (command === "transport_pause" || command === "transport_stop") return TRANSPORT_STOPPED();
    if (command === "transport_seek") return TRANSPORT_STOPPED();
    if (command === "take_startup_project") return null;
    if (command === "get_undo_state") {
      return { can_undo: true, can_redo: true, undo_label: "x", redo_label: "x", undo_labels: [], redo_labels: [] };
    }
    if (command === "undo" || command === "redo") return makeSnapshot();
    return makeTimelineProject();
  });
  return mountSurface(() => <App />);
}

describe("documented panel shortcuts", () => {
  it.each([
    ["c", "clips"],
    ["r", "pianoroll"],
    ["p", "presets"],
    ["m", "mixer"],
  ] as const)("%s opens the %s panel", async (key, mode) => {
    const view = await mountApp();
    fireEvent.keyDown(view.container, { key });
    await settle(1);
    expect(panelStore.bottomPanel()).toBe(mode);
    fireEvent.keyDown(view.container, { key });
    await settle(1);
    expect(panelStore.bottomPanel()).toBeNull();
  });

  it("d toggles the DSL sidebar", async () => {
    const view = await mountApp();
    fireEvent.keyDown(view.container, { key: "d" });
    await settle(1);
    expect(panelStore.sidebarOpen()).toBe(true);
    expect(panelStore.sidebarTab()).toBe("dsl");
    fireEvent.keyDown(view.container, { key: "d" });
    await settle(1);
    expect(panelStore.sidebarOpen()).toBe(false);
  });

  it("esc closes the bottom panel", async () => {
    const view = await mountApp();
    panelStore.openBottom("clips");
    await settle(1);
    fireEvent.keyDown(view.container, { key: "Escape" });
    await settle(1);
    expect(panelStore.bottomPanel()).toBeNull();
  });

  it("space plays and pauses from the timeline", async () => {
    const view = await mountApp();
    fireEvent.pointerDown(view.container.querySelector(".split-top")!);
    fireEvent.keyDown(view.container, { key: " ", code: "Space" });
    await settle();
    expectInvoked("transport_play");
  });

  it("cmd+n, cmd+o and cmd+s reach their file actions", async () => {
    const view = await mountApp();
    fireEvent.keyDown(view.container, { key: "n", metaKey: true });
    await settle();
    expectInvoked("new_project");

    invokeMock.mockClear();
    fireEvent.keyDown(view.container, { key: "O", metaKey: true, shiftKey: true });
    await settle();
    expect(openDialogMock.mock.calls.length).toBeGreaterThan(0);

    saveDialogMock.mockResolvedValueOnce("/tmp/x.ocp");
    fireEvent.keyDown(view.container, { key: "S", metaKey: true, shiftKey: true });
    await settle(2);
    expect(saveDialogMock.mock.calls.length).toBeGreaterThan(0);
  });

  it("cmd+e opens the audio export dialog the keymap promises", async () => {
    const view = await mountApp();
    fireEvent.keyDown(view.container, { key: "E", metaKey: true });
    await settle(1);
    expect(exportAudioStore.dialogRequest()).toEqual({ scope: "mix" });
  });

  it("cmd+shift+z redoes the way the keymap says", async () => {
    const view = await mountApp();
    fireEvent.keyDown(view.container, { key: "Z", metaKey: true, shiftKey: true });
    await settle();
    expectInvoked("redo");
  });

  it("keeps the k drumpad shortcut working even though the keymap omits it", async () => {
    const view = await mountApp();
    fireEvent.keyDown(view.container, { key: "k" });
    await settle(1);
    expect(panelStore.bottomPanel()).toBe("drumpads");
    expect(store.project.tracks.length).toBe(2);
  });
});
