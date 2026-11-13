/**
 * Interaction matrix for the AI panel surface: every header control, the model
 * row under the composer, the toolset chips, the composer and the transcript
 * actions. Each test drives the
 * control and checks both the disabled state and the effect of the click, so a
 * dead button is a failing test instead of a report from the operator.
 *
 * Rows that belong to the settings modal live in ai-settings-matrix.test.tsx.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, makeTrack, openDialogMock, aiConfigWire } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { PreferencesHost } from "../components/PreferencesModal";
import { TooltipLayer, placeTooltip } from "../components/Tooltip";
import { aiConfig, getActiveProvider, hasUnsavedConfig } from "../ai/config";
import { aiLimits } from "../stores/aiLimits";
import { resetStats } from "../stores/aiStats";
import { aiSession } from "../stores/aiSession";
import { preferencesUi } from "../stores/preferencesUi";
import { i18n, type I18nKey } from "../stores/i18n";
import { approvalEffect } from "../ai/approval";
import { allowedTools, clearAllowedTools } from "../ai/approvalMemory";
import { answerFakeTurn, cancelFakeTurn, resetFakeTurns, runFakeTurn, type TurnStartArgs } from "./helpers/fakeTurn";

// jsdom in this suite has no storage backend; the approval memory only needs
// get/set/remove.
const memoryStorage = new Map<string, string>();
if (!window.localStorage) {
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: {
      getItem: (key: string) => memoryStorage.get(key) ?? null,
      setItem: (key: string, value: string) => { memoryStorage.set(key, String(value)); },
      removeItem: (key: string) => { memoryStorage.delete(key); },
      clear: () => { memoryStorage.clear(); },
    },
  });
}

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
        { id: "deepseek-v4-pro", name: "DeepSeek V4 Pro", context: 500000, max_output: 4096, capabilities: CAPABILITIES, status: "active" },
      ],
    },
    {
      id: "google",
      name: "Google",
      kind: "gemini_interactions",
      hosts: ["generativelanguage.googleapis.com"],
      auth_providers: [],
      models: [
        {
          id: "gemini-4-argon",
          name: "Gemini 4 Argon",
          context: 200000,
          max_output: 32000,
          thinking_levels: ["low", "medium", "high"],
          capabilities: { ...CAPABILITIES, audio_in: true },
          status: "active",
        },
      ],
    },
  ],
};

const SETS = [
  { id: "dsl-edit", title: "DSL edit", description: "Read a subtree and propose one replacement", tools: ["edit_subtree"], default_on: true },
  { id: "project-edit", title: "Project edit", description: "Tracks, presets and buses", tools: ["tracks_list"], default_on: true },
  { id: "listen", title: "Listen", description: "Render a range and measure it", tools: ["render_range"], default_on: true },
  { id: "plugins", title: "Plugins", description: "Search the plugin catalog", tools: ["plugin_search"], default_on: true },
  { id: "transport", title: "Transport", description: "Move and query the playhead", tools: ["transport_play"], default_on: false },
];

const TOOLS = [
  { name: "edit_subtree", description: "Propose one complete subtree replacement", area: "project", inputSchema: {} },
  { name: "tracks_list", description: "List tracks and their mix", area: "project", inputSchema: {} },
  { name: "tracks_remove", description: "Remove a track", area: "tracks", mutation: true, approval: "confirm", inputSchema: {} },
  { name: "transaction_rollback", description: "Discard this turn's edits", area: "transaction", mutation: false, approval: "confirm", inputSchema: {} },
  { name: "render_range", description: "Render a range", area: "audio", inputSchema: {} },
  { name: "plugin_search", description: "Search the plugin catalog", area: "plugins", inputSchema: {} },
  { name: "transport_play", description: "Start playback", area: "transport", inputSchema: {} },
];

const ATTACHMENT = {
  kind: "audio",
  name: "take.wav",
  path: "/tmp/take.wav",
  mime: "audio/wav",
  bytes: 320000,
  duration_secs: 10,
  sha256: "a".repeat(64),
};

const SECOND_ATTACHMENT = { ...ATTACHMENT, name: "take-2.wav", path: "/tmp/take-2.wav", sha256: "b".repeat(64) };

type Pending = { promise: Promise<unknown>; resolve: (value: unknown) => void; reject: (reason?: unknown) => void };

function deferred<T = unknown>(): Pending & { promise: Promise<T> } {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((ok, fail) => { resolve = ok; reject = fail; });
  return { promise, resolve: resolve as (value: unknown) => void, reject };
}

/** Everything a test can steer; the mock below only reads it. */
const backend = {
  load: async () => aiConfigWire(),
  chat: async (): Promise<unknown> => ({ role: "assistant", content: "Hello." }),
  chatCalls: [] as Record<string, unknown>[],
  cancel: null as null | ((requestId: string) => void),
  keyStatus: async () => true,
  attach: async (): Promise<unknown> => ATTACHMENT,
  attachCalls: 0,
  validate: async () => ({ valid: true }),
  apply: async (): Promise<unknown> => makeSnapshot(),
  snapshot: null as unknown,
  tool: async (_call: { tool: string; args: Record<string, unknown> }): Promise<unknown> => ({ ok: true }),
  dispatched: [] as string[],
  toolsetReads: [] as Record<string, unknown>[],
  limits: { provider_id: "x", model_id: "y", bundled: null, overridden: null },
};

