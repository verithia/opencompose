/**
 * A message composed while a turn runs is queued, not dropped: it waits above
 * the composer, can be edited or removed, goes out once when the turn settles,
 * and Steer stops the running turn first.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
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
import { cancelFakeTurn, resetFakeTurns, runFakeTurn, steerFakeTurn, type TurnStartArgs } from "./helpers/fakeTurn";

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
  /** What `ai_active_turn` reports; a test sets it to the turn it keeps running. */
  active: null as { request_id: string; session_id: string } | null,
  /** Every `ai_turn_start`, including one the backend refuses. */
  starts: 0,
  /** When set, the next `ai_turn_start` is refused before the turn starts. */
  refuseStart: null as unknown,
  /** Context paths `ai_build_context` was asked for, in order. */
  contexts: [] as string[],
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
      case "ai_build_context":
        backend.contexts.push(String(typed.targetPath ?? typed.path ?? ""));
        return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
      case "ai_active_turn": return backend.active;
      case "ai_turn_start": {
        backend.starts += 1;
        if (backend.refuseStart) {
          const refusal = backend.refuseStart;
          backend.refuseStart = null;
          throw refusal;
        }
        return runFakeTurn(typed as unknown as TurnStartArgs, {
        chat: async (round) => {
          backend.calls.push("ai_turn_start");
          backend.chatCalls.push({ ...round, messages: JSON.parse(JSON.stringify(round.messages)) } as ChatCall);
          if (!backend.hold) return { role: "assistant", content: "Done." };
          return new Promise((resolve, reject) => backend.held.push({ resolve, reject }));
        },
        tool: async () => ({ ok: true }),
      });
      }
      case "ai_turn_questions": return [];
      case "ai_turn_steer": backend.calls.push("ai_turn_steer"); return steerFakeTurn(String(typed.requestId), String(typed.sessionId), typed.message as never);
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
  backend.active = null;
  backend.starts = 0;
  backend.refuseStart = null;
  backend.contexts = [];
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

  it("steers into the same request after the provider finishes, without cancelling", async () => {
    await mount();
    await startTurn();
    await typeAndSend("steer this");
    await typeAndSend("and this");
    expect(queueRows()).toHaveLength(2);
    expect(queueButton("steer")!.getAttribute("title")).toBe(i18n.t("ai.queue.steerHint"));
    expect(screen.getByRole("button", { name: "Steer" })).toBe(queueButton("steer"));
    fireEvent.click(queueButton("steer")!);
    await waitFor(() => expect(backend.calls).toContain("ai_turn_steer"));
    expect(backend.calls).not.toContain("ai_cancel");
    expect(queueTexts()).toEqual(["steer this", "and this"]);
    expect(queueButton("steer")!.disabled).toBe(true);
    fireEvent.click(queueButton("steer")!);
    expect(backend.calls.filter(call => call === "ai_turn_steer")).toHaveLength(1);
    backend.held.shift()!.resolve({ role: "assistant", content: "First answer." });
    await waitFor(() => expect(backend.chatCalls).toHaveLength(2));
    expect(backend.chatCalls[1].requestId).toBe(backend.chatCalls[0].requestId);
    expect(backend.chatCalls[1].messages.at(-1)?.content).toBe("steer this");
    await waitFor(() => expect(queueTexts()).toEqual(["and this"]));
    expect(sentContents().filter(text => text === "steer this")).toHaveLength(1);
    expect(document.body.textContent).not.toContain("Cancelled");
    expect(document.body.textContent).not.toContain("Applied edits kept");
  });

  it("acknowledges a delivered steer even after its panel closes", async () => {
    const view = await mount();
    await startTurn();
    await typeAndSend("steer before closing");
    fireEvent.click(queueButton("steer")!);
    await waitFor(() => expect(backend.calls).toContain("ai_turn_steer"));
    view.unmount();
    backend.hold = false;
    backend.held.shift()!.resolve({ role: "assistant", content: "Round complete." });
    await waitFor(() => expect(aiSession.queued()).toHaveLength(0));
    expect(backend.calls).not.toContain("ai_cancel");
    expect(backend.chatCalls[1].messages.at(-1)?.content).toBe("steer before closing");
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

  it("keeps the queued message across an unmount and re-attaches to the turn still running", async () => {
    const first = await mount();
    await startTurn();
    await typeAndSend("next");
    await waitFor(() => expect(queueText()).toBe("next"));
    first.unmount();
    // Closing the panel leaves the backend turn running, and the backend says so.
    const turn = backend.chatCalls[0] as unknown as { requestId: string; sessionId: string };
    backend.active = { request_id: turn.requestId, session_id: turn.sessionId };

    await mount();
    await waitFor(() => expect(document.querySelector(".ai-turn-orphan")).toBeTruthy());
    expect(queueText()).toBe("next");
    expect(sentContents()).not.toContain("next");
    expect(backend.calls).not.toContain("ai_cancel");
    // The reopened panel follows that turn: the row waits behind it and offers
    // Steer, not a Send now that would race it.
    expect(document.querySelector(".ai-queue-hint")?.textContent).toBe(i18n.t("ai.queue.waitingReload"));
    expect(queueButton("steer")).toBeTruthy();
    expect(queueButton("send")).toBeNull();

    // The turn ends; the queued message goes out once, after it.
    backend.hold = false;
    backend.held.shift()!.resolve({ role: "assistant", content: "First answer." });
    backend.active = null;
    await waitFor(() => expect(backend.chatCalls).toHaveLength(2), { timeout: 3000 });
    expect(backend.chatCalls[1].messages.at(-1)?.content).toBe("next");
    await waitFor(() => expect(document.querySelector(".ai-queue-row")).toBeNull());
    expect(sentContents().filter((text) => text === "next")).toHaveLength(1);
  });

  it("offers Send now for a queued message when nothing is running", async () => {
    backend.sessions = [session()];
    aiSession.setOpen("s1");
    aiSession.setQueued([{ text: "left over", media: [] }]);
    await mount();
    await waitFor(() => expect(queueText()).toBe("left over"));
    expect(document.querySelector(".ai-queue-hint")?.textContent).toBe("No turn is running.");
    expect(queueButton("steer")).toBeNull();
    // The visible word is the accessible name; the hint is the title.
    const sendNow = screen.getByRole("button", { name: "Send now" });
    expect(sendNow).toBe(queueButton("send"));
    expect(sendNow.getAttribute("title")).toBe(i18n.t("ai.queue.sendNowHint"));
    expect(sendNow.hasAttribute("aria-label")).toBe(false);

    backend.hold = false;
    fireEvent.click(sendNow);
    await waitFor(() => expect(backend.chatCalls).toHaveLength(1));
    expect(backend.chatCalls[0].messages.at(-1)?.content).toBe("left over");
  });

  it("puts a flushed row back at the head of the queue when its turn fails to start", async () => {
    await mount();
    await startTurn("first");
    await typeAndSend("next");
    await typeAndSend("third");
    await waitFor(() => expect(queueTexts()).toEqual(["next", "third"]));
    // The composer holds a draft, so it cannot take the message back.
    fireEvent.input(input(), { target: { value: "half typed" } });
    backend.refuseStart = { kind: "provider", message: "start refused" };

    backend.held.shift()!.resolve({ role: "assistant", content: "First answer." });
    await waitFor(() => expect(backend.starts).toBe(2));
    await waitFor(() => expect(document.body.textContent).toContain("Error: start refused"));
    expect(queueTexts()).toEqual(["next", "third"]);
    expect(input().value).toBe("half typed");
    // The failed row waits for the person instead of being retried at once.
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(backend.starts).toBe(2);
    expect(sentContents()).not.toContain("next");
  });

  it("does not steer a row that reads a project path, and sends it with its context afterwards", async () => {
    await mount();
    await startTurn();
    aiSession.setQueued([{ text: "tune the project", media: [], reference: "project" }]);
    await waitFor(() => expect(queueText()).toBe("tune the project"));

    const steer = screen.getByRole("button", { name: "Steer" }) as HTMLButtonElement;
    expect(steer).toBe(queueButton("steer"));
    expect(steer.disabled).toBe(true);
    expect(steer.getAttribute("title")).toBe(i18n.t("ai.queue.steerReference"));
    // The handler refuses too, for a click that reaches it past the button.
    steer.disabled = false;
    fireEvent.click(steer);
    await Promise.resolve();
    expect(backend.calls).not.toContain("ai_turn_steer");

    const contexts = backend.contexts.length;
    backend.hold = false;
    backend.held.shift()!.resolve({ role: "assistant", content: "First answer." });
    await waitFor(() => expect(backend.chatCalls).toHaveLength(2));
    // The person's own words are the message; the path is read as the context.
    expect(backend.chatCalls[1].messages.at(-1)?.content).toBe("tune the project");
    expect(backend.contexts.slice(contexts)).toEqual(["project"]);
    expect([...document.querySelectorAll(".ai-message.user")].at(-1)?.textContent).toBe("tune the project");
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
