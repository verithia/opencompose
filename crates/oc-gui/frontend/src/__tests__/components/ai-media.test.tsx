/**
 * AI panel audio attachments: the chip, its remove path, the model warning and
 * the descriptor that travels with the conversation (never the bytes).
 */
import { describe, it, expect, beforeEach, vi } from "vitest";
import { render, fireEvent } from "@solidjs/testing-library";
import { invokeMock, openDialogMock, aiConfigWire } from "../setup";
import { AiPanel } from "../../components/AiPanel";
import { aiSession } from "../../stores/aiSession";
import { answerFakeTurn, resetFakeTurns, runFakeTurn, type TurnStartArgs } from "../helpers/fakeTurn";

const ATTACHMENT = {
  kind: "audio",
  name: "take.wav",
  path: "/tmp/take.wav",
  mime: "audio/wav",
  bytes: 320000,
  duration_secs: 10.0,
  sha256: "a".repeat(64),
};

function catalog(audioIn: boolean) {
  return {
    version: "test",
    providers: [{
      id: "deepseek",
      name: "DeepSeek",
      kind: "openai_chat_completions",
      hosts: ["api.deepseek.com"],
      auth_providers: [],
      models: [{
        id: "deepseek-flash",
        name: "DeepSeek Flash",
        context: 1000000,
        max_output: 8192,
        capabilities: {
          text: true,
          audio_in: audioIn,
          audio_out: false,
          native_tool_calling: true,
          streaming: true,
        },
        status: "active",
      }],
    }],
  };
}

/** No saved file: the panel installs the built-in defaults and saves them. */
function mockBackend(options: { audioIn: boolean; chat?: (args: Record<string, unknown>) => unknown }) {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const typed = (args ?? {}) as Record<string, unknown>;
    if (cmd === "load_ai_config") return aiConfigWire();
    if (cmd === "save_ai_config") return "ok";
    if (cmd === "ai_key_status") return true;
    if (cmd === "ai_catalog") return catalog(options.audioIn);
    if (cmd === "ai_attach_media") return ATTACHMENT;
    if (cmd === "ai_build_context") return { dsl_context: '@project "Test Project"' };
    if (cmd === "ai_tools") return [];
    if (cmd === "sessions_new") return { id: "s1", title: String(typed.title ?? "new") };
    if (cmd === "sessions_list") return [];
    if (cmd === "sessions_load") return { meta: { id: "s1", title: "new" }, messages: [], skipped: 0 };
    if (cmd === "ai_turn_start") {
      return runFakeTurn(typed as unknown as TurnStartArgs, {
        chat: async (round) => options.chat?.(round as unknown as Record<string, unknown>) ?? { role: "assistant", content: "ok" },
      });
    }
    if (cmd === "ai_turn_answer") return answerFakeTurn(String(typed.requestId), String(typed.callId), typed.decision as never);
    if (cmd === "ai_turn_questions") return [];
    if (cmd === "ai_batch_recover") return null;
    return {};
  });
}

async function attach(container: HTMLElement) {
  openDialogMock.mockResolvedValue("/tmp/take.wav");
  fireEvent.click(container.querySelector(".ai-attach-btn") as HTMLButtonElement);
  await vi.waitFor(() => expect(container.querySelector(".ai-attachment-chip")).toBeTruthy());
  return container.querySelector(".ai-attachment-chip") as HTMLElement;
}

async function ready(container: HTMLElement) {
  await vi.waitFor(() => expect(container.querySelector(".ai-save-notice")).toBeFalsy());
  await vi.waitFor(() => expect(container.querySelector(".ai-toolset-chip")).toBeTruthy());
}

async function send(container: HTMLElement, text: string) {
  fireEvent.input(container.querySelector(".ai-input") as HTMLInputElement, { target: { value: text } });
  fireEvent.click(container.querySelector(".ai-send-btn") as HTMLButtonElement);
}

beforeEach(() => {
  // The composer store outlives a panel on purpose, so a test must start clean.
  aiSession.reset();
  invokeMock.mockClear();
  openDialogMock.mockReset();
  resetFakeTurns();
});

describe("AI panel audio attachment", () => {
  it("shows file name, mime, duration and size and removes on demand", async () => {
    mockBackend({ audioIn: true });
    const { container } = render(() => <AiPanel />);
    await ready(container);
    const chip = await attach(container);
    expect(chip.dataset.mediaPath).toBe("/tmp/take.wav");
    expect(chip.querySelector(".ai-attachment-name")?.textContent).toBe("take.wav");
    expect(chip.textContent).toContain("audio/wav");
    expect(chip.textContent).toContain("0:10");
    expect(chip.textContent).toContain("312.5 KiB");

    fireEvent.click(chip.querySelector(".ai-attachment-remove") as HTMLButtonElement);
    await vi.waitFor(() => expect(container.querySelector(".ai-attachment-chip")).toBeFalsy());
  });

  it("sends local audio metadata when the selected model cannot take audio", async () => {
    let sent: Record<string, unknown> | undefined;
    mockBackend({ audioIn: false, chat: args => { sent = args; return { role: "assistant", content: "ok" }; } });
    const { container } = render(() => <AiPanel />);
    await ready(container);
    const chip = await attach(container);
    expect(chip.textContent).toContain("Local audio reference");
    await send(container, "Analyze this reference");
    await vi.waitFor(() => expect(sent).toBeTruthy());
    const messages = sent?.messages as { media?: Record<string, unknown>[] }[];
    expect(messages.flatMap(message => message.media ?? [])).toEqual([
      expect.objectContaining({ path: "/tmp/take.wav", kind: "audio", local_reference: true }),
    ]);
  });

  it("sends a path and hash descriptor, never inline bytes", async () => {
    let sent: Record<string, unknown> | undefined;
    mockBackend({ audioIn: true, chat: args => { sent = args; return { role: "assistant", content: "ok" }; } });
    const { container } = render(() => <AiPanel />);
    await ready(container);
    await attach(container);
    await send(container, "What do you hear?");
    await vi.waitFor(() => expect(sent).toBeTruthy());

    const messages = sent?.messages as { role: string; media?: Record<string, unknown>[] }[];
    const user = messages.find(message => message.role === "user" && message.media?.length);
    expect(user?.media?.[0]?.path).toBe("/tmp/take.wav");
    expect(user?.media?.[0]?.sha256).toBe("a".repeat(64));
    expect(user?.media?.[0]?.kind).toBe("audio");
    expect(JSON.stringify(messages)).not.toContain("base64");
    expect(JSON.stringify(messages)).not.toContain("inlineData");
    await vi.waitFor(() => expect(container.querySelector(".ai-attachment-chip")).toBeFalsy());
    expect(container.querySelector(".ai-message-media")?.textContent).toContain("take.wav");
  });

  it("reports a changed file after the backend re-hashes it on send", async () => {
    mockBackend({
      audioIn: true,
      chat: () => ({
        role: "assistant",
        content: "ok",
        media_notices: [{ name: "take.wav", path: "/tmp/take.wav", sha256: "b".repeat(64) }],
      }),
    });
    const { container } = render(() => <AiPanel />);
    await ready(container);
    await attach(container);
    await send(container, "What changed?");
    await vi.waitFor(() => expect(container.querySelector(".ai-media-notice")).toBeTruthy());
    expect(container.querySelector(".ai-media-notice")?.textContent).toContain("changed on disk");
  });
});
