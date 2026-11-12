/**
 * Keymap page: the embedded shortcut surface with its search, per-row reset and
 * restore-defaults action.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { KeymapPage } from "../components/preferences/KeymapPage";
import { i18n } from "../stores/i18n";
import { commandBindingLabel, keymapOverrides, resetAllBindings, setBinding } from "../stores/keymap";
import { parseBinding } from "../stores/commands";

// jsdom in this suite has no storage backend; the keymap only needs get/set/clear.
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

function row(commandId: string): HTMLElement | null {
  return document.querySelector(`.shortcut-row[data-command-id="${commandId}"]`);
}

beforeEach(() => {
  i18n.setLocale("en");
  window.localStorage.clear();
  resetAllBindings();
});

afterEach(() => {
  cleanup();
  resetAllBindings();
});

describe("the Keymap page", () => {
  it("embeds the searchable editor and lists every command", () => {
    const { container } = render(() => <KeymapPage />);

    expect(container.querySelector(".preferences-hint")?.textContent).toBe(
      i18n.t("preferences.keymap.hint"),
    );
    const search = container.querySelector(".shortcut-editor-search") as HTMLInputElement;
    expect(search).toBeTruthy();
    expect(container.querySelectorAll(".shortcut-row").length).toBeGreaterThanOrEqual(30);
  });

  it("filters the table with the search box", () => {
    const { container } = render(() => <KeymapPage />);
    const before = container.querySelectorAll(".shortcut-row").length;

    fireEvent.input(container.querySelector(".shortcut-editor-search")!, {
      target: { value: "save" },
    });

    const after = container.querySelectorAll(".shortcut-row").length;
    expect(after).toBeGreaterThan(0);
    expect(after).toBeLessThan(before);
    expect(container.textContent).toContain(i18n.t("command.file.save"));
  });

  it("resets one command and then restores every default", async () => {
    setBinding("file.save", parseBinding("Mod+Shift+Y")!);
    const { container } = render(() => <KeymapPage />);
    await Promise.resolve();

    const saveRow = row("file.save")!;
    expect(saveRow.querySelector(".shortcut-binding")?.textContent).toBe("Cmd/Ctrl+Shift+Y");
    const resetButton = saveRow.querySelector(".shortcut-row-reset") as HTMLButtonElement;
    expect(resetButton.disabled).toBe(false);

    fireEvent.click(resetButton);
    expect(commandBindingLabel("file.save")).toBe("Cmd/Ctrl+S");

    setBinding("edit.redo", parseBinding("Mod+Shift+K")!);
    expect(Object.keys(keymapOverrides()).length).toBeGreaterThan(0);
    fireEvent.click(container.querySelector(".shortcut-restore")!);
    fireEvent.click(container.querySelector(".shortcut-confirm-apply")!);
    expect(keymapOverrides()).toEqual({});
  });
});
