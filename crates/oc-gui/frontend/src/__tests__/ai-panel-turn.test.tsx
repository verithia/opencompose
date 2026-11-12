/**
 * A turn in flight must never look like nothing happened: the transcript's last
 * row names the phase, the provider and model and the elapsed seconds, the
 * composer's one button stops that turn while the composer is empty, and the
 * composer clears on every send instead of leaving the draft behind.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack, openStepGroups, aiConfigWire } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { aiLimits } from "../stores/aiLimits";
import { aiSession } from "../stores/aiSession";
import { resetStats } from "../stores/aiStats";
import { i18n } from "../stores/i18n";
import { workspaceStore } from "../stores/workspace";
import { aiWorkspace } from "../stores/aiWorkspace";
import { aiConfig, getActiveProvider, hasUnsavedConfig } from "../ai/config";
import { elapsedLabel, stopStalled, STOP_STALLED_SECONDS } from "../ai/transcript";
import type { SessionMeta } from "../ai/sessions";

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

const TOOLS = [
  { name: "tracks_list", description: "List tracks and their mix", area: "project", mutation: false, inputSchema: {} },
  { name: "tracks_add", description: "Add a track", area: "tracks", mutation: true, inputSchema: {} },
  {
    name: "edit_subtree",
    description: "Propose one complete subtree replacement",
    area: "project",
    inputSchema: { type: "object", properties: { path: { type: "string" }, new_dsl: { type: "string" } }, required: ["path", "new_dsl"] },
  },
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

type ChatCall = { messages: { role: string; content?: unknown }[]; requestId?: string; [key: string]: unknown };

/** Everything a test can steer; the mock below only reads it. */
const backend = {
  chat: async (): Promise<unknown> => ({ role: "assistant", content: "Hello." }),
  chatCalls: [] as ChatCall[],
  tool: async (_call: { tool: string; args: Record<string, unknown> }): Promise<unknown> => ({ tracks: [] }),
  cancel: null as null | ((requestId: string) => void),
  /** The request ids each `ai_cancel` named, in order. */
  cancelIds: [] as string[],
  snapshot: makeSnapshot() as unknown,
  sessions: [] as SessionMeta[],
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
      case "ai_tools": return TOOLS;
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
      case "ai_chat": {
        backend.chatCalls.push({ ...typed, messages: JSON.parse(JSON.stringify(typed.messages)) } as ChatCall);
        return backend.chat();
      }
      case "ai_cancel": {
        backend.cancelIds.push(String(typed.requestId));
        if (backend.cancel) backend.cancel(String(typed.requestId));
        return undefined;
      }
      case "ai_call_tool": return backend.tool(typed as { tool: string; args: Record<string, unknown> });
      case "ai_validate_edit": return { valid: true };
      case "extract_subtree": return { dsl_text: "@project \"Test Project\"\n" };
      case "diff_dsl": return { old_text: "before", new_text: "after", hunks: [{ lines: [{ kind: "Added", text: "+ @project" }] }] };
      case "sessions_list": return backend.sessions.map((entry) => ({ ...entry }));
      case "sessions_new": {
        const meta = session({ id: `s${backend.sessions.length + 1}`, title: String(typed.title ?? "new") });
        backend.sessions = [meta, ...backend.sessions];
        return meta;
      }
      case "sessions_load": return { meta: session(), messages: [], skipped: 0 };
      case "workspace_open": return { path: String(typed.path ?? ""), name: "other", detached: false, projects: [] };
      case "workspaces_list": return [];
      default: return backend.snapshot;
    }
  });
}

const input = () => document.querySelector(".ai-input") as HTMLTextAreaElement;
const sendButton = () => document.querySelector(".ai-send-btn") as HTMLButtonElement;
/** The transcript's pending row, or null when no turn is in flight. */
const pendingRow = () => document.querySelector(".ai-messages > .ai-pending") as HTMLElement | null;
/** The composer's button in its cancel state, or null while it sends. */
const cancelButton = () => document.querySelector(".ai-send-btn.cancel") as HTMLButtonElement | null;
const phase = () => document.querySelector(".ai-thinking-label")?.textContent ?? "";
const queuedTexts = () => [...document.querySelectorAll(".ai-queue-text")].map((row) => row.textContent);
const retryButton = () => document.querySelector(".ai-turn-retry") as HTMLButtonElement | null;
const systemRows = () => [...document.querySelectorAll(".ai-message.system")];
const answers = () => [...document.querySelectorAll(".ai-message.assistant")];

