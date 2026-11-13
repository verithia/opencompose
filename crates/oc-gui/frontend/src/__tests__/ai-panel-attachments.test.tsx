/**
 * AI panel attachments beyond the audio chip: a saved image or clip is restored
 * from the store, a file that is gone is shown as missing instead of failing the
 * turn, and image input is gated on the model's declared modalities.
 */
import { beforeEach, describe, expect, it, vi } from "vitest";
import { fireEvent, render } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, openDialogMock, aiConfigWire } from "./setup";
import { AiPanel } from "../components/AiPanel";
import { aiSession } from "../stores/aiSession";
import { answerFakeTurn, resetFakeTurns, runFakeTurn, type TurnStartArgs } from "./helpers/fakeTurn";
import type { MediaAttachment } from "../ai/media";

const CAPABILITIES = { text: true, audio_in: false, audio_out: false, native_tool_calling: true, streaming: true };

const CATALOG = {
  version: "fixture",
  providers: [{
    id: "deepseek",
    name: "DeepSeek",
    kind: "openai_chat_completions",
    hosts: ["api.deepseek.com"],
    auth_providers: [],
    models: [{
      id: "deepseek-flash",
      name: "DeepSeek Flash",
      context: 900000,
      max_output: 8192,
      capabilities: CAPABILITIES,
      status: "active",
    }],
  }],
};

const AUDIO: MediaAttachment = {
  kind: "audio", name: "take.wav", path: "/tmp/take.wav", mime: "audio/wav",
  bytes: 320000, duration_secs: 10, sha256: "a".repeat(64),
};

const IMAGE: MediaAttachment = {
  kind: "image", name: "mix.png", path: "/tmp/mix.png", mime: "image/png",
  bytes: 2048, duration_secs: null, sha256: "c".repeat(64),
};

const META = {
  id: "saved-1", title: "reference mix", created: "2026-10-29T07:00:00.000Z",
  updated: "2026-10-29T07:00:00.000Z", model: "deepseek-flash", provider: "deepseek-v4-flash",
  workspace_key: "3f2a000000000000", turns: 1, tokens: 12,
};

type Backend = {
  attach: () => unknown;
  chat: (args: Record<string, unknown>) => unknown;
  limits: unknown;
  loaded: unknown;
  chatCalls: Record<string, unknown>[];
};

function installBackend(overrides: Partial<Backend> = {}): Backend {
  const backend: Backend = {
    attach: () => AUDIO,
    chat: () => ({ role: "assistant", content: "ok" }),
    limits: {
      provider_id: "deepseek-v4-flash", model_id: "deepseek-flash",
      bundled: { context: 900000, max_output: 8192, input: { text: true, image: false, video: false, audio: false }, price: null },
      overridden: null,
    },
    loaded: { meta: META, messages: [], skipped: 0 },
    chatCalls: [],
    ...overrides,
  };
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const typed = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "load_ai_config": return aiConfigWire();
      case "save_ai_config": return "ok";
      case "ai_key_status": return true;
      case "ai_catalog": return CATALOG;
      case "ai_model_limits": return backend.limits;
      case "ai_toolsets": return [];
      case "ai_tools": return [];
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
      case "ai_attach_media": return backend.attach();
      case "sessions_list": return [META];
      case "sessions_new": return { ...META, id: String(typed.title ?? META.id) };
      case "sessions_load": return backend.loaded;
      case "ai_turn_start": {
        return runFakeTurn(typed as unknown as TurnStartArgs, {
          chat: async (round) => {
            backend.chatCalls.push(round as unknown as Record<string, unknown>);
            return backend.chat(round as unknown as Record<string, unknown>);
          },
        });
      }
      case "ai_turn_answer": return answerFakeTurn(String(typed.requestId), String(typed.callId), typed.decision as never);
      case "ai_turn_questions": return [];
      case "ai_batch_recover": return null;
      default: return makeSnapshot();
    }
  });
  return backend;
}

async function ready(container: HTMLElement) {
  await vi.waitFor(() => expect(container.querySelector(".ai-save-notice")).toBeFalsy());
  await vi.waitFor(() => expect(container.querySelector(".ai-toolset-chip")).toBeTruthy());
}

async function attach(container: HTMLElement, path: string) {
  openDialogMock.mockResolvedValue(path);
  fireEvent.click(container.querySelector(".ai-attach-btn") as HTMLButtonElement);
  await vi.waitFor(() => expect(container.querySelector(".ai-attachment-chip")).toBeTruthy());
}

