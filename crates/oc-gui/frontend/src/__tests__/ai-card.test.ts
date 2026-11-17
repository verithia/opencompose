import { describe, expect, it } from "vitest";
import { createCardStore } from "../ai/card";

describe("the one card slot", () => {
  it("replaces an approval with a question and clears the old error", () => {
    const card = createCardStore();
    card.showApproval("turn-1", { call_id: "call-1", tool: "tracks_remove", args: {} }, "kick");
    expect(card.approval()).toMatchObject({ kind: "approval", requestId: "turn-1", target: "kick" });
    card.showQuestion("turn-1", { call_id: "call-2", question: "Which take?", free_text: true, max_chars: 40 });
    expect(card.approval()).toBeNull();
    expect(card.question()).toMatchObject({ replyText: "", error: null, answering: false });
    card.setError("too long");
    card.setReplyText("take two");
    card.setAnswering(true);
    expect(card.question()).toMatchObject({ error: "too long", replyText: "take two", answering: true });
    card.clear();
    expect(card.card()).toBeNull();
  });

  it("clears only the card the call names", () => {
    const card = createCardStore();
    card.showQuestion("turn-1", { call_id: "call-1", question: "Which take?", free_text: false, max_chars: 10 });
    card.setError("too long");
    card.clearIf("turn-2", "call-1");
    expect(card.question()).not.toBeNull();
    card.clearIf("turn-1", "call-1");
    expect(card.card()).toBeNull();
  });
});