function installBackend() {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const typed = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "ai_batch_recover": return null;
      case "load_ai_config": return backend.load();
      case "save_ai_config": return { path: "/tmp/ai-providers.yaml", warnings: [] };
      case "ai_key_status": return backend.keyStatus();
      case "ai_catalog": return CATALOG;
      case "ai_model_limits": return backend.limits;
      case "ai_toolsets": return SETS;
      case "ai_tools": backend.toolsetReads.push(typed); return TOOLS;
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
      case "ai_turn_start": return runFakeTurn(typed as unknown as TurnStartArgs, {
        chat: (round) => {
          backend.chatCalls.push({ ...round, messages: JSON.parse(JSON.stringify(round.messages)) });
          return backend.chat();
        },
        tool: (call) => { backend.dispatched.push(call.tool); return backend.tool(call); },
        ask: (call) => call.tool === "edit_subtree" || call.tool === "tracks_remove" || call.tool === "transaction_rollback",
      });
      case "ai_turn_answer": return answerFakeTurn(String(typed.requestId), String(typed.callId), typed.decision as never);
      case "ai_turn_questions": return [];
      case "ai_cancel": {
        cancelFakeTurn(String(typed.requestId));
        backend.cancel?.(typed.requestId as string);
        return undefined;
      }
      case "ai_attach_media": backend.attachCalls += 1; return backend.attach();
      case "ai_validate_edit": return backend.validate();
      case "extract_subtree": return { dsl_text: "track lead\n" };
      case "diff_dsl": return { old_text: "a", new_text: "b", hunks: [{ lines: [{ kind: "Added", text: "b" }] }] };
      case "sessions_new": return {id:"checkpoint-session",title:"Test conversation"};
      case "sessions_set_permission":
        return {id: typed.id, title: "Test conversation", permission: typed.permission};
      case "get_project": return backend.snapshot ?? makeSnapshot();
      default: return makeSnapshot();
    }
  });
}

async function mount(options: { onClose?: () => void } = {}) {
  await store.initProject();
  const view = render(() => <><AiPanel onClose={options.onClose} /><PreferencesHost /><TooltipLayer /></>);
  await waitFor(() => expect(view.container.querySelector(".ai-save-notice")).toBeFalsy());
  await waitFor(() => expect(view.container.querySelector(".ai-toolset-chip")).toBeTruthy());
  await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "ai_tools")).toBe(true));
  return view;
}

const input = () => document.querySelector(".ai-input") as HTMLInputElement;
const sendButton = () => document.querySelector(".ai-send-btn") as HTMLButtonElement;
const attachButton = () => document.querySelector(".ai-attach-btn") as HTMLButtonElement;
const modelTrigger = () => document.querySelector(".ai-model-picker .oc-dropdown-trigger") as HTMLElement;
const effortTrigger = () => document.querySelector(".ai-model-effort .oc-dropdown-trigger") as HTMLElement | null;
const settingsButton = () => screen.getByRole("button", { name: "Provider settings" });
const dialog = () => screen.queryByRole("dialog", { name: /AI settings|Preferences/ });
const sendCalls = () => invokeMock.mock.calls.filter(([command]) => command === "ai_build_context").length;
const chatCalls = () => invokeMock.mock.calls.filter(([command]) => command === "ai_turn_start").length;

function optionLabels(listbox: HTMLElement): string[] {
  return [...listbox.querySelectorAll('[role="option"]')]
    .map((option) => option.querySelector(".oc-dropdown-option-label")?.textContent ?? "");
}

async function pickFromTrigger(trigger: HTMLElement, matcher: string) {
  fireEvent.click(trigger);
  const listbox = await screen.findByRole("listbox");
  const option = [...listbox.querySelectorAll('[role="option"]')]
    .find((entry) => entry.textContent?.includes(matcher)) as HTMLElement | undefined;
  if (!option) throw new Error(`no option ${matcher} in ${optionLabels(listbox).join(", ")}`);
  fireEvent.click(option);
}

async function type(text: string) {
  fireEvent.input(input(), { target: { value: text } });
}

async function send(text: string) {
  await type(text);
  fireEvent.click(sendButton());
}

/** A tool call that makes the agent hand back one proposal. */
function proposalReply(path = "track:lead") {
  return {
    role: "assistant",
    content: null,
    tool_calls: [{
      id: "call-1",
      type: "function",
      function: { name: "edit_subtree", arguments: JSON.stringify({ path, new_dsl: "track lead\n  instrument: piano\n" }) },
    }],
  };
}

/** The segmented DSL-permission control in the session bar. */
const permissionOption = (mode: string) =>
  document.querySelector(`.ai-permission-option[data-permission="${mode}"]`) as HTMLButtonElement | null;

/** Chooses a session mode before a turn runs; with no open session it only
 * changes what the turn's first session will store. */
async function choosePermission(mode: "require_approval" | "autoreview" | "write") {
  const option = permissionOption(mode);
  expect(option, `permission option ${mode}`).toBeTruthy();
  fireEvent.click(option!);
  await waitFor(() => expect(option!.getAttribute("aria-pressed")).toBe("true"));
}

/** The approval card the loop waits on, and its two answers. */
async function approvalCard(): Promise<HTMLElement> {
  await waitFor(() => expect(document.querySelector(".ai-approval-card")).toBeTruthy());
  return document.querySelector(".ai-approval-card") as HTMLElement;
}

beforeEach(() => {
  window.localStorage.clear();
  preferencesUi.close();
  invokeMock.mockReset();
  backend.load = async () => aiConfigWire();
  backend.chat = async () => ({ role: "assistant", content: "Hello." });
  backend.chatCalls = [];
  backend.cancel = null;
  backend.keyStatus = async () => true;
  backend.attach = async () => ATTACHMENT;
  backend.attachCalls = 0;
  backend.validate = async () => ({ valid: true });
  backend.apply = async () => makeSnapshot();
  backend.snapshot = null;
  backend.tool = async () => ({ ok: true });
  backend.dispatched = [];
  backend.toolsetReads = [];
  backend.limits = { provider_id: "x", model_id: "y", bundled: null, overridden: null };
  i18n.setLocale("en");
  aiLimits.setActiveContext(null);
  resetStats();
  // The draft and its chips live in the store now, so one test cannot leak
  // into the next one's composer.
  aiSession.reset();
  // A tool a test allowed must not silence the next test's fresh conversation.
  clearAllowedTools();
  openDialogMock.mockReset();
  installBackend();
});

