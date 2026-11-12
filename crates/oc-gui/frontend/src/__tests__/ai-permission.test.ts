/**
 * The one decision every DSL-modifying call goes through, and the stored value
 * it comes from. A read-only tool must never be gated, a destructive call must
 * ask in every mode, and an unknown stored value must never widen what a
 * session may do.
 */
import { afterEach, describe, expect, it } from "vitest";
import {
  DEFAULT_PERMISSION,
  PERMISSION_MODES,
  callPath,
  dslCall,
  permissionOf,
} from "../ai/permission";
import { allowTool, allowedTools, clearAllowedTools } from "../ai/approvalMemory";
import type { ToolDef } from "../ai/agent";

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

const tool = (name: string, mutation = false, approval: "automatic" | "confirm" = "automatic") =>
  ({ name, mutation, approval }) as Pick<ToolDef, "name" | "mutation" | "approval">;

afterEach(() => {
  clearAllowedTools();
});

describe("the DSL-modifying decision", () => {
  it("never gates a read-only tool in any mode", () => {
    for (const mode of PERMISSION_MODES) {
      expect(dslCall(tool("tracks_list"), mode)).toEqual({ modifies: false, need: "none" });
    }
  });

  it("asks, reviews or applies a registry mutation by mode", () => {
    const mutation = tool("tracks_add", true);
    expect(dslCall(mutation, "require_approval")).toEqual({ modifies: true, need: "approval" });
    expect(dslCall(mutation, "autoreview")).toEqual({ modifies: true, need: "review" });
    expect(dslCall(mutation, "write")).toEqual({ modifies: true, need: "none" });
  });

  it("treats the panel-local replacement as modifying even though the registry does not", () => {
    const replacement = tool("edit_subtree");
    expect(dslCall(replacement, "require_approval").need).toBe("approval");
    expect(dslCall(replacement, "autoreview").need).toBe("review");
    expect(dslCall(replacement, "write").need).toBe("none");
  });

  it("asks for a confirm tool in every mode, the default included", () => {
    const removal = tool("tracks_remove", true, "confirm");
    for (const mode of PERMISSION_MODES) {
      expect(dslCall(removal, mode)).toEqual({ modifies: true, need: "approval" });
    }
    expect(dslCall(removal, DEFAULT_PERMISSION)).toEqual({ modifies: true, need: "approval" });
  });

  it("gates a confirm tool that was not marked mutating", () => {
    expect(dslCall(tool("project_open", false, "confirm"), "autoreview")).toEqual({
      modifies: true,
      need: "approval",
    });
  });

  it("stops asking for a tool the conversation already allowed", () => {
    const removal = tool("tracks_remove", true, "confirm");
    expect(dslCall(removal, "write", ["tracks_remove"])).toEqual({ modifies: true, need: "none" });
    // An allowed tool still obeys an explicit mode that asks for every change.
    expect(dslCall(removal, "require_approval", ["tracks_remove"]).need).toBe("approval");
    expect(dslCall(removal, "autoreview", ["tracks_remove"]).need).toBe("review");
    // Another tool is unaffected.
    expect(dslCall(tool("presets_remove", true, "confirm"), "write", ["tracks_remove"]).need).toBe(
      "approval",
    );
  });

  it("gates a tool it does not know", () => {
    expect(dslCall(undefined, "require_approval")).toEqual({ modifies: false, need: "none" });
  });

  it("reads an unknown stored value as the default", () => {
    for (const mode of PERMISSION_MODES) expect(permissionOf(mode)).toBe(mode);
    for (const value of [undefined, null, "", "always", 1, {}]) {
      expect(permissionOf(value)).toBe(DEFAULT_PERMISSION);
    }
    expect(DEFAULT_PERMISSION).toBe("write");
  });

  it("names the subtree or object a call touches", () => {
    expect(callPath("edit_subtree", { path: "track:kick" })).toBe("track:kick");
    expect(callPath("tracks_mix", { track: "kick" })).toBe("kick");
    expect(callPath("effects_add", { target: "master" })).toBe("master");
    expect(callPath("tracks_add", { name: "lead" })).toBeUndefined();
    expect(callPath(undefined, {})).toBeUndefined();
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
