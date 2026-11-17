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
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack, aiConfigWire } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { aiLimits } from "../stores/aiLimits";
import { aiSession } from "../stores/aiSession";
import { aiWorkspace } from "../stores/aiWorkspace";
import { workspaceStore } from "../stores/workspace";
import { resetStats } from "../stores/aiStats";
import { i18n } from "../stores/i18n";
import type { ActiveTurn, SessionMeta } from "../ai/sessions";
import type { ProjectSnapshot } from "../types/project";
import type { WireMessage } from "../ai/wire";
import type { MediaAttachment } from "../ai/media";
import { answerFakeTurn, cancelFakeTurn, emitTurnEvent, emitTurnProject, replyFakeTurn, resetFakeTurns, runFakeTurn, type TurnStartArgs } from "./helpers/fakeTurn";

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
  /** What `sessions_open` returns; the panel repaints this after a reload. */
  loaded: [] as WireMessage[],
  /** Answers of `sessions_open`, oldest first; the last one repeats. */
  loads: [] as WireMessage[][],
  /** The conversation each `sessions_open` asked for, in order. */
  loadedIds: [] as string[],
  /** What `ai_batch_recover` answers: the committed document, or null. */
  recovered: null as { snapshot: ProjectSnapshot; unsaved?: string } | null,
  project: makeSnapshot() as ProjectSnapshot,
  /** The questions the inherited turn waits on. */
  questions: [] as ({ kind?: "approval"; call_id: string; tool: string; args: Record<string, unknown> }
    | { kind: "user_question"; call_id: string; question: string; options?: string[]; free_text: boolean; max_chars: number })[],
  /** Every `ai_turn_answer`, in order. */
  answers: [] as Record<string, unknown>[],
  /** Every `ai_turn_reply`, in order. */
  replies: [] as Record<string, unknown>[],
  /** Held `ai_turn_questions` reply, when a test races it. */
  questionsReply: null as Promise<unknown> | null,
  /** True when `ai_cancel` leaves the inherited turn running for a while. */
  cancelKeepsActive: false,
  /** Held `ai_turn_steer` replies, oldest first, when a test holds them. */
  holdSteer: false,
  steers: [] as { resolve: () => void; reject: (reason: unknown) => void }[],
  /** The message of every `ai_turn_steer`, in order. */
  steerMessages: [] as Record<string, unknown>[],
  /** Held `ai_active_turn` replies: one test holds the next answers until it releases them. */
  activeReply: null as Promise<ActiveTurn | null> | null,
  /** Held `sessions_open` replies: the next load waits on this once. */
  loadReply: null as Promise<void> | null,
  /** Every `sessions_new`, in order. */
  newCalls: [] as Record<string, unknown>[],
  /** Every `sessions_delete`, in order. */
  deletes: [] as string[],
  /** Every `sessions_compact`, in order. */
  compacts: [] as string[],
  /** Every `sessions_original`, in order. */
  originals: [] as string[],
};

function deferred<T = unknown>() {
  let resolve!: (value: T) => void;
  const promise = new Promise<T>((ok) => { resolve = ok; });
  return { promise, resolve };
}

/** Lets the inherited turn's poll run a few times. */
const pollRounds = (count = 2) => new Promise((resolve) => setTimeout(resolve, 400 * count + 100));

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
      case "get_project": return backend.project;
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
      case "ai_active_turn": {
        if (backend.activeReply) return backend.activeReply;
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
      case "ai_turn_questions": return backend.questionsReply ?? backend.questions;
      case "ai_turn_reply": {
        backend.replies.push(typed);
        if (backend.active && typed.requestId === backend.active.request_id) return undefined;
        return replyFakeTurn(String(typed.requestId), String(typed.callId), typed.reply as never);
      }
      case "ai_turn_steer": {
        expect(typed.requestId).toBe(backend.active?.request_id);
        expect(typed.sessionId).toBe(backend.active?.session_id);
        backend.steerMessages.push(typed.message as Record<string, unknown>);
        const save = () => { backend.loaded.push({ role: "user", ...(typed.message as {content: string}) }); };
        if (!backend.holdSteer) { save(); return undefined; }
        return new Promise<void>((resolve, reject) => backend.steers.push({ resolve: () => { save(); resolve(); }, reject }));
      }
      case "ai_cancel": {
        backend.cancels.push({ requestId: String(typed.requestId ?? "") });
        cancelFakeTurn(String(typed.requestId ?? ""));
        // The inherited turn closes and lets go of its slot.
        if (!backend.cancelKeepsActive && backend.active?.request_id === typed.requestId) backend.active = null;
        return undefined;
      }
      case "workspace_open": return { path: String(typed.path ?? ""), name: "other", detached: false, projects: [] };
      case "workspaces_list": return [];
      case "sessions_list": return backend.sessions.map((entry) => ({ ...entry }));
      case "sessions_new": {
        backend.newCalls.push(typed);
        const meta = session({ id: `s${backend.sessions.length + 1}`, title: String(typed.title ?? "new") });
        backend.sessions = [meta, ...backend.sessions];
        return meta;
      }
      case "sessions_delete": backend.deletes.push(String(typed.id)); return undefined;
      case "sessions_compact": backend.compacts.push(String(typed.id)); return null;
      case "sessions_original": backend.originals.push(String(typed.id)); return { messages: [], compactions: 1 };
      case "sessions_load":
      case "sessions_open": {
        const hold = backend.loadReply;
        if (hold) {
          backend.loadReply = null;
          await hold;
        }
        const id = String(typed.id ?? "s1");
        backend.loadedIds.push(id);
        const next = backend.loads.length > 0 ? backend.loads.shift()! : backend.loaded;
        return { meta: session({ id }), messages: next, skipped: 0, start: 0, total: next.length, revision: "0", older_tokens: 0 };
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
const sessionRow = (id: string) => document.querySelector(`.ai-session-row[data-session-row="${id}"]`) as HTMLElement | null;
/** Opens the row menu of one listed conversation and returns its item. */
function menuItem(id: string, label: string): HTMLButtonElement {
  fireEvent.contextMenu(sessionRow(id)!);
  const item = [...document.querySelectorAll<HTMLButtonElement>('.ai-session-menu [role="menuitem"]')]
    .find((button) => button.textContent === label);
  if (!item) throw new Error(`no menu item ${label}`);
  return item;
}
/** Clicks a control the view disabled, as a click that raced the lock would. */
function forceClick(button: HTMLButtonElement) {
  button.disabled = false;
  fireEvent.click(button);
}

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
  backend.replies = [];
  backend.chatCalls = [];
  backend.calls = [];
  backend.cancels = [];
  backend.sessions = [];
  backend.loaded = [];
  backend.loads = [];
  backend.loadedIds = [];
  backend.recovered = null;
  backend.project = makeSnapshot();
  backend.questionsReply = null;
  backend.cancelKeepsActive = false;
  backend.holdSteer = false;
  backend.steers = [];
  backend.steerMessages = [];
  backend.activeReply = null;
  backend.loadReply = null;
  backend.newCalls = [];
  backend.deletes = [];
  backend.compacts = [];
  backend.originals = [];
  i18n.setLocale("en");
  aiLimits.setActiveContext(null);
  resetStats();
  aiSession.reset();
  workspaceStore.reset();
  installBackend();
});

