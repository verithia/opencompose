/**
 * What a reloaded window keeps. The composer, its chips and the messages queued
 * behind a running turn come back with the conversation; the turn itself is
 * reported only when the backend still has it, because a window that paints a
 * turn from a marker alone would be painting something that is not running.
 *
 * A reload is simulated the way the browser does it: the store is read again
 * from `localStorage` without the unmounted panel's cleanup having cleared it,
 * which is what a new page instance sees.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, aiConfigWire } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { aiLimits } from "../stores/aiLimits";
import { aiSession } from "../stores/aiSession";
import { resetStats } from "../stores/aiStats";
import { i18n } from "../stores/i18n";
import type { ActiveTurn, SessionMeta } from "../ai/sessions";
import type { ProjectSnapshot } from "../types/project";
import type { WireMessage } from "../ai/wire";
import type { MediaAttachment } from "../ai/media";
import { answerFakeTurn, cancelFakeTurn, resetFakeTurns, runFakeTurn, type TurnStartArgs } from "./helpers/fakeTurn";

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

const STORAGE_KEY = "opencompose.ai.panel.v1";

// jsdom does not expose Web Storage in this environment; the store only needs a map.
const storage = new Map<string, string>();
if (!window.localStorage) {
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      get length() {
        return storage.size;
      },
      key: (index: number) => Array.from(storage.keys())[index] ?? null,
      getItem: (key: string) => storage.get(key) ?? null,
      setItem: (key: string, value: string) => {
        storage.set(key, String(value));
      },
      removeItem: (key: string) => {
        storage.delete(key);
      },
      clear: () => {
        storage.clear();
      },
    },
  });
}

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

type ChatCall = { messages: { role: string; content?: unknown; media?: MediaAttachment[] }[]; [key: string]: unknown };

/** Everything a test can steer; the mock only reads it. */
const backend = {
  /** False once a test wants later turns to answer at once. */
  hold: true,
  /** The turn `ai_active_turn` reports; the mock clears it after one answer. */
  active: null as ActiveTurn | null,
  /** True when a single answer has to be consumed, as a turn that just ended. */
  activeOnce: false,
  chatCalls: [] as ChatCall[],
  /** Commands in the order the panel issued them. */
  calls: [] as string[],
  /** Arguments of each `ai_cancel`, in order. */
  cancels: [] as { requestId: string }[],
  sessions: [] as SessionMeta[],
  /** What `sessions_load` returns; the panel repaints this after a reload. */
  loaded: [] as WireMessage[],
  /** Answers of `sessions_load`, oldest first; the last one repeats. */
  loads: [] as WireMessage[][],
  /** The conversation each `sessions_load` asked for, in order. */
  loadedIds: [] as string[],
  /** What `ai_batch_recover` answers: the restored document, or null. */
  recovered: null as ProjectSnapshot | null,
  /** The questions the inherited turn waits on. */
  questions: [] as { call_id: string; tool: string; args: Record<string, unknown> }[],
  /** Every `ai_turn_answer`, in order. */
  answers: [] as Record<string, unknown>[],
};

function installBackend() {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    backend.calls.push(cmd);
    const typed = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "load_ai_config": return aiConfigWire();
      case "save_ai_config": return { path: "/tmp/ai-providers.yaml", warnings: [] };
      case "ai_key_status": return true;
      case "ai_catalog": return CATALOG;
      case "ai_model_limits": return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
      case "ai_toolsets": return SETS;
      case "ai_tools": return [];
      case "ai_batch_recover": return backend.recovered;
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
      case "ai_active_turn": {
        const answer = backend.active;
        if (backend.activeOnce) backend.active = null;
        return answer;
      }
      case "ai_turn_start": {
        return runFakeTurn(typed as unknown as TurnStartArgs, {
          chat: async (round) => {
            backend.chatCalls.push({ ...round, messages: JSON.parse(JSON.stringify(round.messages)) } as ChatCall);
            if (!backend.hold) return { role: "assistant", content: "Done." };
            return new Promise(() => {});
          },
        });
      }
      case "ai_turn_answer": {
        backend.answers.push(typed);
        if (backend.active && typed.requestId === backend.active.request_id) return undefined;
        return answerFakeTurn(String(typed.requestId), String(typed.callId), typed.decision as never);
      }
      case "ai_turn_questions": return backend.questions;
      case "ai_cancel": {
        backend.cancels.push({ requestId: String(typed.requestId ?? "") });
        cancelFakeTurn(String(typed.requestId ?? ""));
        // The inherited turn closes and lets go of its slot.
        if (backend.active?.request_id === typed.requestId) backend.active = null;
        return undefined;
      }
      case "sessions_list": return backend.sessions.map((entry) => ({ ...entry }));
      case "sessions_new": {
        const meta = session({ id: `s${backend.sessions.length + 1}`, title: String(typed.title ?? "new") });
        backend.sessions = [meta, ...backend.sessions];
        return meta;
      }
      case "sessions_load": {
        const id = String(typed.id ?? "s1");
        backend.loadedIds.push(id);
        const next = backend.loads.length > 0 ? backend.loads.shift()! : backend.loaded;
        return { meta: session({ id }), messages: next, skipped: 0 };
      }
      default: return makeSnapshot();
    }
  });
}

