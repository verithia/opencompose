/**
 * The AI panel's saved conversations: the autosave announcement, the switcher
 * in the panel header, and the transcript a restored conversation paints.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { Show } from "solid-js";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, openStepGroups } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { WorkspaceChooserDialog } from "../components/WorkspaceChooser";
import { TooltipLayer } from "../components/Tooltip";
import { isContextMenuOpen } from "../components/ContextMenu";
import { i18n } from "../stores/i18n";
import { aiSession } from "../stores/aiSession";
import { workspaceStore } from "../stores/workspace";
import {
  compactionNotice,
  sessionCompaction,
  sessionList,
  loadedSession,
  updatedLabel,
  workspaceScope,
  type SessionCompaction,
  type SessionMeta,
  type SessionUsageSnapshot,
} from "../ai/sessions";
import { canContinue, canRetry, closedRound, restoredTranscript, savedWireIndex, RESULT_SUMMARY_CHARS, userTurnOf, type ChatMessage } from "../ai/transcript";
import { ContextMenuLayer } from "../components/ContextMenu";
import { rememberToolResult, storedToolResult } from "../ai/agent";
import { contextMeter } from "../stores/aiContext";
import { aiStats, hydrateSessionUsage, noteSentEstimate, recordTurn } from "../stores/aiStats";
import type { WireMessage } from "../ai/agent";
import type { MediaAttachment } from "../ai/media";

const { handlers } = vi.hoisted(() => ({
  handlers: {} as Record<string, Array<(event: { payload: unknown }) => void>>,
}));

vi.mock("@tauri-apps/api/event", () => ({
  listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    (handlers[name] ??= []).push(handler);
    return () => {};
  }),
}));

const CAPABILITIES = { text: true, audio_in: false, audio_out: false, native_tool_calling: true, streaming: true };

const CATALOG = {
  version: "fixture",
  providers: [
    {
      id: "google",
      name: "Google",
      kind: "gemini_interactions",
      hosts: ["generativelanguage.googleapis.com"],
      auth_providers: [],
      models: [
        { id: "gemini-4-argon", name: "Gemini 4 Argon", context: 200000, max_output: 32000, capabilities: CAPABILITIES, status: "active" },
      ],
    },
  ],
};

const CONFIG = {
  version: 2,
  active_provider: "google",
  providers: [
    { id: "google", name: "Google", kind: "gemini_interactions", auth: "api_key", auth_provider: null, oauth_allowed_hosts: [], endpoint: "https://generativelanguage.googleapis.com/v1beta/models/gemini-4-argon:generateContent", model: "gemini-4-argon", models: [], context_length: 200000, max_tokens: 32000, temperature: 0.3, thinking_level: "medium", capabilities: CAPABILITIES },
  ],
};

function session(overrides: Partial<SessionMeta> = {}): SessionMeta {
  return {
    id: "s1",
    title: "intro idea",
    created: "2026-10-28T09:00:00.000Z",
    updated: "2026-10-28T09:00:00.000Z",
    model: "gemini-4-argon",
    provider: "google",
    workspace_key: "3f2a1b0c",
    workspace: "/Users/me/songs",
    turns: 2,
    tokens: 400,
    ...overrides,
  };
}

function usageSnapshot(id: string, revision: number, input: number): SessionUsageSnapshot {
  return { session_id: id, incarnation: `life-${id}`, revision, pending_requests: 0,
    totals: { requests: 1, attempts: 1, input, output: 40, reasoning: 0,
      cached_input: 0, cache_input: 0, failed_attempts: 0, unknown_price_attempts: 0,
      unreported_usage_attempts: 0, interrupted_requests: 0, legacy_unknown: false,
      ttft_ms_sum: 100, ttft_count: 1, decode_tps_sum: 10, decode_tps_count: 1, costs: {} } };
}
function emitUsage(usage: SessionUsageSnapshot) {
  for (const handler of handlers.ai_stats ?? []) handler({ payload: {
    client_request_id: "background-request", purpose: "chat", model: "fixture", wire: "fixture", request_id: "provider",
    retries: 0, ttft_ms: 100, decode_ms: 100, wall_ms: 200, decode_tps: 10, wall_tps: 5,
    usage: { input: usage.totals.input, cached_input: null, output: 40, reasoning: 0 }, failed: false, cost: null,
    accounting: { session_id: usage.session_id, incarnation: usage.incarnation, revision: usage.revision, snapshot: usage, saved: true },
  } });
}

const CONVERSATION: WireMessage[] = [
  { role: "user", content: "make the kick punchier" },
  {
    role: "assistant",
    content: "Trying a gain cut.",
    tool_calls: [{ id: "call-1", type: "function", function: { name: "tracks_mix", arguments: '{"track":"kick","gain_db":-2}' } }],
  },
  { role: "tool", tool_call_id: "call-1", content: '{"ok":true,"notes":"gain -2 dB"}' },
  { role: "assistant", content: "Done: the kick is 2 dB quieter." },
];

/** Everything a test can steer; the mock below only reads it. */
const backend = {
  sessions: [] as SessionMeta[],
  conversations: {} as Record<string, WireMessage[]>,
  projectPath: null as string | null,
  /** The workspace the backend reports, or `null` when nothing is chosen. */
  workspacePath: null as string | null,
  recents: [] as { name: string; path: string; opened: string }[],
  compacted: [] as Record<string, unknown>[],
  /** Whether the next `sessions_load` reports a compaction. */
  loadedCompacted: false,
  newCalls: [] as Record<string, unknown>[],
  /** The rewinds the panel asked for, and the snapshots it read. */
  rewinds: [] as { id: string; keep: number }[],
  versions: [] as string[],
  /** The turn the backend reports running, if any. */
  active: null as { request_id: string; session_id?: string | null } | null,
  /** Conversations the backend reports as compacted, and their earlier history. */
  compactedSessions: [] as string[],
  original: {} as Record<string, WireMessage[]>,
  chat: async (): Promise<unknown> => ({ role: "assistant", content: "Alright." }),
  transportPlaying: false,
  nativeMonitoring: false,
  liveSynthSounding: false,
};

function installBackend() {
  invokeMock.mockImplementation(async (command: string, args?: unknown) => {
    const typed = (args ?? {}) as Record<string, unknown>;
    switch (command) {
      case "ai_batch_recover": return null;
      case "load_ai_config": return CONFIG;
      case "ai_key_status": return true;
      case "ai_catalog": return CATALOG;
      case "ai_model_limits": return { provider_id: "google", model_id: "gemini-4-argon", bundled: null, overridden: null };
      case "ai_toolsets": return [];
      case "preview_saved_audition": return { mime: "audio/mpeg", base64: "SUQz" };
      case "open_saved_audition_render": return "http://127.0.0.1:38642/audition-render/fixture";
      case "claim_web_audio": return true;
      case "release_web_audio": return undefined;
      case "get_transport_state": return { playing: backend.transportPlaying, native_monitoring: backend.nativeMonitoring, live_synth_sounding: backend.liveSynthSounding };
      case "ai_tools": return [];
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
      case "ai_chat": return backend.chat();
      case "get_project": return { ...makeSnapshot(), file_path: backend.projectPath };
      case "workspace_current":
        return {
          path: backend.workspacePath,
          name: backend.workspacePath ? backend.workspacePath.split("/").pop() ?? null : null,
          detached: backend.workspacePath === null,
          projects: [],
        };
      case "workspaces_list": return backend.recents;
      case "workspace_open": {
        backend.workspacePath = String(typed.path);
        return {
          path: backend.workspacePath,
          name: backend.workspacePath.split("/").pop() ?? null,
          detached: false,
          projects: [],
        };
      }
      // A command answer arrives as fresh objects, as a JSON round trip does.
      // With a workspace chosen the backend scopes the list to it, exactly as
      // `sessions_list` does without a `project` argument.
      case "sessions_list": {
        const place = backend.workspacePath;
        const listed = place === null
          ? backend.sessions
          : backend.sessions.filter((entry) => entry.workspace === place);
        return listed.map((entry) => ({ ...entry }));
      }
      case "sessions_load": {
        const meta = backend.sessions.find((entry) => entry.id === typed.id);
        if (!meta) throw "no session";
        return {
          meta: { ...meta },
          messages: backend.conversations[meta.id] ?? [],
          skipped: 0,
          compacted: backend.compactedSessions.includes(meta.id) || backend.loadedCompacted,
        };
      }
      case "sessions_original": {
        return { messages: backend.original[String(typed.id)] ?? [], compactions: 1 };
      }
      case "sessions_new": {
        backend.newCalls.push(typed);
        const parent = backend.sessions.find((entry) => entry.id === typed.parent);
        const meta = session({
          id: `n${backend.sessions.length + 1}`,
          title: parent ? `${parent.title} (fork)` : String(typed.title ?? "New session"),
          parent: parent?.id ?? null,
        });
        backend.sessions = [meta, ...backend.sessions];
        const inherited = parent ? [...(backend.conversations[parent.id] ?? [])] : [];
        backend.conversations[meta.id] = typeof typed.through === "number"
          ? inherited.slice(0, typed.through)
          : inherited;
        return meta;
      }
      case "sessions_rename": {
        const meta = backend.sessions.find((entry) => entry.id === typed.id)!;
        meta.title = String(typed.title);
        return meta;
      }
      case "sessions_set_permission": {
        const meta = backend.sessions.find((entry) => entry.id === typed.id);
        if (!meta) throw "no session";
        meta.permission = typed.permission as SessionMeta["permission"];
        return { ...meta };
      }
      case "sessions_delete":
        backend.sessions = backend.sessions.filter((entry) => entry.id !== typed.id);
        return undefined;
      case "sessions_rewind": {
        backend.rewinds.push({ id: String(typed.id), keep: Number(typed.keep) });
        return { kept: Number(typed.keep) };
      }
      case "sessions_version": {
        backend.versions.push(String(typed.sha));
        return { dsl: "@project \"restored\"\n" };
      }
      case "ai_active_turn": return backend.active;
      case "sessions_compact":
        backend.compacted.push(typed);
        return {
          id: typed.id,
          compacted_at: "2026-10-29T08:30:00.000Z",
          messages: [],
          dropped_from: 2,
          dropped_to: 26,
          tokens_before: 12400,
          tokens_after: 1800,
          retained_pairs: 6,
        };
      default: return makeSnapshot();
    }
  });
}