afterEach(() => {
  cleanup();
  vi.useRealTimers();
  resetFakeTurns();
  aiSession.reset();
  workspaceStore.reset();
  aiLimits.setActiveContext(null);
  resetStats();
});

describe("a reloaded window", () => {
  it("fits draft text inside the composer borders and caps long drafts without sending", async () => {
    await mount();
    const field = input();
    field.style.borderTopWidth = "1px";
    field.style.borderBottomWidth = "1px";
    let contentHeight = 31;
    Object.defineProperty(HTMLTextAreaElement.prototype, "scrollHeight", { configurable: true, get: () => contentHeight });
    fireEvent.input(field, { target: { value: "An unsent draft" } });
    await waitFor(() => expect(field.style.height).toBe("33px"));
    contentHeight = 280;
    const draft = "A long draft line.\n".repeat(20);
    fireEvent.input(field, { target: { value: draft } });
    await waitFor(() => expect(field.style.height).toBe("160px"));
    expect(field.value).toBe(draft);
    contentHeight = 31;
    fireEvent.input(field, { target: { value: "" } });
    await waitFor(() => expect(field.style.height).toBe("33px"));
    expect(backend.chatCalls).toHaveLength(0);
    Reflect.deleteProperty(HTMLTextAreaElement.prototype, "scrollHeight");
  });

  it("keeps saved listening clips inside the step, with tool and audio controls folded", async () => {
    reloadWith({ open: "s1" });
    const sha = "a".repeat(64);
    backend.loaded = [
      { role: "user", content: "Listen to the ending" },
      { role: "assistant", content: null, tool_calls: [{ id: "listen", type: "function", function: { name: "audition_range", arguments: '{"start_s":98.8235,"end_s":120}' } }] },
      { role: "tool", tool_call_id: "listen", content: '{"duration_s":16.3835}' },
      { role: "user", context: "audition", content: "You asked to hear 98.8235–120 s: this is that render.",
        message_meta: { version: 1, kind: "audition", audio_range: { start_seconds: 98.8235, end_seconds: 120, source: false } },
        source_render: "/sessions/render.wav",
        media: [{ kind: "audio", path: `/sessions/audition-media/${sha}.wav`, name: `${sha}.wav`, mime: "audio/wav", bytes: 128044, duration_secs: 16.3835, sha256: sha }] },
      { role: "assistant", content: "The ending needs another listen." },
    ];
    await mount();
    const group = document.querySelector<HTMLDetailsElement>(".ai-step-group")!;
    expect(group.open).toBe(false);
    expect(document.querySelector(".ai-audition-detail")).toBeNull();
    group.open = true;
    fireEvent(group, new Event("toggle"));
    const clip = document.querySelector<HTMLDetailsElement>(".ai-audition-detail")!;
    expect(group.contains(clip)).toBe(true);
    expect(clip.open).toBe(false);
    expect(clip.querySelector("summary")?.textContent).toContain("Audition 98.82–115.21 s");
    expect(document.querySelector<HTMLDetailsElement>(".ai-tool-result")!.open).toBe(false);
    expect(clip.querySelector("button")).toBeNull();
    expect(clip.textContent).not.toContain("You asked to hear");
    clip.open = true;
    fireEvent(clip, new Event("toggle"));
    expect([...clip.querySelectorAll("button")].map(button => button.textContent)).toEqual(["Load audio", "Load full render"]);
    expect(backend.calls).not.toContain("preview_saved_audition");
    expect(backend.calls).not.toContain("open_saved_audition_render");
  });

  it("refreshes project edits made with no view and follows later edits of the inherited turn", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.project = makeSnapshot({ tracks: [makeTrack("Background piano", 0)] });
    backend.loaded = [
      { role: "user", content: "Compose" },
      { role: "assistant", content: null, tool_calls: [{ id: "read", type: "function", function: { name: "tracks_list", arguments: "{}" } }] },
      { role: "tool", tool_call_id: "read", content: "{}" },
    ];
    await mount();
    await waitFor(() => expect(orphanBanner()).not.toBeNull());
    await waitFor(() => expect(store.project.tracks.map(track => track.name)).toEqual(["Background piano"]));
    expect((document.querySelector(".ai-step-group") as HTMLDetailsElement).open).toBe(true);
    const updated = makeSnapshot({ tracks: [makeTrack("Background piano", 0), makeTrack("Cello", 1)] });
    emitTurnProject(updated);
    await waitFor(() => expect(store.project.tracks).toHaveLength(2));
    const foreign = { ...makeSnapshot({ tracks: [] }), file_path: "/other/song.ocp" };
    emitTurnProject(foreign);
    expect(store.project.tracks).toHaveLength(2);
  });
  it("recovers a stranded panel batch once on mount and shows the document it committed", async () => {
    reloadWith({ open: "s1" });
    backend.loaded = [{ role: "user", content: "first" }];
    const restored = { ...makeSnapshot(), dsl_source: '@project "Committed"\n  tempo: 120\n' };
    backend.recovered = { snapshot: restored };
    await mount();

    // The panel recovers the orphan exactly once, before its first turn.
    expect(backend.calls.filter((command) => command === "ai_batch_recover")).toHaveLength(1);
    // What is on screen is the document the recovery committed.
    expect(store.dslSource()).toBe(restored.dsl_source);
  });

  it("says so when a recovered batch did not reach the project file", async () => {
    reloadWith({ open: "s1" });
    backend.loaded = [{ role: "user", content: "first" }];
    const restored = { ...makeSnapshot(), dsl_source: '@project "Committed"\n  tempo: 120\n' };
    backend.recovered = { snapshot: restored, unsaved: "disk full" };
    await mount();
    expect(store.dslSource()).toBe(restored.dsl_source);
    await waitFor(() => expect(aiWorkspace.notice()).toEqual({
      // The backend sends only the reason; the sentence around it is the panel's.
      text: "Edits from an interrupted turn were kept, but the project file was not written: disk full. Save the project to keep them.",
      tone: "error",
    }));
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

    // Detaching preserves the marker and queue. This fixture then ends the
    // backend turn while the view is gone; the remount confirms it is settled.
    cleanup();
    expect(storedState().turn).toEqual(before.turn);
    cancelFakeTurn(String(backend.chatCalls[0].requestId));
    aiSession.reload();
    await mount();
    await waitFor(() => expect(storedState().turn).toBeNull());

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

    // Repainting after Stop must not reinstall a cached prompt over this draft.
    fireEvent.input(input(), { target: { value: "My unsent next instruction" } });
    // Stop reaches the request the backend named.
    fireEvent.click(document.querySelector(".ai-turn-orphan-stop")!);
    await waitFor(() => expect(backend.cancels).toEqual([{ requestId: "r1" }]));
    await waitFor(() => expect(orphanBanner()).toBeNull());
    expect(runningRow()).toBeNull();
    expect(storedState().turn).toBeNull();
    await waitFor(() => expect(rows().at(-1)?.textContent).toContain("was stopped"));
    expect(input().value).toBe("My unsent next instruction");
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

  it("shows an inherited question and sends its reply", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.loaded = [{ role: "user", content: "what should we do" }];
    backend.questions = [{
      kind: "user_question",
      call_id: "q7",
      question: "Which direction?",
      options: ["Darker", "Brighter"],
      free_text: true,
      max_chars: 4_000,
    }];
    await mount();

    await waitFor(() => expect(document.querySelector(".ai-question-card")).toBeTruthy());
    expect(document.querySelector(".ai-question-card")!.textContent).toContain("Which direction?");
    fireEvent.input(document.querySelector(".ai-question-input")!, { target: { value: "Darker, but sparse" } });
    fireEvent.click(document.querySelector(".ai-question-submit")!);
    await waitFor(() => expect(backend.replies).toHaveLength(1));
    expect(backend.replies[0]).toMatchObject({
      requestId: "r1",
      callId: "q7",
      reply: { text: "Darker, but sparse" },
    });
    expect(document.querySelector(".ai-question-card")).toBeNull();
  });

  it("drops the card of a question whose wait ran out, and only that one", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.loaded = [{ role: "user", content: "remove the bass" }];
    backend.questions = [{ call_id: "c7", tool: "tracks_remove", args: { track: "Bass" } }];
    await mount();
    await waitFor(() => expect(document.querySelector(".ai-approval-card")).toBeTruthy());

    // Another turn's or another call's expiry leaves this card alone.
    emitTurnEvent("r9", "s1", { type: "expired", call_id: "c7" });
    emitTurnEvent("r1", "s1", { type: "expired", call_id: "c8" });
    expect(document.querySelector(".ai-approval-card")).toBeTruthy();

    emitTurnEvent("r1", "s1", { type: "expired", call_id: "c7" });
    await waitFor(() => expect(document.querySelector(".ai-approval-card")).toBeNull());
    expect(backend.answers).toHaveLength(0);
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
    expect(rows()[2].textContent).toContain(i18n.t("ai.reload.settled"));
    expect(storedState().turn).toBeNull();
    // The answer landed in the conversation the marker named, not in a new one.
    expect(backend.loadedIds).toEqual(["s1", "s1"]);
  });

  it("steers a queued message into the turn the window inherited without stopping it", async () => {
    reloadWith({ open: "s1", queue: [{ text: "next" }], turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    await mount();

    await waitFor(() => expect(orphanBanner()).toBeTruthy());
    // The inherited turn is a turn: the row offers the same Steer as any turn
    // this window started, and no Send now that would race it.
    expect(queueButton("steer")).toBeTruthy();
    expect(queueButton("send")).toBeNull();

    const row = aiSession.queued()[0].id;
    fireEvent.click(queueButton("steer")!);
    await waitFor(() => expect(queueRows()).toHaveLength(0));
    // The steer names its row, so the saved message says which row it delivered.
    expect(backend.steerMessages).toEqual([expect.objectContaining({ id: row, content: "next" })]);
    expect(backend.cancels).toEqual([]);
    expect(backend.chatCalls).toHaveLength(0);
    expect(backend.active).toEqual({ request_id: "r1", session_id: "s1" });
    expect(orphanBanner()).toBeTruthy();
    expect(storedState().turn).not.toBeNull();
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

  it("says a turn finished while the panel was away when its answer is saved", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = null;
    backend.loaded = [
      { role: "user", content: "first" },
      { role: "assistant", content: "Here is the answer." },
    ];
    await mount();

    await waitFor(() => expect(lostBanner()).toBeTruthy());
    expect(lostBanner()!.textContent).toContain(i18n.t("ai.reload.finished"));
    expect(lostBanner()!.textContent).not.toContain(i18n.t("ai.reload.lost"));
    // Nothing is missing, so the banner offers no reload of the conversation.
    expect(document.querySelector(".ai-turn-reloaded-open")).toBeNull();
    expect(rows().map((row) => row.className)).toEqual(["ai-message user", "ai-message assistant"]);
    expect(storedState().turn).toBeNull();
  });

  it("keeps an inherited turn out of the workspace the panel moves to", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.loaded = [{ role: "user", content: "first" }];
    backend.questions = [{ call_id: "c7", tool: "tracks_remove", args: { track: "Bass" } }];
    await mount();
    await waitFor(() => expect(orphanBanner()).toBeTruthy());
    await waitFor(() => expect(document.querySelector(".ai-approval-card")).toBeTruthy());
    const loads = backend.loadedIds.length;

    await workspaceStore.open("/Users/me/other");
    await waitFor(() => expect(aiSession.open()).toBeNull());
    expect(orphanBanner()).toBeNull();
    expect(document.querySelector(".ai-approval-card")).toBeNull();

    // The turn ends in the workspace it belongs to; nothing of it lands here.
    backend.active = null;
    await pollRounds();
    expect(backend.loadedIds).toHaveLength(loads);
    expect(aiSession.open()).toBeNull();
    expect(rows()).toHaveLength(0);
    expect(document.body.textContent).not.toContain(i18n.t("ai.reload.settled"));
  });

  it("clears an inherited question when the panel moves to another workspace", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.loaded = [{ role: "user", content: "what should we do" }];
    backend.questions = [{
      kind: "user_question",
      call_id: "q7",
      question: "Which direction?",
      options: ["Darker", "Brighter"],
      free_text: true,
      max_chars: 4_000,
    }];
    await mount();
    await waitFor(() => expect(document.querySelector(".ai-question-card")).toBeTruthy());

    await workspaceStore.open("/Users/me/other");
    await waitFor(() => expect(aiSession.open()).toBeNull());
    expect(document.querySelector(".ai-question-card")).toBeNull();
  });

  it("clears an inherited question when its turn ends", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.loaded = [{ role: "user", content: "what should we do" }];
    backend.questions = [{
      kind: "user_question",
      call_id: "q7",
      question: "Which direction?",
      options: ["Darker", "Brighter"],
      free_text: true,
      max_chars: 4_000,
    }];
    await mount();
    await waitFor(() => expect(document.querySelector(".ai-question-card")).toBeTruthy());

    backend.active = null;
    await pollRounds();
    expect(document.querySelector(".ai-question-card")).toBeNull();
  });

  it("does not paint a stopped inherited turn into the workspace the panel moved to", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.cancelKeepsActive = true;
    backend.loaded = [{ role: "user", content: "first" }];
    await mount();
    await waitFor(() => expect(orphanBanner()).toBeTruthy());

    fireEvent.click(document.querySelector(".ai-turn-orphan-stop")!);
    await waitFor(() => expect(backend.cancels).toEqual([{ requestId: "r1" }]));
    const loads = backend.loadedIds.length;
    await workspaceStore.open("/Users/me/other");
    await waitFor(() => expect(aiSession.open()).toBeNull());

    backend.active = null;
    await pollRounds();
    expect(backend.loadedIds).toHaveLength(loads);
    expect(aiSession.open()).toBeNull();
    expect(rows()).toHaveLength(0);
    expect(document.body.textContent).not.toContain(i18n.t("ai.reload.stopped"));
  });

  it("refuses to change the conversation while it follows an inherited turn, and keeps the rest usable", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.sessions = [session({ id: "s1" }), session({ id: "s2", title: "second" })];
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.loaded = [{ role: "user", content: "first" }];
    backend.questions = [{ call_id: "c7", tool: "tracks_remove", args: { track: "Bass" } }];
    await mount();
    await waitFor(() => expect(orphanBanner()).toBeTruthy());
    await waitFor(() => expect(document.querySelector(".ai-approval-card")).toBeTruthy());

    // "New conversation" says why it waits instead of ignoring the click.
    const fresh = document.querySelector(".ai-session-new") as HTMLButtonElement;
    expect(fresh.disabled).toBe(true);
    expect(fresh.getAttribute("title")).toBe(i18n.t("ai.sessions.newBlocked"));
    // The handler refuses on its own, for a click the button let through.
    fresh.disabled = false;
    fireEvent.click(fresh);
    expect(aiSession.open()).toBe("s1");
    expect(rows().map((row) => row.className)).toEqual(["ai-message user"]);

    // Stop, the card's answers, the permission and the list stay usable.
    expect((document.querySelector(".ai-turn-orphan-stop") as HTMLButtonElement).disabled).toBe(false);
    expect((document.querySelector(".ai-approval-approve") as HTMLButtonElement).disabled).toBe(false);
    expect([...document.querySelectorAll<HTMLButtonElement>(".ai-permission-option")].every((option) => !option.disabled)).toBe(true);
    const toggle = document.querySelector(".ai-session-current") as HTMLButtonElement;
    expect(toggle.disabled).toBe(false);
    fireEvent.click(toggle);
    await waitFor(() => expect(sessionRow("s2")).toBeTruthy());

    // Opening another conversation leaves this one: the row waits, and so
    // does the handler behind it.
    const away = sessionRow("s2")!.querySelector(".ai-session-row-main") as HTMLButtonElement;
    expect(away.disabled).toBe(true);
    away.disabled = false;
    fireEvent.click(away);
    await settleMount();
    expect(aiSession.open()).toBe("s1");
    expect(backend.loadedIds).not.toContain("s2");

    // The open conversation's fork, compact and delete wait, in the menu and in
    // their handlers.
    const loads = backend.loadedIds.length;
    expect(menuItem("s1", "Fork").disabled).toBe(true);
    forceClick(menuItem("s1", "Fork"));
    expect(menuItem("s1", "Compact").disabled).toBe(true);
    forceClick(menuItem("s1", "Compact"));
    expect(menuItem("s1", "View original history").disabled).toBe(true);
    forceClick(menuItem("s1", "View original history"));
    expect(menuItem("s1", "Delete").disabled).toBe(true);
    forceClick(menuItem("s1", "Delete"));
    const confirm = sessionRow("s1")!.querySelector(".ai-session-action-delete") as HTMLButtonElement;
    expect(confirm.disabled).toBe(true);
    forceClick(confirm);
    await settleMount();
    expect(backend.newCalls).toEqual([]);
    expect(backend.compacts).toEqual([]);
    expect(backend.originals).toEqual([]);
    expect(backend.deletes).toEqual([]);
    expect(backend.loadedIds).toHaveLength(loads);
    expect(aiSession.open()).toBe("s1");

    // Another conversation is not locked: it is renamed, forked and deleted
    // while the conversation on screen stays where it is.
    expect(menuItem("s2", "Rename").disabled).toBe(false);
    expect(menuItem("s2", "Fork").disabled).toBe(false);
    fireEvent.click(menuItem("s2", "Fork"));
    await waitFor(() => expect(backend.newCalls).toEqual([expect.objectContaining({ parent: "s2" })]));
    await settleMount();
    expect(aiSession.open()).toBe("s1");
    expect(backend.loadedIds).toHaveLength(loads);
    expect(menuItem("s2", "Delete").disabled).toBe(false);
    fireEvent.click(menuItem("s2", "Delete"));
    const other = sessionRow("s2")!.querySelector(".ai-session-action-delete") as HTMLButtonElement;
    expect(other.disabled).toBe(false);
    fireEvent.click(other);
    await waitFor(() => expect(backend.deletes).toEqual(["s2"]));
    expect(aiSession.open()).toBe("s1");
    expect(orphanBanner()).toBeTruthy();
  });

  it("keeps the conversation locked while an inherited turn's stop settles", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.sessions = [session({ id: "s1" }), session({ id: "s2", title: "second" })];
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.cancelKeepsActive = true;
    backend.loaded = [{ role: "user", content: "first" }];
    await mount();
    await waitFor(() => expect(orphanBanner()).toBeTruthy());
    fireEvent.click(document.querySelector(".ai-turn-orphan-stop")!);
    await waitFor(() => expect(orphanBanner()!.textContent).toContain(i18n.t("ai.turn.stopping")));

    fireEvent.click(document.querySelector(".ai-session-current")!);
    await waitFor(() => expect(sessionRow("s2")).toBeTruthy());
    const away = sessionRow("s2")!.querySelector(".ai-session-row-main") as HTMLButtonElement;
    expect(away.disabled).toBe(true);
    forceClick(away);
    forceClick(menuItem("s1", "Fork"));
    await settleMount();
    expect(backend.loadedIds).not.toContain("s2");
    expect(backend.newCalls).toEqual([]);
    expect(aiSession.open()).toBe("s1");
  });

  it("keeps the turn marker until the backend lets go of a stopped inherited turn", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.cancelKeepsActive = true;
    backend.loaded = [{ role: "user", content: "first" }];
    await mount();
    await waitFor(() => expect(orphanBanner()).toBeTruthy());
    fireEvent.click(document.querySelector(".ai-turn-orphan-stop")!);
    await waitFor(() => expect(backend.cancels).toEqual([{ requestId: "r1" }]));
    expect(storedState().turn?.session).toBe("s1");

    // A window reloaded during the wait finds the turn and stays guarded.
    cleanup();
    aiSession.reload();
    await mount();
    await waitFor(() => expect(orphanBanner()).toBeTruthy());
    expect((document.querySelector(".ai-session-new") as HTMLButtonElement).disabled).toBe(true);
    expect(storedState().turn?.session).toBe("s1");

    // Once the backend stops reporting it, the marker goes.
    fireEvent.click(document.querySelector(".ai-turn-orphan-stop")!);
    backend.active = null;
    await waitFor(() => expect(rows().at(-1)?.textContent).toContain(i18n.t("ai.reload.stopped")), { timeout: 3000 });
    expect(storedState().turn).toBeNull();
  });

  it("keeps a steer this page still waits on when the inherited turn ends, then sends the row once", async () => {
    reloadWith({ open: "s1", queue: [{ text: "next" }], turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.holdSteer = true;
    backend.hold = false;
    backend.loaded = [{ role: "user", content: "first" }];
    await mount();
    await waitFor(() => expect(queueButton("steer")).toBeTruthy());
    fireEvent.click(queueButton("steer")!);
    await waitFor(() => expect(backend.steers).toHaveLength(1));

    // The turn ends before the steer answers: the saved conversation does not
    // hold the message yet, but the steer is not lost, so the row is not sent.
    backend.active = null;
    await waitFor(() => expect(rows().at(-1)?.textContent).toContain(i18n.t("ai.reload.settled")), { timeout: 3000 });
    await settleMount();
    expect(backend.chatCalls).toHaveLength(0);
    expect(queueTexts()).toEqual(["next"]);
    expect(queueButton("edit")!.disabled).toBe(true);

    // Admission closed: the steer's own reply hands the row back, and it goes out once.
    backend.steers[0].reject({ kind: "steer_closed", message: "Turn finished" });
    await waitFor(() => expect(backend.chatCalls).toHaveLength(1));
    expect(backend.chatCalls[0].messages.at(-1)?.content).toBe("next");
    await waitFor(() => expect(queueRows()).toHaveLength(0));
  });

  it("does not settle an inherited turn whose poll answered after Stop was pressed", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.loaded = [{ role: "user", content: "first" }];
    await mount();
    await waitFor(() => expect(orphanBanner()).toBeTruthy());

    // A poll is on its way when Stop is pressed; both answers arrive after it.
    const reply = deferred<ActiveTurn | null>();
    backend.activeReply = reply.promise;
    const asked = backend.calls.filter((command) => command === "ai_active_turn").length;
    await waitFor(() => expect(backend.calls.filter((command) => command === "ai_active_turn").length).toBeGreaterThan(asked), { timeout: 2000 });
    fireEvent.click(document.querySelector(".ai-turn-orphan-stop")!);
    await waitFor(() => expect(backend.cancels).toEqual([{ requestId: "r1" }]));
    backend.activeReply = null;
    reply.resolve(null);

    await waitFor(() => expect(rows().at(-1)?.textContent).toContain(i18n.t("ai.reload.stopped")), { timeout: 3000 });
    await settleMount();
    expect(document.body.textContent).not.toContain(i18n.t("ai.reload.settled"));
    expect(document.body.textContent).not.toContain(i18n.t("ai.reload.settledUnpainted"));
  });

  it("does not report an inherited turn's end into the workspace the panel came back to", async () => {
    await workspaceStore.open("/Users/me/songs");
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.loaded = [{ role: "user", content: "first" }];
    await mount();
    await waitFor(() => expect(orphanBanner()).toBeTruthy());

    // The turn ends, and the repaint that reports it is held.
    const gate = deferred<void>();
    backend.loadReply = gate.promise;
    const loads = backend.calls.filter((command) => command === "sessions_open").length;
    backend.active = null;
    await waitFor(() => expect(backend.calls.filter((command) => command === "sessions_open").length).toBeGreaterThan(loads), { timeout: 2000 });

    // The panel leaves and comes back: the same conversation is open again, in
    // another visit of the workspace.
    await workspaceStore.open("/Users/me/other");
    await waitFor(() => expect(aiSession.open()).toBeNull());
    await workspaceStore.open("/Users/me/songs");
    await waitFor(() => expect(aiSession.open()).toBe("s1"));
    await waitFor(() => expect(rows().map((row) => row.className)).toEqual(["ai-message user"]));

    gate.resolve();
    await pollRounds(1);
    expect(document.body.textContent).not.toContain(i18n.t("ai.reload.settled"));
    expect(document.body.textContent).not.toContain(i18n.t("ai.reload.settledUnpainted"));
    expect(rows().map((row) => row.className)).toEqual(["ai-message user"]);
  });

  it("holds a row a closed steer hands back until the inherited turn's stop settles", async () => {
    reloadWith({ open: "s1", queue: [{ text: "waiting" }], turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.cancelKeepsActive = true;
    backend.holdSteer = true;
    backend.hold = false;
    backend.loaded = [{ role: "user", content: "first" }];
    await mount();
    await waitFor(() => expect(queueButton("steer")).toBeTruthy());
    fireEvent.click(queueButton("steer")!);
    await waitFor(() => expect(backend.steers).toHaveLength(1));
    fireEvent.click(document.querySelector(".ai-turn-orphan-stop")!);
    await waitFor(() => expect(orphanBanner()!.textContent).toContain(i18n.t("ai.turn.stopping")));

    // The steer is refused while the stopped turn still writes: the row waits.
    backend.steers[0].reject({ kind: "steer_closed", message: "Turn finished" });
    await waitFor(() => expect(aiSession.steering()).toBeNull());
    await settleMount();
    expect(backend.chatCalls).toHaveLength(0);
    expect(queueTexts()).toEqual(["waiting"]);

    backend.active = null;
    await waitFor(() => expect(backend.chatCalls).toHaveLength(1), { timeout: 3000 });
    expect(backend.chatCalls[0].messages.at(-1)?.content).toBe("waiting");
  });

  it("offers no Steer for a queue that belongs to another conversation than the inherited turn", async () => {
    reloadWith({ open: "s2", queue: [{ text: "for s2" }], turn: { session: "s1", started: Date.now() } });
    backend.sessions = [session({ id: "s1" }), session({ id: "s2", title: "second" })];
    backend.active = { request_id: "r1", session_id: "s1" };
    await mount();
    await waitFor(() => expect(orphanBanner()).toBeTruthy());

    expect(queueTexts()).toEqual(["for s2"]);
    expect(queueButton("steer")).toBeNull();
    expect(queueButton("send")).toBeNull();
    expect(backend.calls).not.toContain("ai_turn_steer");
  });

  it("guards the panel while an inherited turn's stop settles, then refreshes and sends", async () => {
    reloadWith({ open: "s1", queue: [{ text: "waiting" }], turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.cancelKeepsActive = true;
    backend.hold = false;
    backend.loaded = [{ role: "user", content: "first" }];
    await mount();
    await waitFor(() => expect(orphanBanner()).toBeTruthy());

    fireEvent.click(document.querySelector(".ai-turn-orphan-stop")!);
    await waitFor(() => expect(backend.cancels).toEqual([{ requestId: "r1" }]));
    // The turn still writes until the backend lets go of it: nothing is sent,
    // the conversation stays, and its edits still reach the project.
    expect(orphanBanner()!.textContent).toContain(i18n.t("ai.turn.stopping"));
    expect(queueButton("send")).toBeNull();
    expect(queueButton("steer")).toBeNull();
    expect(document.querySelector(".ai-queue-hint")?.textContent).toBe(i18n.t("ai.queue.waitingReload"));
    await queue("typed while stopping");
    expect(backend.chatCalls).toHaveLength(0);
    fireEvent.click(document.querySelector(".ai-session-new")!);
    expect(aiSession.open()).toBe("s1");
    emitTurnProject(makeSnapshot({ tracks: [makeTrack("Late cello", 0)] }));
    await waitFor(() => expect(store.project.tracks.map((track) => track.name)).toEqual(["Late cello"]));

    const reads = backend.calls.filter((command) => command === "get_project").length;
    backend.project = makeSnapshot({ tracks: [makeTrack("Committed", 0)] });
    backend.active = null;
    await waitFor(() => expect(rows().at(-1)?.textContent).toContain(i18n.t("ai.reload.stopped")), { timeout: 3000 });
    // The project is read once more after the turn let go, for its last commit.
    expect(backend.calls.filter((command) => command === "get_project").length).toBeGreaterThan(reads);
    expect(store.project.tracks.map((track) => track.name)).toEqual(["Committed"]);
    await waitFor(() => expect(backend.chatCalls).toHaveLength(1));
    expect(backend.chatCalls[0].messages.at(-1)?.content).toBe("waiting");
  });

  it("keeps following, and holds the queue, when the turn outlasts its stop", async () => {
    vi.useFakeTimers({ shouldAdvanceTime: true });
    reloadWith({ open: "s1", queue: [{ text: "waiting" }], turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.cancelKeepsActive = true;
    backend.loaded = [{ role: "user", content: "first" }];
    await mount();
    await waitFor(() => expect(orphanBanner()).toBeTruthy());

    fireEvent.click(document.querySelector(".ai-turn-orphan-stop")!);
    await waitFor(() => expect(backend.cancels).toEqual([{ requestId: "r1" }]));
    await vi.advanceTimersByTimeAsync(11_000);
    await waitFor(() => expect(orphanBanner()?.textContent).toContain(i18n.t("ai.reload.running")));
    expect(backend.chatCalls).toHaveLength(0);
    expect(queueTexts()).toEqual(["waiting"]);
    expect(queueButton("steer")).toBeTruthy();
    expect(storedState().turn?.session).toBe("s1");
    expect(document.body.textContent).not.toContain(i18n.t("ai.reload.stopped"));
    cleanup();
  });

  it("does not show a question whose wait ran out before the questions reply", async () => {
    reloadWith({ open: "s1", turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.loaded = [{ role: "user", content: "remove the bass" }];
    const reply = deferred();
    backend.questionsReply = reply.promise;
    await mount();
    await waitFor(() => expect(backend.calls).toContain("ai_turn_questions"));

    emitTurnEvent("r1", "s1", { type: "expired", call_id: "c7" });
    reply.resolve([
      { call_id: "c7", tool: "tracks_remove", args: { track: "Bass" } },
      { call_id: "c8", tool: "tracks_remove", args: { track: "Lead" } },
    ]);
    await waitFor(() => expect(document.querySelector(".ai-approval-card")).toBeTruthy());
    expect(document.querySelector(".ai-approval-arguments")?.textContent).toContain("Lead");
    expect(document.querySelector(".ai-approval-arguments")?.textContent).not.toContain("Bass");
  });

  it("does not steer a row again from a panel reopened while its steer is in flight", async () => {
    reloadWith({ open: "s1", queue: [{ text: "next" }], turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.holdSteer = true;
    await mount();
    await waitFor(() => expect(queueButton("steer")).toBeTruthy());
    fireEvent.click(queueButton("steer")!);
    await waitFor(() => expect(backend.steers).toHaveLength(1));

    cleanup();
    await mount();
    await waitFor(() => expect(queueButton("steer")).toBeTruthy());
    expect(queueButton("steer")!.disabled).toBe(true);
    expect(queueButton("edit")!.disabled).toBe(true);
    fireEvent.click(queueButton("steer")!);
    await settleMount();
    expect(backend.calls.filter((command) => command === "ai_turn_steer")).toHaveLength(1);

    backend.steers[0].resolve();
    await waitFor(() => expect(queueRows()).toHaveLength(0));
    expect(backend.calls.filter((command) => command === "ai_turn_steer")).toHaveLength(1);
  });

  it("acknowledges a steered row after the store was read again", async () => {
    reloadWith({ open: "s1", queue: [{ text: "next" }, { text: "after" }], turn: { session: "s1", started: Date.now() } });
    backend.active = { request_id: "r1", session_id: "s1" };
    backend.holdSteer = true;
    await mount();
    await waitFor(() => expect(queueButton("steer")).toBeTruthy());
    fireEvent.click(queueButton("steer")!);
    await waitFor(() => expect(backend.steers).toHaveLength(1));

    // The store is read again: every row is a new object with the same id.
    aiSession.reload();
    await waitFor(() => expect(queueTexts()).toEqual(["next", "after"]));
    backend.steers[0].resolve();
    await waitFor(() => expect(queueTexts()).toEqual(["after"]));
    expect(storedState().conversations.s1?.queue?.map((row) => row.text)).toEqual(["after"]);
  });

  it("settles a steer whose reply a closed page lost from the saved conversation, and sends nothing on its own", async () => {
    // Delivered: the saved conversation holds the message, so the row goes,
    // and the row behind it waits for Send now as any row found idle does.
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify({
      v: 1, open: "s1", turn: null,
      conversations: { s1: { queue: [{ id: "q1", text: "next", media: [] }, { id: "q4", text: "later", media: [] }], steering: { id: "q1" } } },
    }));
    backend.sessions = [session({ id: "s1" })];
    aiSession.reload();
    backend.hold = false;
    backend.loaded = [
      { role: "user", content: "first" },
      { role: "user", content: "next", message_meta: { version: 1, kind: "user", steer_id: "q1" } },
      { role: "assistant", content: "Both done." },
    ];
    await mount();
    await waitFor(() => expect(queueTexts()).toEqual(["later"]));
    await settleMount();
    expect(backend.chatCalls).toHaveLength(0);
    expect(queueButton("send")).toBeTruthy();
    cleanup();

    // The same words sent as an ordinary message are not this steer: only
    // the id the backend saved with the delivery settles it, and the row is
    // an ordinary queued message again.
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify({
      v: 1, open: "s1", turn: null,
      conversations: { s1: { queue: [{ id: "q3", text: "same words", media: [] }], steering: { id: "q3" } } },
    }));
    aiSession.reload();
    backend.loaded = [{ role: "user", content: "same words" }, { role: "assistant", content: "Done." }];
    await mount();
    await waitFor(() => expect(aiSession.steering()).toBeNull());
    await settleMount();
    expect(queueTexts()).toEqual(["same words"]);
    expect(backend.chatCalls).toHaveLength(0);
    cleanup();

    // Not delivered: the row waits for Send now instead of starting a turn
    // nobody asked for, and goes out once it is clicked.
    window.localStorage.setItem(STORAGE_KEY, JSON.stringify({
      v: 1, open: "s1", turn: null,
      conversations: { s1: { queue: [{ id: "q2", text: "again", media: [] }], steering: { id: "q2" } } },
    }));
    aiSession.reload();
    backend.loaded = [{ role: "user", content: "first" }, { role: "assistant", content: "Done." }];
    await mount();
    await waitFor(() => expect(aiSession.steering()).toBeNull());
    await settleMount();
    expect(backend.chatCalls).toHaveLength(0);
    expect(queueTexts()).toEqual(["again"]);
    expect(queueButton("send")!.disabled).toBe(false);
    fireEvent.click(queueButton("send")!);
    await waitFor(() => expect(backend.chatCalls).toHaveLength(1));
    expect(backend.chatCalls[0].messages.at(-1)?.content).toBe("again");
  });
});
