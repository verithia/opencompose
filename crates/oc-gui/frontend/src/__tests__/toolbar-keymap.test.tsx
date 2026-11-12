/**
 * The preferences keymap list is generated from the registry and follows the
 * keymap, including a rebinding made while the app runs.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { Toolbar } from "../components/Toolbar";
import { parseBinding } from "../stores/commands";
import { resetAllBindings, setBinding } from "../stores/keymap";
import { overlayStore } from "../stores/overlays";
import { preferencesUi } from "../stores/preferencesUi";
import { store } from "../stores/project";
import { themeStore } from "../stores/theme";
import { i18n } from "../stores/i18n";
import { askDialogMock, invokeMock, makeSnapshot, makeTrack, messageDialogMock, openDialogMock, TRANSPORT_STOPPED } from "./setup";

function bindingText(commandId: string): string | null {
  const row = document.querySelector(`.shortcut-row[data-command-id="${commandId}"]`);
  return row?.querySelector(".shortcut-binding")?.textContent ?? null;
}

/** Open Preferences, then the page that embeds the shortcut editor. */
async function openPreferences() {
  const view = render(() => <Toolbar />);
  fireEvent.click(view.container.querySelector(".preferences-btn")!);
  await new Promise((resolve) => setTimeout(resolve, 0));
  fireEvent.click(view.baseElement.querySelector('[data-page="keymap"]')!);
  await new Promise((resolve) => setTimeout(resolve, 0));
  return view;
}

beforeEach(async () => {
  i18n.setLocale("en");
  themeStore.setTheme("default", "dark");
  resetAllBindings();
  preferencesUi.close();
  invokeMock.mockClear();
  invokeMock.mockImplementation(async (command: string) => {
    if (command === "scan_plugins") return [];
    if (command === "get_transport_state") return TRANSPORT_STOPPED();
    if (command === "get_undo_state") {
      return { can_undo: false, can_redo: false, undo_label: null, redo_label: null, undo_labels: [], redo_labels: [] };
    }
    return makeSnapshot();
  });
  await store.initProject();
});

afterEach(() => {
  cleanup();
  overlayStore.closeShortcutEditor();
  resetAllBindings();
});

describe("preferences keymap list", () => {
  it("lists every bound command from the registry", async () => {
    await openPreferences();
    const rows = document.querySelectorAll(".shortcut-row");
    expect(rows.length).toBeGreaterThanOrEqual(30);
    expect(bindingText("file.save")).toBe("Cmd/Ctrl+S");
    expect(bindingText("edit.redo")).toBe("Cmd/Ctrl+Shift+Z");
    expect(bindingText("view.togglePianoRoll")).toBe("R");
  });

  it("follows a rebinding made after the list rendered", async () => {
    await openPreferences();
    expect(bindingText("file.save")).toBe("Cmd/Ctrl+S");

    setBinding("file.save", parseBinding("Mod+Shift+Y")!);
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(bindingText("file.save")).toBe("Cmd/Ctrl+Shift+Y");
    expect(bindingText("file.save")).not.toBe("Cmd/Ctrl+S");
  });

  it("embeds the editor surface instead of opening a second dialog", async () => {
    const view = await openPreferences();
    expect(document.querySelector(".shortcut-editor-search")).toBeTruthy();
    expect(document.querySelector(".shortcut-table")).toBeTruthy();
    expect(overlayStore.shortcutEditorOpen()).toBe(false);
    expect(view.baseElement.querySelector(".preferences-modal")).toBeTruthy();
  });
});

it("requires confirmation for an aligned import and publishes the committed result", async () => {
  const saved = { ...makeSnapshot({ tracks: [], sections: [] }), file_path: "/project/new.ocp", revision: 20 };
  const imported = { ...makeSnapshot({ tracks: [makeTrack("Drums", 0)] }), file_path: saved.file_path, revision: 21 };
  store.applyPreviewSnapshot(saved);
  invokeMock.mockImplementation(async (command) => command === "import_aligned_stems"
    ? { snapshot: imported, stem_count: 1, timestamped_stem_count: 0, midi_track_count: 0, midi: null }
    : TRANSPORT_STOPPED());
  openDialogMock.mockResolvedValue(["/export/Drums.wav"]);
  askDialogMock.mockClear().mockResolvedValueOnce(false).mockResolvedValueOnce(true);
  messageDialogMock.mockClear().mockResolvedValue(undefined);
  const view = render(() => <Toolbar />);
  const choose = () => {
    fireEvent.click(view.getByRole("button", { name: "More file actions" }));
    fireEvent.click(view.getByRole("menuitem", { name: "Import Aligned Stems…" }));
  };

  choose();
  await waitFor(() => expect(askDialogMock).toHaveBeenCalledOnce());
  expect(invokeMock).not.toHaveBeenCalledWith("import_aligned_stems", expect.anything());
  choose();
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("import_aligned_stems", {
    paths: ["/export/Drums.wav"], expectedRevision: 20,
  }));
  expect(messageDialogMock).toHaveBeenCalledWith("WAV stems imported · 1\nMIDI tracks imported (muted) · 0\nSome WAVs lack a Broadcast WAV start timestamp. Check that the stems share the same origin.", {
    title: "Import Aligned Stems…",
  });
  expect(store.projectRevision).toBe(21);
});
