/**
 * A message composed while a turn runs is queued, not dropped: it waits above
 * the composer, can be edited or removed, goes out once when the turn settles,
 * and Steer stops the running turn first.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { aiConfigWire, invokeMock, makeSnapshot } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { aiLimits } from "../stores/aiLimits";
import { workspaceStore } from "../stores/workspace";
import { aiSession } from "../stores/aiSession";
import { resetStats } from "../stores/aiStats";
import { i18n } from "../stores/i18n";
import type { SessionMeta } from "../ai/sessions";
import type { MediaAttachment } from "../ai/media";
import { cancelFakeTurn, resetFakeTurns, runFakeTurn, type TurnStartArgs } from "./helpers/fakeTurn";

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

function session(overrides: Partial<SessionMeta> = {}): SessionMeta {
  return {
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
    ...overrides,
  };
}

const ATTACHMENT: MediaAttachment = { kind: "audio", name: "take.wav", path: "/tmp/take.wav", mime: "audio/wav", bytes: 320000, duration_secs: 10, sha256: "a".repeat(64) };

type ChatCall = { messages: { role: string; content?: unknown; media?: MediaAttachment[] }[]; [key: string]: unknown };

/** Everything a test can steer; the mock below only reads it. */
const backend = {
  /** The turns the test holds open, oldest first. */
  held: [] as { resolve: (value: unknown) => void; reject: (reason: unknown) => void }[],
  /** False once the test wants later turns to answer at once. */
  hold: true,
  chatCalls: [] as ChatCall[],
  /** Commands in the order the panel issued them. */
  calls: [] as string[],
  sessions: [] as SessionMeta[],
  /** Held to keep a flush between taking its row and starting its turn. */
  snapshot: null as Promise<unknown> | null,
};

function deferred<T = unknown>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((ok, fail) => { resolve = ok; reject = fail; });
  return { promise, resolve, reject };
}

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
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
      case "ai_turn_start": return runFakeTurn(typed as unknown as TurnStartArgs, {
        chat: async (round) => {
          backend.calls.push("ai_turn_start");
          backend.chatCalls.push({ ...round, messages: JSON.parse(JSON.stringify(round.messages)) } as ChatCall);
          if (!backend.hold) return { role: "assistant", content: "Done." };
          return new Promise((resolve, reject) => backend.held.push({ resolve, reject }));
        },
        tool: async () => ({ ok: true }),
      });
      case "ai_turn_questions": return [];
      case "ai_cancel": {
        backend.calls.push("ai_cancel");
        cancelFakeTurn(String(typed.requestId));
        backend.held.shift()?.reject({ kind: "cancelled", message: "Request cancelled" });
        return undefined;
      }
      case "sessions_snapshot": {
        if (backend.snapshot) return backend.snapshot;
        return { sha: "a".repeat(64) };
      }
      case "ai_attach_media": return ATTACHMENT;
      case "workspace_open": return { path: String(typed.path ?? ""), name: "other", detached: false, projects: [] };
      case "workspaces_list": return [];
      case "sessions_list": return backend.sessions.map((entry) => ({ ...entry }));
      case "sessions_new": {
        const meta = session({ id: `s${backend.sessions.length + 1}`, title: String(typed.title ?? "new") });
        backend.sessions = [meta, ...backend.sessions];
        return meta;
      }
      case "sessions_load": return { meta: session(), messages: [], skipped: 0 };
      default: return makeSnapshot();
    }
  });
}

const input = () => document.querySelector(".ai-input") as HTMLInputElement;
const sendButton = () => document.querySelector(".ai-send-btn") as HTMLButtonElement;
const switcher = () => document.querySelector(".ai-session-current") as HTMLButtonElement;
const queueRows = () => [...document.querySelectorAll(".ai-queue-row")];
const queueTexts = () => queueRows().map((row) => row.querySelector(".ai-queue-text")?.textContent ?? "");
const queueText = () => document.querySelector(".ai-queue-text")?.textContent ?? "";
const queueField = () => document.querySelector(".ai-queue-field") as HTMLInputElement | null;
const queueButton = (name: string) => document.querySelector(`.ai-queue-${name}`) as HTMLButtonElement | null;
const queueButtons = (name: string) => [...document.querySelectorAll(`.ai-queue-${name}`)] as HTMLButtonElement[];