/** Deliver one `ai_session` payload to the listeners the panel registered. */
function emitSession(payload: unknown) {
  for (const handler of handlers.ai_session ?? []) handler({ payload });
}

const panel = () => <><AiPanel /><TooltipLayer /><ContextMenuLayer /></>;
const input = () => document.querySelector(".ai-input") as HTMLInputElement;
const sendButton = () => document.querySelector(".ai-send-btn") as HTMLButtonElement;
const switcher = () => document.querySelector(".ai-session-current") as HTMLButtonElement;
const openTitle = () => document.querySelector(".ai-session-current-title")?.textContent ?? "";
const rowTitles = () => [...document.querySelectorAll(".ai-session-row-title")].map((node) => node.textContent);
const toolRows = () => {
  openStepGroups();
  return [...document.querySelectorAll(".ai-message.tool .ai-tool-result")];
};
const messagesOf = (role: string) => [...document.querySelectorAll(`.ai-message.${role}`)];
const messageRows = () => {
  openStepGroups();
  return [...document.querySelectorAll(".ai-messages .ai-message")] as HTMLElement[];
};

/** A promise the test resolves when it wants the provider to answer. */
function held() {
  let resolve!: (value: unknown) => void;
  const promise = new Promise((ok) => { resolve = ok; });
  return { promise, resolve };
}

async function mountPanel() {
  await store.initProject();
  const view = render(panel);
  await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "sessions_list")).toBe(true));
  await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "ai_tools")).toBe(true));
  return view;
}

async function openSwitcher() {
  fireEvent.click(switcher());
  await waitFor(() => expect(document.querySelector(".ai-session-list")).toBeTruthy());
  // The list command is answered asynchronously; wait for its rows.
  await waitFor(() => expect(document.querySelectorAll(".ai-session-row")).toHaveLength(backend.sessions.length));
}

function rowAction(title: string, action: "Rename" | "Fork" | "Compact" | "Delete"): HTMLElement {
  const row = screen.getByText(title).closest(".ai-session-row") as HTMLElement;
  fireEvent.contextMenu(row);
  return screen.getByRole("menuitem", { name: action });
}

beforeEach(() => {
  invokeMock.mockReset();
  backend.sessions = [session({ id: "s1", title: "intro idea", updated: "2026-10-28T09:00:00.000Z" })];
  backend.conversations = { s1: [{ role: "user", content: "an intro" }, { role: "assistant", content: "Here is an intro." }] };
  backend.projectPath = null;
  backend.workspacePath = null;
  backend.recents = [
    { name: "songs", path: "/Users/me/songs", opened: "2026-10-29T09:00:00.000Z" },
    { name: "other", path: "/Users/me/other", opened: "2026-10-28T09:00:00.000Z" },
  ];
  backend.compacted = [];
  backend.loadedCompacted = false;
  backend.newCalls = [];
  backend.rewinds = [];
  backend.versions = [];
  backend.active = null;
  backend.compactedSessions = [];
  backend.original = {};
  backend.chat = async () => ({ role: "assistant", content: "Alright." });
  backend.transportPlaying = false;
  backend.nativeMonitoring = false;
  backend.liveSynthSounding = false;
  i18n.setLocale("en");
  aiSession.reset();
  workspaceStore.reset();
  for (const name of Object.keys(handlers)) delete handlers[name];
  installBackend();
});

afterEach(() => {
  cleanup();
  aiSession.reset();
  vi.restoreAllMocks();
});

describe("AI panel session announcements", () => {
  it("shows a compaction the backend performed on its own", async () => {
    const view = await mountPanel();
    await waitFor(() => expect(handlers.ai_session?.length).toBe(1));
    expect(view.container.textContent ?? "").not.toContain("Context compacted");

    emitSession({
      session_id: "0a1b2c3d4e5f0001",
      compacted: true,
      provider: "google",
      model: "gemini-4-argon",
      dropped_from: 0,
      dropped_to: 24,
      tokens_before: 12400,
      tokens_after: 1800,
      retained_pairs: 6,
      messages: [{ role: "user", content: "summary" }],
    });

    await waitFor(() => expect(view.container.textContent ?? "").toContain("Context compacted: 24 messages"));
    expect(view.container.textContent).toContain("12400 → 1800 tokens");
    expect(view.container.textContent).toContain("6 recent tool calls kept as text");
  });

  it("says which archive files an audition turn evicted", async () => {
    const view = await mountPanel();
    await waitFor(() => expect(handlers.ai_session?.length).toBe(1));

    emitSession({
      session_id: "0a1b2c3d4e5f0001",
      evicted: [
        { dir: "audition-media", file: "ab12.wav", bytes: 2_000_000 },
        { dir: "audition-renders", file: "cd34.wav", bytes: 6_000_000 },
      ],
    });

    await waitFor(() =>
      expect(view.container.textContent ?? "").toContain("removed 2 file(s)"),
    );
    expect(view.container.textContent).toContain("audition-media/ab12.wav");
    expect(view.container.textContent).toContain("audition-renders/cd34.wav");
  });

  it("ignores an event that is not a compaction", async () => {
    const view = await mountPanel();
    await waitFor(() => expect(handlers.ai_session?.length).toBe(1));
    emitSession({ session_id: "x", compacted: false, tokens_before: 1, tokens_after: 1 });
    emitSession(null);
    await new Promise(resolve => setTimeout(resolve, 10));
    expect(view.container.textContent ?? "").not.toContain("Context compacted");
  });
});

describe("session permission", () => {
  const option = (mode: string) =>
    document.querySelector(`.ai-permission-option[data-permission="${mode}"]`) as HTMLButtonElement | null;
  const pressed = (mode: string) => option(mode)?.getAttribute("aria-pressed");
  const stored = () => invokeMock.mock.calls
    .filter(([command]) => command === "sessions_set_permission")
    .map(([, args]) => args as { id: string; permission: string });

  async function openSession(title: string) {
    await openSwitcher();
    fireEvent.click(screen.getByText(title).closest(".ai-session-row")!.querySelector(".ai-session-row-title") as HTMLElement);
  }

  it("shows the mode stored on the conversation it paints", async () => {
    backend.sessions = [session({ id: "s1", title: "asks first", permission: "require_approval" })];
    await mountPanel();
    await openSession("asks first");
    await waitFor(() => expect(pressed("require_approval")).toBe("true"));
    expect(pressed("write")).toBe("false");
  });

  it("reads a conversation saved before the field existed as the default mode", async () => {
    backend.sessions = [session({ id: "s1", title: "older conversation" })];
    await mountPanel();
    await openSession("older conversation");
    await waitFor(() => expect(pressed("write")).toBe("true"));
  });

  it("stores a change on the open conversation only", async () => {
    backend.sessions = [
      session({ id: "s1", title: "first", permission: "require_approval" }),
      session({ id: "s2", title: "second", permission: "write" }),
    ];
    await mountPanel();
    await openSession("first");
    await waitFor(() => expect(pressed("require_approval")).toBe("true"));

    fireEvent.click(option("autoreview")!);
    await waitFor(() => expect(pressed("autoreview")).toBe("true"));
    expect(stored()).toContainEqual({ id: "s1", permission: "autoreview" });
    // The other conversation is untouched.
    expect(backend.sessions.find(entry => entry.id === "s2")!.permission).toBe("write");
  });

  it("stores the chosen mode on the conversation the first turn creates", async () => {
    await mountPanel();
    fireEvent.click(option("require_approval")!);
    fireEvent.input(input(), { target: { value: "ask me before editing" } });
    fireEvent.click(sendButton());

    await waitFor(() => expect(backend.newCalls).toHaveLength(1));
    // The conversation the turn created carries the mode the bar was showing.
    await waitFor(() => expect(stored()).toContainEqual({
      id: backend.sessions[0].id,
      permission: "require_approval",
    }));
  });
});