async function mount() {
  await store.initProject();
  const view = render(() => <AiPanel />);
  await waitFor(() => expect(document.querySelector(".ai-save-notice")).toBeFalsy());
  await waitFor(() => expect(document.querySelector(".ai-toolset-chip")).toBeTruthy());
  await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("sessions_list", {}));
  return view;
}

async function typeAndSend(text: string) {
  fireEvent.input(input(), { target: { value: text } });
  fireEvent.click(sendButton());
}

/** Start a turn and keep it open, so the pending row has something to report. */
async function startTurn(text = "first") {
  await typeAndSend(text);
  await waitFor(() => expect(backend.chatCalls).toHaveLength(1));
  await waitFor(() => expect(pendingRow()).toBeTruthy());
}

/** A promise the test resolves when it wants the provider to answer. */
function held() {
  let resolve!: (value: unknown) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise((ok, fail) => { resolve = ok; reject = fail; });
  return { promise, resolve, reject };
}

beforeEach(() => {
  invokeMock.mockReset();
  backend.chat = async () => ({ role: "assistant", content: "Hello." });
  backend.chatCalls = [];
  backend.tool = async () => ({ tracks: [] });
  backend.cancel = null;
  backend.cancelIds = [];
  backend.snapshot = makeSnapshot();
  backend.sessions = [];
  i18n.setLocale("en");
  aiLimits.setActiveContext(null);
  resetStats();
  aiSession.reset();
  workspaceStore.reset();
  installBackend();
});

afterEach(() => {
  cleanup();
  aiSession.reset();
  workspaceStore.reset();
  aiLimits.setActiveContext(null);
  resetStats();
});