async function mount() {
  await store.initProject();
  const view = render(() => <AiPanel />);
  await waitFor(() => expect(document.querySelector(".ai-save-notice")).toBeFalsy());
  await waitFor(() => expect(document.querySelector(".ai-toolset-chip")).toBeTruthy());
  return view;
}

async function typeAndSend(text: string) {
  fireEvent.input(input(), { target: { value: text } });
  fireEvent.click(sendButton());
}

/** Start a turn the test keeps open, so the composer has a running turn. */
async function startTurn(text = "first") {
  await typeAndSend(text);
  await waitFor(() => expect(backend.chatCalls).toHaveLength(1));
  await waitFor(() => expect(queueButton("steer")).toBeNull());
}

/** The contents of every message the panel ever sent to the provider. */
function sentContents(): string[] {
  return backend.chatCalls.flatMap((call) => call.messages.map((message) => String(message.content ?? "")));
}

beforeEach(() => {
  invokeMock.mockReset();
  backend.held = [];
  backend.hold = true;
  backend.chatCalls = [];
  backend.calls = [];
  backend.sessions = [];
  backend.snapshot = null;
  i18n.setLocale("en");
  aiLimits.setActiveContext(null);
  resetStats();
  aiSession.reset();
  installBackend();
});

afterEach(() => {
  cleanup();
  resetFakeTurns();
  aiSession.reset();
  aiLimits.setActiveContext(null);
  resetStats();
});