const input = () => document.querySelector(".ai-input") as HTMLTextAreaElement;
const sendButton = () => document.querySelector(".ai-send-btn") as HTMLButtonElement;
const queueRows = () => [...document.querySelectorAll(".ai-queue-row")];
const queueTexts = () => queueRows().map((row) => row.querySelector(".ai-queue-text")?.textContent ?? "");
const queueButton = (name: string) => document.querySelector(`.ai-queue-${name}`) as HTMLButtonElement | null;
const orphanBanner = () => document.querySelector(".ai-turn-orphan");
const lostBanner = () => document.querySelector(".ai-turn-reloaded");
const runningRow = () => document.querySelector(".ai-pending");
const rows = () => [...document.querySelectorAll(".ai-message")];

/** The stored panel state, as a reloaded window would read it. */
function storedState(): {
  open: string | null;
  conversations: Record<string, { draft?: string; queue?: { text: string }[]; attachments?: MediaAttachment[] }>;
  turn: { session: string; started: number } | null;
} {
  return JSON.parse(window.localStorage.getItem(STORAGE_KEY) ?? "{}");
}

/** Writes what the previous window left on disk, then reads it as a reload does. */
function reloadWith(state: {
  open: string | null;
  draft?: string;
  queue?: { text: string }[];
  turn?: { session: string; started: number } | null;
}) {
  // A persisted conversation must also exist in this workspace's backend index.
  // The panel deliberately refuses storage-only IDs from another workspace.
  backend.sessions = state.open ? [session({ id: state.open })] : [];
  const conversations: Record<string, unknown> = {};
  if (state.draft || state.queue?.length) {
    conversations[state.open ?? ""] = {
      ...(state.draft ? { draft: state.draft } : {}),
      ...(state.queue?.length ? { queue: state.queue.map((row) => ({ text: row.text, media: [] })) } : {}),
    };
  }
  window.localStorage.setItem(STORAGE_KEY, JSON.stringify({
    v: 1,
    open: state.open,
    conversations,
    turn: state.turn ?? null,
  }));
  aiSession.reload();
}

/** Lets the panel's mount chain finish: every mocked invoke resolves at once. */
const settleMount = () => new Promise((resolve) => setTimeout(resolve, 0));

async function mount() {
  await store.initProject();
  const view = render(() => <AiPanel />);
  await waitFor(() => expect(document.querySelector(".ai-save-notice")).toBeFalsy());
  await waitFor(() => expect(document.querySelector(".ai-toolset-chip")).toBeTruthy());
  await settleMount();
  return view;
}

/** Starts the turn and leaves it running, so the composer has a turn to queue behind. */
async function startTurn(text = "first") {
  fireEvent.input(input(), { target: { value: text } });
  fireEvent.keyDown(input(), { key: "Enter" });
  await waitFor(() => expect(backend.chatCalls).toHaveLength(1));
}

async function queue(text: string) {
  fireEvent.input(input(), { target: { value: text } });
  fireEvent.keyDown(input(), { key: "Enter" });
  await waitFor(() => expect(queueTexts()).toContain(text));
}

