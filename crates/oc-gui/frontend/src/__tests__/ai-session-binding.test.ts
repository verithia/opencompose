import { describe, expect, it } from "vitest";
import { bindingBar, projectName, sessionBinding } from "../ai/binding";
import { hostNotice, messageKind, projectBoundNotice, toolSurfaceNotice, type MessageMeta } from "../ai/messageProtocol";
import { restoredTranscript } from "../ai/transcript";
import type { WireMessage } from "../ai/wire";

function noticeMeta(notice: unknown): MessageMeta {
  return { version: 1, kind: "host_notice", notice } as MessageMeta;
}

describe("typed host notices in the transcript", () => {
  it("renders a project binding as one small context row", () => {
    const rows = restoredTranscript([
      { role: "user", content: "bound", message_meta: noticeMeta({ kind: "project_bound", from: "/songs/old.ocp", to: "/songs/new.ocp", cause: "moved" }) } as WireMessage,
      { role: "user", content: "next", message_meta: { version: 1, kind: "user" } } as WireMessage,
    ]);
    expect(rows).toHaveLength(2);
    expect(rows[0].role).toBe("context");
    expect(rows[0].binding).toEqual({ kind: "project_bound", from: "/songs/old.ocp", to: "/songs/new.ocp", cause: "moved" });
    expect(rows[0].content).toBe("");
    expect(rows[1].content).toBe("next");
  });

  it("keeps untyped and malformed host notices out of the transcript", () => {
    const rows = restoredTranscript([
      { role: "user", content: "earlier messages were dropped", message_meta: { version: 1, kind: "host_notice" } } as WireMessage,
      { role: "user", content: "not a valid notice", message_meta: noticeMeta({ kind: "project_bound", to: "", cause: "chosen" }) } as WireMessage,
    ]);
    expect(rows).toEqual([]);
  });

  it("validates an outside changes notice and keeps it out of the transcript", () => {
    const notice = {
      kind: "outside_changes",
      entries: [{
        author: { kind: "person" },
        cause: "edit",
        changes: { changes: [{ kind: "tempo", from: 120, to: 130 }], more: 0 },
      }],
      more: 2,
      undone: 1,
      rewritten: false,
    };
    expect(hostNotice(notice)).not.toBeNull();
    const rows = restoredTranscript([
      { role: "user", content: "outside", message_meta: noticeMeta(notice) } as WireMessage,
      { role: "user", content: "next", message_meta: { version: 1, kind: "user" } } as WireMessage,
    ]);
    expect(rows).toHaveLength(1);
    expect(rows[0].content).toBe("next");
    expect(messageKind({ role: "user", content: "outside", message_meta: noticeMeta(notice) } as WireMessage)).toBe("host_notice");
    expect(hostNotice({ ...notice, entries: Array(9).fill(notice.entries[0]) })).toBeNull();
    expect(hostNotice({ ...notice, more: 1_000_001 })).toBeNull();
    expect(hostNotice({
      ...notice,
      entries: [{ author: { kind: "client", owner: "x".repeat(513) }, changes: { changes: [], more: 0 } }],
    })).toBeNull();
  });

  it("reads only a valid typed project binding", () => {
    expect(projectBoundNotice(noticeMeta({ kind: "project_bound", to: "/songs/song.ocp", cause: "chosen" }))?.to).toBe("/songs/song.ocp");
    expect(projectBoundNotice(noticeMeta({ kind: "project_bound", to: "/songs/song.ocp", cause: "elsewhere" }))).toBeNull();
    expect(hostNotice({ kind: "project_bound", to: "x".repeat(4097), cause: "chosen" })).toBeNull();
    expect(messageKind({ role: "user", content: "x", message_meta: { version: 1, kind: "user", notice: { kind: "project_bound", to: "/songs/song.ocp", cause: "chosen" } } as MessageMeta })).toBe("user");
  });

  it("renders a tool-surface change as one small context row", () => {
    const notice = {
      kind: "tool_surface",
      added: ["tracks_remove"],
      removed: ["tracks_add"],
      permission: { from: "autoreview", to: "write" },
    };
    const rows = restoredTranscript([
      { role: "user", content: "changed", message_meta: noticeMeta(notice) } as WireMessage,
      { role: "user", content: "next", message_meta: { version: 1, kind: "user" } } as WireMessage,
    ]);
    expect(rows).toHaveLength(2);
    expect(rows[0].role).toBe("context");
    expect(rows[0].surface).toEqual(notice);
    expect(rows[0].binding).toBeUndefined();
    expect(rows[0].content).toBe("");
    expect(rows[1].content).toBe("next");
  });

  it("validates a tool-surface notice and ignores a malformed one", () => {
    expect(toolSurfaceNotice(noticeMeta({
      kind: "tool_surface", added: ["tracks_add"], removed: [], permission: { from: "write", to: "autoreview" },
    }))).toEqual({ kind: "tool_surface", added: ["tracks_add"], removed: [], permission: { from: "write", to: "autoreview" } });
    expect(hostNotice({ kind: "tool_surface", added: Array(513).fill("tracks_add"), removed: [] })).toBeNull();
    expect(hostNotice({ kind: "tool_surface", added: ["x".repeat(129)], removed: [] })).toBeNull();
    expect(hostNotice({ kind: "tool_surface", added: [], removed: [], permission: { from: "write", to: "elsewhere" } })).toBeNull();
    expect(hostNotice({ kind: "tool_surface", added: [], removed: [], permission: { from: "write" } })).toBeNull();
    const rows = restoredTranscript([
      { role: "user", content: "bad", message_meta: noticeMeta({ kind: "tool_surface", added: ["x".repeat(129)], removed: [] }) } as WireMessage,
    ]);
    expect(rows).toEqual([]);
  });
});

describe("session binding status", () => {
  it("validates the backend answer and shows a bar only on a mismatch", () => {
    expect(sessionBinding({ bound: "/songs/a.ocp", open: "/songs/b.ocp", matches: false })).toEqual({ bound: "/songs/a.ocp", open: "/songs/b.ocp", matches: false });
    expect(sessionBinding({ bound: null, open: "/songs/b.ocp", matches: true })).toEqual({ bound: null, open: "/songs/b.ocp", matches: true });
    expect(sessionBinding({ bound: 7, open: null, matches: false })).toBeNull();
    expect(sessionBinding(null)).toBeNull();
    expect(bindingBar({ bound: "/songs/a.ocp", open: "/songs/b.ocp", matches: false })).toEqual({ bound: "/songs/a.ocp", open: "/songs/b.ocp" });
    expect(bindingBar({ bound: "/songs/a.ocp", open: "/songs/a.ocp", matches: true })).toBeNull();
    expect(bindingBar({ bound: null, open: "/songs/b.ocp", matches: true })).toBeNull();
  });

  it("shows a file name instead of the whole path", () => {
    expect(projectName("/songs/song.ocp")).toBe("song.ocp");
    expect(projectName("C:\\songs\\song.ocp")).toBe("song.ocp");
    expect(projectName(null)).toBe("");
  });
});