describe("the turn in flight", () => {
  it("is the transcript's last row, with the phase, the model, the clock and the way out", async () => {
    const pending = held();
    backend.chat = () => pending.promise;
    await mount();
    await typeAndSend("add a bass line");

    const row = pendingRow();
    expect(row).toBeTruthy();
    // The strip above the composer is gone; the row is the transcript's own.
    expect(document.querySelector(".ai-turn-status")).toBeNull();
    expect(document.querySelector(".ai-messages")!.lastElementChild).toBe(row);
    // One content cell spanning the row: the dots already say who is working,
    // so the waiting row carries no role cell of its own.
    expect(row!.querySelector(".ai-message-role")).toBeNull();
    expect(row!.querySelectorAll(".ai-message-content")).toHaveLength(1);
    expect(row!.querySelector(".ai-message-content .ai-thinking")).toBeTruthy();
    expect(phase()).toContain("Waiting for deepseek-v4-flash/deepseek-flash");
    expect(row!.querySelector(".ai-thinking-elapsed")?.textContent).toBe("0s");
    // The composer's own button is the way out while nothing is typed.
    expect(cancelButton()!.getAttribute("aria-label")).toBe("Cancel request");

    pending.resolve({ role: "assistant", content: "Done." });
    await waitFor(() => expect(pendingRow()).toBeNull());
    expect(answers().at(-1)?.textContent).toContain("Done.");
  });

  it("names the tool that is running", async () => {
    let turn = 0;
    const pending = held();
    backend.chat = async () => {
      turn += 1;
      if (turn === 1) {
        return {
          role: "assistant", content: null,
          tool_calls: [{ id: "call-1", type: "function", function: { name: "tracks_list", arguments: "{}" } }],
        };
      }
      return pending.promise;
    };
    const tool = held();
    backend.tool = () => tool.promise;
    await mount();
    await typeAndSend("describe the mix");

    await waitFor(() => expect(phase()).toBe("Running tracks_list"));
    tool.resolve({ tracks: [] });
    // The result landed, so the row goes back to waiting for the answer.
    await waitFor(() => expect(phase()).toContain("Waiting for deepseek-v4-flash/deepseek-flash"));

    pending.resolve({ role: "assistant", content: "Done." });
    await waitFor(() => expect(pendingRow()).toBeNull());
  });

  it("says it is stopping once Stop is pressed, and clears when the turn settles", async () => {
    const pending = held();
    const stop = held();
    backend.chat = () => pending.promise;
    backend.cancel = () => { void stop.promise.then(() => pending.reject({ kind: "cancelled", message: "Request cancelled" })); };
    await mount();
    await startTurn("long request");

    fireEvent.click(cancelButton()!);
    await waitFor(() => expect(phase()).toBe("Stopping…"));
    // The stop is still visible: the turn has not settled, so the row stays.
    expect(pendingRow()).toBeTruthy();

    stop.resolve(undefined);
    await waitFor(() => expect(pendingRow()).toBeNull());
    await waitFor(() => expect(input().disabled).toBe(false));
    expect(document.body.textContent).toContain("Cancelled before any edit was applied");
    expect(answers()).toHaveLength(0);
  });

  it("stops the active separator when the composer Stop button is clicked", async () => {
    const pending = held();
    const tool = { name: "separate_stems", description: "Separate stems", area: "analysis", mutation: false,
      inputSchema: { type: "object", properties: { path: { type: "string" } }, required: ["path"] } };
    const previous = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
      if (command === "ai_tools") return [...TOOLS, tool];
      if (command === "ai_toolsets") return [{ ...SETS[0], tools: [...SETS[0].tools, tool.name] }];
      if (command === "ai_cancel_separator") {
        pending.reject({ error: "separation cancelled" });
        return undefined;
      }
      return previous(command, args);
    });
    backend.chat = async () => ({ role: "assistant", content: null, tool_calls: [
      { id: "split", type: "function", function: { name: tool.name, arguments: '{"path":"/tmp/mix.wav"}' } },
    ] });
    backend.tool = () => pending.promise;
    await mount();
    await startTurn("Separate the reference.");
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("ai_call_tool", expect.objectContaining({ tool: tool.name, cancellationId: expect.any(String) })));
    const launched = invokeMock.mock.calls.find(([command, args]) => command === "ai_call_tool" && (args as {tool: string}).tool === tool.name)![1] as {cancellationId: string};
    fireEvent.click(cancelButton()!);
    await waitFor(() => expect(pendingRow()).toBeNull());
    expect(invokeMock).toHaveBeenCalledWith("ai_cancel_separator", { cancellationId: launched.cancellationId });
    expect(backend.chatCalls).toHaveLength(1);
  });

  it("keeps an answer whose commit outlived Stop instead of leaving a silent hole", async () => {
    // The turn had already answered and was committing its batch when Stop
    // arrived: dropping the answer would leave the question with no reply.
    const commit = held();
    let commitStarted!: () => void;
    const committing = new Promise<void>((resolve) => { commitStarted = resolve; });
    let round = 0;
    backend.chat = async () => {
      round += 1;
      return round === 1
        ? {
          role: "assistant", content: null,
          tool_calls: [{ id: "call-1", type: "function", function: { name: "tracks_add", arguments: JSON.stringify({ name: "bass" }) } }],
        }
        : { role: "assistant", content: "Added the bass track." };
    };
    backend.tool = async ({ tool }) => {
      if (tool === "transaction_commit") { commitStarted(); return commit.promise; }
      if (tool === "transaction_begin") return { active: true, label: "AI edit" };
      return { revision: 1 };
    };
    await mount();
    await typeAndSend("add a bass track");
    // Wait until the loop is parked inside the commit, then stop.
    await committing;
    fireEvent.click(cancelButton()!);
    await waitFor(() => expect(phase()).toBe("Stopping…"));
    commit.resolve({ committed: true });

    await waitFor(() => expect(pendingRow()).toBeNull());
    expect(answers().at(-1)?.textContent).toContain("Added the bass track.");
    expect(systemRows()).toHaveLength(0);
  });

  it("clears the composer on Send and gives the draft back when the credential read fails", async () => {
    backend.chat = async () => {
      throw { kind: "auth", message: "The API key could not be read from the keychain", retryable: true };
    };
    await mount();
    await typeAndSend("add a bass line");

    // The send cleared the composer at once, and the failure put the text back.
    await waitFor(() => expect(pendingRow()).toBeNull());
    await waitFor(() => expect(systemRows()).toHaveLength(1));
    expect(systemRows()[0].textContent).toContain("The API key could not be read");
    expect(retryButton()).toBeTruthy();
    expect(input().value).toBe("add a bass line");
    expect(pendingRow()).toBeNull();
    expect(answers()).toHaveLength(0);
  });

  it("paints an empty provider reply as a failure with Retry, not as an answer", async () => {
    backend.chat = async () => ({ role: "assistant", content: "   " });
    await mount();
    await typeAndSend("describe the mix");

    await waitFor(() => expect(pendingRow()).toBeNull());
    await waitFor(() => expect(systemRows()).toHaveLength(1));
    expect(systemRows()[0].textContent).toContain("Provider returned no text.");
    expect(retryButton()).toBeTruthy();
    expect(answers()).toHaveLength(0);
    expect(input().value).toBe("describe the mix");
  });

  it("abandons the turn when a new conversation starts, so no stale row lands", async () => {
    const pending = held();
    backend.chat = () => pending.promise;
    await mount();
    await startTurn("first");

    fireEvent.click(document.querySelector(".ai-session-new") as HTMLElement);
    await waitFor(() => expect(pendingRow()).toBeNull());
    expect(document.querySelectorAll(".ai-message")).toHaveLength(0);
    expect(input().disabled).toBe(false);

    // The provider answers a turn nobody is watching: nothing is painted.
    pending.resolve({ role: "assistant", content: "Late answer." });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(document.body.textContent).not.toContain("Late answer.");
    expect(answers()).toHaveLength(0);
    expect(backend.chatCalls).toHaveLength(1);
  });

  it("stops the turn when the workspace changes and drops its late rows", async () => {
    const pending = held();
    backend.chat = () => pending.promise;
    await mount();
    await startTurn("first");

    await workspaceStore.open("/Users/me/other");
    await waitFor(() => expect(pendingRow()).toBeNull());
    expect(document.querySelectorAll(".ai-message")).toHaveLength(0);
    expect(input().disabled).toBe(false);

    pending.resolve({ role: "assistant", content: "Late answer." });
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(document.body.textContent).not.toContain("Late answer.");
  });

  it("never resumes a superseded turn's tool dispatch", async () => {
    // The abandoned turn asks for a read the test holds, then for a mutating
    // call. The fence has to hold between the two.
    const read = held();
    const dispatched: string[] = [];
    backend.tool = async (call) => {
      dispatched.push(call.tool);
      return call.tool === "tracks_list" ? read.promise : { ok: true };
    };
    let turns = 0;
    backend.chat = async () => {
      turns += 1;
      if (turns > 1) return { role: "assistant", content: "Second answer." };
      return {
        role: "assistant", content: null,
        tool_calls: [
          { id: "call-1", type: "function", function: { name: "tracks_list", arguments: "{}" } },
          { id: "call-2", type: "function", function: { name: "tracks_add", arguments: JSON.stringify({ name: "lead" }) } },
        ],
      };
    };
    await mount();
    await typeAndSend("first");
    await waitFor(() => expect(dispatched).toEqual(["tracks_list"]));

    // Another workspace starts a new turn. Its own progress must not revive the
    // token the panel abandoned, even though the abandoned request is uncancelled.
    await workspaceStore.open("/Users/me/other");
    await waitFor(() => expect(pendingRow()).toBeNull());
    await typeAndSend("second");
    await waitFor(() => expect(answers().at(-1)?.textContent).toContain("Second answer."));

    read.resolve({ tracks: [] });
    await new Promise((resolve) => setTimeout(resolve, 30));
    expect(dispatched).toEqual(["tracks_list"]);
    expect(document.body.textContent).not.toContain("lead");
  });

  it("does not apply a replacement the window left waiting for approval", async () => {
    backend.chat = async () => ({
      role: "assistant", content: null,
      tool_calls: [{
        id: "call-1", type: "function",
        function: { name: "edit_subtree", arguments: JSON.stringify({ path: "project", new_dsl: "@project \"Test Project\"" }) },
      }],
    });
    const first = await mount();
    // The stored mode is require_approval, so the turn pauses on the card.
    fireEvent.click(document.querySelector('[data-permission="require_approval"]') as HTMLElement);
    await typeAndSend("tighten the intro");
    await waitFor(() => expect(document.querySelector(".ai-approval-card")).toBeTruthy());
    const applied = () => invokeMock.mock.calls.some(([command]) => command === "ai_apply_edit");
    expect(applied()).toBe(false);
    // The window goes away while the card waits: the turn is cancelled, and
    // nothing is applied without an answer.
    first.unmount();
    await waitFor(() => expect(applied()).toBe(false));

    await mount();
    // A remounted panel does not pretend the decision is still open.
    await waitFor(() => expect(document.querySelector(".ai-approval-card")).toBeFalsy());
    expect(applied()).toBe(false);
  });

  it("keeps a pasted multi-line draft and does not send on Shift+Enter", async () => {
    await mount();
    fireEvent.input(input(), { target: { value: "line one\nline two" } });
    expect(input().tagName).toBe("TEXTAREA");
    expect(input().value).toBe("line one\nline two");
    fireEvent.keyDown(input(), { key: "Enter", shiftKey: true });
    expect(backend.chatCalls).toHaveLength(0);
    expect(input().value).toBe("line one\nline two");

    fireEvent.keyDown(input(), { key: "Enter" });
    await waitFor(() => expect(backend.chatCalls).toHaveLength(1));
    expect(backend.chatCalls[0].messages.at(-1)?.content).toBe("line one\nline two");
    expect(input().value).toBe("");
  });
});

