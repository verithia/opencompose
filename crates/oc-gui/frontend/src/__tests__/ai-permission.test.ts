/**
 * The session mode the panel stores and shows. The backend loop gates every
 * call from it; an unknown stored value must never widen what a session may do.
 */
import { afterEach, describe, expect, it } from "vitest";
import {
  DEFAULT_PERMISSION,
  PERMISSION_MODES,
  permissionOf,
} from "../ai/permission";
import { allowTool, allowedTools, clearAllowedTools } from "../ai/approvalMemory";

// jsdom in this suite has no storage backend; the approval memory only needs
// get/set/remove.
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

afterEach(() => {
  clearAllowedTools();
});

describe("the stored mode", () => {
  it("reads an unknown stored value as the default", () => {
    for (const mode of PERMISSION_MODES) expect(permissionOf(mode)).toBe(mode);
    for (const value of [undefined, null, "", "always", 1, {}]) {
      expect(permissionOf(value)).toBe(DEFAULT_PERMISSION);
    }
    expect(DEFAULT_PERMISSION).toBe("write");
  });
});

describe("the tools a conversation allowed", () => {
  it("keeps each conversation's answer apart", () => {
    expect(allowedTools("first")).toEqual([]);
    expect(allowTool("first", "tracks_remove")).toEqual(["tracks_remove"]);
    expect(allowedTools("first")).toEqual(["tracks_remove"]);
    // A fresh conversation starts from the default and asks again.
    expect(allowedTools("second")).toEqual([]);
    // Answering twice records the tool once.
    expect(allowTool("first", "tracks_remove")).toEqual(["tracks_remove"]);
    expect(allowTool("first", "presets_remove")).toEqual(["tracks_remove", "presets_remove"]);
    expect(allowedTools("second")).toEqual([]);
  });

  it("survives a reload of the panel, because the choice is a preference", () => {
    allowTool("kept", "tracks_remove");
    // A fresh read of the store, as a remount does.
    expect(allowedTools("kept")).toEqual(["tracks_remove"]);
    clearAllowedTools();
    expect(allowedTools("kept")).toEqual([]);
  });

  it("reads a corrupt or foreign value as no answer at all", () => {
    window.localStorage.setItem("opencompose.ai.allowed-tools", "{not json");
    expect(allowedTools("any")).toEqual([]);
    window.localStorage.setItem("opencompose.ai.allowed-tools", '{"any":"tracks_remove"}');
    expect(allowedTools("any")).toEqual([]);
    window.localStorage.setItem(
      "opencompose.ai.allowed-tools",
      '{"any":["tracks_remove",7,""]}',
    );
    expect(allowedTools("any")).toEqual(["tracks_remove"]);
  });

  it("has no answer for a conversation that has no id yet", () => {
    expect(allowTool("", "tracks_remove")).toEqual([]);
    expect(allowedTools("")).toEqual([]);
  });
});
