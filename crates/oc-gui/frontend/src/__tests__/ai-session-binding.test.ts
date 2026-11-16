import { describe, expect, it } from "vitest";
import { bindingBar, projectName, sessionBinding } from "../ai/binding";
import { hostNotice, messageKind, projectBoundNotice, type MessageMeta } from "../ai/messageProtocol";
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

  it("reads only a valid typed project binding", () => {
    expect(projectBoundNotice(noticeMeta({ kind: "project_bound", to: "/songs/song.ocp", cause: "chosen" }))?.to).toBe("/songs/song.ocp");
    expect(projectBoundNotice(noticeMeta({ kind: "project_bound", to: "/songs/song.ocp", cause: "elsewhere" }))).toBeNull();
    expect(hostNotice({ kind: "project_bound", to: "x".repeat(4097), cause: "chosen" })).toBeNull();
    expect(messageKind({ role: "user", content: "x", message_meta: { version: 1, kind: "user", notice: { kind: "project_bound", to: "/songs/song.ocp", cause: "chosen" } } as MessageMeta })).toBe("user");
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