afterEach(() => {
  cleanup();
  resetFakeTurns();
  aiSession.reset();
  aiLimits.setActiveContext(null);
  resetStats();
});

// ── Header ─────────────────────────────────────────────────────────────────

describe("model selector", () => {
  it("never renders blank and picking the current model leaves sending possible", async () => {
    await mount();
    expect(modelTrigger().textContent).toContain("deepseek-flash");
    expect(modelTrigger().getAttribute("data-model-known")).toBe("true");

    await pickFromTrigger(modelTrigger(), "deepseek-flash");
    expect(getActiveProvider().model).toBe("deepseek-flash");
    // A no-op pick must not invent an unsaved edit that disables Send.
    expect(document.querySelector(".ai-save-notice")).toBeFalsy();
    await type("hello");
    expect(sendButton().disabled).toBe(false);
  });

  it("moves limits with another model of the same provider and saves the pick itself", async () => {
    await mount();
    const saves = () => invokeMock.mock.calls.filter(([command]) => command === "save_ai_config").length;
    const savesBefore = saves();
    await pickFromTrigger(modelTrigger(), "deepseek-v4-pro");
    await waitFor(() => expect(getActiveProvider().model).toBe("deepseek-v4-pro"));
    expect(getActiveProvider().contextLength).toBe(500000);
    expect(getActiveProvider().maxTokens).toBe(4096);
    expect(modelTrigger().textContent).toContain("deepseek-v4-pro");

    // Picking a model is a choice, not a draft: it is written without a Save.
    await waitFor(() => expect(saves()).toBe(savesBefore + 1));
    expect(document.querySelector(".ai-save-notice")).toBeFalsy();

    await type("hello");
    expect(sendButton().disabled).toBe(false);
    fireEvent.click(sendButton());
    await waitFor(() => expect(sendCalls()).toBe(1));
    expect(getActiveProvider().model).toBe("deepseek-v4-pro");
  });

  it("switches provider with the model, follows its thinking levels and clears a stale one", async () => {
    await mount();
    await pickFromTrigger(modelTrigger(), "gemini-4-argon");
    await waitFor(() => expect(getActiveProvider().id).toBe("gemini-2.5-pro"));
    expect(getActiveProvider().model).toBe("gemini-4-argon");
    expect(getActiveProvider().contextLength).toBe(200000);
    expect(getActiveProvider().maxTokens).toBe(32000);
    expect(effortTrigger()).toBeTruthy();
    expect(effortTrigger()!.textContent).toContain("medium");

    await pickFromTrigger(effortTrigger()!, "high");
    await waitFor(() => expect(getActiveProvider().thinkingLevel).toBe("high"));
    expect(effortTrigger()!.textContent).toContain("high");

    await pickFromTrigger(modelTrigger(), "deepseek-v4-pro");
    await waitFor(() => expect(getActiveProvider().model).toBe("deepseek-v4-pro"));
    expect(getActiveProvider().thinkingLevel).toBe("medium");
    expect(effortTrigger()).toBeNull();
  });

  it("keeps each provider's chosen level when switching away and back", async () => {
    await mount();
    await pickFromTrigger(modelTrigger(), "gemini-4-argon");
    await waitFor(() => expect(effortTrigger()).toBeTruthy());
    await pickFromTrigger(effortTrigger()!, "low");
    await waitFor(() => expect(getActiveProvider().thinkingLevel).toBe("low"));

    await pickFromTrigger(modelTrigger(), "deepseek-v4-pro");
    await waitFor(() => expect(getActiveProvider().model).toBe("deepseek-v4-pro"));
    expect(effortTrigger()).toBeNull();

    await pickFromTrigger(modelTrigger(), "gemini-4-argon");
    await waitFor(() => expect(getActiveProvider().id).toBe("gemini-2.5-pro"));
    expect(effortTrigger()!.textContent).toContain("low");
  });

  it("offers only the declared levels of the shown model", async () => {
    await mount();
    await pickFromTrigger(modelTrigger(), "gemini-4-argon");
    await waitFor(() => expect(effortTrigger()).toBeTruthy());
    fireEvent.click(effortTrigger()!);
    const listbox = await screen.findByRole("listbox");
    expect(optionLabels(listbox)).toEqual(["low", "medium", "high"]);
    fireEvent.pointerDown(document.body);
  });

  it("names an empty model instead of an empty trigger", async () => {
    backend.load = async () => ({
      version: 2,
      active_provider: "custom",
      providers: [{
        id: "custom", name: "Custom Endpoint", kind: "openai_chat_completions", auth: "api_key",
        endpoint: "", model: "", context_length: 1000000, max_tokens: 16000, temperature: 0.3,
        thinking_level: "medium",
        capabilities: { text: true, audio_in: false, audio_out: false, native_tool_calling: false, streaming: false },
      }],
    });
    await mount();
    expect(getActiveProvider().id).toBe("custom");
    expect(modelTrigger().textContent?.trim()).not.toBe("");
    expect(modelTrigger().textContent).toContain("No model");
    // A custom endpoint is the user's own namespace: nothing is "not in catalog".
    expect(modelTrigger().getAttribute("data-model-known")).toBe("true");
  });

  it("marks a saved model the catalog cannot describe", async () => {
    backend.load = async () => ({
      version: 2,
      active_provider: "deepseek-v4-flash",
      providers: [{
        id: "deepseek-v4-flash", name: "DeepSeek Flash", kind: "openai_chat_completions", auth: "api_key",
        endpoint: "https://api.deepseek.com/chat/completions", model: "deepseek-unlisted",
        context_length: 900000, max_tokens: 8192, temperature: 0.3, thinking_level: "medium",
        capabilities: { text: true, audio_in: false, audio_out: false, native_tool_calling: true, streaming: true },
      }],
    });
    await mount();
    await waitFor(() => expect(modelTrigger().getAttribute("data-model-known")).toBe("false"));
    expect(modelTrigger().textContent).toContain("deepseek-unlisted");
    expect(modelTrigger().textContent).toContain("not in catalog");
  });
});