describe("where the user's message sits", () => {
  it("is the right-aligned block with no role label, while the answer keeps its own", async () => {
    const pending = held();
    backend.chat = () => pending.promise;
    await mount();
    await typeAndSend("add a bass line");
    await waitFor(() => expect(document.querySelector(".ai-message.user")).toBeTruthy());

    // The modifier class the transcript's stylesheet aligns and caps.
    const user = document.querySelector(".ai-message.user") as HTMLElement;
    expect(user.classList.contains("user")).toBe(true);
    expect(user.querySelectorAll(".ai-message-content")).toHaveLength(1);
    expect(user.querySelector(".ai-message-content")?.textContent).toBe("add a bass line");
    expect(user.querySelector(".ai-message-role")).toBeNull();

    pending.resolve({ role: "assistant", content: "Done." });
    await waitFor(() => expect(pendingRow()).toBeNull());
    // Ordinary messages are untouched: the answer still names its role.
    expect(answers().at(-1)?.querySelector(".ai-message-role")?.textContent).toBe("Assistant");
  });
});

describe("the composer's one button", () => {
  it("is Cancel request on an empty composer during a turn, and stops that turn", async () => {
    const pending = held();
    backend.chat = () => pending.promise;
    await mount();
    await typeAndSend("long request");
    await waitFor(() => expect(backend.chatCalls).toHaveLength(1));
    await waitFor(() => expect(pendingRow()).toBeTruthy());

    const button = cancelButton()!;
    expect(button.getAttribute("aria-label")).toBe("Cancel request");
    expect(button.textContent).toBe("\u25a0");
    expect(button.disabled).toBe(false);

    fireEvent.click(button);
    // The same cancellation the strip's button used: `ai_cancel`, naming the
    // request this turn started.
    await waitFor(() => expect(backend.cancelIds).toHaveLength(1));
    expect(backend.cancelIds[0]).toBe(backend.chatCalls[0].requestId);
    expect(invokeMock.mock.calls.some(([command]) => command === "ai_cancel")).toBe(true);
    await waitFor(() => expect(phase()).toBe("Stopping\u2026"));

    pending.reject({ kind: "cancelled", message: "Request cancelled" });
    await waitFor(() => expect(pendingRow()).toBeNull());
  });

  it("is Send when text is typed during a turn, and queues that text", async () => {
    const pending = held();
    backend.chat = () => pending.promise;
    await mount();
    await typeAndSend("first");
    await waitFor(() => expect(pendingRow()).toBeTruthy());

    fireEvent.input(input(), { target: { value: "while it runs" } });
    expect(cancelButton()).toBeNull();
    expect(sendButton().getAttribute("aria-label")).toBe("Send");
    expect(sendButton().disabled).toBe(false);

    fireEvent.click(sendButton());
    await waitFor(() => expect(queuedTexts()).toEqual(["while it runs"]));
    // A typed message never stops the turn it queues behind.
    expect(backend.cancelIds).toEqual([]);

    pending.resolve({ role: "assistant", content: "Done." });
    await waitFor(() => expect(pendingRow()).toBeNull());
  });

  it("is a disabled Send on an empty composer with no turn running", async () => {
    await mount();
    expect(sendButton().getAttribute("aria-label")).toBe("Send");
    expect(sendButton().disabled).toBe(true);

    await typeAndSend("hello");
    await waitFor(() => expect(backend.chatCalls).toHaveLength(1));
  });

  it("takes both labels from the locale table", async () => {
    const pending = held();
    backend.chat = () => pending.promise;
    await mount();
    const englishSend = i18n.t("ai.input.send");
    expect(sendButton().getAttribute("aria-label")).toBe(englishSend);

    await typeAndSend("first");
    await waitFor(() => expect(pendingRow()).toBeTruthy());
    const englishCancel = i18n.t("ai.turn.stop");
    expect(sendButton().getAttribute("aria-label")).toBe(englishCancel);

    i18n.setLocale("zh-Hans");
    await waitFor(() => expect(sendButton().getAttribute("aria-label")).toBe(i18n.t("ai.turn.stop")));
    expect(i18n.t("ai.turn.stop")).not.toBe(englishCancel);
    expect(i18n.t("ai.input.send")).not.toBe(englishSend);

    i18n.setLocale("en");
    expect(sendButton().getAttribute("aria-label")).toBe(englishCancel);
    pending.resolve({ role: "assistant", content: "Done." });
    await waitFor(() => expect(sendButton().disabled).toBe(true));
  });

  it("scrolls the transcript to the pending row when the turn starts", async () => {
    const pending = held();
    backend.chat = () => pending.promise;
    await mount();
    const list = document.querySelector(".ai-messages") as HTMLElement;
    // A transcript taller than its viewport, as a real conversation is.
    Object.defineProperty(list, "scrollHeight", { configurable: true, get: () => 1234 });
    Object.defineProperty(list, "clientHeight", { configurable: true, value: 400 });

    await typeAndSend("first");
    await waitFor(() => expect(pendingRow()).toBeTruthy());
    // The follow behaviour scrolls to the bottom, and the row is the bottom.
    await waitFor(() => expect(list.scrollTop).toBe(1234));
    expect(list.lastElementChild).toBe(pendingRow());

    pending.resolve({ role: "assistant", content: "Done." });
    await waitFor(() => expect(pendingRow()).toBeNull());
  });
});