async function send(container: HTMLElement, text: string) {
  fireEvent.input(container.querySelector(".ai-input") as HTMLInputElement, { target: { value: text } });
  fireEvent.click(container.querySelector(".ai-send-btn") as HTMLButtonElement);
}

beforeEach(() => {
  aiSession.reset();
  invokeMock.mockClear();
  openDialogMock.mockReset();
  resetFakeTurns();
});

describe("restored attachments", () => {
  it("paints a saved chip and lets the next turn send without retyping", async () => {
    const backend = installBackend({
      loaded: {
        meta: META,
        messages: [{ role: "user", content: "what do you hear?", media: [AUDIO] }],
        skipped: 0,
      },
    });
    aiSession.setOpen(META.id);
    const { container } = render(() => <AiPanel />);
    await ready(container);
    await vi.waitFor(() => {
      const tag = container.querySelector(".ai-media-tag") as HTMLElement | null;
      expect(tag).toBeTruthy();
      expect(tag!.textContent).toContain("take.wav");
      expect(tag!.textContent).toContain("audio/wav");
    });

    await send(container, "and now the low end?");
    await vi.waitFor(() => expect(backend.chatCalls.length).toBe(1));
    // The panel hands the new question to the backend; the saved message, media
    // included, is replayed from the store by `prepare_turn` and re-hashed there.
    const sent = backend.chatCalls[0].messages as { role: string; content: string }[];
    expect(sent.at(-1)).toEqual({ role: "user", message_meta: { version: 1, kind: "user" }, content: "and now the low end?" });
    expect(container.querySelector(".ai-media-tag")?.textContent).toContain("take.wav");
  });

  it("shows a missing saved file as a state and still sends the text", async () => {
    const missing = { ...AUDIO, missing: true };
    const backend = installBackend({
      loaded: {
        meta: META,
        messages: [{ role: "user", content: "what do you hear?", media: [missing] }],
        skipped: 0,
      },
      chat: () => ({
        role: "assistant", content: "ok",
        media_notices: [{ name: AUDIO.name, path: AUDIO.path, sha256: AUDIO.sha256, missing: true }],
      }),
    });
    aiSession.setOpen(META.id);
    const { container } = render(() => <AiPanel />);
    await ready(container);
    await vi.waitFor(() => {
      const tag = container.querySelector(".ai-media-tag") as HTMLElement | null;
      expect(tag?.textContent).toContain("file is missing");
    });

    await send(container, "keep going");
    await vi.waitFor(() => expect(backend.chatCalls.length).toBe(1));
    await vi.waitFor(() => {
      expect(container.querySelector(".ai-media-error")?.textContent).toContain(
        "is missing on disk; it was not sent",
      );
    });
    expect((backend.chatCalls[0].messages as unknown[]).length).toBeGreaterThan(0);
  });
});

describe("image input gate", () => {
  it("refuses an image on a text-only model before the request, naming the model", async () => {
    const backend = installBackend({ attach: () => IMAGE });
    const { container } = render(() => <AiPanel />);
    await ready(container);
    await attach(container, "/tmp/mix.png");
    await vi.waitFor(() => {
      expect(container.querySelector(".ai-media-warning")?.textContent).toContain("DeepSeek Flash");
    });

    await send(container, "match this mix");
    await vi.waitFor(() => {
      expect(container.querySelector(".ai-media-error")?.textContent).toContain(
        "DeepSeek Flash cannot accept image input",
      );
    });
    expect(container.querySelector(".ai-media-error")?.textContent).toContain("declare text");
    expect(backend.chatCalls).toHaveLength(0);
    // The file stays attached, so the user can switch models and send it.
    expect(container.querySelector(".ai-attachment-chip")).toBeTruthy();
  });

  it("sends the image when the model limits declare image input", async () => {
    const backend = installBackend({
      attach: () => IMAGE,
      limits: {
        provider_id: "deepseek-v4-flash", model_id: "deepseek-flash",
        bundled: { context: 900000, max_output: 8192, input: { text: true, image: true, video: false, audio: false }, price: null },
        overridden: null,
      },
    });
    const { container } = render(() => <AiPanel />);
    await ready(container);
    await attach(container, "/tmp/mix.png");
    await vi.waitFor(() => expect(container.querySelector(".ai-media-warning")).toBeFalsy());

    await send(container, "match this mix");
    await vi.waitFor(() => expect(backend.chatCalls.length).toBe(1));
    const sent = backend.chatCalls[0].messages as { role: string; media?: MediaAttachment[] }[];
    const user = sent.find(message => message.role === "user");
    expect(user?.media?.[0]).toMatchObject({ kind: "image", name: "mix.png" });
  });
});