describe("header: settings and close", () => {
  it("opens one dialog on a double click, focuses inside, and closes by Done, Escape and backdrop", async () => {
    await mount();
    fireEvent.click(settingsButton());
    fireEvent.click(settingsButton());
    await waitFor(() => expect(dialog()).toBeTruthy());
    expect(screen.getAllByRole("dialog", { name: "Preferences" })).toHaveLength(1);
    expect(dialog()!.contains(document.activeElement)).toBe(true);

    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(dialog()).toBeNull());

    fireEvent.click(settingsButton());
    await waitFor(() => expect(dialog()).toBeTruthy());
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    await waitFor(() => expect(dialog()).toBeNull());

    fireEvent.click(settingsButton());
    await waitFor(() => expect(dialog()).toBeTruthy());
    fireEvent.pointerDown(document.querySelector(".preferences-overlay") as HTMLElement);
    await waitFor(() => expect(dialog()).toBeNull());
  });

  it("opens while an approval is pending and keeps the card behind it", async () => {
    await mount();
    backend.chat = async () => proposalReply();
    await choosePermission("require_approval");
    await send("add a lead");
    await approvalCard();

    // The turn is paused on a decision, not finished: the dialog opens over the
    // card, and the composer's button is the way out of that turn.
    fireEvent.click(settingsButton());
    await waitFor(() => expect(dialog()).toBeTruthy());
    expect(document.querySelector(".ai-approval-card")).toBeTruthy();
    expect(sendButton().getAttribute("aria-label")).toBe("Cancel request");
  });

  it("leaves the panel and its pending approval alone on Escape", async () => {
    const onClose = vi.fn();
    await mount({ onClose });
    backend.chat = async () => proposalReply();
    await choosePermission("require_approval");
    await send("add a lead");
    await approvalCard();
    fireEvent.keyDown(document.body, { key: "Escape" });
    expect(onClose).not.toHaveBeenCalled();
    expect(document.querySelector(".ai-approval-card")).toBeTruthy();
    expect(document.querySelector(".ai-settings-modal")).toBeNull();
  });
});

// ── Toolset chips ──────────────────────────────────────────────────────────

describe("toolset chips", () => {
  function chips(container: HTMLElement): string[] {
    return [...container.querySelectorAll(".ai-toolset-chip")].map((chip) => chip.textContent ?? "");
  }

  it("opens the settings dialog on the toolset switchboard from the strip", async () => {
    const view = await mount();
    expect(chips(view.container)).toEqual(["dsl-edit", "project-edit", "listen", "plugins"]);
    fireEvent.click(view.container.querySelector(".ai-toolsets-strip") as HTMLElement);
    await waitFor(() => expect(dialog()).toBeTruthy());
    await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("checkbox", { name: /DSL edit/ })));
  });

  it("shows the empty-selection warning after unchecking every set and reflects the re-check", async () => {
    const view = await mount();
    fireEvent.click(settingsButton());
    await waitFor(() => expect(dialog()).toBeTruthy());

    for (const title of ["DSL edit", "Project edit", "Listen", "Plugins"]) {
      fireEvent.click(screen.getByRole("checkbox", { name: new RegExp(title) }));
    }
    expect(chips(view.container)).toEqual(["no tools"]);
    expect(view.container.querySelector(".ai-toolset-chip-empty")).toBeTruthy();
    expect(dialog()!.textContent).toContain("No toolset is selected");

    fireEvent.click(screen.getByRole("checkbox", { name: /Transport/ }));
    expect(chips(view.container)).toEqual(["transport"]);
    expect(view.container.querySelector(".ai-toolset-chip-empty")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(dialog()).toBeNull());
    // The selection belongs to the session, not to the dialog: it survives a
    // close and reopen.
    fireEvent.click(settingsButton());
    await waitFor(() => expect(dialog()).toBeTruthy());
    expect(chips(view.container)).toEqual(["transport"]);
    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(dialog()).toBeNull());
    await type("what can you do");
    fireEvent.click(sendButton());
    await waitFor(() => expect(backend.toolsetReads.some((read) => Array.isArray(read.toolsets))).toBe(true));
    const read = backend.toolsetReads.filter((entry) => Array.isArray(entry.toolsets)).at(-1)!;
    expect(read.toolsets).toEqual(["transport"]);
  });

  it("advertises an empty selection as an empty list, not the whole surface", async () => {
    await mount();
    fireEvent.click(settingsButton());
    await waitFor(() => expect(dialog()).toBeTruthy());
    for (const title of ["DSL edit", "Project edit", "Listen", "Plugins"]) {
      fireEvent.click(screen.getByRole("checkbox", { name: new RegExp(title) }));
    }
    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(dialog()).toBeNull());

    await type("answer in prose");
    fireEvent.click(sendButton());
    await waitFor(() => expect(backend.toolsetReads.some((read) => Array.isArray(read.toolsets))).toBe(true));
    expect(backend.toolsetReads.filter((entry) => Array.isArray(entry.toolsets)).at(-1)!.toolsets).toEqual([]);
  });

  it("keeps the toolset card inside the viewport near the bottom edge", () => {
    // jsdom has no layout: assert the placement helper's output instead.
    const anchor = { left: 100, top: 700, right: 300, bottom: 720, width: 200, height: 20 };
    const placed = placeTooltip(anchor, { width: 320, height: 240 }, { width: 1024, height: 768 });
    expect(placed.side).toBe("top");
    expect(placed.y).toBe(452);
    expect(placed.maxHeight).toBeNull();
    expect(placed.y + 240).toBeLessThanOrEqual(768);

    const tall = placeTooltip(anchor, { width: 320, height: 900 }, { width: 1024, height: 768 });
    expect(tall.maxHeight).not.toBeNull();
    expect(tall.y + tall.maxHeight!).toBeLessThanOrEqual(768);
  });

  it("lists six tools then the remainder, and degrades on a missing description", async () => {
    const many = ["a", "b", "c", "d", "e", "f", "g", "h"];
    const sets = [{ id: "big", title: "Big set", description: "Eight tools", tools: many, default_on: true }];
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "load_ai_config") return aiConfigWire();
      if (cmd === "save_ai_config") return { path: "/tmp/x", warnings: [] };
      if (cmd === "ai_key_status") return true;
      if (cmd === "ai_catalog") return CATALOG;
      if (cmd === "ai_model_limits") return backend.limits;
      if (cmd === "ai_toolsets") return sets;
      if (cmd === "ai_tools") return many.map((name, index) => ({ name, description: index === 0 ? "first tool" : "", area: "x", inputSchema: {} }));
      if (cmd === "ai_build_context") return { dsl_context: "" };
      if (cmd === "ai_turn_start") return { text: "ok", failed: false };
      return makeSnapshot();
    });
    const view = await mount();
    fireEvent.click(view.container.querySelector(".ai-toolsets-strip") as HTMLElement);
    await waitFor(() => expect(dialog()).toBeTruthy());
    const row = [...document.querySelectorAll(".ai-toolset-row")].find((element) => element.textContent?.includes("Big set")) as HTMLElement;
    fireEvent.focusIn(row);
    const tooltip = await screen.findByRole("tooltip", {}, { timeout: 3000 });
    expect(tooltip.textContent).toContain("first tool");
    expect(tooltip.textContent).toContain("no description");
    expect(tooltip.textContent).toContain("… and 2 more");
    expect(tooltip.querySelectorAll(".oc-tooltip-row")).toHaveLength(7);
  });
});