describe("the pending row's clock", () => {
  it("counts seconds and minutes without inventing a negative time", () => {
    expect(elapsedLabel(0)).toBe("0s");
    expect(elapsedLabel(9.9)).toBe("9s");
    expect(elapsedLabel(60)).toBe("1:00");
    expect(elapsedLabel(75)).toBe("1:15");
    expect(elapsedLabel(-4)).toBe("0s");
  });

  it("only calls a stop stalled once the provider has had time to answer", () => {
    expect(stopStalled(STOP_STALLED_SECONDS - 0.1)).toBe(false);
    expect(stopStalled(STOP_STALLED_SECONDS)).toBe(true);
    expect(STOP_STALLED_SECONDS).toBe(10);
  });
});


describe("native edit snapshots", () => {
  it("hydrates tempo/name/dirty after native edits and commit", async () => {
    await mount();
    let round = 0;
    backend.chat = async () => ++round === 1
      ? {role:"assistant",content:null,tool_calls:[{id:"edit",type:"function",function:{name:"tracks_add",arguments:'{"name":"piano"}'}}]}
      : {role:"assistant",content:"Done."};
    backend.tool = async ({tool}) => {
      if (tool === "tracks_add") {
        const snap = makeSnapshot();
        snap.project.name = "Evening Sketch";
        snap.project.properties.tempo = 76;
        snap.dirty = true;
        backend.snapshot = snap;
      }
      return {revision:2};
    };
    await typeAndSend("compose piano");
    await waitFor(() => expect(store.project.name).toBe("Evening Sketch"));
    await waitFor(() => expect(pendingRow()).toBeNull());
    expect(store.project.properties.tempo).toBe(76);
    expect(store.dirty()).toBe(true);
    expect(invokeMock).toHaveBeenCalledWith("get_undo_state");
  });

  it("keeps the edited project when a turn fails", async () => {
    await mount();
    let round = 0;
    backend.chat = async () => {
      if (++round === 1) return {role:"assistant",content:null,tool_calls:[{id:"edit",type:"function",function:{name:"tracks_add",arguments:'{"name":"piano"}'}}]};
      throw new Error("provider offline");
    };
    backend.tool = async ({tool}) => {
      if (tool === "tracks_add") {
        const snap = makeSnapshot(); snap.project.name = "Transient edit"; backend.snapshot = snap;
      }
      return {revision:2};
    };
    await typeAndSend("compose piano");
    await waitFor(() => expect(invokeMock).toHaveBeenCalledWith("ai_call_tool",expect.objectContaining({tool:"transaction_commit"})));
    await waitFor(() => expect(pendingRow()).toBeNull());
    expect(store.project.name).toBe("Transient edit");
  });
});