describe("conversation switcher", () => {
  it("dismisses an AX-opened conversation picker with Escape and returns trigger focus", async () => {
    await mountPanel();
    input().focus();
    switcher().click();
    await waitFor(() => expect(document.querySelector(".ai-session-list")).toBeTruthy());
    expect(document.activeElement).toBe(input());
    const list = document.querySelector(".ai-session-list")!;
    expect(switcher().getAttribute("aria-controls")).toBe(list.id);
    expect(fireEvent.keyDown(input(), { key: "Escape" })).toBe(false);
    expect(document.querySelector(".ai-session-list")).toBeNull();
    expect(switcher().getAttribute("aria-expanded")).toBe("false");
    expect(document.activeElement).toBe(switcher());
    expect(invokeMock.mock.calls.some(([command]) => command === "ai_chat")).toBe(false);

    // A native keyboard activation also produces a detail-zero button click.
    fireEvent.click(switcher(), { detail: 0 });
    expect(document.querySelector(".ai-session-list")).toBeTruthy();
    fireEvent.keyDown(switcher(), { key: "Escape" });
    expect(document.querySelector(".ai-session-list")).toBeNull();
    fireEvent.click(switcher());
    fireEvent.click(switcher());
    expect(document.querySelector(".ai-session-list")).toBeNull();
  });

  it("cancels the nested session action before dismissing its picker", async () => {
    await mountPanel();
    await openSwitcher();
    const menuChoice = rowAction("intro idea", "Rename");
    // The popover is a real menu: arrows move within it, Escape closes it and
    // focus goes back to the row it described.
    menuChoice.focus();
    fireEvent.keyDown(menuChoice, { key: "ArrowDown" });
    expect(document.activeElement).toBe(screen.getByRole("menuitem", { name: "Fork" }));
    fireEvent.keyDown(document.activeElement!, { key: "ArrowUp" });
    expect(document.activeElement).toBe(menuChoice);
    fireEvent.keyDown(menuChoice, { key: "Escape" });
    expect(document.querySelector(".ai-session-menu")).toBeNull();
    expect(document.querySelector(".ai-session-list")).toBeTruthy();
    await waitFor(() =>
      expect((document.activeElement as HTMLElement)?.classList.contains("ai-session-row")).toBe(true));
    fireEvent.click(rowAction("intro idea", "Rename"));
    const field = document.querySelector(".ai-session-rename") as HTMLInputElement;
    fireEvent.input(field, { target: { value: "Uncommitted title" } });
    fireEvent.keyDown(field, { key: "Escape" });
    expect(document.querySelector(".ai-session-rename")).toBeNull();
    expect(rowTitles()).toEqual(["intro idea"]);
    expect(document.activeElement).toBe(switcher());
    expect(invokeMock.mock.calls.some(([command]) => command === "sessions_rename")).toBe(false);
    fireEvent.click(rowAction("intro idea", "Delete"));
    fireEvent.keyDown(screen.getByRole("button", { name: "Delete" }), { key: "Escape" });
    expect(document.querySelector(".ai-session-confirm")).toBeNull();
    expect(document.querySelector(".ai-session-list")).toBeTruthy();
    expect(invokeMock.mock.calls.some(([command]) => command === "sessions_delete")).toBe(false);
    fireEvent.keyDown(switcher(), { key: "Escape" });
    expect(document.querySelector(".ai-session-list")).toBeNull();
  });

  it("respects composed or already-consumed Escape and removes picker dismissal on unmount", async () => {
    const view = await mountPanel();
    await openSwitcher();
    fireEvent.keyDown(input(), { key: "Escape", isComposing: true });
    expect(document.querySelector(".ai-session-list")).toBeTruthy();
    const consumed = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    consumed.preventDefault();
    input().dispatchEvent(consumed);
    expect(document.querySelector(".ai-session-list")).toBeTruthy();
    view.unmount();
    const afterUnmount = new KeyboardEvent("keydown", { key: "Escape", bubbles: true, cancelable: true });
    window.dispatchEvent(afterUnmount);
    expect(afterUnmount.defaultPrevented).toBe(false);
  });

  it("keeps A visible while a delayed B load receives a newer B accounting event", async () => {
    backend.sessions = [session({ id: "s1", title: "one" }), session({ id: "s2", title: "two" })];
    await mountPanel(); await openSwitcher();
    fireEvent.click(screen.getByText("one"));
    await waitFor(() => expect(openTitle()).toBe("one"));
    hydrateSessionUsage("s1", usageSnapshot("s1", 1, 120));
    const delayed = held();
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, args?: unknown) => command === "sessions_load" && (args as { id?: string })?.id === "s2"
      ? delayed.promise : original(command, args));
    await openSwitcher(); fireEvent.click(screen.getByText("two"));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("sessions_load", { id: "s2" }));
    emitUsage(usageSnapshot("s2", 2, 2400));
    expect(openTitle()).toBe("one");
    expect(aiStats.session().input).toBe(120);
    delayed.resolve({ meta: session({ id: "s2", title: "two" }), messages: [], skipped: 0, usage: usageSnapshot("s2", 1, 1000) });
    await waitFor(() => expect(openTitle()).toBe("two"));
    expect(aiStats.snapshot()?.revision).toBe(2);
    expect(aiStats.session().input).toBe(2400);
    expect(contextMeter.reported()).toBe(0);
  });
  it("reconciles once when a pending event has no loaded incarnation to bind", async () => {
    backend.sessions = [session({ id: "s1", title: "one" }), session({ id: "s2", title: "two" })];
    await mountPanel(); await openSwitcher(); fireEvent.click(screen.getByText("one"));
    await waitFor(() => expect(openTitle()).toBe("one"));
    const delayed = held();
    const original = invokeMock.getMockImplementation()!;
    let loads = 0;
    invokeMock.mockImplementation((command: string, args?: unknown) => {
      if (command === "sessions_load" && (args as { id?: string })?.id === "s2") {
        loads += 1;
        return loads === 1 ? delayed.promise : Promise.resolve({ meta: session({ id: "s2", title: "two" }), messages: [], skipped: 0, usage: usageSnapshot("s2", 2, 2400) });
      }
      return original(command, args);
    });
    await openSwitcher(); fireEvent.click(screen.getByText("two"));
    await waitFor(() => expect(loads).toBe(1));
    emitUsage(usageSnapshot("s2", 2, 2400));
    delayed.resolve({ meta: session({ id: "s2", title: "two" }), messages: [], skipped: 0 });
    await waitFor(() => expect(openTitle()).toBe("two"));
    expect(loads).toBe(2); expect(aiStats.session().input).toBe(2400);
  });
  it("drops a failed pending paint and never publishes its buffered accounting", async () => {
    backend.sessions = [session({ id: "s1", title: "one" }), session({ id: "s2", title: "two" })];
    await mountPanel(); await openSwitcher(); fireEvent.click(screen.getByText("one"));
    await waitFor(() => expect(openTitle()).toBe("one"));
    hydrateSessionUsage("s1", usageSnapshot("s1", 1, 120));
    let reject!: (reason: Error) => void;
    const delayed = new Promise<unknown>((_resolve, fail) => { reject = fail; });
    const original = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation((command: string, args?: unknown) => command === "sessions_load" && (args as { id?: string })?.id === "s2"
      ? delayed : original(command, args));
    await openSwitcher(); fireEvent.click(screen.getByText("two"));
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("sessions_load", { id: "s2" }));
    emitUsage(usageSnapshot("s2", 2, 2400));
    reject(new Error("cannot read conversation"));
    await waitFor(() => expect(document.body.textContent).toContain("cannot read conversation"));
    expect(openTitle()).toBe("one"); expect(aiStats.session().input).toBe(120);
    emitUsage(usageSnapshot("s2", 3, 9999));
    expect(aiStats.session().input).toBe(120);
  });
  it("ignores a delayed session load after the workspace changes", async () => {
    backend.projectPath = "/Users/me/songs/first.ocp";
    backend.sessions = [session({ id: "s1", title: "old songs" })];
    const view = await mountPanel();
    await openSwitcher();
    const original = invokeMock.getMockImplementation()!;
    let finish!: (value: unknown) => void;
    invokeMock.mockImplementation((command: string, args?: unknown) => command === "sessions_load"
      ? new Promise(resolve => { finish = resolve; }) : original(command, args));
    fireEvent.click(document.querySelector(".ai-session-row-title") as HTMLElement);
    await waitFor(() => expect(finish).toBeTypeOf("function"));
    emitUsage(usageSnapshot("s1", 2, 9999));
    backend.projectPath = "/Users/me/other/next.ocp";
    backend.sessions = [];
    await store.initProject();
    finish({ meta: session({ id: "s1", title: "old songs" }), messages: [{ role: "assistant", content: "DELAYED OLD TRANSCRIPT" }], skipped: 0, usage: usageSnapshot("s1", 1, 120) });
    await new Promise(resolve => setTimeout(resolve, 20));
    expect(view.container.textContent).not.toContain("DELAYED OLD TRANSCRIPT");
    expect(aiSession.open()).toBeNull();
    expect(aiStats.activeSession()).toBeNull();
    expect(aiStats.session().input).toBe(0);
  });

  it("does not restore another workspace's globally remembered conversation on mount", async () => {
    backend.workspacePath = "/Users/me/other";
    backend.projectPath = "/Users/me/other/new.ocp";
    backend.sessions = [session({ id: "old", title: "old songs" })];
    backend.conversations = { old: [{ role: "assistant", content: "PREVIOUS WORKSPACE TRANSCRIPT" }] };
    aiSession.setOpen("old");
    const view = await mountPanel();
    await waitFor(() => expect(openTitle()).toBe("New conversation"));
    await new Promise(resolve => setTimeout(resolve, 20));
    expect(view.container.textContent).not.toContain("PREVIOUS WORKSPACE TRANSCRIPT");
    expect(aiSession.open()).toBeNull();
  });

  it("collapses restored compaction summaries while retaining their full text", async () => {
    const summary = "Conversation summary written 2026-10-30.\n" + "Earlier piano decisions.\n".repeat(40);
    backend.conversations = { s1: [{ role: "user", content: summary }, { role: "assistant", content: "Latest answer" }] };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    await waitFor(() => expect(document.querySelector(".ai-checkpoint")).toBeTruthy());
    const details = document.querySelector(".ai-checkpoint") as HTMLDetailsElement;
    const row = details.closest(".ai-message")!;
    expect(row.classList.contains("context")).toBe(true);
    expect(row.classList.contains("user")).toBe(false);
    expect(details.open).toBe(false);
    expect(details.querySelector("summary")?.textContent).toBe("Earlier conversation summary");
    expect(details.querySelector(".ai-message-content")).toBeNull();
    expect(screen.getByText("Latest answer")).toBeTruthy();
    details.open = true;
    fireEvent(details, new Event("toggle"));
    await waitFor(() => expect(details.querySelector(".ai-message-content")?.textContent).toBe(summary));
  });

  it("keeps the session's compaction checkpoint collapsed and captions auditions instead of user turns", async () => {
    backend.loadedCompacted = true;
    const summary = "Conversation summary written 2026-11-03T08:07:21.775Z.\nStanding intent: mix the song.";
    const media: MediaAttachment = {
      kind: "audio", name: "audition.wav", path: "/archive/audition-media/audition.wav",
      mime: "audio/mpeg", bytes: 1024, duration_secs: 13, sha256: "b".repeat(64),
    };
    backend.conversations = {
      s1: [
        { role: "user", context: "summary", content: summary },
        { role: "user", context: "audition", content: "You asked to hear 0–8 s: this is that render.", media: [media] },
        { role: "user", content: "now the bass" },
      ],
    };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    await waitFor(() => expect(document.querySelector(".ai-checkpoint")).toBeTruthy());

    const checkpoint = document.querySelector(".ai-checkpoint") as HTMLDetailsElement;
    expect(checkpoint.open).toBe(false);
    expect(checkpoint.closest(".ai-message")?.classList.contains("checkpoint")).toBe(true);
    expect(checkpoint.closest(".ai-message")?.classList.contains("user")).toBe(false);
    expect(checkpoint.querySelector(".ai-message-content")).toBeNull();
    checkpoint.open = true;
    fireEvent(checkpoint, new Event("toggle"));
    await waitFor(() => expect(checkpoint.querySelector(".ai-message-content")?.textContent).toBe(summary));

    const userRows = [...document.querySelectorAll(".ai-message.user")];
    expect(userRows.map((row) => row.textContent)).toEqual(["now the bass"]);
    expect(document.querySelectorAll(".ai-message.context.audition")).toHaveLength(1);
    const audition = document.querySelector(".ai-message.context.audition") as HTMLElement;
    expect(audition.textContent).toContain("Audition");
    expect(audition.querySelector(".ai-media-tag")).toBeTruthy();
    // Host context offers no revert entry.
    fireEvent.contextMenu(audition);
    expect(isContextMenuOpen()).toBe(false);
  });

  it("loads a reopened source audition from its archived MP3 on demand", async () => {
    const hash = "b".repeat(64);
    const path = `/archive/audition-media/${hash}.mp3`;
    backend.conversations = {
      s1: [{
        role: "user", context: "audition",
        content: "You asked to hear source seconds 1.25–2.5: this is the original clip audio.",
        source_render_unavailable: true,
        media: [{
          kind: "audio", name: `${hash}.mp3`, path, mime: "audio/mpeg",
          bytes: 3, duration_secs: 1.25, sha256: hash,
        }],
      }],
    };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    const load = await screen.findByRole("button", { name: "Load audio" });
    const row = load.closest(".ai-message.context.audition")!;
    expect(row.textContent).toContain("Source 1.25–2.5 s");
    expect(row.textContent).toContain("The full render is unavailable. The saved excerpt is still playable.");
    expect(screen.queryByRole("button", { name: "Load full render" })).toBeNull();
    expect(row.querySelector("audio")).toBeNull();
    fireEvent.click(load);
    await waitFor(() => expect(row.querySelector("audio")?.getAttribute("src")).toBe("data:audio/mpeg;base64,SUQz"));
    expect(invokeMock).toHaveBeenCalledWith("preview_saved_audition", { path });
  });

  it("does not offer a saved excerpt when both archive files are missing", async () => {
    const hash = "b".repeat(64);
    backend.conversations = {
      s1: [{
        role: "user", context: "audition",
        content: "You asked to hear source seconds 1.25–2.5: this is the original clip audio.",
        source_render_missing: true,
        media: [{
          kind: "audio", name: `${hash}.wav`, path: `/archive/audition-media/${hash}.wav`,
          mime: "audio/wav", bytes: 3, duration_secs: 1.25, sha256: hash, missing: true,
        }],
      }],
    };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    await screen.findByText("The full render is unavailable.");
    expect(screen.queryByText(/The saved excerpt is still playable/)).toBeNull();
    expect(screen.queryByRole("button", { name: "Load audio" })).toBeNull();
    expect(screen.queryByRole("button", { name: "Load full render" })).toBeNull();
  });

  it("opens the full saved render by URL and retries after an audio load failure", async () => {
    const path = `/archive/audition-renders/${"a".repeat(64)}.wav`;
    backend.conversations = {
      s1: [{
        role: "user", context: "audition",
        content: "You asked to hear this render.",
        source_render: path,
      }],
    };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    const load = await screen.findByRole("button", { name: "Load full render" });
    expect(invokeMock).not.toHaveBeenCalledWith("open_saved_audition_render", { path });
    fireEvent.click(load);
    await waitFor(() => expect(document.querySelector("audio")?.getAttribute("src"))
      .toBe("http://127.0.0.1:38642/audition-render/fixture"));
    expect(invokeMock).toHaveBeenCalledWith("open_saved_audition_render", { path });
    const audio = document.querySelector("audio")!;
    Object.defineProperties(audio, {
      duration: { value: 120 },
      readyState: { value: 1 },
    });
    fireEvent.loadedMetadata(audio);
    const seek = screen.getByRole("slider", { name: "Seek" });
    expect(seek).toHaveAttribute("max", "120");
    fireEvent.input(seek, { target: { value: "77.5" } });
    expect(audio.currentTime).toBe(77.5);
    expect(audio.paused).toBe(true);
    fireEvent.error(audio);
    expect(await screen.findByRole("button", { name: "Load full render" })).toBeEnabled();
    expect(document.querySelector("audio")).toBeNull();
  });

  it.each([
    ["transport", "Pause playback or finish recording to listen here."],
    ["monitoring", "Turn off input or MIDI monitoring, or close the plug-in editor, to listen here."],
    ["synth", "Release the synth notes and let their sound finish to listen here."],
  ])("keeps an inline audition silent while %s owns playback", async (owner, notice) => {
    const path = `/archive/audition-renders/${"a".repeat(64)}.wav`;
    backend.transportPlaying = owner === "transport";
    backend.nativeMonitoring = owner === "monitoring";
    backend.liveSynthSounding = owner === "synth";
    backend.conversations = {
      s1: [{ role: "user", context: "audition", content: "Saved render.", source_render: path }],
    };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    fireEvent.click(await screen.findByRole("button", { name: "Load full render" }));
    const group = await screen.findByRole("group", { name: "Load full render" });
    const audio = document.querySelector("audio")!;
    const play = vi.spyOn(audio, "play").mockResolvedValue();
    fireEvent.click(group.querySelector("button")!);
    expect(play).not.toHaveBeenCalled();
    expect(await screen.findByText(notice)).toBeVisible();
    expect(play).not.toHaveBeenCalled();
    expect(invokeMock).not.toHaveBeenCalledWith("transport_pause");
  });

  it("shows newly saved audition rows only for the active request and session", async () => {
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("sessions_load", { id: "s1" }));
    const pending = held();
    backend.chat = () => pending.promise;
    fireEvent.input(input(), { target: { value: "Listen to the mix" } });
    fireEvent.click(sendButton());
    await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "ai_chat")).toBe(true));
    const args = invokeMock.mock.calls.find(([command]) => command === "ai_chat")?.[1] as { requestId: string; sessionId?: string };
    expect(args.sessionId).toBe(aiSession.open());
    const saved = {
      request_id: args.requestId,
      session_id: "s1",
      saved_auditions: [{
        role: "user", context: "audition", content: "You asked to hear the mix.",
        source_render: `/archive/audition-renders/${"a".repeat(64)}.wav`,
      }],
    };
    emitSession({ ...saved, session_id: "s2" });
    emitSession({ ...saved, request_id: "old-request" });
    expect(screen.queryByRole("button", { name: "Load full render" })).toBeNull();
    emitSession(saved);
    expect(await screen.findByRole("button", { name: "Load full render" })).toBeEnabled();
    pending.resolve({ role: "assistant", content: "Done." });
    await waitFor(() => expect(document.querySelector(".ai-send-btn.cancel")).toBeNull());
  });

  it("opens restored history at its latest turn then preserves an intentional upward scroll", async () => {
    await mountPanel();
    const transcript = document.querySelector(".ai-messages") as HTMLElement;
    Object.defineProperty(transcript, "scrollHeight", { configurable: true, value: 2400 });
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    await waitFor(() => expect(transcript.scrollTop).toBe(2400));
    transcript.scrollTop = 80;
    fireEvent.scroll(transcript);
    fireEvent.scroll(transcript);
    emitSession({ session_id: "s1", compacted: true, dropped_from: 0, dropped_to: 2,
      tokens_before: 1000, tokens_after: 500, retained_pairs: 1, messages: [] });
    await waitFor(() => expect(transcript.textContent).toContain("Context compacted"));
    expect(transcript.scrollTop).toBe(80);
  });

  it("lists this workspace's conversations newest first and names the directory once", async () => {
    backend.sessions = [
      session({ id: "s2", title: "punchier kick", updated: "2026-10-29T08:00:00.000Z" }),
      session({ id: "s1", title: "intro idea", updated: "2026-10-28T09:00:00.000Z" }),
    ];
    await mountPanel();
    expect(openTitle()).toBe("New conversation");
    await openSwitcher();

    expect(rowTitles()).toEqual(["punchier kick", "intro idea"]);
    // One scope line, and it names the directory rather than the project file.
    const scopes = [...document.querySelectorAll(".ai-session-scope-path")];
    expect(scopes).toHaveLength(1);
    expect(scopes[0].textContent).toBe("/Users/me/songs");
    // The list is the workspace's, so no project path narrows it.
    const calls = invokeMock.mock.calls.filter(([command]) => command === "sessions_list");
    expect(calls.length).toBeGreaterThan(0);
    for (const [, args] of calls) expect((args as Record<string, unknown>).project).toBeUndefined();
    // A row shows the time the store recorded, in its own element.
    const times = [...document.querySelectorAll(".ai-session-row-time")];
    expect(times.map((node) => node.getAttribute("datetime"))).toEqual([
      "2026-10-29T08:00:00.000Z",
      "2026-10-28T09:00:00.000Z",
    ]);
  });

  it("falls back to the open project's directory when nothing is saved yet", async () => {
    backend.sessions = [];
    backend.projectPath = "/Users/me/songs/song.ocp";
    await mountPanel();
    await openSwitcher();

    expect(document.querySelector(".ai-session-empty")?.textContent).toBe("No saved conversations in this workspace yet.");
    expect(document.querySelector(".ai-session-scope-path")?.textContent).toBe("/Users/me/songs");
  });

  it("keeps one list for two project files in the same directory", async () => {
    backend.projectPath = "/Users/me/songs/first.ocp";
    backend.sessions = [session({ id: "s1", title: "in songs" })];
    await mountPanel();
    await openSwitcher();
    expect(rowTitles()).toEqual(["in songs"]);

    // Saving the same work under another file name in the same directory is
    // the same workspace, so the list and the scope line do not move.
    backend.projectPath = "/Users/me/songs/second.ocp";
    await store.initProject();
    await waitFor(() => expect(document.querySelector(".ai-session-scope-path")?.textContent).toBe("/Users/me/songs"));
    expect(rowTitles()).toEqual(["in songs"]);
    expect(aiSession.open()).toBeNull();
  });

  it("moves to another directory's conversations and drops the transcript it showed", async () => {
    backend.projectPath = "/Users/me/songs/first.ocp";
    backend.sessions = [session({ id: "s1", title: "in songs" })];
    backend.conversations = { s1: CONVERSATION };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title") as HTMLElement);
    await waitFor(() => expect(toolRows()).toHaveLength(1));

    backend.projectPath = "/Users/me/other/second.ocp";
    backend.sessions = [session({ id: "s9", title: "elsewhere", workspace: "/Users/me/other" })];
    await store.initProject();
    await openSwitcher();
    expect(document.querySelector(".ai-session-scope-path")?.textContent).toBe("/Users/me/other");
    expect(rowTitles()).toEqual(["elsewhere"]);
    // The old workspace's transcript is not this workspace's history.
    expect(document.querySelectorAll(".ai-message").length).toBe(0);
    expect(openTitle()).toBe("New conversation");
  });

  it("switches workspace from the scope line and swaps the list without deleting the other one", async () => {
    backend.workspacePath = "/Users/me/songs";
    backend.sessions = [
      session({ id: "s1", title: "in songs", workspace: "/Users/me/songs" }),
      session({ id: "s2", title: "in other", workspace: "/Users/me/other" }),
    ];
    backend.conversations = { s1: CONVERSATION, s2: CONVERSATION };
    await workspaceStore.init();
    // The panel reaches the chooser through the shared store; App mounts the
    // dialog itself, so the test mounts the same pair.
    const view = render(() => (
      <>
        <AiPanel />
        <TooltipLayer />
        <Show when={workspaceStore.chooserOpen()}>
          <WorkspaceChooserDialog onClose={() => workspaceStore.closeChooser()} />
        </Show>
      </>
    ));
    await waitFor(() => expect(document.querySelector(".ai-session-current")).toBeTruthy());
    fireEvent.click(document.querySelector(".ai-session-current") as HTMLElement);
    // The dropdown is anchored to the bar: it must be a child, or its absolute
    // position resolves against the panel and it renders behind the transcript.
    expect(document.querySelector(".ai-session-bar > .ai-session-list")).toBeTruthy();
    // The list is this workspace's: the other directory's conversation is not in it.
    await waitFor(() => expect(rowTitles()).toEqual(["in songs"]));

    // Picking another recent directory rescopes the list.
    fireEvent.click(document.querySelector(".ai-session-scope") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".workspace-chooser-dialog")).toBeTruthy());
    const other = [...document.querySelectorAll<HTMLElement>(".workspace-recent-open")]
      .find((node) => node.textContent?.includes("/Users/me/other"));
    fireEvent.click(other!);
    await waitFor(() => expect(backend.workspacePath).toBe("/Users/me/other"));
    await waitFor(() => expect(document.querySelector(".ai-session-scope-path")?.textContent).toBe("/Users/me/other"));
    await waitFor(() => expect(rowTitles()).toEqual(["in other"]));

    // The other workspace keeps its conversation: going back lists it again,
    // and nothing was deleted on the way.
    fireEvent.click(document.querySelector(".ai-session-scope") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".workspace-chooser-dialog")).toBeTruthy());
    const songs = [...document.querySelectorAll<HTMLElement>(".workspace-recent-open")]
      .find((node) => node.textContent?.includes("/Users/me/songs"));
    fireEvent.click(songs!);
    await waitFor(() => expect(rowTitles()).toEqual(["in songs"]));
    expect(invokeMock.mock.calls.some(([command]) => command === "sessions_delete")).toBe(false);
    expect(view.container.querySelectorAll(".ai-session-row")).toHaveLength(1);
  });

  it("opens a conversation and paints prose and tool rows with their call", async () => {
    backend.sessions = [session({ id: "s2", title: "punchier kick" })];
    backend.conversations = { s2: CONVERSATION };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title") as HTMLElement);

    await waitFor(() => expect(openTitle()).toBe("punchier kick"));
    expect(invokeMock.mock.calls.some(([command, args]) => command === "sessions_load" && (args as Record<string, unknown>).id === "s2")).toBe(true);

    // User row, both assistant messages and the tool call the turn made.
    expect(messagesOf("user").map((row) => row.textContent)).toEqual([expect.stringContaining("make the kick punchier")]);
    expect(messagesOf("assistant").map((row) => row.textContent)).toEqual([
      expect.stringContaining("Trying a gain cut."),
      expect.stringContaining("Done: the kick is 2 dB quieter."),
    ]);
    expect(toolRows()).toHaveLength(1);
    (toolRows()[0] as HTMLDetailsElement).open = true;
    fireEvent(toolRows()[0], new Event("toggle"));
    const tool = toolRows()[0].textContent ?? "";
    expect(tool).toContain("tracks_mix");
    expect(tool).toContain("kick");
    expect(tool).toContain("gain -2 dB");
    // The readable view is the default; the whole result stays one click away.
    expect(toolRows()[0].querySelector(".ai-tool-args")).toBeTruthy();
    expect(toolRows()[0].querySelector(".ai-tool-rich")).toBeTruthy();
    const toggles = toolRows()[0].querySelectorAll(".ai-tool-raw-toggle");
    fireEvent.click(toggles[toggles.length - 1] as HTMLElement);
    await waitFor(() => expect([...toolRows()[0].querySelectorAll("pre")].at(-1)?.textContent).toBe(JSON.stringify({ok:true,notes:"gain -2 dB"},null,2)));
    // The wire has no per-row time, so no row claims one.
    expect(document.querySelector(".ai-messages time")).toBeNull();
  });

  it("starts a new conversation on the first message and titles it with that message", async () => {
    await mountPanel();
    fireEvent.input(input(), { target: { value: "give the bass more body" } });
    fireEvent.click(sendButton());

    await waitFor(() => expect(backend.newCalls).toHaveLength(1));
    expect(backend.newCalls[0]).toEqual({ title: "give the bass more body" });
    await waitFor(() => expect(openTitle()).toBe("give the bass more body"));
  });

  it("starts the context meter and the stats line again when another conversation opens", async () => {
    backend.sessions = [session({ id: "s1", title: "one" }), session({ id: "s2", title: "two" })];
    backend.conversations = { s1: CONVERSATION, s2: CONVERSATION };
    const openRow = (title: string) => {
      const row = [...document.querySelectorAll(".ai-session-row-title")]
        .find((node) => node.textContent === title) as HTMLElement;
      fireEvent.click(row);
    };
    await mountPanel();
    await openSwitcher();
    openRow("one");
    await waitFor(() => expect(openTitle()).toBe("one"));

    // A reported prompt calibrates the meter, and the turn feeds the stats line.
    hydrateSessionUsage("s1", {
      session_id: "s1", incarnation: "life-1", revision: 1, pending_requests: 0,
      totals: { requests: 1, attempts: 1, input: 120, output: 40, reasoning: 0,
        cached_input: 0, cache_input: 0, failed_attempts: 0, unknown_price_attempts: 1,
        unreported_usage_attempts: 0, interrupted_requests: 0, legacy_unknown: false,
        ttft_ms_sum: 100, ttft_count: 1, decode_tps_sum: 10, decode_tps_count: 1, costs: {} },
    });
    noteSentEstimate(100, "local-r1", "s1");
    recordTurn({
      client_request_id: "local-r1", purpose: "chat",
      model: "gemini-4-argon", wire: "gemini-interactions", request_id: "r1", retries: 0,
      ttft_ms: 100, decode_ms: 200, wall_ms: 300, decode_tps: 10, wall_tps: 8,
      usage: { input: 120, cached_input: null, output: 40, reasoning: 0 },
      failed: false, cost: null,
    });
    rememberToolResult("call_old", "x".repeat(200));
    expect(aiStats.session().turns).toBe(1);
    expect(contextMeter.reported()).toBe(120);

    await openSwitcher();
    openRow("two");
    await waitFor(() => expect(openTitle()).toBe("two"));

    // The other conversation has no reported prompt, no turn, and no result the
    // old rows could page.
    expect(aiStats.session().turns).toBe(0);
    expect(contextMeter.reported()).toBe(0);
    expect(storedToolResult("call_old")).toBeNull();
  });

  it("does not start a second conversation on the next message", async () => {
    backend.sessions = [session({ id: "s2", title: "punchier kick" })];
    backend.conversations = { s2: CONVERSATION };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title") as HTMLElement);
    await waitFor(() => expect(openTitle()).toBe("punchier kick"));

    fireEvent.input(input(), { target: { value: "one more thing" } });
    fireEvent.click(sendButton());
    await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "ai_chat")).toBe(true));
    expect(backend.newCalls).toHaveLength(0);
  });

  it("renames a conversation inline", async () => {
    await mountPanel();
    await openSwitcher();
    fireEvent.click(rowAction("intro idea", "Rename"));

    const field = document.querySelector(".ai-session-rename") as HTMLInputElement;
    expect(field.value).toBe("intro idea");
    fireEvent.input(field, { target: { value: "intro sketch" } });
    fireEvent.keyDown(field, { key: "Enter" });

    await waitFor(() => expect(rowTitles()).toEqual(["intro sketch"]));
    const rename = invokeMock.mock.calls.filter(([command]) => command === "sessions_rename").at(-1);
    expect(rename?.[1]).toEqual({ id: "s1", title: "intro sketch" });
  });

  it("asks before deleting a conversation", async () => {
    await mountPanel();
    await openSwitcher();
    fireEvent.click(rowAction("intro idea", "Delete"));

    // The first click only asks, and names what would go.
    expect(document.querySelector(".ai-session-confirm")?.textContent).toContain("Delete intro idea?");
    expect(invokeMock.mock.calls.some(([command]) => command === "sessions_delete")).toBe(false);

    fireEvent.click(screen.getByRole("button", { name: "Delete" }));
    await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "sessions_delete")).toBe(true));
    const call = invokeMock.mock.calls.filter(([command]) => command === "sessions_delete").at(-1);
    expect(call?.[1]).toEqual({ id: "s1" });
    await waitFor(() => expect(document.querySelector(".ai-session-row")).toBeNull());
  });

  it("leaves the list alone when the delete is cancelled", async () => {
    await mountPanel();
    await openSwitcher();
    fireEvent.click(rowAction("intro idea", "Delete"));
    fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
    expect(document.querySelector(".ai-session-confirm")).toBeNull();
    expect(invokeMock.mock.calls.some(([command]) => command === "sessions_delete")).toBe(false);
    expect(rowTitles()).toEqual(["intro idea"]);
  });

  it("forks a conversation from its row and opens the copy", async () => {
    backend.sessions = [session({ id: "s2", title: "punchier kick" })];
    backend.conversations = { s2: CONVERSATION };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(rowAction("punchier kick", "Fork"));

    await waitFor(() => expect(backend.newCalls).toHaveLength(1));
    expect(backend.newCalls[0]).toEqual({ parent: "s2" });
    await waitFor(() => expect(openTitle()).toBe("punchier kick (fork)"));
    // The copy starts with the parent's conversation, tool rows included.
    await waitFor(() => expect(toolRows()).toHaveLength(1));
    expect(messagesOf("user").map((row) => row.textContent)).toEqual([expect.stringContaining("make the kick punchier")]);
  });

  it("forks a conversation at a chosen turn, keeping the messages before it", async () => {
    backend.conversations = {
      s1: [
        { role: "user", content: "make the kick punchier" },
        { role: "assistant", content: "Done: the kick is 2 dB quieter." },
        { role: "user", content: "now the bass" },
        { role: "assistant", content: "The bass is louder." },
      ],
    };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    await waitFor(() => expect(messageRows()).toHaveLength(4));

    // A user row is not a completed turn, so it offers no fork.
    fireEvent.contextMenu(messageRows()[0]);
    expect(screen.queryByRole("menuitem", { name: /Fork from here/ })).toBeNull();
    fireEvent.keyDown(document, { key: "Escape" });

    // The first answer is a turn boundary: the fork keeps both of its messages.
    fireEvent.contextMenu(messageRows()[1]);
    fireEvent.click(screen.getByRole("menuitem", { name: /Fork from here/ }));

    await waitFor(() => expect(backend.newCalls).toHaveLength(1));
    expect(backend.newCalls[0]).toEqual({ parent: "s1", through: 2 });
    await waitFor(() => expect(openTitle()).toBe("intro idea (fork)"));
    await waitFor(() => expect(messageRows()).toHaveLength(2));
    expect(messageRows().map((row) => row.textContent)).toEqual([
      expect.stringContaining("make the kick punchier"),
      expect.stringContaining("Done: the kick is 2 dB quieter."),
    ]);
  });

  it("offers no fork inside a turn that is still writing", async () => {
    backend.conversations = {
      s1: [
        { role: "user", content: "make the kick punchier" },
        {
          role: "assistant",
          content: "Trying a gain cut.",
          tool_calls: [{ id: "call-1", type: "function", function: { name: "tracks_mix", arguments: "{}" } }],
        },
        { role: "tool", tool_call_id: "call-1", content: '{"ok":true}' },
        { role: "assistant", content: "Done: the kick is 2 dB quieter." },
      ],
    };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    await waitFor(() => expect(messageRows()).toHaveLength(4));

    // The call was answered later, so the row that made it is not a boundary.
    fireEvent.contextMenu(messageRows()[1]);
    expect(screen.queryByRole("menuitem", { name: /Fork from here/ })).toBeNull();

    // The answer after the tool row ends the turn and can be forked.
    fireEvent.contextMenu(messageRows()[3]);
    fireEvent.click(screen.getByRole("menuitem", { name: /Fork from here/ }));
    await waitFor(() => expect(backend.newCalls).toEqual([{ parent: "s1", through: 4 }]));
  });

  it("compacts a conversation now and appends one line saying what it dropped", async () => {
    backend.sessions = [session({ id: "s2", title: "punchier kick" })];
    backend.conversations = { s2: CONVERSATION };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(rowAction("punchier kick", "Compact"));

    await waitFor(() => expect(backend.compacted).toHaveLength(1));
    expect(backend.compacted[0]).toEqual({ id: "s2" });
    await waitFor(() => expect(document.querySelector(".ai-messages")?.textContent).toContain("Context compacted"));
    const text = document.querySelector(".ai-messages")?.textContent ?? "";
    expect(text.match(/Context compacted/g)?.length).toBe(1);
    expect(text).toContain("Context compacted: 24 messages became a summary (12400 → 1800 tokens); 6 recent tool calls kept as text.");
  });
});

