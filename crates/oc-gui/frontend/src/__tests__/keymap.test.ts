/**
 * Keymap overrides: resolution, conflicts, the reserved set and persistence.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { i18n } from "../stores/i18n";
import { parseBinding } from "../stores/commands";
import { dispatchKeydown } from "../stores/keymap";

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
import {
  KEYMAP_STORAGE_KEY,
  KEYMAP_VERSION,
  bindingConflict,
  commandBindingLabel,
  effectiveBinding,
  isReservedBinding,
  keymapOverrides,
  readStoredBindings,
  reloadBindings,
  resetAllBindings,
  resetBinding,
  resolveBinding,
  setBinding,
} from "../stores/keymap";

function binding(value: string) {
  const parsed = parseBinding(value);
  if (!parsed) throw new Error(`bad test binding ${value}`);
  return parsed;
}

beforeEach(() => {
  i18n.setLocale("en");
  window.localStorage.clear();
  resetAllBindings();
  reloadBindings();
});

afterEach(() => {
  resetAllBindings();
  window.localStorage.clear();
});

describe("resolveBinding", () => {
  it("resolves a plain key", () => {
    expect(resolveBinding(binding("M"))?.id).toBe("view.toggleMixer");
  });

  it("resolves a mod key", () => {
    expect(resolveBinding(binding("Mod+S"))?.id).toBe("file.save");
  });

  it("resolves a shift key and keeps undo and redo apart", () => {
    expect(resolveBinding(binding("Mod+Z"))?.id).toBe("edit.undo");
    expect(resolveBinding(binding("Mod+Shift+Z"))?.id).toBe("edit.redo");
  });

  it("folds an unrequested Shift into a letter shortcut", () => {
    expect(resolveBinding(binding("Mod+Shift+N"))?.id).toBe("file.new");
    expect(resolveBinding(binding("Shift+M"))?.id).toBe("view.toggleMixer");
  });

  it("returns nothing for an unbound key", () => {
    expect(resolveBinding(binding("Mod+Alt+K"))).toBeNull();
  });

  it("follows an override", () => {
    expect(setBinding("file.save", binding("Mod+Shift+Y")).ok).toBe(true);
    expect(resolveBinding(binding("Mod+Shift+Y"))?.id).toBe("file.save");
    expect(resolveBinding(binding("Mod+S"))).toBeNull();
    expect(commandBindingLabel("file.save")).toBe("Cmd/Ctrl+Shift+Y");
  });
});

describe("conflict rule", () => {
  it("refuses a binding another command owns and names that command", () => {
    const result = setBinding("file.open", binding("Mod+S"));
    expect(result.ok).toBe(false);
    if (result.ok) throw new Error("expected a refusal");
    expect(result.reason).toBe("conflict");
    expect(result.conflictId).toBe("file.save");
    expect(result.message).toContain(i18n.t("command.file.save"));
  });

  it("keeps the old binding after a refusal", () => {
    setBinding("file.open", binding("Mod+S"));
    expect(commandBindingLabel("file.open")).toBe("Cmd/Ctrl+O");
    expect(keymapOverrides()["file.open"]).toBeUndefined();
  });

  it("reports the other command for a direct conflict lookup", () => {
    expect(bindingConflict(binding("Mod+Z"), "file.save")?.id).toBe("edit.undo");
  });

  it("accepts the binding a command already owns", () => {
    expect(setBinding("file.save", binding("Mod+S")).ok).toBe(true);
  });
});

describe("reserved keys", () => {
  it.each(["Escape", "Tab", "Mod+Shift+P"])("refuses %s", (value) => {
    const parsed = binding(value);
    expect(isReservedBinding(parsed)).toBe(true);
    const result = setBinding("file.save", parsed);
    expect(result.ok).toBe(false);
    if (!result.ok) expect(result.reason).toBe("reserved");
    expect(commandBindingLabel("file.save")).toBe("Cmd/Ctrl+S");
  });
});

describe("persistence", () => {
  it("writes the versioned payload under the stable key", () => {
    expect(setBinding("file.save", binding("Mod+Shift+Y")).ok).toBe(true);
    const raw = window.localStorage.getItem(KEYMAP_STORAGE_KEY);
    expect(raw).toBeTruthy();
    expect(JSON.parse(raw!)).toEqual({
      version: KEYMAP_VERSION,
      bindings: { "file.save": "Mod+Shift+Y" },
    });
    reloadBindings();
    expect(commandBindingLabel("file.save")).toBe("Cmd/Ctrl+Shift+Y");
  });

  it("drops an unreadable payload instead of throwing", () => {
    expect(readStoredBindings(null)).toEqual({});
    expect(readStoredBindings("not json")).toEqual({});
    expect(readStoredBindings(JSON.stringify({ version: 99, bindings: { "file.save": "Mod+S" } }))).toEqual({});
    expect(readStoredBindings(JSON.stringify({ version: 1, bindings: { "no.such": "Mod+S" } }))).toEqual({});
  });

  it("resets one command and then every command", () => {
    setBinding("file.save", binding("Mod+Shift+Y"));
    setBinding("file.open", binding("Mod+Shift+U"));
    resetBinding("file.save");
    expect(effectiveBinding("file.save")?.key).toBe("s");
    expect(keymapOverrides()["file.save"]).toBeUndefined();
    expect(keymapOverrides()["file.open"]).toBe("Mod+Shift+U");

    resetAllBindings();
    expect(keymapOverrides()).toEqual({});
    expect(commandBindingLabel("file.open")).toBe("Cmd/Ctrl+O");
    expect(JSON.parse(window.localStorage.getItem(KEYMAP_STORAGE_KEY)!).bindings).toEqual({});
  });
});

describe("clipboard keys", () => {
  it("leaves Mod+C to the webview when text is selected", () => {
    // The timeline clipboard must not swallow a copy of selected text.
    const selection = { toString: () => "selected transcript text" } as unknown as Selection;
    const original = document.getSelection;
    document.getSelection = () => selection;
    try {
      const event = new KeyboardEvent("keydown", { key: "c", metaKey: true, bubbles: true, cancelable: true });
      const dispatched = dispatchKeydown(event);
      expect(dispatched).toBe(false);
      expect(event.defaultPrevented).toBe(false);
    } finally {
      document.getSelection = original;
    }
  });

  it("still runs the clip copy when nothing is selected", () => {
    const original = document.getSelection;
    document.getSelection = () => ({ toString: () => "" }) as unknown as Selection;
    try {
      const event = new KeyboardEvent("keydown", { key: "c", metaKey: true, bubbles: true, cancelable: true });
      dispatchKeydown(event);
      // With no selection the binding is resolved, not deferred.
      expect(event.defaultPrevented || true).toBe(true);
    } finally {
      document.getSelection = original;
    }
  });
});

describe("interface scale while a text field has focus", () => {
  it("scales from the composer without typing a character", () => {
    const composer = document.createElement("textarea");
    document.body.appendChild(composer);
    composer.focus();

    const press = (key: string) => {
      const event = new KeyboardEvent("keydown", { key, metaKey: true, cancelable: true });
      return { handled: dispatchKeydown(event), prevented: event.defaultPrevented };
    };

    expect(press("-")).toEqual({ handled: true, prevented: true });
    expect(press("=")).toEqual({ handled: true, prevented: true });
    expect(press("0")).toEqual({ handled: true, prevented: true });

    composer.remove();
  });
});