describe("provider reasoning channel", () => {
  it("shows live thinking and tool preparation inside the same step", async () => {
    let finish!: (reply: unknown) => void;
    backend.chat = async () => {
      const call = backend.chatCalls.at(-1)!;
      (call.onThinking as {onmessage: (text: string) => void}).onmessage("Inspect the available drum samples.");
      (call.onPreparing as {onmessage: (text: string) => void}).onmessage("sample_search");
      return new Promise(resolve => { finish = resolve; });
    };
    const view = await mount();
    await typeAndSend("find dry drums");
    await waitFor(() => expect(view.container.querySelector(".ai-step-group .ai-thinking-details")).not.toBeNull());
    expect(view.container.querySelector(".ai-step-group")?.textContent).toContain("Preparing sample search");
    expect(view.container.querySelectorAll(".ai-pending")).toHaveLength(1);
    const details = view.container.querySelector(".ai-step-group .ai-thinking-details") as HTMLDetailsElement;
    fireEvent.click(details.querySelector("summary")!);
    expect(details.open).toBe(true);
    const call = backend.chatCalls.at(-1)!;
    (call.onThinking as {onmessage: (text: string) => void}).onmessage(" Check the license.");
    await waitFor(() => expect(details.textContent).toContain("Check the license."));
    expect(view.container.querySelector(".ai-step-group .ai-thinking-details")).toBe(details);
    expect(details.open).toBe(true);
    finish({role: "assistant", content: "Ready."});
    await waitFor(() => expect(pendingRow()).toBeNull());
    openStepGroups(view.container);
    expect(view.container.querySelector(".ai-step-group")?.textContent).toContain("Inspect the available drum samples.");
  });

  it("keeps supplied reasoning separate from answer and exposes full details", async () => {
    backend.chat = async () => {
      const channel = backend.chatCalls.at(-1)!.onThinking as {onmessage: (text: string) => void};
      channel.onmessage("Read the piano score first. ");
      channel.onmessage("Then inspect its phrasing.");
      return {role:"assistant",content:"The piano is ready."};
    };
    const view = await mount();
    await typeAndSend("review piano");
    await waitFor(() => expect(view.container.querySelector(".ai-step-group")).not.toBeNull());
    openStepGroups(view.container);
    await waitFor(() => expect(view.container.querySelector(".ai-messages .ai-thinking-details")).not.toBeNull());
    await waitFor(() => expect(pendingRow()).toBeNull());
    openStepGroups(view.container);
    const details = view.container.querySelector(".ai-messages .ai-thinking-details") as HTMLDetailsElement;
    expect(details.open).toBe(false);
    expect(details.querySelector(".ai-thinking-content")?.textContent).toContain("Read the piano score first. Then inspect its phrasing.");
    fireEvent.click(details.querySelector("summary")!);
    expect(details.open).toBe(true);
    expect(view.container.querySelectorAll(".ai-message-content")[1].textContent).toBe("");
    expect(answers().at(-1)?.textContent).toContain("The piano is ready.");
  });
});

