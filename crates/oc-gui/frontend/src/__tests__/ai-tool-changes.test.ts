import { afterEach, describe, expect, it } from "vitest";
import { activitySummary } from "../ai/activity";
import { restoredTranscript } from "../ai/transcript";
import type { WireMessage } from "../ai/wire";
import { i18n } from "../stores/i18n";

const call = (name: string, args: string) => ({
  role: "assistant",
  content: null,
  tool_calls: [{ id: "c1", type: "function" as const, function: { name, arguments: args } }],
});

function toolRow(changes: unknown): WireMessage {
  return { role: "tool", tool_call_id: "c1", content: '{"ok":true}', changes } as WireMessage;
}

afterEach(() => i18n.setLocale("en"));

describe("tool rows with typed changes", () => {
  it("carries the change set and names the row from it", () => {
    i18n.setLocale("en");
    const changes = { changes: [{ kind: "track_added", track: "keys" }], more: 0 };
    const rows = restoredTranscript([call("tracks_add", '{"name":"Keys"}'), toolRow(changes)]);
    expect(rows[0].tool?.changes).toEqual(changes);
    expect(activitySummary(rows[0].tool!, rows[0].content)).toBe("Add track “keys”");
  });

  it("keeps the argument summary when the saved changes are malformed", () => {
    i18n.setLocale("en");
    const rows = restoredTranscript([call("tracks_add", '{"name":"Keys"}'), toolRow({ changes: "no" })]);
    expect(rows[0].tool?.changes).toBeUndefined();
    expect(activitySummary(rows[0].tool!, rows[0].content)).toBe("Add track “Keys”");
  });
});
