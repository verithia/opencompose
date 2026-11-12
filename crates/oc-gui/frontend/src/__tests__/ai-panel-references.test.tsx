/**
 * The `@` palette scopes one request to a project path: the composer shows the
 * chosen path, `ai_build_context` receives it, and a queued message keeps it
 * until that message is the one being sent.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack, aiConfigWire } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { aiLimits } from "../stores/aiLimits";
import { aiSession } from "../stores/aiSession";
import { resetStats } from "../stores/aiStats";
import { i18n } from "../stores/i18n";

const CAPABILITIES = { text: true, audio_in: false, audio_out: false, native_tool_calling: true, streaming: true };

const CATALOG = {
  version: "fixture",
  providers: [
    {
      id: "deepseek",
      name: "DeepSeek",
      kind: "openai_chat_completions",
      hosts: ["api.deepseek.com"],
      auth_providers: [],
      models: [
        { id: "deepseek-flash", name: "DeepSeek Flash", context: 900000, max_output: 8192, capabilities: CAPABILITIES, status: "active" },
      ],
    },
  ],
};

const SETS = [
  { id: "project-edit", title: "Project edit", description: "Tracks, presets and buses", tools: ["tracks_list"], default_on: true },
];

const SESSION = {
  id: "s1",
  title: "first",
  created: "2026-10-28T09:00:00.000Z",
  updated: "2026-10-28T09:00:00.000Z",
  model: "deepseek-flash",
  provider: "deepseek",
  workspace_key: "3f2a1b0c",
  workspace: "/Users/me/songs",
  turns: 1,
  tokens: 20,
};

const backend = {
  /** The turn the test holds open, when it wants the composer to queue. */
  held: [] as { resolve: (value: unknown) => void; reject: (reason: unknown) => void }[],
  hold: false,
  chatCalls: 0,
  /** The path each `ai_build_context` call asked for, in order. */
  contexts: [] as string[],
};

function installBackend() {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const typed = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "load_ai_config": return aiConfigWire();
      case "save_ai_config": return { path: "/tmp/ai-providers.yaml", warnings: [] };
      case "ai_key_status": return true;
      case "ai_catalog": return CATALOG;
      case "ai_model_limits": return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
      case "ai_toolsets": return SETS;
      case "ai_tools": return [];
      // Nothing to recover: a snapshot here would replace the open project.
      case "ai_batch_recover": return null;
      case "ai_active_turn": return null;
      case "ai_build_context": {
        backend.contexts.push(String(typed.targetPath));
        return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
      }
      case "ai_chat": {
        backend.chatCalls += 1;
        if (!backend.hold) return { role: "assistant", content: "Done." };
        return new Promise((resolve, reject) => backend.held.push({ resolve, reject }));
      }
      case "ai_cancel": return undefined;
      case "sessions_snapshot": return { sha: "a".repeat(64) };
      case "sessions_list": return [];
      case "sessions_new": return SESSION;
      case "sessions_load": return { meta: SESSION, messages: [], skipped: 0 };
      case "get_project": return makeSnapshot({ tracks: [makeTrack("Bass", 0), makeTrack("Pad", 1)] });
      default: return makeSnapshot();
    }
  });
}

const input = () => document.querySelector(".ai-input") as HTMLTextAreaElement;
const sendButton = () => document.querySelector(".ai-send-btn") as HTMLButtonElement;
const rows = () => [...document.querySelectorAll(".ai-reference-row")];
const rowPaths = () => rows().map(row => row.querySelector(".ai-reference-path")?.textContent ?? "");
const chip = () => document.querySelector(".ai-scope-chip");
const chipPath = () => document.querySelector(".ai-scope-path")?.textContent ?? "";

async function mount() {
  await store.initProject();
  const view = render(() => <AiPanel />);
  await waitFor(() => expect(document.querySelector(".ai-save-notice")).toBeFalsy());
  await waitFor(() => expect(document.querySelector(".ai-toolset-chip")).toBeTruthy());
  return view;
}

/** Type a draft that ends in an `@` token and pick the first palette row. */
async function scopeTo(draft: string, key: "Enter" | "Escape" = "Enter") {
  fireEvent.input(input(), { target: { value: draft } });
  await waitFor(() => expect(rows().length).toBeGreaterThan(0));
  fireEvent.keyDown(input(), { key });
}