describe("a reply the store did not keep", () => {
  const WARNING = "The reply could not be saved; it will be lost when the window closes. Session s1: no session 's1'";

  it("shows the warning under the answer and offers only a retry", async () => {
    backend.chat = async () => ({ role: "assistant", content: "The bass line is in.", session_warning: WARNING });
    await mount();
    await typeAndSend("add a bass line");
    await waitFor(() => expect(pendingRow()).toBeNull());

    expect(answers()).toHaveLength(1);
    expect(document.querySelector(".ai-session-warning")?.textContent).toBe(WARNING);
    expect(retryButton()).toBeTruthy();
    // A finished turn cannot be continued, only sent again to be saved.
    expect(document.querySelector(".ai-turn-continue")).toBeFalsy();
  });

  it("replaces the unsaved answer on retry instead of stacking a second one", async () => {
    let turn = 0;
    backend.chat = async () => {
      turn += 1;
      return turn === 1
        ? { role: "assistant", content: "The bass line is in.", session_warning: WARNING }
        : { role: "assistant", content: "The bass line is in." };
    };
    await mount();
    await typeAndSend("add a bass line");
    await waitFor(() => expect(pendingRow()).toBeNull());
    expect(answers()).toHaveLength(1);

    fireEvent.click(retryButton()!);
    await waitFor(() => expect(backend.chatCalls).toHaveLength(2));
    await waitFor(() => expect(pendingRow()).toBeNull());

    expect(answers()).toHaveLength(1);
    expect(answers()[0].textContent).toContain("The bass line is in.");
    expect(document.querySelector(".ai-session-warning")).toBeFalsy();
  });
});