describe("a message composed while a turn runs", () => {
  it("queues every send while a turn runs and clears the composer each time", async () => {
    await mount();
    await startTurn();

    // The composer is usable during the turn, and Enter queues instead of sending.
    expect(input().disabled).toBe(false);
    await typeAndSend("next");
    await waitFor(() => expect(queueRows()).toHaveLength(1));
    expect(queueText()).toBe("next");
    expect(document.querySelector(".ai-queue-hint")?.textContent).toBe("Sends when this turn ends.");
    expect(input().value).toBe("");
    expect(backend.chatCalls).toHaveLength(1);

    // A second send is queued behind the first, and the composer clears again:
    // the click always does something, and no draft is left behind.
    fireEvent.input(input(), { target: { value: "third" } });
    fireEvent.keyDown(input(), { key: "Enter" });
    await waitFor(() => expect(queueRows()).toHaveLength(2));
    expect(queueTexts()).toEqual(["next", "third"]);
    expect(input().value).toBe("");
    expect(input().disabled).toBe(false);
    expect(backend.chatCalls).toHaveLength(1);
  });

  it("sends the queued messages in order, once each, when the turn settles", async () => {
    await mount();
    await startTurn();
    await typeAndSend("next");
    await typeAndSend("third");
    await waitFor(() => expect(queueRows()).toHaveLength(2));

    backend.hold = false;
    backend.held.shift()!.resolve({ role: "assistant", content: "First answer." });

    await waitFor(() => expect(backend.chatCalls).toHaveLength(3));
    await waitFor(() => expect(document.querySelector(".ai-queue-row")).toBeNull());
    // The second turn is the first queued message: nothing from the queue is
    // sent behind it, so the one message the user typed is one request.
    expect(backend.chatCalls[1].messages.map((message) => message.content)).toEqual([
      expect.stringContaining("Test Project"),
      "next",
    ]);
    expect(backend.chatCalls[2].messages.at(-1)?.content).toBe("third");
    expect(sentContents().filter((text) => text === "next")).toHaveLength(1);
    expect(sentContents().filter((text) => text === "third")).toHaveLength(1);
    // Both answers landed: the first turn's, then the queued ones'.
    await waitFor(() => expect([...document.querySelectorAll(".ai-message.assistant")].at(-1)?.textContent).toContain("Done."));
  });

  it("starts one turn when two flushes race for the queue", async () => {
    await mount();
    await startTurn("first");
    backend.hold = false;
    backend.held.shift()!.resolve({ role: "assistant", content: "First answer." });
    await waitFor(() => expect(backend.chatCalls).toHaveLength(1));
    await waitFor(() => expect(document.querySelector(".ai-pending")).toBeNull());

    // The conversation exists now, so a flush first waits for its snapshot:
    // that is the window where `loading` is still false.
    aiSession.setQueued([{ text: "one", media: [] }, { text: "two", media: [] }]);
    await waitFor(() => expect(queueRows()).toHaveLength(2));
    backend.hold = true;
    const snapshot = deferred();
    backend.snapshot = snapshot.promise;

    // The second click lands while the first flush holds the row it took.
    fireEvent.click(queueButton("send")!);
    fireEvent.click(queueButton("send")!);

    snapshot.resolve({ sha: "b".repeat(64) });
    await waitFor(() => expect(backend.chatCalls).toHaveLength(2));
    // Only the row the first flush took left the queue, and it started one
    // turn: the second row waits for that turn instead of racing it.
    expect(backend.chatCalls[1].messages.at(-1)?.content).toBe("one");
    expect(queueTexts()).toEqual(["two"]);
  });

  it("keeps the message queued when its turn is superseded before it starts", async () => {
    await mount();
    await startTurn("first");
    backend.hold = false;
    backend.held.shift()!.resolve({ role: "assistant", content: "First answer." });
    await waitFor(() => expect(backend.chatCalls).toHaveLength(1));
    await waitFor(() => expect(document.querySelector(".ai-pending")).toBeNull());

    aiSession.setQueued([{ text: "one", media: [] }]);
    await waitFor(() => expect(queueTexts()).toEqual(["one"]));
    backend.hold = true;
    const snapshot = deferred();
    backend.snapshot = snapshot.promise;
    fireEvent.click(queueButton("send")!);

    // The panel leaves for another workspace before the turn starts: the
    // message is not lost with the abandoned turn.
    await workspaceStore.open("/Users/me/other");
    await waitFor(() => expect(aiSession.open()).toBeNull());
    snapshot.resolve({ sha: "c".repeat(64) });

    await waitFor(() => expect(queueTexts()).toEqual(["one"]));
    expect(backend.chatCalls).toHaveLength(1);
  });

  it("edits the queued text in place and sends the edit", async () => {
    await mount();
    await startTurn();
    await typeAndSend("next");
    await waitFor(() => expect(queueText()).toBe("next"));

    fireEvent.click(queueButton("edit")!);
    const field = queueField()!;
    expect(field.value).toBe("next");
    fireEvent.input(field, { target: { value: "next, but quieter" } });
    fireEvent.keyDown(field, { key: "Enter" });
    await waitFor(() => expect(queueField()).toBeNull());
    expect(queueText()).toBe("next, but quieter");

    backend.hold = false;
    backend.held.shift()!.resolve({ role: "assistant", content: "First answer." });
    await waitFor(() => expect(backend.chatCalls).toHaveLength(2));
    expect(backend.chatCalls[1].messages.at(-1)?.content).toBe("next, but quieter");
    expect(sentContents()).not.toContain("next");
  });

  it("drops the queued message on Remove and sends nothing", async () => {
    await mount();
    await startTurn();
    await typeAndSend("next");
    await waitFor(() => expect(queueText()).toBe("next"));

    fireEvent.click(queueButton("remove")!);
    await waitFor(() => expect(document.querySelector(".ai-queue-row")).toBeNull());

    backend.hold = false;
    backend.held.shift()!.resolve({ role: "assistant", content: "First answer." });
    await waitFor(() => expect(document.querySelector(".ai-message.assistant")?.textContent).toContain("First answer."));
    expect(backend.chatCalls).toHaveLength(1);
    expect(sentContents()).not.toContain("next");
  });

  it("steers: cancels the running turn first, then sends the oldest message", async () => {
    await mount();
    await startTurn();
    await typeAndSend("steer this");
    await typeAndSend("and this");
    await waitFor(() => expect(queueRows()).toHaveLength(2));

    // The label says what it does to the running turn, and only the oldest row
    // carries it: the one behind it keeps waiting.
    expect(queueButtons("steer")).toHaveLength(1);
    expect(queueButton("steer")!.getAttribute("title")).toBe("Stops the current turn, then sends this message.");
    expect(backend.chatCalls).toHaveLength(1);

    // The cancel rejects the held turn and the steered one is held open too, so
    // the row behind the head is still waiting when the smoke clears.
    fireEvent.click(queueButton("steer")!);
    await waitFor(() => expect(backend.calls).toContain("ai_cancel"));
    await waitFor(() => expect(backend.chatCalls).toHaveLength(2));

    // The cancel came before the send, never after it, and only the head left.
    expect(backend.calls.indexOf("ai_cancel")).toBeLessThan(backend.calls.lastIndexOf("ai_turn_start"));
    expect(backend.chatCalls[1].messages.at(-1)?.content).toBe("steer this");
    expect(sentContents().filter((text) => text === "steer this")).toHaveLength(1);
    await waitFor(() => expect(queueTexts()).toEqual(["and this"]));
  });

  it("clears the queue when another conversation opens", async () => {
    backend.sessions = [session({ id: "s1", title: "one" }), session({ id: "s2", title: "two" })];
    await mount();
    aiSession.setQueued([{ text: "for the other conversation", media: [] }]);
    await waitFor(() => expect(queueText()).toBe("for the other conversation"));

    fireEvent.click(switcher());
    await waitFor(() => expect(document.querySelectorAll(".ai-session-row")).toHaveLength(2));
    fireEvent.click(document.querySelectorAll(".ai-session-row-title")[1] as HTMLElement);

    await waitFor(() => expect(aiSession.queued()).toEqual([]));
    expect(document.querySelector(".ai-queue-row")).toBeNull();
  });

  it("keeps the queued message across an unmount and offers to send it, not Steer", async () => {
    const first = await mount();
    await startTurn();
    await typeAndSend("next");
    await waitFor(() => expect(queueText()).toBe("next"));
    first.unmount();

    await mount();
    await waitFor(() => expect(queueText()).toBe("next"));
    // Leaving the panel cancelled the turn; the queued message did not go out on its own.
    expect(sentContents()).not.toContain("next");
    // Nothing is running, so the row says so and offers a way to send it: the
    // message is not stranded with only Edit and Remove.
    expect(document.querySelector(".ai-queue-hint")?.textContent).toBe("No turn is running.");
    expect(queueButton("steer")).toBeNull();
    const sendNow = queueButton("send")!;
    expect(sendNow.textContent).toBe("Send now");

    backend.hold = false;
    fireEvent.click(sendNow);
    await waitFor(() => expect(backend.chatCalls).toHaveLength(2));
    expect(backend.chatCalls[1].messages.at(-1)?.content).toBe("next");
    await waitFor(() => expect(document.querySelector(".ai-queue-row")).toBeNull());
  });

  it("queues the composer's attachment chips with the message", async () => {
    await mount();
    await startTurn();
    aiSession.setAttachments([ATTACHMENT]);
    await typeAndSend("next");

    await waitFor(() => expect(queueText()).toBe("next"));
    expect(document.querySelector(".ai-queue-media")?.textContent).toBe("take.wav");
    expect(aiSession.attachments()).toHaveLength(0);

    backend.hold = false;
    backend.held.shift()!.resolve({ role: "assistant", content: "First answer." });
    await waitFor(() => expect(backend.chatCalls).toHaveLength(2));
    expect(backend.chatCalls[1].messages.at(-1)?.media).toEqual([{ ...ATTACHMENT, local_reference: true }]);
  });
});