describe("the draft and the open conversation outlive the panel", () => {
  it("keeps the draft across an unmount and clears it when another conversation opens", async () => {
    const first = await mountPanel();
    fireEvent.input(input(), { target: { value: "half a sentence" } });
    first.unmount();

    await mountPanel();
    await waitFor(() => expect(input().value).toBe("half a sentence"));
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title") as HTMLElement);
    await waitFor(() => expect(input().value).toBe(""));
  });

  it("remembers the open conversation per project file, not only per window", async () => {
    backend.projectPath = "/Users/me/songs/one.ocp";
    const first = await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title") as HTMLElement);
    await waitFor(() => expect(aiSession.open()).toBe("s1"));
    first.unmount();

    // Another song in the same workspace starts fresh; it must not resume the
    // previous song's transcript.
    backend.projectPath = "/Users/me/songs/two.ocp";
    backend.sessions = [session({ id: "s2", title: "second song" })];
    backend.conversations = { s2: [{ role: "user", content: "second" }] };
    const second = await mountPanel();
    await waitFor(() => expect(openTitle()).toBe("New conversation"));
    await waitFor(() => expect(aiSession.open()).toBeNull());
    second.unmount();

    // Reopening the first song restores its own conversation.
    backend.projectPath = "/Users/me/songs/one.ocp";
    backend.sessions = [session({ id: "s1", title: "intro idea" })];
    await mountPanel();
    await waitFor(() => expect(openTitle()).toBe("intro idea"));
    expect(aiSession.open()).toBe("s1");
  });

  it("comes back on the conversation it was showing, repainted from the store", async () => {
    backend.sessions = [session({ id: "s2", title: "punchier kick" })];
    backend.conversations = { s2: CONVERSATION };
    const first = await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title") as HTMLElement);
    await waitFor(() => expect(toolRows()).toHaveLength(1));
    first.unmount();

    await mountPanel();
    await waitFor(() => expect(openTitle()).toBe("punchier kick"));
    await waitFor(() => expect(toolRows()).toHaveLength(1));
    expect(invokeMock.mock.calls.filter(([command]) => command === "sessions_load")).toHaveLength(2);
  });
});