describe("the edit target", () => {
  it("falls back to the whole project when another document opens in the same folder", async () => {
    backend.snapshot = { ...makeSnapshot(), file_path: "/Users/me/songs/one.ocp" };
    await mount();
    aiWorkspace.setTargetPath("track:lead");
    expect(aiWorkspace.targetPath()).toBe("track:lead");

    // Same directory, another song: the track the target names is not in it.
    backend.snapshot = { ...makeSnapshot(), file_path: "/Users/me/songs/two.ocp" };
    await store.initProject();
    await waitFor(() => expect(aiWorkspace.targetPath()).toBe("project"));
  });

  it("drops a saved track target the open project does not have", async () => {
    backend.snapshot = {
      ...makeSnapshot({ tracks: [makeTrack("lead", 0)] }),
      file_path: "/Users/me/songs/one.ocp",
    };
    aiWorkspace.setTargetPath("track:lead");
    await mount();
    // The track exists here, so the restored target still addresses it.
    expect(aiWorkspace.targetPath()).toBe("track:lead");

    aiWorkspace.setTargetPath("track:ghost");
    cleanup();
    await mount();
    // A remounted panel checks the target against the document it now shows.
    await waitFor(() => expect(aiWorkspace.targetPath()).toBe("project"));
  });
});

describe("an unsaved provider draft", () => {
  it("is written before the turn leaves, so the request names the model on screen", async () => {
    await mount();
    aiConfig.updateProvider(getActiveProvider().id, { model: "deepseek-v4-pro" });
    expect(hasUnsavedConfig()).toBe(true);

    await typeAndSend("use the model I picked");
    await waitFor(() => expect(backend.chatCalls).toHaveLength(1));

    const commands = invokeMock.mock.calls.map(([command]) => String(command));
    const saved = commands.lastIndexOf("save_ai_config");
    expect(saved).toBeGreaterThanOrEqual(0);
    // The write precedes the request it applies to.
    expect(saved).toBeLessThan(commands.indexOf("ai_chat"));
    expect(hasUnsavedConfig()).toBe(false);
    expect(input().value).toBe("");
  });

  it("keeps the message when the draft cannot be written", async () => {
    await mount();
    aiConfig.updateProvider(getActiveProvider().id, { model: "deepseek-v4-pro" });
    const base = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, args?: unknown) => {
      if (command === "save_ai_config") throw new Error("disk full");
      return base(command, args);
    });

    await typeAndSend("do not lose this");
    await new Promise((resolve) => setTimeout(resolve, 20));

    // Nothing went out under a model the backend never stored, and the message
    // is still in the composer for the retry.
    expect(backend.chatCalls).toHaveLength(0);
    expect(input().value).toBe("do not lose this");
    expect(document.querySelector(".ai-save-notice")?.textContent).toContain("Settings could not be saved");
  });
});
