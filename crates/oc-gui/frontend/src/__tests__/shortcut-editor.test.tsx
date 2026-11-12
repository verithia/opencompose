/**
 * Shortcut editor: capture, conflict refusal, per-row reset, restore defaults.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { ShortcutEditor } from "../components/ShortcutEditor";
import { resetRegistryForTests } from "../stores/commands";
import {
  commandBindingLabel,
  keymapOverrides,
  resetAllBindings,
} from "../stores/keymap";
import { overlayStore } from "../stores/overlays";
import { i18n } from "../stores/i18n";

const memoryStorage = new Map<string, string>();
if (!window.localStorage) {
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => memoryStorage.get(key) ?? null,
      setItem: (key: string, value: string) => { memoryStorage.set(key, String(value)); },
      removeItem: (key: string) => { memoryStorage.delete(key); },
      clear: () => { memoryStorage.clear(); },
    },
  });
}

function row(commandId: string): HTMLElement {
  return document.querySelector(`.shortcut-row[data-command-id="${commandId}"]`) as HTMLElement;
}

async function open() {
  overlayStore.openShortcutEditor();
  render(() => <ShortcutEditor />);
  await new Promise((resolve) => setTimeout(resolve, 0));
}

async function settle(rounds = 2) {
  for (let i = 0; i < rounds; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
  }
}

async function startRecording(commandId: string) {
  fireEvent.click(row(commandId).querySelector(".shortcut-binding")!);
  await settle(1);
}

beforeEach(() => {
  i18n.setLocale("en");
  window.localStorage.clear();
  resetAllBindings();
  overlayStore.closeShortcutEditor();
});

afterEach(() => {
  cleanup();
  overlayStore.closeShortcutEditor();
  resetAllBindings();
  window.localStorage.clear();
  resetRegistryForTests();
});

describe("shortcut editor", () => {
  it("rebinds a command and persists it immediately", async () => {
    await open();
    await startRecording("file.save");
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    await settle();

    expect(commandBindingLabel("file.save")).toBe("Cmd/Ctrl+K");
    expect(keymapOverrides()["file.save"]).toBe("Mod+K");
    expect(JSON.parse(window.localStorage.getItem("opencompose.keymap")!).bindings["file.save"]).toBe("Mod+K");
    expect(row("file.save").textContent).toContain("Cmd/Ctrl+K");
    expect(row("file.save").classList.contains("recording")).toBe(false);
  });

  it("refuses a conflict, names the other command and keeps the old binding", async () => {
    await open();
    await startRecording("file.save");
    fireEvent.keyDown(document.body, { key: "z", ctrlKey: true });
    await settle();

    const error = row("file.save").querySelector(".shortcut-error");
    expect(error?.textContent).toContain(i18n.t("command.edit.undo"));
    expect(commandBindingLabel("file.save")).toBe("Cmd/Ctrl+S");
    expect(keymapOverrides()["file.save"]).toBeUndefined();
    expect(overlayStore.shortcutEditorOpen()).toBe(true);
  });

  it("cancels a capture with Escape without closing the editor", async () => {
    await open();
    await startRecording("file.save");
    fireEvent.keyDown(document.body, { key: "Escape" });
    await settle();
    expect(overlayStore.shortcutEditorOpen()).toBe(true);
    expect(row("file.save").classList.contains("recording")).toBe(false);
    expect(commandBindingLabel("file.save")).toBe("Cmd/Ctrl+S");
  });

  it("resets one command", async () => {
    await open();
    await startRecording("file.save");
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    await settle();
    expect(commandBindingLabel("file.save")).toBe("Cmd/Ctrl+K");

    fireEvent.click(row("file.save").querySelector(".shortcut-row-reset")!);
    await settle();
    expect(commandBindingLabel("file.save")).toBe("Cmd/Ctrl+S");
    expect(row("file.save").textContent).toContain("Cmd/Ctrl+S");
  });

  it("restores every default only after the confirmation", async () => {
    await open();
    await startRecording("file.save");
    fireEvent.keyDown(document.body, { key: "k", ctrlKey: true });
    await settle();

    fireEvent.click(document.querySelector(".shortcut-restore")!);
    await settle(1);
    expect(document.querySelector(".shortcut-confirm")).toBeTruthy();
    // The confirmation owns the change: nothing is reset until it is accepted.
    expect(commandBindingLabel("file.save")).toBe("Cmd/Ctrl+K");

    fireEvent.click(document.querySelector(".shortcut-confirm-apply")!);
    await settle();
    expect(commandBindingLabel("file.save")).toBe("Cmd/Ctrl+S");
    expect(keymapOverrides()).toEqual({});
  });

  it("filters the table by label and category", async () => {
    await open();
    const search = document.querySelector(".shortcut-editor-search") as HTMLInputElement;
    search.value = "quantise";
    fireEvent.input(search);
    await settle(1);

    const rows = [...document.querySelectorAll<HTMLElement>(".shortcut-row")];
    expect(rows.map((entry) => entry.dataset.commandId)).toEqual(["pattern.quantise"]);
  });
});
