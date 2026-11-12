import { describe, expect, it } from "vitest";
import { createSignal } from "solid-js";
import { fireEvent, render } from "@solidjs/testing-library";
import { AiStepGroup, stepDuration } from "../components/AiStepGroup";
import { restoredTranscript, stepTranscript, type ChatMessage } from "../ai/transcript";

function row(role: ChatMessage["role"], timestamp?: number, content = ""): ChatMessage {
  return { role, content, ...(timestamp === undefined ? {} : { timestamp }) };
}

describe("transcript step groups", () => {
  it("restores exposed thinking in order with tool work and omits private provider state", () => {
    const rows = restoredTranscript([
      { role: "user", content: "Find a drum loop" },
      { role: "assistant", content: null, thinking: [
        { text: "Find a dry loop before editing." },
        { text: "Do not display this", redacted: "encrypted-state" },
      ], tool_calls: [{ id: "s", type: "function", function: { name: "sample_search", arguments: '{"query":"dry drums"}' } }] },
      { role: "tool", tool_call_id: "s", content: '{"count":2}' },
      { role: "assistant", content: "Found two loops.", thinking: [{ text: "Check their licenses." }] },
    ]);
    const entries = stepTranscript(rows, false);
    expect(entries.map(entry => entry.kind)).toEqual(["message", "step", "message"]);
    const group = entries[1];
    if (group.kind !== "step") throw new Error("expected step");
    expect(group.rows.map(row => row.reasoning ?? row.tool?.name)).toEqual([
      "Find a dry loop before editing.", "sample_search", "Check their licenses.",
    ]);
    expect(group.toolCalls).toBe(1);
    expect(JSON.stringify(rows)).not.toContain("encrypted-state");
    expect(JSON.stringify(rows)).not.toContain("Do not display this");
  });

  it("restores legacy public reasoning without exposing opaque state", () => {
    const rows = restoredTranscript([
      { role: "assistant", content: "Done.", reasoning_content: "Read the score." },
      { role: "assistant", content: "Done.", reasoning: { encrypted: "opaque" } },
    ]);
    expect(rows.filter(row => row.reasoning).map(row => row.reasoning)).toEqual(["Read the score."]);
    expect(JSON.stringify(rows)).not.toContain("opaque");
  });

  it("starts at the user turn and closes at the next assistant message", () => {
    const entries = stepTranscript([
      row("user", 1000, "make it punchier"),
      row("tool", 1100, "tracks_list: {}"),
      row("tool", 1200, "tracks_mix: {}"),
      row("assistant", 3200, "Done."),
    ], false);
    expect(entries.map((entry) => entry.kind)).toEqual(["message", "step", "message"]);
    const step = entries[1];
    expect(step.kind).toBe("step");
    if (step.kind !== "step") return;
    expect(step.start).toBe(1000);
    expect(step.end).toBe(3200);
    expect(step.toolCalls).toBe(2);
    expect(step.running).toBe(false);
  });

  it("keeps the last group running while the turn works", () => {
    const entries = stepTranscript([
      row("user", 1000),
      row("tool", 1100),
    ], true);
    const step = entries.at(-1)!;
    expect(step.kind).toBe("step");
    if (step.kind !== "step") return;
    expect(step.running).toBe(true);
    expect(step.start).toBe(1000);
  });

  it("starts the next group after an assistant preamble and closes it at the final answer", () => {
    const entries = stepTranscript([
      row("user", 1000),
      row("assistant", 2000, "Trying a gain cut."),
      row("tool", 2100),
      row("assistant", 3000, "Done."),
    ], false);
    expect(entries.map((entry) => entry.kind)).toEqual(["message", "message", "step", "message"]);
    const step = entries[2];
    if (step.kind !== "step") throw new Error("expected a step");
    expect(step.start).toBe(2000);
    expect(step.end).toBe(3000);
  });

  it("keeps live tool rounds in one group until assistant prose appears", () => {
    const think0 = { ...row("assistant", 1400, ""), reasoning: "First round" };
    const first = row("tool", 2100);
    const think1 = { ...row("assistant", 4400, ""), reasoning: "Second round" };
    const second = row("tool", 5100);
    const entries = stepTranscript([
      row("user", 1000),
      think0,
      first,
      think1,
      second,
      row("assistant", 6200, "Done."),
    ], false);
    const steps = entries.filter((entry) => entry.kind === "step");
    expect(steps).toHaveLength(1);
    if (steps[0].kind !== "step") return;
    expect(steps[0].rows.map((message) => message.reasoning ?? message.role)).toEqual([
      "First round", "tool", "Second round", "tool",
    ]);
    expect(steps[0].start).toBe(1000);
    expect(steps[0].end).toBe(6200);
    expect(steps[0].toolCalls).toBe(2);
  });

  it("closes the group when the user steers the turn", () => {
    const entries = stepTranscript([
      row("user", 1000, "first"),
      row("tool", 1100),
      row("user", 2000, "no, quieter"),
      row("tool", 2100),
      row("assistant", 3000, "Done."),
    ], false);
    const steps = entries.filter((entry) => entry.kind === "step");
    expect(steps).toHaveLength(2);
    if (steps[0].kind !== "step" || steps[1].kind !== "step") return;
    expect(steps[0].start).toBe(1000);
    expect(steps[0].end).toBe(1100);
    expect(steps[1].start).toBe(2000);
    expect(steps[1].end).toBe(3000);
  });

  it("keeps a thinking row inside the step and counts only tool calls", () => {
    const thinking: ChatMessage = { role: "assistant", content: "", reasoning: "Checking the mix.", timestamp: 1200 };
    const entries = stepTranscript([
      row("user", 1000),
      thinking,
      row("tool", 1300),
      row("assistant", 2000, "Done."),
    ], false);
    const step = entries[1];
    if (step.kind !== "step") throw new Error("expected a step");
    expect(step.rows.map((message) => message.role)).toEqual(["assistant", "tool"]);
    expect(step.toolCalls).toBe(1);
  });

  it("keeps a restored group's tool count without inventing a duration", () => {
    const entries = stepTranscript([
      row("user", undefined),
      row("tool", undefined),
      row("assistant", undefined, "Done."),
    ], false);
    const step = entries[1];
    if (step.kind !== "step") throw new Error("expected a step");
    expect(step.start).toBeNull();
    expect(step.end).toBeNull();
    expect(step.toolCalls).toBe(1);
  });

  it("formats seconds and minutes", () => {
    expect(stepDuration(999)).toBe("0s");
    expect(stepDuration(42_000)).toBe("42s");
    expect(stepDuration(62_000)).toBe("1m 02s");
  });

  it("shows the latest activity while a folded step is running", () => {
    const messages = [row("tool", 1100, "tracks_list: {}")];
    const view = render(() => (
      <AiStepGroup
        rows={messages}
        start={1000}
        end={null}
        toolCalls={2}
        running
        latest="render range"
        now={() => 62_000}
        render={(message) => <div>{message.content}</div>}
      />
    ));
    expect(view.container.textContent).toContain("Working · 1m 01s · render range");
    const details = view.container.querySelector("details") as HTMLDetailsElement;
    expect(details.open).toBe(true);
    details.open = false;
    fireEvent(details, new Event("toggle"));
    expect(view.container.textContent).toContain("Working · 1m 01s · render range");
  });

  it("keeps a running step folded while new tool rows land", () => {
    const [rows, setRows] = createSignal([row("tool", 1100, "first")]);
    const view = render(() => (
      <AiStepGroup
        rows={rows()}
        start={1000}
        end={null}
        toolCalls={rows().length}
        running
        latest="render range"
        now={() => 2000}
        render={(message) => <div class="step-row">{message.content}</div>}
      />
    ));
    const details = view.container.querySelector("details") as HTMLDetailsElement;
    expect(details.open).toBe(true);
    details.open = false;
    fireEvent(details, new Event("toggle"));
    expect(details.open).toBe(false);
    setRows([row("tool", 1200, "second")]);
    expect(details.open).toBe(false);
    expect(view.container.textContent).toContain("Working · 1s · render range");
  });

  it("renders a completed label and an expandable disclosure", () => {
    const messages = [row("tool", 1100, "tracks_list: {}")];
    const view = render(() => (
      <AiStepGroup
        rows={messages}
        start={1000}
        end={62_000}
        toolCalls={3}
        running={false}
        now={() => 0}
        render={(message) => <div class="step-row">{message.content}</div>}
      />
    ));
    expect(view.container.textContent).toContain("Completed in 1m 01s · 3 tool calls");
    const details = view.container.querySelector("details") as HTMLDetailsElement;
    expect(details.open).toBe(false);
    details.open = true;
    fireEvent(details, new Event("toggle"));
    expect(view.container.querySelector(".step-row")?.textContent).toContain("tracks_list");
  });
});
