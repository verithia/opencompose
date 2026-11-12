/**
 * The quit prompt is a modal. It owns Escape and the Tab ring while it is open,
 * starts on Save, and hands focus back to where it came from; a document that is
 * recording and a bottom panel stay untouched behind it.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot } from "../setup";
import { store } from "../../stores/project";
import { recordStore, adoptRecord } from "../../stores/record";
import { panelStore } from "../../stores/panels";
import { i18n } from "../../stores/i18n";
import App from "../../App";

const windowMock = vi.hoisted(() => ({
  destroy: vi.fn(),
  onCloseRequested: vi.fn(),
  setTitle: vi.fn(),
}));
vi.mock("@tauri-apps/api/window", () => ({ getCurrentWindow: () => windowMock }));

/** A dirty open project with an optional name; unknown commands answer with it. */
function mockBackend(name = "Test Project") {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "scan_plugins") return [];
    if (cmd === "prepare_quit") return { dirty: true, recording: recordStore.recording() };
    if (cmd === "save_file") return { path: "/old/song.ocp", snapshot: null };
    if (cmd === "discard_unsaved") {
      return { ...makeSnapshot({ name }), file_path: "/old/song.ocp", dirty: false };
    }
    return { ...makeSnapshot({ name }), file_path: "/old/song.ocp", dirty: true };
  });
}

async function requestQuit() {
  const view = render(() => <App />);
  await waitFor(() => expect(windowMock.onCloseRequested).toHaveBeenCalled());
  await waitFor(() => expect(store.dirty()).toBe(true));
  // Put focus somewhere real in the shell so the return trip is observable.
  const trigger = view.getByLabelText(i18n.t("toolbar.newProject"));
  trigger.focus();
  await windowMock.onCloseRequested.mock.calls.at(-1)![0]({ preventDefault: vi.fn() });
  await waitFor(() => expect(view.container.querySelector(".quit-dialog")).toBeTruthy());
  return { view, trigger, dialog: view.container.querySelector(".quit-dialog") as HTMLElement };
}

beforeEach(() => {
  vi.clearAllMocks();
  i18n.setLocale("en");
  windowMock.onCloseRequested.mockResolvedValue(() => {});
  windowMock.destroy.mockImplementation(() => {});
  mockBackend();
  adoptRecord({ recording: false, armed: null });
  panelStore.closeBottom();
});

afterEach(() => {
  adoptRecord({ recording: false, armed: null });
  panelStore.closeBottom();
  i18n.setLocale("en");
});

describe("the quit prompt as a modal", () => {
  it("is a named dialog, starts on Save, wraps Tab and returns focus", async () => {
    const { view, trigger, dialog } = await requestQuit();

    expect(dialog.getAttribute("role")).toBe("dialog");
    expect(dialog.getAttribute("aria-modal")).toBe("true");
    expect(dialog.getAttribute("aria-label")).toBe(i18n.t("dialog.unsavedTitle"));
    const save = dialog.querySelector(".quit-btn-save") as HTMLElement;
    const cancel = dialog.querySelector(".quit-btn-cancel") as HTMLElement;
    expect(document.activeElement).toBe(save);

    // Shift+Tab from the first control wraps to the last, Tab from the last
    // wraps back: the ring never leaves the dialog.
    fireEvent.keyDown(save, { key: "Tab", shiftKey: true });
    expect(document.activeElement).toBe(cancel);
    fireEvent.keyDown(cancel, { key: "Tab" });
    expect(document.activeElement).toBe(save);

    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(view.container.querySelector(".quit-overlay")).toBeNull());
    expect(document.activeElement).toBe(trigger);
    expect(store.dirty()).toBe(true);
  });

  it("does not stop a take or close the bottom panel behind it", async () => {
    const stop = vi.spyOn(recordStore, "stop").mockResolvedValue(null);
    adoptRecord({ recording: true, armed: "Keys" });
    panelStore.openBottom("clips");

    const { view } = await requestQuit();
    expect(panelStore.bottomPanel()).toBe("clips");

    fireEvent.keyDown(document.activeElement!, { key: "Escape" });
    await waitFor(() => expect(view.container.querySelector(".quit-overlay")).toBeNull());
    expect(stop).not.toHaveBeenCalled();
    expect(panelStore.bottomPanel()).toBe("clips");

    // The key is the shell's again once the dialog is gone: it stops the take,
    // and the press after that closes the panel.
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(stop).toHaveBeenCalledTimes(1));
    adoptRecord({ recording: false, armed: "Keys" });
    fireEvent.keyDown(document, { key: "Escape" });
    await waitFor(() => expect(panelStore.bottomPanel()).toBeNull());
  });

  it("names an untitled project in the active locale", async () => {
    i18n.setLocale("zh-Hans");
    mockBackend("");
    const { dialog } = await requestQuit();
    const body = dialog.querySelector(".quit-dialog-body")!;
    expect(body.textContent).toContain(i18n.t("status.untitled"));
    expect(body.textContent).not.toContain("Untitled");
  });
});