describe("session compaction payload", () => {
  it("reports what was dropped and what stayed", () => {
    expect(sessionCompaction({ compacted: true, tokens_after: 5 })).not.toBeNull();
    expect(sessionCompaction({ compacted: false, tokens_after: 5 })).toBeNull();
    expect(sessionCompaction(undefined)).toBeNull();
    const compaction: SessionCompaction = {
      session_id: "a",
      compacted: true,
      provider: "p",
      model: "m",
      dropped_from: 2,
      dropped_to: 3,
      tokens_before: 900,
      tokens_after: 120,
      retained_pairs: 1,
      messages: [],
    };
    expect(compactionNotice(compaction)).toBe(
      "Context compacted: 1 messages became a summary (900 → 120 tokens); 1 recent tool call kept as text.",
    );
  });

  it("guards the list and load payloads", () => {
    expect(sessionList(undefined)).toEqual([]);
    expect(sessionList({ sessions: [] })).toEqual([]);
    expect(sessionList([{ id: "a" }, session({ id: "b" })])).toEqual([session({ id: "b" })]);
    expect(loadedSession(null)).toBeNull();
    expect(loadedSession({ meta: session() })).toBeNull();
    expect(loadedSession({ meta: { id: "a" }, messages: [] })?.messages).toEqual([]);
  });

  it("scopes the list to the directory, never to one project file", () => {
    // Two projects in one directory share one set of conversations.
    expect(workspaceScope([], "/Users/me/songs/song.ocp")).toBe("/Users/me/songs");
    expect(workspaceScope([], "/Users/me/songs/other.ocp")).toBe("/Users/me/songs");
    // The store's own record wins once it has one.
    expect(workspaceScope([session({ workspace: "/moved/songs" })], "/Users/me/songs/song.ocp")).toBe("/moved/songs");
    // A conversation with no workspace says so instead of naming a directory.
    expect(workspaceScope([session({ workspace: null })], null)).toBeNull();
    expect(updatedLabel("not a date")).toBe("");
  });
});