beforeEach(() => {
  invokeMock.mockReset();
  backend.held = [];
  backend.hold = false;
  backend.chatCalls = 0;
  backend.contexts = [];
  i18n.setLocale("en");
  aiLimits.setActiveContext(null);
  resetStats();
  aiSession.reset();
  installBackend();
});

afterEach(() => {
  cleanup();
  aiSession.reset();
  aiLimits.setActiveContext(null);
  resetStats();
});

describe("the @ palette", () => {
  it("lists matching project paths as the draft is typed", async () => {
    await mount();
    expect(store.project.tracks.map(track => track.name)).toEqual(["Bass", "Pad"]);
    fireEvent.input(input(), { target: { value: "turn up @tr" } });
    await waitFor(() => expect(rowPaths()).toEqual(["track:Bass", "track:Pad"]));

    // The query narrows the list without touching the draft.
    fireEvent.input(input(), { target: { value: "turn up @pad" } });
    await waitFor(() => expect(rowPaths()).toEqual(["track:Pad"]));
    expect(input().value).toBe("turn up @pad");

    // A draft that starts with `/` is a command: the palette stands aside.
    fireEvent.input(input(), { target: { value: "/goal @tr" } });
    await waitFor(() => expect(rows()).toHaveLength(0));
  });

  it("scopes the request to the chosen path and clears the token", async () => {
    await mount();
    await scopeTo("make the bass louder @ba");
    await waitFor(() => expect(chip()).toBeTruthy());
    expect(chipPath()).toContain("track:Bass");
    expect(input().value).toBe("make the bass louder");
    expect(rows()).toHaveLength(0);

    fireEvent.click(sendButton());
    await waitFor(() => expect(backend.chatCalls).toBe(1));
    expect(backend.contexts).toEqual(["track:Bass"]);
    // The path covers one request; the next one reads the whole project again.
    await waitFor(() => expect(chip()).toBeFalsy());
  });

  it("drops the token on Escape instead of sending", async () => {
    await mount();
    await scopeTo("make the bass louder @ba", "Escape");
    expect(input().value).toBe("make the bass louder");
    expect(rows()).toHaveLength(0);
    expect(chip()).toBeFalsy();
    expect(backend.chatCalls).toBe(0);
  });

  it("clears a chosen path from the chip", async () => {
    await mount();
    await scopeTo("make the bass louder @ba");
    await waitFor(() => expect(chip()).toBeTruthy());

    fireEvent.click(document.querySelector(".ai-scope-clear")!);
    await waitFor(() => expect(chip()).toBeFalsy());
    fireEvent.click(sendButton());
    await waitFor(() => expect(backend.contexts).toEqual(["project"]));
  });

  it("reads the whole project when a queued path is no longer in it", async () => {
    await mount();
    aiSession.setQueued([{ text: "make it louder", media: [], reference: "track:Ghost" }]);
    await waitFor(() => expect(document.querySelector(".ai-queue-row")).toBeTruthy());

    fireEvent.click(document.querySelector(".ai-queue-send")!);
    await waitFor(() => expect(backend.contexts).toEqual(["project"]));
  });

  it("keeps the path of a queued message until that message is sent", async () => {
    await mount();
    // A turn in flight takes the scoped message as a queued one.
    backend.hold = true;
    fireEvent.input(input(), { target: { value: "first" } });
    fireEvent.click(sendButton());
    await waitFor(() => expect(backend.chatCalls).toBe(1));

    await scopeTo("make the pad quieter @pa");
    await waitFor(() => expect(chip()).toBeTruthy());
    fireEvent.click(sendButton());
    await waitFor(() => expect(document.querySelector(".ai-queue-row")).toBeTruthy());
    // The composer is free for the next message, which reads the whole project.
    await waitFor(() => expect(chip()).toBeFalsy());

    backend.hold = false;
    backend.held.shift()!.resolve({ role: "assistant", content: "Done." });
    await waitFor(() => expect(backend.contexts).toEqual(["project", "track:Pad"]));
  });
});
