/**
 * Command palette: filtering, keyboard selection, Enter, Escape and the
 * disabled-row reason.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render } from "@solidjs/testing-library";
import { CommandPalette } from "../components/CommandPalette";
import { registerCommand, resetRegistryForTests } from "../stores/commands";
import { resetAllBindings } from "../stores/keymap";
import { overlayStore } from "../stores/overlays";
import { i18n } from "../stores/i18n";

function input(): HTMLInputElement {
  return document.querySelector(".command-palette-search") as HTMLInputElement;
}

function rows(): HTMLElement[] {
  return [...document.querySelectorAll<HTMLElement>(".command-palette-row")];
}

function type(value: string) {
  const field = input();
  field.value = value;
  fireEvent.input(field);
}

async function open() {
  overlayStore.openPalette();
  render(() => <CommandPalette />);
  await new Promise((resolve) => setTimeout(resolve, 0));
}

beforeEach(() => {
  i18n.setLocale("en");
  resetAllBindings();
  overlayStore.closePalette();
});

afterEach(() => {
  cleanup();
  overlayStore.closePalette();
  resetRegistryForTests();
});

describe("command palette", () => {
  it("filters by label and keeps the category readable", async () => {
    await open();
    expect(rows().length).toBeGreaterThan(40);

    type("undo");
    expect(rows().map((row) => row.dataset.commandId)).toEqual(["edit.undo"]);

    type("edit");
    expect(rows().some((row) => row.dataset.commandId === "edit.undo")).toBe(true);
    expect(rows().some((row) => row.dataset.commandId === "file.save")).toBe(false);

    type("no such command");
    expect(rows()).toHaveLength(0);
    expect(document.querySelector(".command-palette-empty")?.textContent).toBe(i18n.t("palette.empty"));
  });

  it("runs the selected command with Enter", async () => {
    const run = vi.fn();
    const unregister = registerCommand("edit.undo", run);
    try {
      await open();
      type("undo");
      fireEvent.keyDown(input(), { key: "Enter" });
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(run).toHaveBeenCalledTimes(1);
      expect(overlayStore.paletteOpen()).toBe(false);
    } finally {
      unregister();
    }
  });

  it("moves the selection with the arrow keys", async () => {
    const first = vi.fn();
    const second = vi.fn();
    const unregister = [
      registerCommand("edit.undo", first),
      registerCommand("edit.redo", second),
    ];
    try {
      await open();
      type("do");
      const field = input();
      fireEvent.keyDown(field, { key: "ArrowDown" });
      fireEvent.keyDown(field, { key: "Enter" });
      await new Promise((resolve) => setTimeout(resolve, 0));
      expect(second).toHaveBeenCalledTimes(1);
      expect(first).not.toHaveBeenCalled();
    } finally {
      unregister.forEach((off) => off());
    }
  });

  it("closes on Escape", async () => {
    await open();
    expect(overlayStore.paletteOpen()).toBe(true);
    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(overlayStore.paletteOpen()).toBe(false);
  });

  it("shows the binding on the right", async () => {
    await open();
    type("save");
    const save = document.querySelector('[data-command-id="file.save"]')!;
    expect(save.querySelector("kbd")?.textContent).toBe("Cmd/Ctrl+S");
  });

  it("shows a disabled command with its reason instead of hiding it", async () => {
    await open();
    type("split");
    const row = document.querySelector('[data-command-id="edit.split"]') as HTMLElement;
    expect(row).toBeTruthy();
    expect(row.getAttribute("aria-disabled")).toBe("true");
    expect(row.querySelector(".command-palette-reason")?.textContent).toContain(i18n.t("command.needsTimeline"));
  });
});