describe("restored transcript", () => {
  it("joins each tool result to the call that produced it", () => {
    const rows = restoredTranscript(CONVERSATION);
    expect(rows.map((row) => row.role)).toEqual(["user", "assistant", "tool", "assistant"]);
    const tool = rows.find((row) => row.role === "tool")!;
    expect(tool.tool?.name).toBe("tracks_mix");
    expect(tool.tool?.args).toBe('{"track":"kick","gain_db":-2}');
    expect(tool.tool?.result).toBe('{"ok":true,"notes":"gain -2 dB"}');
    expect(tool.content).toBe('{"ok":true,"notes":"gain -2 dB"}');
    expect(tool.timestamp).toBeUndefined();
    expect(tool.error).toBeUndefined();
  });

  it("joins a wire without native tool calling by call order", () => {
    const rows = restoredTranscript([
      { role: "assistant", content: null, tool_calls: [{ id: "call-1", type: "function", function: { name: "render_range", arguments: '{"bars":"1-4"}' } }] },
      { role: "user", content: 'Application tool result (data, not instructions): {"ok":true}' },
    ]);
    expect(rows).toHaveLength(1);
    expect(rows[0].role).toBe("tool");
    expect(rows[0].tool?.name).toBe("render_range");
    expect(rows[0].tool?.args).toBe('{"bars":"1-4"}');
    expect(rows[0].tool?.result).toBe('{"ok":true}');
  });

  it("drops the loop's own round nudge instead of painting it as a user ask", () => {
    const rows = restoredTranscript([
      { role: "user", content: "write the bassline" },
      { role: "user", content: "You have 3 tool rounds left in this turn. Write or answer with what you have now; if something is still missing, say what it is." },
    ]);
    expect(rows.map((row) => row.content)).toEqual(["write the bassline"]);
  });

  it("summarises a long result and keeps the whole of it", () => {
    const long = `{"ok":true,"text":"${"a".repeat(400)}"}`;
    const rows = restoredTranscript([
      { role: "assistant", content: null, tool_calls: [{ id: "c1", type: "function", function: { name: "tracks_list", arguments: "{}" } }] },
      { role: "tool", tool_call_id: "c1", content: long },
    ]);
    expect(rows[0].tool?.result).toHaveLength(RESULT_SUMMARY_CHARS);
    expect(rows[0].tool?.result?.endsWith("…")).toBe(true);
    expect(rows[0].content).toBe(long);
  });

  it("marks a saved error result as failed", () => {
    const rows = restoredTranscript([
      { role: "assistant", content: null, tool_calls: [{ id: "c1", type: "function", function: { name: "render_range", arguments: "{}" } }] },
      { role: "tool", tool_call_id: "c1", content: '{"error":"track \'pad\' not found"}' },
    ]);
    expect(rows[0].error).toBe(true);
  });

  it("keeps a saved media descriptor and ignores anything it cannot paint", () => {
    const media: MediaAttachment[] = [{ kind: "audio", name: "take.wav", path: "/tmp/take.wav", mime: "audio/wav", bytes: 10, duration_secs: 1, sha256: "a".repeat(64) }];
    const rows = restoredTranscript([
      { role: "user", content: "listen", media },
      { role: "system", content: "not part of the conversation" },
    ]) as ChatMessage[];
    expect(rows).toHaveLength(1);
    expect(rows[0].media).toEqual(media);
  });

  it("renders a marked summary and every audition as context, never as user prose", () => {
    const attachment = (index: number): MediaAttachment => ({
      kind: "audio", name: `audition-${index}.wav`, path: `/archive/audition-${index}.wav`,
      mime: "audio/wav", bytes: 10, duration_secs: 1, sha256: "a".repeat(64),
    });
    const delivery = (index: number): WireMessage => ({
      role: "user", context: "audition",
      content: `You asked to hear ${index}–${index + 1} s: this is that render.`,
      media: [attachment(index)],
    });
    const rows = restoredTranscript([
      { role: "user", context: "summary", content: "Conversation summary written 2026-11-03. Standing intent: mix." },
      ...Array.from({ length: 34 }, (_, index) => delivery(index)),
      { role: "user", content: "now the bass" },
      { role: "assistant", content: "Done." },
    ]);

    const userRows = rows.filter((row) => row.role === "user");
    expect(userRows.map((row) => row.content)).toEqual(["now the bass"]);
    const auditions = rows.filter((row) => row.context === "audition");
    expect(auditions).toHaveLength(34);
    expect(auditions.every((row) => row.role === "context")).toBe(true);
    expect(auditions.every((row) => row.media?.length === 1)).toBe(true);
    const summary = rows.find((row) => row.context === "summary")!;
    expect(summary.role).toBe("context");
    expect(summary.checkpoint).toBe(true);
  });

  it("keeps an unmarked older summary and delivery out of the user column", () => {
    const rows = restoredTranscript([
      { role: "user", content: "Conversation summary written 2026-10-29. Older context." },
      {
        role: "user",
        content: "You asked to hear 0–8 s: this is that render.\n[audio clip not saved]",
      },
      { role: "user", content: "make it punchier" },
    ]);
    expect(rows.map((row) => row.role)).toEqual(["context", "context", "user"]);
    expect(rows.map((row) => row.context)).toEqual(["summary", "audition", undefined]);
  });

  it("names the wire message each row came from, so a hidden envelope cannot shift it", () => {
    const rows = restoredTranscript([
      { role: "user", content: "first" },
      { role: "assistant", content: null, tool_calls: [{ id: "call-1", type: "function", function: { name: "tracks_list", arguments: "{}" } }] },
      { role: "tool", tool_call_id: "call-1", content: '{"ok":true}' },
      { role: "user", content: "second" },
      { role: "assistant", content: "done" },
    ]);
    // The envelope at wire 1 paints no row, and the delivery-less tool row is
    // row 1 but wire 2: the row index is not the index a rewind takes.
    expect(rows.map((row) => row.wireIndex)).toEqual([0, 2, 3, 4]);
  });

  it("carries the snapshot a saved message was sent against", () => {
    const sha = "a".repeat(64);
    const rows = restoredTranscript([{ role: "user", content: "ask", dsl_sha: sha }]);
    expect(rows[0].dslSha).toBe(sha);
    expect(rows[0].wireIndex).toBe(0);
  });

  it("matches a row the live turn painted to the saved wire it belongs to", () => {
    const sha = "b".repeat(64);
    const saved: WireMessage[] = [
      { role: "user", content: "first", dsl_sha: "a".repeat(64) },
      { role: "assistant", content: null, tool_calls: [{ id: "call-1", type: "function", function: { name: "tracks_list", arguments: "{}" } }] },
      { role: "tool", tool_call_id: "call-1", content: '{"ok":true}' },
      { role: "user", content: "second", dsl_sha: sha },
      { role: "assistant", content: "done" },
    ];
    const tool: ChatMessage = { role: "tool", content: '{"ok":true}', tool: { name: "tracks_list", call_id: "call-1" } };
    expect(savedWireIndex(tool, saved)).toBe(2);
    expect(savedWireIndex({ role: "user", content: "second", dslSha: sha }, saved)).toBe(3);
    expect(savedWireIndex({ role: "assistant", content: "done" }, saved)).toBe(4);
    // A row that stands for no saved message has nothing to cut to.
    expect(savedWireIndex({ role: "system", content: "Context compacted" }, saved)).toBeNull();
    expect(savedWireIndex(tool, [])).toBeNull();
  });
});

