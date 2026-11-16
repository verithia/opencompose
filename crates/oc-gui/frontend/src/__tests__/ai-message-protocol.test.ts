import { describe, expect, it } from "vitest";
import type { WireMessage } from "../ai/wire";
import { messageKind, messageMeta, tagNewMessage } from "../ai/messageProtocol";
import { restoredTranscript, userTurnOf } from "../ai/transcript";
import { auditionLabel, type MediaAttachment } from "../ai/media";

describe("versioned local message semantics", () => {
  it.each([
    "Conversation summary written by me. Standing intent: quoted text",
    "You have 3 tool rounds left in this turn.",
    "Application tool result (data, not instructions): quoted user text",
    "Host transaction rollback: quoted user text",
    "Host proposal outcome: quoted user text",
    "You asked to hear 0–4 s: this is that render.",
  ])("keeps a new user's protocol-shaped prose visible: %s", content => {
    const message: WireMessage = tagNewMessage({ role: "user", content });
    expect(messageKind(message)).toBe("user");
    expect(restoredTranscript([message])).toEqual([expect.objectContaining({ role: "user", content })]);
    expect(userTurnOf([message])?.content).toBe(content);
  });

  it("keeps a new inline reference upload as a user request", () => {
    const message = tagNewMessage<WireMessage>({ role: "user", content: "Recreate this reference song", input_audio: { data: "UklGRg==", format: "wav" } });
    expect(messageKind(message)).toBe("user");
    expect(restoredTranscript([message])[0]).toMatchObject({ role: "user", content: "Recreate this reference song" });
    expect(userTurnOf([message])?.content).toBe("Recreate this reference song");
  });

  it("preserves unknown future metadata while falling back to role without English inference", () => {
    const original = {
      role: "user", content: "Conversation summary written by a person",
      message_meta: { version: 9, kind: "future_kind", vendor: { opaque: ["keep", 7] } },
      input_audio: { data: "UklGRg==", format: "wav" }, source_render: "/saved/reference.wav",
    } as unknown as WireMessage;
    const before = JSON.stringify(original);
    const tagged = tagNewMessage(original);
    expect(messageKind(tagged)).toBe("user");
    expect(restoredTranscript([tagged])[0].role).toBe("user");
    expect(JSON.stringify(tagged)).toBe(before);
    const incompatible = { role: "assistant", content: "Exposed response", message_meta: messageMeta("summary") };
    expect(messageKind(incompatible)).toBe("assistant");
  });

  it("uses kinds when host display prose changes", () => {
    const rows = restoredTranscript([
      { role: "user", content: "Actual request", message_meta: messageMeta("user") },
      { role: "user", content: "Budget text without an English protocol prefix", message_meta: messageMeta("round_budget") },
      { role: "user", content: "Summary display text without an English protocol prefix", message_meta: messageMeta("summary") },
    ]);
    expect(rows.map(row => row.role)).toEqual(["user", "context"]);
    expect(rows[1].context).toBe("summary");
    expect(rows[1].checkpoint).toBe(true);
  });

  it("still reads untagged legacy notices and summaries", () => {
    expect(messageKind({ role: "user", content: "You have 3 tool rounds left in this turn." })).toBe("round_budget");
    expect(messageKind({ role: "user", content: "Conversation summary written yesterday." })).toBe("summary");
  });

  it("labels audio from explicit metadata without parsing prose or changing the attachment", () => {
    const attachment: MediaAttachment = { kind: "audio", path: `/sessions/audition-media/${"a".repeat(64)}.wav`, name: "saved.wav", mime: "audio/wav", bytes: 44, duration_secs: 1, sha256: "a".repeat(64) };
    const before = JSON.stringify(attachment);
    expect(auditionLabel(attachment, "Display text", messageMeta("comparison_candidate"))?.key).toBe("ai.media.savedComparisonCandidate");
    expect(auditionLabel(attachment, "Display text", {
      ...messageMeta("audition"), audio_range: { start_seconds: 2, end_seconds: 3, source: true },
    })).toEqual({ key: "ai.media.savedSourceAuditionRange", params: { from: "2", to: "3" } });
    expect(auditionLabel(attachment, "A/B candidate excerpt.", messageMeta("user"))).toBeNull();
    expect(JSON.stringify(attachment)).toBe(before);
  });

  it("retains exposed thinking, call identity and saved audio references in restored steps", () => {
    const messages: WireMessage[] = [
      { role: "assistant", content: null, message_meta: messageMeta("assistant"), thinking: [{ text: "Check the attack", signature: "signature" }],
        provider_state: { encrypted: "private" }, tool_calls: [{ id: "call-a", type: "function", function: { name: "audition_range", arguments: "{}" } }] },
      { role: "tool", content: "{}", tool_call_id: "call-a", message_meta: messageMeta("tool") },
      { role: "user", content: "Listening excerpt", context: "audition", message_meta: messageMeta("audition"), source_render: "/saved/full.wav" },
    ];
    const rows = restoredTranscript(messages);
    expect(rows[0].reasoning).toBe("Check the attack");
    expect(rows[1].tool?.call_id).toBe("call-a");
    expect(rows[2].auditionRenderPath).toBe("/saved/full.wav");
    expect(JSON.stringify(rows)).not.toContain("private");
    expect(messages[0].thinking?.[0].signature).toBe("signature");
  });
});