// ── Composer ───────────────────────────────────────────────────────────────

describe("composer: attach", () => {
  it("opens the picker, stays silent when the picker is cancelled and shows a removable chip", async () => {
    await mount();
    openDialogMock.mockResolvedValueOnce(null);
    fireEvent.click(attachButton());
    await waitFor(() => expect(openDialogMock).toHaveBeenCalledTimes(1));
    expect(document.querySelector(".ai-attachment-chip")).toBeFalsy();
    expect(document.querySelector(".ai-media-error")).toBeFalsy();

    openDialogMock.mockResolvedValueOnce("/tmp/take.wav");
    fireEvent.click(attachButton());
    const chip = await waitFor(() => {
      const element = document.querySelector(".ai-attachment-chip") as HTMLElement | null;
      expect(element).toBeTruthy();
      return element!;
    });
    expect(chip.textContent).toContain("take.wav");
    fireEvent.click(chip.querySelector(".ai-attachment-remove") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".ai-attachment-chip")).toBeFalsy());
    expect(document.querySelector(".ai-attachments")).toBeFalsy();
  });

  it("removes exactly the clicked attachment", async () => {
    backend.attach = async () => (backend.attachCalls === 1 ? ATTACHMENT : SECOND_ATTACHMENT);
    await mount();
    openDialogMock.mockResolvedValueOnce("/tmp/take.wav");
    fireEvent.click(attachButton());
    await waitFor(() => expect(document.querySelectorAll(".ai-attachment-chip")).toHaveLength(1));
    openDialogMock.mockResolvedValueOnce("/tmp/take-2.wav");
    fireEvent.click(attachButton());
    await waitFor(() => expect(document.querySelectorAll(".ai-attachment-chip")).toHaveLength(2));

    const first = document.querySelector('.ai-attachment-chip[data-media-path="/tmp/take.wav"]') as HTMLElement;
    fireEvent.click(first.querySelector(".ai-attachment-remove") as HTMLElement);
    await waitFor(() => expect(document.querySelectorAll(".ai-attachment-chip")).toHaveLength(1));
    expect(document.querySelector(".ai-attachment-chip")?.getAttribute("data-media-path")).toBe("/tmp/take-2.wav");
  });

  it("keeps attachment failures inline and never throws", async () => {
    backend.attach = async () => { throw new Error("unsupported file type"); };
    await mount();
    openDialogMock.mockResolvedValueOnce("/tmp/notes.txt");
    fireEvent.click(attachButton());
    const error = await waitFor(() => {
      const element = document.querySelector(".ai-media-error") as HTMLElement | null;
      expect(element).toBeTruthy();
      return element!;
    });
    expect(error.textContent).toContain("unsupported file type");

    openDialogMock.mockRejectedValueOnce(new Error("dialog unavailable"));
    fireEvent.click(attachButton());
    await waitFor(() => expect(document.querySelector(".ai-media-error")?.textContent).toContain("dialog unavailable"));
  });

  it("is disabled while a request runs and while an approval waits", async () => {
    await mount();
    const pending = deferred();
    backend.chat = () => pending.promise;
    await send("describe the mix");
    await waitFor(() => expect(attachButton().disabled).toBe(true));
    pending.resolve({ role: "assistant", content: "ok" });
    await waitFor(() => expect(attachButton().disabled).toBe(false));

    backend.chat = async () => proposalReply();
    await choosePermission("require_approval");
    await send("add a lead");
    await approvalCard();
    expect(attachButton().disabled).toBe(true);
  });
});