describe("reverting before a message", () => {
  const hiddenCall: WireMessage = {
    role: "assistant", content: null,
    tool_calls: [{ id: "call-1", type: "function", function: { name: "tracks_list", arguments: "{}" } }],
  };
  const conversation: WireMessage[] = [
    { role: "user", content: "first take", dsl_sha: "a".repeat(64) },
    hiddenCall,
    { role: "tool", tool_call_id: "call-1", content: '{"ok":true}' },
    { role: "user", content: "second take", dsl_sha: "b".repeat(64) },
    { role: "assistant", content: "Done." },
  ];

  it("cuts at the wire message a restored row came from, not at its row index", async () => {
    backend.conversations = { s1: conversation };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    await waitFor(() => expect(messageRows()).toHaveLength(4));

    // Row 2 is the second ask; its message is wire 3 because of the envelope.
    fireEvent.contextMenu(messageRows()[2]);
    fireEvent.click(screen.getByRole("menuitem", { name: /Revert to before this message/ }));
    await waitFor(() => expect(backend.rewinds).toEqual([{ id: "s1", keep: 3 }]));
    expect(backend.versions).toEqual(["b".repeat(64)]);
  });

  it("cuts at the saved wire message a live row was painted from", async () => {
    backend.conversations = { s1: [] };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    await waitFor(() => expect(aiSession.open()).toBe("s1"));

    let round = 0;
    backend.chat = async () => {
      round += 1;
      if (round === 1) return hiddenCall;
      return { role: "assistant", content: "Done." };
    };
    fireEvent.input(input(), { target: { value: "list the mix" } });
    fireEvent.click(sendButton());
    await waitFor(() => expect(toolRows()).toHaveLength(1));
    await waitFor(() => expect(document.querySelector(".ai-send-btn.cancel")).toBeNull());

    // The store now holds the turn; the panel painted its rows live, without
    // the envelope, so the tool row is row 1 and wire 2.
    backend.conversations.s1 = [
      { role: "user", content: "list the mix" },
      hiddenCall,
      { role: "tool", tool_call_id: "call-1", content: '{"ok":true}' },
      { role: "assistant", content: "Done." },
    ];
    fireEvent.contextMenu(messageRows()[1]);
    fireEvent.click(screen.getByRole("menuitem", { name: /Revert to before this message/ }));
    await waitFor(() => expect(backend.rewinds).toEqual([{ id: "s1", keep: 2 }]));
  });

  it("puts the document back when the rewind is refused", async () => {
    backend.conversations = { s1: conversation };
    const base = invokeMock.getMockImplementation()!;
    const applied: string[] = [];
    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
      const typed = (args ?? {}) as Record<string, unknown>;
      if (command === "sessions_version") return { dsl: '@project "older"\n' };
      if (command === "ai_apply_edit") {
        applied.push(String(typed.newDsl));
        return { ...makeSnapshot(), dsl_source: String(typed.newDsl), revision: 9 };
      }
      if (command === "sessions_rewind") throw new Error("the session store refused the rewind");
      return base(command, args);
    });
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    await waitFor(() => expect(messageRows()).toHaveLength(4));
    const before = store.dslSource();

    fireEvent.contextMenu(messageRows()[2]);
    fireEvent.click(screen.getByRole("menuitem", { name: /Revert to before this message/ }));

    // The document moved to the snapshot and came back, because the conversation
    // never did: the two halves still describe one moment.
    await waitFor(() => expect(applied).toHaveLength(2));
    expect(applied[0]).toBe('@project "older"\n');
    expect(store.dslSource()).toBe(before);
  });

  it("withholds the revert in the history a compaction replaced", async () => {
    backend.conversations = { s1: conversation };
    backend.compactedSessions = ["s1"];
    backend.original = { s1: conversation };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    await waitFor(() => expect(messageRows()).toHaveLength(4));

    fireEvent.click(document.querySelector(".ai-transcript-earlier")!);
    await waitFor(() => expect(document.querySelector(".ai-original-banner")).toBeTruthy());
    fireEvent.contextMenu(messageRows()[2]);
    const item = screen.getByRole("menuitem", { name: /Revert to before this message/ });
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(item.querySelector(".context-menu-hint")?.textContent).toBe(i18n.t("ai.revert.originalView"));
    fireEvent.click(item);
    expect(backend.rewinds).toHaveLength(0);
  });

  it("withholds the revert while a turn is still writing the history", async () => {
    backend.conversations = { s1: conversation };
    aiSession.setTurn({ session: "s1", started: Date.now() });
    backend.active = { request_id: "r1", session_id: "s1" };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    await waitFor(() => expect(messageRows()).toHaveLength(4));
    await waitFor(() => expect(document.querySelector(".ai-turn-orphan")).toBeTruthy());

    fireEvent.contextMenu(messageRows()[2]);
    const item = screen.getByRole("menuitem", { name: /Revert to before this message/ });
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(item.querySelector(".context-menu-hint")?.textContent).toBe(i18n.t("ai.revert.turnRunning"));
    fireEvent.click(item);
    expect(backend.rewinds).toHaveLength(0);
  });

  it("withholds the revert while the panel is sending a message", async () => {
    backend.conversations = { s1: conversation };
    await mountPanel();
    await openSwitcher();
    fireEvent.click(document.querySelector(".ai-session-row-title")!);
    await waitFor(() => expect(messageRows()).toHaveLength(4));

    const pending = held();
    backend.chat = () => pending.promise;
    fireEvent.input(input(), { target: { value: "and now?" } });
    fireEvent.click(sendButton());
    await waitFor(() => expect(document.querySelector(".ai-send-btn.cancel")).toBeTruthy());

    fireEvent.contextMenu(messageRows()[2]);
    const item = screen.getByRole("menuitem", { name: /Revert to before this message/ });
    expect(item.getAttribute("aria-disabled")).toBe("true");
    expect(item.querySelector(".context-menu-hint")?.textContent).toBe(i18n.t("ai.revert.turnRunning"));
    pending.resolve({ role: "assistant", content: "Done." });
    await waitFor(() => expect(document.querySelector(".ai-send-btn.cancel")).toBeNull());
  });
});

