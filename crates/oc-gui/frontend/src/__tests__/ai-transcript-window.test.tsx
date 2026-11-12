/**
 * Long conversations render their newest slice; the wire keeps every message.
 * Without the window, a 2 000-message session lays out every row on open.
 */
import { describe, expect, it } from "vitest";
import { render } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { For, Show } from "solid-js";
import { hiddenTranscriptCount, visibleTranscript, TRANSCRIPT_RENDER_LIMIT } from "../ai/transcript";

/** The panel's own slice rule, rendered without the rest of the panel. */
function Transcript(props: { messages: string[] }) {
  const [limit, setLimit] = createSignal(TRANSCRIPT_RENDER_LIMIT);
  const hidden = () => hiddenTranscriptCount(props.messages.length, limit());
  const visible = () => visibleTranscript(props.messages, limit());
  return (
    <div>
      <Show when={hidden() > 0}>
        <button type="button" class="ai-transcript-earlier" onClick={() => setLimit((value) => value + TRANSCRIPT_RENDER_LIMIT)}>
          Show {hidden()} earlier messages
        </button>
      </Show>
      <For each={visible()}>{(message) => <div class="ai-message">{message}</div>}</For>
    </div>
  );
}

describe("the transcript window", () => {
  it("renders the newest 300 messages and reveals the rest on demand", () => {
    const messages = Array.from({ length: 1_000 }, (_, index) => `message ${index}`);
    const view = render(() => <Transcript messages={messages} />);

    expect(view.container.querySelectorAll(".ai-message")).toHaveLength(300);
    expect(view.container.querySelector(".ai-message")?.textContent).toBe("message 700");
    expect(view.container.querySelector(".ai-transcript-earlier")?.textContent).toContain("700 earlier");

    view.container.querySelector<HTMLButtonElement>(".ai-transcript-earlier")!.click();
    expect(view.container.querySelectorAll(".ai-message")).toHaveLength(600);
    view.container.querySelector<HTMLButtonElement>(".ai-transcript-earlier")!.click();
    expect(view.container.querySelectorAll(".ai-message")).toHaveLength(900);
    view.container.querySelector<HTMLButtonElement>(".ai-transcript-earlier")!.click();
    expect(view.container.querySelectorAll(".ai-message")).toHaveLength(1_000);
  });
});