describe("composer: input and send", () => {
  it("sends on Enter, not on Shift+Enter and not on blank text", async () => {
    await mount();
    await type("hello");
    fireEvent.keyDown(input(), { key: "Enter", shiftKey: true });
    expect(sendCalls()).toBe(0);
    fireEvent.keyDown(input(), { key: "Enter", shiftKey: false });
    await waitFor(() => expect(sendCalls()).toBe(1));
    await waitFor(() => expect(chatCalls()).toBe(1));
    expect(input().value).toBe("");
    expect(sendButton().disabled).toBe(true);
  });

  it("does not send the Enter that confirms an IME composition", async () => {
    await mount();
    await type("加一个鼓组");
    fireEvent.keyDown(input(), { key: "Enter", isComposing: true });
    expect(sendCalls()).toBe(0);
    expect(input().value).toBe("加一个鼓组");
    // The same key without composition still sends.
    fireEvent.keyDown(input(), { key: "Enter" });
    await waitFor(() => expect(sendCalls()).toBe(1));
  });

  it("keeps a whitespace-only draft from sending", async () => {
    await mount();
    await type("   ");
    expect(sendButton().disabled).toBe(true);
    fireEvent.keyDown(input(), { key: "Enter" });
    expect(sendCalls()).toBe(0);
  });

  it("keeps Send disabled with no model and names the way out", async () => {
    backend.load = async () => ({
      version: 2,
      active_provider: "custom",
      providers: [{
        id: "custom", name: "Custom Endpoint", kind: "openai_chat_completions", auth: "api_key",
        endpoint: "", model: "", models: ["llama3"],
        context_length: 1000000, max_tokens: 16000, temperature: 0.3, thinking_level: "medium",
        capabilities: { text: true, audio_in: false, audio_out: false, native_tool_calling: false, streaming: false },
      }],
    });
    await mount();
    await type("hello");
    expect(sendButton().disabled).toBe(true);
    expect(document.body.textContent).toContain("Pick a model below the composer before sending.");
    fireEvent.keyDown(input(), { key: "Enter" });
    expect(sendCalls()).toBe(0);

    await pickFromTrigger(modelTrigger(), "llama3");
    await waitFor(() => expect(getActiveProvider().model).toBe("llama3"));
    // Choosing a model writes it at once, so nothing has to be saved by hand.
    expect(sendButton().disabled).toBe(false);
    fireEvent.click(sendButton());
    await waitFor(() => expect(sendCalls()).toBe(1));
  });

  it("locks both model selectors while a request runs and keeps the composer ready to queue", async () => {
    await mount();
    await pickFromTrigger(modelTrigger(), "gemini-4-argon");
    await waitFor(() => expect(effortTrigger()).toBeTruthy());
    // The pick wrote itself, so a request may start immediately.
    const pending = deferred();
    backend.chat = () => pending.promise;
    await send("first");
    await waitFor(() => expect(sendCalls()).toBe(1));
    // The composer stays usable: an empty one offers the way out of the turn,
    // and a message typed now turns the button back into Send and queues.
    expect(input().disabled).toBe(false);
    expect(sendButton().getAttribute("aria-label")).toBe("Cancel request");
    expect(modelTrigger().hasAttribute("disabled")).toBe(true);
    expect(effortTrigger()!.hasAttribute("disabled")).toBe(true);
    await type("queued");
    expect(sendButton().getAttribute("aria-label")).toBe("Send");
    expect(sendButton().disabled).toBe(false);
    fireEvent.click(sendButton());
    await waitFor(() => expect(document.querySelector(".ai-queue-text")?.textContent).toBe("queued"));
    expect(sendCalls()).toBe(1);

    pending.resolve({ role: "assistant", content: "done" });
    await waitFor(() => expect(input().disabled).toBe(false));
    await type("second");
    expect(sendButton().disabled).toBe(false);
  });

  it("cancels without leaving an error bubble or a disabled composer", async () => {
    await mount();
    const pending = deferred();
    backend.chat = () => pending.promise;
    await send("long request");
    await waitFor(() => expect(backend.chatCalls).toHaveLength(1));
    const cancel = await waitFor(() => {
      const button = screen.getByRole("button", { name: "Cancel request" });
      expect(button).toBeTruthy();
      return button;
    });
    fireEvent.click(cancel);
    await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "ai_cancel")).toBe(true));

    // The composer stays usable during a turn, so the settling turn is what
    // removes the Cancel button, not the composer coming back.
    await waitFor(() => expect(screen.queryByRole("button", { name: "Cancel request" })).toBeNull());
    expect(input().disabled).toBe(false);
    expect(document.body.textContent).not.toContain("Request cancelled");
    expect(document.querySelector(".ai-message.assistant")).toBeFalsy();
    await type("second try");
    expect(sendButton().disabled).toBe(false);
    fireEvent.click(sendButton());
    await waitFor(() => expect(sendCalls()).toBe(2));
  });

  it("reports a pick that could not be written and retries it", async () => {
    await mount();
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "save_ai_config") throw new Error("Invalid or duplicate provider settings");
      if (cmd === "ai_model_limits") return backend.limits;
      if (cmd === "ai_catalog") return CATALOG;
      if (cmd === "ai_key_status") return true;
      if (cmd === "ai_toolsets") return SETS;
      if (cmd === "ai_tools") return TOOLS;
      return makeSnapshot();
    });
    await pickFromTrigger(modelTrigger(), "deepseek-v4-pro");
    await waitFor(() => expect(document.querySelector(".ai-save-notice")).toBeTruthy());
    expect(document.body.textContent).toContain("Invalid or duplicate provider settings");

    // The banner's own button is the retry; a good write clears it.
    installBackend();
    fireEvent.click(document.querySelector(".ai-save-notice button") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".ai-save-notice")).toBeFalsy());
    expect(getActiveProvider().model).toBe("deepseek-v4-pro");
  });

  it("writes a form draft before sending instead of sending with the saved one", async () => {
    await mount();
    // A settings-dialog edit stays a draft until the dialog's Save. It does not
    // block the composer: the send writes the draft first, so the request names
    // the model and options on screen rather than the last saved ones.
    aiConfig.updateProvider(getActiveProvider().id, { temperature: 0.8 });
    await type("hello");
    expect(sendButton().disabled).toBe(false);
    const before = invokeMock.mock.calls.length;
    fireEvent.click(sendButton());
    await waitFor(() => expect(sendCalls()).toBe(1));
    const commands = invokeMock.mock.calls.slice(before).map(([command]) => command);
    expect(commands).toContain("save_ai_config");
    // The write precedes the request it applies to.
    expect(commands.indexOf("save_ai_config")).toBeLessThan(commands.indexOf("ai_turn_start"));
    expect(hasUnsavedConfig()).toBe(false);
  });

  it("names why Send is unavailable when it really is", async () => {
    await mount();
    // An empty composer explains itself instead of showing a dead arrow.
    await waitFor(() => expect(document.querySelector(".ai-send-blocked")).toBeTruthy());
    expect(document.querySelector(".ai-send-blocked")!.textContent).toContain("Type a message");
    expect(sendButton().disabled).toBe(true);
    await type("hello");
    await waitFor(() => expect(document.querySelector(".ai-send-blocked")).toBeFalsy());
    expect(sendButton().disabled).toBe(false);
  });
});