beforeEach(() => {
  invokeMock.mockReset();
  resetFakeTurns();
  backend.hold = true;
  backend.active = null;
  backend.activeOnce = false;
  backend.questions = [];
  backend.answers = [];
  backend.chatCalls = [];
  backend.calls = [];
  backend.cancels = [];
  backend.sessions = [];
  backend.loaded = [];
  backend.loads = [];
  backend.loadedIds = [];
  backend.recovered = null;
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

describe("a reloaded window", () => {
  it("recovers a stranded panel batch once on mount and shows the document it committed", async () => {
    reloadWith({ open: "s1" });
    backend.loaded = [{ role: "user", content: "first" }];
    const restored = { ...makeSnapshot(), dsl_source: '@project "Committed"\n  tempo: 120\n' };
    backend.recovered = restored;
    await mount();

    // The panel recovers the orphan exactly once, before its first turn.
    expect(backend.calls.filter((command) => command === "ai_batch_recover")).toHaveLength(1);
    // What is on screen is the document the recovery committed.
    expect(store.dslSource()).toBe(restored.dsl_source);
  });

  it("keeps the draft and the queued messages with their conversation, in order", async () => {
    await mount();
    await startTurn();
    await queue("next");
    await queue("third");
    // The composer holds typing that was never sent.
    fireEvent.input(input(), { target: { value: "half-typed ask" } });
    await waitFor(() => expect(input().value).toBe("half-typed ask"));

    // The draft write is debounced, so it lands once typing pauses. This is
    // what the next page instance finds: the draft and the queue under the
    // conversation, plus the marker naming the running turn.
    await waitFor(() => expect(storedState().conversations.s1?.draft).toBe("half-typed ask"));
    const before = storedState();
    expect(before.open).toBe("s1");
    expect(before.conversations.s1.draft).toBe("half-typed ask");
    expect(before.conversations.s1.queue?.map((row) => row.text)).toEqual(["next", "third"]);
    expect(before.turn?.session).toBe("s1");

    // The window goes away. The panel cancels its own turn on the way out, so
    // the marker it kept is spent; the draft and the queue are not.
    cleanup();
    await waitFor(() => expect(storedState().turn).toBeNull());
    aiSession.reload();
    await mount();

    expect(input().value).toBe("half-typed ask");
    expect(queueTexts()).toEqual(["next", "third"]);

    // One message leaves the queue per settled turn, oldest first.
    backend.hold = false;
    fireEvent.click(queueButton("send")!);
    await waitFor(() => expect(backend.chatCalls.length).toBe(2));
    expect(backend.chatCalls[1].messages.map((message) => message.content)).toEqual([
      expect.stringContaining("Test Project"),
      "next",
    ]);
    await waitFor(() => expect(backend.chatCalls.length).toBe(3));
    expect(backend.chatCalls[2].messages.at(-1)?.content).toBe("third");
    await waitFor(() => expect(queueRows()).toHaveLength(0));
  });

  it("keeps the turn marker while the backend still reports the turn", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.loaded = [{ role: "user", content: "first" }];
    await mount();

    await waitFor(() => expect(orphanBanner()).toBeTruthy());
    // The backend has the turn, so it is reported — with its own words, and
    // without the strip a loop this window does not have would paint.
    expect(orphanBanner()!.textContent).toContain(i18n.t("ai.reload.running"));
    expect(runningRow()).toBeNull();
    expect(rows().map((row) => row.className)).toEqual(["ai-message user"]);
    expect(storedState().turn?.session).toBe("s1");

    // Stop is the honest action, and it reaches the request the backend named.
    fireEvent.click(document.querySelector(".ai-turn-orphan-stop")!);
    await waitFor(() => expect(backend.cancels).toEqual([{ requestId: "r1" }]));
    await waitFor(() => expect(orphanBanner()).toBeNull());
    expect(runningRow()).toBeNull();
    expect(storedState().turn).toBeNull();
    await waitFor(() => expect(rows().at(-1)?.textContent).toContain("was stopped"));
  });

  it("shows the card the inherited turn waits on and answers that turn", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.loaded = [{ role: "user", content: "remove the bass" }];
    backend.questions = [{ call_id: "c7", tool: "tracks_remove", args: { track: "Bass" } }];
    await mount();

    await waitFor(() => expect(document.querySelector(".ai-approval-card")).toBeTruthy());
    expect(backend.calls).toContain("ai_turn_questions");
    fireEvent.click(document.querySelector(".ai-approval-reject")!);
    await waitFor(() => expect(backend.answers).toHaveLength(1));
    // The answer names the turn the backend reported and the call it asked
    // about, so it reaches the loop that is still waiting.
    expect(backend.answers[0]).toMatchObject({
      requestId: "r1",
      callId: "c7",
      decision: { approved: false, always: false },
    });
    expect(document.querySelector(".ai-approval-card")).toBeNull();
  });

  it("clears the marker when the backend reports no turn, and paints none", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.loaded = [{ role: "user", content: "first" }];
    backend.active = null;
    await mount();

    await waitFor(() => expect(lostBanner()).toBeTruthy());
    expect(lostBanner()!.textContent).toContain(i18n.t("ai.reload.lost"));
    expect(orphanBanner()).toBeNull();
    expect(runningRow()).toBeNull();
    // Nothing claims a turn, and the marker is gone from the store.
    expect(storedState().turn).toBeNull();
    // The saved conversation is on screen, without a row for a turn.
    expect(rows().map((row) => row.className)).toEqual(["ai-message user"]);

    fireEvent.click(document.querySelector(".ai-turn-reloaded-dismiss")!);
    await waitFor(() => expect(lostBanner()).toBeNull());
    expect(storedState().turn).toBeNull();
  });

  it("does not keep a marker for another conversation's turn", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r9", session_id: "s2" };
    await mount();

    await waitFor(() => expect(lostBanner()).toBeTruthy());
    expect(orphanBanner()).toBeNull();
    expect(runningRow()).toBeNull();
    expect(storedState().turn).toBeNull();
  });

  it("paints the answer that arrives after the reload into the restored conversation", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.activeOnce = true;
    // The reply is saved into the conversation when the inherited turn ends.
    backend.loaded = [{ role: "user", content: "first" }];
    backend.loads = [
      [{ role: "user", content: "first" }],
      [
        { role: "user", content: "first" },
        { role: "assistant", content: "the kick is now at -3 dB" },
      ],
    ];
    await mount();

    await waitFor(() => expect(orphanBanner()).toBeTruthy());
    // The turn ends; the panel follows it to its end and repaints the saved one.
    await waitFor(() => expect(orphanBanner()).toBeNull(), { timeout: 4000 });
    await waitFor(() => expect(rows().map((row) => row.className)).toEqual([
      "ai-message user",
      "ai-message assistant",
      "ai-message system",
    ]));
    expect(rows()[1].textContent).toContain("the kick is now at -3 dB");
    expect(rows()[2].textContent).toContain("outlived the reload finished");
    expect(storedState().turn).toBeNull();
    // The answer landed in the conversation the marker named, not in a new one.
    expect(backend.loadedIds).toEqual(["s1", "s1"]);
  });

  it("steers a queued message by stopping the turn the window inherited", async () => {
    reloadWith({ open: "s1", queue: [{ text: "next" }], turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    await mount();

    await waitFor(() => expect(orphanBanner()).toBeTruthy());
    // The inherited turn is a turn: the row offers the same Steer as any turn
    // this window started, and no Send now that would race it.
    expect(queueButton("steer")).toBeTruthy();
    expect(queueButton("send")).toBeNull();

    backend.hold = false;
    fireEvent.click(queueButton("steer")!);
    await waitFor(() => expect(backend.cancels).toEqual([{ requestId: "r1" }]));
    await waitFor(() => expect(backend.chatCalls).toHaveLength(1));
    expect(backend.chatCalls[0].messages.at(-1)?.content).toBe("next");
    await waitFor(() => expect(queueRows()).toHaveLength(0));
    expect(orphanBanner()).toBeNull();
    expect(storedState().turn).toBeNull();
  });

  it("does not restore or control a stored turn outside the current workspace", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.sessions = [session({ id: "other-workspace-session" })];
    backend.active = { request_id: "r1", session_id: "s1" };
    await mount();

    expect(backend.loadedIds).toEqual([]);
    expect(backend.calls).not.toContain("ai_active_turn");
    expect(backend.cancels).toEqual([]);
    expect(storedState().open).toBeNull();
    expect(orphanBanner()).toBeNull();
    expect(lostBanner()).toBeNull();
    expect(rows()).toHaveLength(0);
  });

  it("sends the composer text the window was reloaded with", async () => {
    await mount();
    fireEvent.input(input(), { target: { value: "left over from the reload" } });
    await waitFor(() => expect(storedState().conversations[""]?.draft).toBe("left over from the reload"));

    // No conversation exists yet: the draft is kept for the conversation the
    // first send will start, and comes back with it.
    cleanup();
    aiSession.reload();
    await mount();
    expect(input().value).toBe("left over from the reload");

    backend.hold = false;
    fireEvent.click(sendButton());
    await waitFor(() => expect(backend.chatCalls).toHaveLength(1));
    expect(backend.chatCalls[0].messages.at(-1)?.content).toBe("left over from the reload");
  });
});