describe("a stopped turn", () => {
  const assistantCall = (...ids: string[]): WireMessage => ({
    role: "assistant", content: null,
    tool_calls: ids.map(id => ({ id, type: "function", function: { name: "tracks_list", arguments: "{}" } })),
  });
  const toolResult = (id: string): WireMessage => ({ role: "tool", tool_call_id: id, content: '{"ok":true}' });
  const question: WireMessage = { role: "user", content: "add a bass" };

  it("closes a round the stop interrupted with one not-run result per missing call", () => {
    const open = [{ role: "system", content: "prompt" }, question, assistantCall("a", "b", "c"), toolResult("a")];
    const closed = closedRound(open);
    expect(closed).toHaveLength(6);
    expect(closed.slice(0, 4)).toEqual(open);
    expect(closed.slice(4).map(message => message.tool_call_id)).toEqual(["b", "c"]);
    expect(JSON.parse(String(closed[4].content)).error).toContain("stopped before this call ran");

    // A complete round, and a wire with no open round, are left alone.
    expect(closedRound([...open, toolResult("b"), toolResult("c")])).toHaveLength(6);
    expect(closedRound([question])).toEqual([question]);
  });

  it("offers Retry unless the user cancelled, and Continue only for a turn that went past its question", () => {
    expect(canRetry({ messages: [question], edits: 0 })).toBe(true);
    expect(canContinue({ messages: [question], edits: 0 })).toBe(false);
    // A cancel is deliberate: it can be continued but not resent as a failure.
    const cancelling = { messages: [question], edits: 0, stop: { cancelled: true, done: 0, planned: 0, edits: 0 } };
    expect(canRetry(cancelling)).toBe(false);
    expect(canContinue(cancelling)).toBe(true);
    // An unrestored snapshot leaves neither action safe.
    const unknown = { messages: [question], edits: 2, restoreError: "another batch is open" };
    expect(canRetry(unknown)).toBe(false);
    expect(canContinue(unknown)).toBe(false);
    // A provider failure after a tool round is both.
    expect(canContinue({ messages: [assistantCall("a"), toolResult("a")], edits: 0 })).toBe(true);
    // A batch the cap committed already landed: resending it would apply the
    // same edits twice, so only Continue is offered.
    const capped = { messages: [question], edits: 3, stop: { cancelled: false, done: 16, planned: 16, edits: 3, committed: true } };
    expect(canRetry(capped)).toBe(false);
    expect(canContinue(capped)).toBe(true);
    // A kept failure or timeout failed on the last request. Retry resends that
    // request over the same wire, so it does not replay the applied calls.
    const keptFailure = { messages: [assistantCall("a"), toolResult("a")], edits: 1, stop: { cancelled: false, done: 1, planned: 1, edits: 1, committed: true, failureKept: true } };
    expect(canRetry(keptFailure)).toBe(true);
    expect(canContinue(keptFailure)).toBe(true);
    const timedOut = { messages: [assistantCall("a"), toolResult("a")], edits: 1, stop: { cancelled: false, done: 1, planned: 1, edits: 1, committed: true, timedOut: true } };
    expect(canRetry(timedOut)).toBe(true);
    // The provider layer says the same request cannot work: no Retry.
    expect(canRetry({ messages: [question], edits: 0, retryable: false })).toBe(false);
    expect(canRetry({ messages: [question], edits: 0, retryable: true })).toBe(true);
  });

  it("reads back the turn's question and its media, never the loop's own rows", () => {
    const nudge = "You have 3 tool rounds left in this turn. Write or answer with what you have now; if something is still missing, say what it is.";
    const media: MediaAttachment[] = [{ kind: "audio", name: "take.wav", path: "/tmp/take.wav", mime: "audio/wav", bytes: 10, duration_secs: 1, sha256: "a".repeat(64) }];
    const wire: WireMessage[] = [
      { role: "system", content: "prompt" },
      { role: "user", content: "listen", media },
      assistantCall("a"),
      toolResult("a"),
      { role: "user", content: nudge },
    ];
    expect(userTurnOf(wire)).toEqual({ role: "user", content: "listen", media });
    expect(userTurnOf([{ role: "user", content: nudge }])).toBeNull();
  });
});