describe("composer: the approval card for a subtree edit", () => {
  it("shows the tool, its arguments and the subtree it touches, and pauses before applying", async () => {
    await mount();
    backend.chat = async () => proposalReply();
    await choosePermission("require_approval");
    await send("add a lead");
    const card = await approvalCard();

    // Paused, not dispatched: the turn asked once and nothing ran.
    expect(chatCalls()).toBe(1);
    expect(backend.dispatched).toEqual([]);
    const text = card.textContent ?? "";
    expect(text).toContain("edit_subtree");
    expect(text).toContain("Target: track:lead");
    expect(text).toContain("Arguments");
    expect(text).toContain('"path":"track:lead"');
    expect(input().disabled).toBe(true);
  });

  it("cancels while the card is open without running the replacement", async () => {
    await mount();
    backend.chat = async () => proposalReply();
    await choosePermission("require_approval");
    const before = store.dslSource();
    await send("add a lead");
    await approvalCard();
    fireEvent.click(screen.getByRole("button", { name: "Cancel request" }));

    await waitFor(() => expect(input().disabled).toBe(false));
    expect(document.querySelector(".ai-approval-card")).toBeNull();
    expect(backend.dispatched).toEqual([]);
    expect(store.dslSource()).toBe(before);
    // The loop's own cancel path: nothing had been applied, and the notice says so.
    await waitFor(() => expect(document.body.textContent).toContain("Cancelled before any edit was applied"));
  });
});

describe("composer: the approval card for a destructive call", () => {
  const REMOVE_BASS = {
    id: "call-1",
    type: "function",
    function: { name: "tracks_remove", arguments: JSON.stringify({ index: 1 }) },
  };
  const PROJECT = () => makeSnapshot({ tracks: [makeTrack("Lead", 0), makeTrack("Bass", 1)] });
  const card = () => document.querySelector(".ai-approval-card") as HTMLElement | null;
  const approveButton = () => document.querySelector(".ai-approval-approve") as HTMLButtonElement | null;
  const rejectButton = () => document.querySelector(".ai-approval-reject") as HTMLButtonElement | null;
  /** First round asks to delete the bass track; every later round answers. */
  const askingChat = () => {
    let round = 0;
    return async () => (round++ === 0
      ? { role: "assistant", content: null, tool_calls: [REMOVE_BASS] }
      : { role: "assistant", content: "The bass track is still there." });
  };
  const toolCalls = () => backend.dispatched;

  it("names the tool, the track and the consequence, and pauses before running it", async () => {
    backend.snapshot = PROJECT();
    backend.chat = askingChat();
    await mount();
    // Every DSL-modifying call asks under this mode.
    await choosePermission("require_approval");
    await send("delete the bass track");
    await waitFor(() => expect(card()).toBeTruthy());

    // Paused, not cancelled: no second request, no tool dispatched.
    expect(chatCalls()).toBe(1);
    expect(toolCalls()).toEqual([]);
    const text = card()!.textContent ?? "";
    expect(text).toContain("Approve a project change");
    expect(text).toContain("tracks_remove");
    expect(text).toContain("Target: Bass");
    expect(text).toContain("Deletes the track, its clips, automation and devices.");
    expect(approveButton()!.textContent).toBe("Approve");
    expect(rejectButton()!.textContent).toBe("Reject");
    // The composer waits with the turn.
    expect(input().disabled).toBe(true);
  });

  it("runs the same turn after Approve", async () => {
    backend.snapshot = PROJECT();
    backend.chat = askingChat();
    await mount();
    // Every DSL-modifying call asks under this mode.
    await choosePermission("require_approval");
    await send("delete the bass track");
    await waitFor(() => expect(approveButton()).toBeTruthy());
    fireEvent.click(approveButton()!);

    await waitFor(() => expect(backend.chatCalls).toHaveLength(2));
    await waitFor(() => expect(card()).toBeNull());
    expect(toolCalls()).toContain("tracks_remove");
    await waitFor(() => expect(document.body.textContent).toContain("The bass track is still there."));
  });

  it("cancels while the card is open without dispatching the call", async () => {
    backend.snapshot = PROJECT();
    backend.chat = askingChat();
    await mount();
    // Every DSL-modifying call asks under this mode.
    await choosePermission("require_approval");
    await send("delete the bass track");
    await waitFor(() => expect(card()).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Cancel request" }));

    await waitFor(() => expect(input().disabled).toBe(false));
    expect(card()).toBeNull();
    expect(toolCalls()).toEqual([]);
    expect(chatCalls()).toBe(1);
    // The loop's own cancel path: nothing had been applied, and the notice says so.
    await waitFor(() => expect(document.body.textContent).toContain("Cancelled before any edit was applied"));
    expect(document.querySelector(".ai-message.assistant")).toBeFalsy();
  });

  const alwaysButton = () => document.querySelector(".ai-approval-always") as HTMLButtonElement | null;

  it("stops asking for a tool the conversation allowed, on the next turn", async () => {
    backend.snapshot = PROJECT();
    backend.chat = askingChat();
    await mount();
    await send("delete the bass track");
    await waitFor(() => expect(alwaysButton()).toBeTruthy());
    fireEvent.click(alwaysButton()!);
    const deletions = () => toolCalls().filter(tool => tool === "tracks_remove");
    // The answer applies the call and the turn ends with the model's reply.
    await waitFor(() => expect(document.body.textContent).toContain("The bass track is still there."));
    expect(deletions()).toHaveLength(1);
    expect(allowedTools("checkpoint-session")).toEqual(["tracks_remove"]);

    // The same conversation asks for another deletion: no card this time.
    backend.chat = askingChat();
    await send("delete it again");
    await waitFor(() => expect(deletions()).toHaveLength(2));
    expect(card()).toBeNull();
  });

  it("never asks for a read-only call", async () => {
    backend.snapshot = PROJECT();
    let round = 0;
    backend.chat = async () => (round++ === 0
      ? {
        role: "assistant",
        content: null,
        tool_calls: [{ id: "call-1", type: "function", function: { name: "tracks_list", arguments: "{}" } }],
      }
      : { role: "assistant", content: "Two tracks." });
    await mount();
    await send("list the tracks");
    await waitFor(() => expect(toolCalls()).toEqual(["tracks_list"]));
    expect(card()).toBeNull();
    await waitFor(() => expect(document.body.textContent).toContain("Two tracks."));
  });

  it("has every approval string in all five locales", () => {
    const keys: I18nKey[] = [
      "ai.approval.title",
      "ai.approval.target",
      "ai.approval.unknownTarget",
      "ai.approval.approve",
      "ai.approval.always",
      "ai.approval.reject",
      approvalEffect("tracks_remove"),
      approvalEffect("buses_remove"),
      approvalEffect("sections_remove"),
      approvalEffect("presets_remove"),
      approvalEffect("effects_remove"),
      approvalEffect("project_new"),
      approvalEffect("project_open"),
      approvalEffect("something-else"),
    ];
    for (const key of keys) {
      for (const locale of i18n.locales) expect(i18n.has(locale, key), `${locale} ${key}`).toBe(true);
    }
  });
});

describe("composer: auth banner", () => {
  it("shows the reason, opens the settings from Reauth and clears after a successful send", async () => {
    await mount();
    backend.chat = async () => { throw { kind: "auth_expired", message: "The login expired" }; };
    await send("hello");
    const banner = await waitFor(() => {
      const element = document.querySelector(".ai-auth-banner") as HTMLElement | null;
      expect(element).toBeTruthy();
      return element!;
    });
    expect(banner.textContent).toContain("The login expired");

    fireEvent.click(screen.getByRole("button", { name: "Sign in again" }));
    await waitFor(() => expect(dialog()).toBeTruthy());
    expect(dialog()!.textContent).toContain("Authentication");
    // Reauth opens the dialog on the authentication field, not the protocol.
    await waitFor(() => expect(document.activeElement).toBe(screen.getByText("Authentication").nextElementSibling));
    fireEvent.click(screen.getByRole("button", { name: "Done" }));
    await waitFor(() => expect(dialog()).toBeNull());

    backend.chat = async () => ({ role: "assistant", content: "hi" });
    await send("hello again");
    await waitFor(() => expect(document.querySelector(".ai-auth-banner")).toBeFalsy());
  });
});

describe("composer: quick actions", () => {
  it.each(["zh-Hans", "zh-Hant", "ja", "es"] as const)("updates starter labels and sends localized prompt in %s", async locale => {
    await mount();
    i18n.setLocale(locale);
    const keys = ["describe", "reverb", "drums", "mix"] as const;
    const buttons = [...document.querySelectorAll<HTMLElement>(".ai-quick-btn")];
    expect(buttons.map(button => button.textContent)).toEqual(keys.map(key => i18n.t(`ai.quick.${key}`)));
    const prompt = i18n.t("ai.quick.describePrompt");
    fireEvent.click(buttons[0]);
    await waitFor(() => expect(chatCalls()).toBe(1));
    expect(document.body.textContent).toContain(prompt);
  });

  const actions = [
    "Describe the current arrangement structure and suggest improvements",
    "Add a hall reverb bus and route the lead track to it at 30%",
    "Add a drum track with a basic 4/4 rock beat using kick, snare, and hihat",
    "Suggest mix improvements: check levels, panning, and effects for balance",
  ];

  for (const [index, action] of actions.entries()) {
    it(`fills the composer and sends quick action ${index + 1} exactly once`, async () => {
      await mount();
      const buttons = [...document.querySelectorAll<HTMLElement>(".ai-quick-btn")];
      expect(buttons).toHaveLength(4);
      fireEvent.click(buttons[index]);
      await waitFor(() => expect(sendCalls()).toBe(1));
      // One click, one request: a second send would have been a duplicate.
      await waitFor(() => expect(chatCalls()).toBe(1));
      expect(document.querySelector(".ai-quick-actions")).toBeFalsy();
      expect(document.body.textContent).toContain(action);
    });
  }
});

describe("composer: attachments between sends", () => {
  it("does not resend an attachment that belonged to an earlier message", async () => {
    await mount();
    openDialogMock.mockResolvedValueOnce("/tmp/take.wav");
    fireEvent.click(attachButton());
    await waitFor(() => expect(document.querySelector(".ai-attachment-chip")).toBeTruthy());
    await send("first with audio");
    await waitFor(() => expect(chatCalls()).toBe(1));

    await send("second without");
    await waitFor(() => expect(chatCalls()).toBe(2));
    const last = backend.chatCalls.at(-1)!;
    const messages = last.messages as { role: string; content: string; media?: unknown[] }[];
    const latestUser = messages[messages.length - 1];
    expect(latestUser.content).toBe("second without");
    expect(latestUser.media).toBeUndefined();
    // The request is the system prompt plus the pending message: the earlier
    // turn, its attachment included, is replayed from the saved session.
    expect(messages.filter((message) => message.role === "user")).toHaveLength(1);
    expect(messages.some((message) => message.role === "system")).toBe(true);
    expect(document.querySelector(".ai-attachment-chip")).toBeFalsy();
  });
});
