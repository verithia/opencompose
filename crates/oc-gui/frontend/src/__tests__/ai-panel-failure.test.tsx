/**
 * A turn that stops early is a failure, not an answer: the reason appears once,
 * in the failed row the turn already reported, and never again as an assistant
 * bubble that reads like finished work.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, openDialogMock, openStepGroups, aiConfigWire } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { MAX_TOOL_ROUNDS } from "../ai/agent";
import { aiLimits } from "../stores/aiLimits";
import { resetStats } from "../stores/aiStats";
import { aiSession } from "../stores/aiSession";
import { i18n, type I18nKey } from "../stores/i18n";

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
  { id: "project-edit", title: "Project edit", description: "Tracks, presets and buses", tools: ["tracks_list", "tracks_rename"], default_on: true },
];

const TOOLS = [
  { name: "tracks_list", description: "List tracks and their mix", area: "project", mutation: false, inputSchema: {} },
  { name: "tracks_rename", description: "Rename a track", area: "project", mutation: true, inputSchema: {} },
];

/** The clip the composer keeps after a failure. */
const ATTACHMENT = { kind: "audio", name: "take.wav", path: "/tmp/take.wav", mime: "audio/wav", bytes: 320000, duration_secs: 10, sha256: "a".repeat(64) };

/** Everything a test can steer; the mock below only reads it. */
const backend = {
  chat: async (): Promise<unknown> => ({ role: "assistant", content: "Hello." }),
  /** Every `ai_chat` request, with the messages copied at the time of the call. */
  chatCalls: [] as { messages: { role: string; content?: unknown; tool_call_id?: string }[]; [key: string]: unknown }[],
  tools: TOOLS as { name: string; description: string; area: string; mutation: boolean; inputSchema: object }[],
  tool: async (_call: { tool: string; args: Record<string, unknown> }): Promise<unknown> => ({ tracks: [] }),
};

function installBackend() {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    switch (cmd) {
      case "load_ai_config": return aiConfigWire();
      case "save_ai_config": return { path: "/tmp/ai-providers.yaml", warnings: [] };
      case "ai_key_status": return true;
      case "ai_catalog": return CATALOG;
      case "ai_model_limits": return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
      case "ai_toolsets": return SETS;
      case "ai_tools": return backend.tools;
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
      case "ai_chat": {
        // The loop keeps mutating its own array, so the request is copied here.
        backend.chatCalls.push({ ...(args as Record<string, unknown>), messages: JSON.parse(JSON.stringify((args as Record<string, unknown>).messages)) });
        return backend.chat();
      }
      case "ai_attach_media": return ATTACHMENT;
      case "ai_call_tool": return backend.tool((args ?? {}) as { tool: string; args: Record<string, unknown> });
      default: return makeSnapshot();
    }
  });
}

async function mount() {
  await store.initProject();
  const view = render(() => <AiPanel />);
  await waitFor(() => expect(view.container.querySelector(".ai-save-notice")).toBeFalsy());
  await waitFor(() => expect(view.container.querySelector(".ai-toolset-chip")).toBeTruthy());
  await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "ai_tools")).toBe(true));
  return view;
}

const input = () => document.querySelector(".ai-input") as HTMLInputElement;
const sendButton = () => document.querySelector(".ai-send-btn") as HTMLButtonElement;

async function send(text: string) {
  fireEvent.input(input(), { target: { value: text } });
  fireEvent.click(sendButton());
}

const failures = () => {
  openStepGroups();
  return [...document.querySelectorAll(".ai-tool-result.error")];
};
const answers = () => [...document.querySelectorAll(".ai-message.assistant")];

/** The turn settled once the waiting row is gone. */
async function settle() {
  await waitFor(() => expect(document.querySelector(".ai-thinking")).toBeFalsy());
}

beforeEach(() => {
  invokeMock.mockReset();
  backend.chat = async () => ({ role: "assistant", content: "Hello." });
  backend.chatCalls = [];
  backend.tools = TOOLS;
  backend.tool = async () => ({ tracks: [] });
  i18n.setLocale("en");
  aiLimits.setActiveContext(null);
  resetStats();
  aiSession.reset();
  openDialogMock.mockReset();
  installBackend();
});

afterEach(() => {
  cleanup();
  aiSession.reset();
  aiLimits.setActiveContext(null);
  resetStats();
});

describe("a turn that stops early", () => {
  it("reports the cap exit once, in the failed row, and never as an assistant answer", async () => {
    // Different work each round, so the round cap (not the repeat guard) ends it.
    let step = 0;
    backend.chat = async () => ({
      role: "assistant",
      content: null,
      tool_calls: [{ id: `call-${step}`, type: "function", function: { name: "tracks_list", arguments: JSON.stringify({ step: step++ }) } }],
    });
    await mount();
    await send("describe the mix");
    // The cap reports a stop row, so the turn settles when that row's Continue
    // is on screen and the composer is usable again.
    await waitFor(() => expect(continueButton()).toBeTruthy());
    await waitFor(() => expect(input().disabled).toBe(false));

    expect(failures()).toHaveLength(1);
    expect(answers()).toHaveLength(0);
    // Folded by default; the reason is one click away.
    expect(failures()[0].hasAttribute("open")).toBe(false);
    expect(failures()[0].textContent).toContain("Failed");
    (failures()[0] as HTMLDetailsElement).open = true;
    const failure = failures()[0].textContent ?? "";
    expect(failure).toContain(
      `stopped after ${MAX_TOOL_ROUNDS} tool rounds; the work is unfinished; the last action was tracks_list`,
    );
    // With no edit applied, the notice says the project is unchanged and still
    // offers Continue.
    expect(lastSystemRow()?.textContent).toContain("stopped at the round limit");
    expect(lastSystemRow()?.textContent).toContain("no edit had been applied");
    expect(retryButton()).toBeNull();
  });

  it("keeps the edits a timed-out turn had already applied", async () => {
    backend.tool = async ({ tool }) => {
      if (tool === "transaction_begin") return { active: true, label: "AI edit" };
      if (tool === "transaction_commit") return { committed: true, revision: 2 };
      return { revision: 1 };
    };
    let round = 0;
    backend.chat = async () => {
      round += 1;
      if (round === 1) {
        return {
          role: "assistant", content: null,
          tool_calls: [{ id: "call-1", type: "function", function: { name: "tracks_rename", arguments: "{}" } }],
        };
      }
      throw { kind: "timeout", message: "Provider request timed out" };
    };
    await mount();
    await send("rename the track");
    await waitFor(() => expect(continueButton()).toBeTruthy());

    expect(retryButton()).toBeTruthy();
    expect(lastSystemRow()?.textContent).toContain("model request timed out");
    expect(lastSystemRow()?.textContent).toContain("Applied edits kept: 1");
  });

  it("reports the repeated-call stop once and keeps the tool name in the failed row", async () => {
    // Only a repeated mutation stops the loop; a repeated read is allowed to
    // continue, because stopping it would roll back edits it never caused.
    const call = { id: "call-1", type: "function", function: { name: "tracks_rename", arguments: "{}" } };
    backend.chat = async () => ({ role: "assistant", content: null, tool_calls: [call] });
    await mount();
    await send("describe the mix");
    await settle();

    expect(failures()).toHaveLength(1);
    expect(answers()).toHaveLength(0);
    expect(failures()[0].hasAttribute("open")).toBe(false);
    (failures()[0] as HTMLDetailsElement).open = true;
    await waitFor(() =>
      expect(failures()[0].textContent ?? "").toContain(
        "the model repeated the same call twice",
      ),
    );
  });

  it("still shows a normal answer as an assistant bubble", async () => {
    backend.chat = async () => ({ role: "assistant", content: "The mix has one track." });
    await mount();
    await send("describe the mix");
    await settle();

    expect(answers()).toHaveLength(1);
    expect(answers()[0].textContent).toContain("The mix has one track.");
    expect(failures()).toHaveLength(0);
  });
});

/** The composer's button in its cancel state, or null when it is not. */
const cancelButton = () => document.querySelector(".ai-send-btn.cancel") as HTMLButtonElement | null;

const systemRows = () => [...document.querySelectorAll(".ai-message.system")];

describe("a cancelled turn", () => {
  /** TOOLS for a turn whose every call edits the project. */
  const MUTATING = [
    { name: "tracks_add", description: "Add a track", area: "tracks", mutation: true, inputSchema: {} },
  ];

  it("cancels while a call is in flight: the landed edits stay", async () => {
    let project = "snapshot";
    let snapshot: string | null = null;
    const undo: string[] = [];
    const commands: string[] = [];
    backend.tools = MUTATING;
    backend.chat = async (): Promise<unknown> => ({
      role: "assistant",
      content: null,
      tool_calls: ["a", "b", "c", "d", "e"].map((name, index) => ({
        id: `call-${index}`,
        type: "function",
        function: { name: "tracks_add", arguments: JSON.stringify({ name }) },
      })),
    });
    backend.tool = async ({ tool, args }) => {
      commands.push(tool);
      if (tool === "transaction_begin") { snapshot = project; return { active: true, label: args.label }; }
      if (tool === "transaction_commit") { undo.push(snapshot ?? project); snapshot = null; return { committed: true }; }
      if (tool === "transaction_rollback") { project = snapshot ?? project; snapshot = null; return { rolled_back: true }; }
      project = `${project}+${tool}`;
      // The user clicks Cancel while the second call is still running.
      if (commands.filter((name) => name === "tracks_add").length === 2) fireEvent.click(cancelButton()!);
      return { revision: 1 };
    };
    await mount();
    await send("add five tracks");

    await waitFor(() => expect(document.body.textContent).toContain("Cancelled. Applied edits kept: 2"));
    await waitFor(() => expect(input().disabled).toBe(false));
    expect(systemRows()).toHaveLength(1);
    const notice = systemRows()[0].textContent ?? "";
    expect(notice).toContain("Cancelled. Applied edits kept: 2");
    expect(notice).toContain("Right-click the user message to roll back this turn.");
    // A cancelled turn is a notice, not an answer, and not an error bubble.
    expect(answers()).toHaveLength(0);
    expect(document.body.textContent).not.toContain("Request cancelled");
    // The edits stay as the turn's one entry; the user can revert it from the
    // message menu.
    expect(project).toBe("snapshot+tracks_add+tracks_add");
    expect(snapshot).toBeNull();
    expect(undo).toEqual(["snapshot"]);
    expect(commands).toEqual([
      "transaction_begin", "tracks_add", "tracks_add", "transaction_commit",
    ]);
  });

  it("cancels before any call runs and says the project is unchanged", async () => {
    let rejectChat: ((reason?: unknown) => void) | null = null;
    backend.chat = () => new Promise((_resolve, reject) => { rejectChat = reject; });
    await mount();
    await send("describe the mix");
    const cancel = await waitFor(() => {
      const button = cancelButton();
      expect(button).toBeTruthy();
      return button!;
    });
    await waitFor(() => expect(rejectChat).toBeTruthy());
    fireEvent.click(cancel);
    rejectChat!({ kind: "cancelled", message: "Request cancelled" });

    await waitFor(() => expect(document.body.textContent).toContain("Cancelled before any edit was applied"));
    await waitFor(() => expect(input().disabled).toBe(false));
    expect(systemRows()).toHaveLength(1);
    expect(systemRows()[0].querySelector(".ai-message-content")?.textContent).toBe("Cancelled before any edit was applied; the project is unchanged.");
    expect(answers()).toHaveLength(0);
    expect(document.body.textContent).not.toContain("Request cancelled");
    expect(invokeMock.mock.calls.some(([command]) => command === "ai_call_tool")).toBe(false);
    expect(document.querySelector(".ai-tool-result")).toBeFalsy();
  });
});

const userRows = () => [...document.querySelectorAll(".ai-message.user")];
const retryButton = () => document.querySelector(".ai-turn-retry") as HTMLButtonElement | null;
const continueButton = () => document.querySelector(".ai-turn-continue") as HTMLButtonElement | null;
const lastSystemRow = () => ([...document.querySelectorAll(".ai-message.system")] as HTMLElement[]).at(-1) ?? null;

/** A provider that answers `round`, then fails; the loop has already run the calls. */
function failingRound(round: number, failure: unknown) {
  let call = 0;
  return async (): Promise<unknown> => {
    call += 1;
    if (call === round) throw failure;
    return {
      role: "assistant", content: null,
      tool_calls: [{ id: `call-${call}`, type: "function", function: { name: "tracks_list", arguments: "{}" } }],
    };
  };
}

describe("retry and continue", () => {
  it("offers one Retry on a transport failure, restores the draft and the attachment, and resends the same messages", async () => {
    backend.chat = failingRound(2, { kind: "network", message: "connection reset" });
    await mount();
    openDialogMock.mockResolvedValueOnce("/tmp/take.wav");
    fireEvent.click(document.querySelector(".ai-attach-btn") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".ai-attachment-chip")).toBeTruthy());
    await send("add a bass line");

    await waitFor(() => expect(retryButton()).toBeTruthy());
    expect(document.querySelectorAll(".ai-turn-retry")).toHaveLength(1);
    expect(lastSystemRow()?.textContent).toContain("Error: connection reset");
    expect(userRows()).toHaveLength(1);
    // The composer kept what the failure ate: the same text and the same chip.
    expect(input().value).toBe("add a bass line");
    expect(document.querySelector(".ai-attachment-chip")?.getAttribute("data-media-path")).toBe("/tmp/take.wav");

    const failed = backend.chatCalls.at(-1)!;
    backend.chat = async () => ({ role: "assistant", content: "The bass line is in." });
    fireEvent.click(retryButton()!);
    await waitFor(() => expect(document.body.textContent).toContain("The bass line is in."));

    // The retry sent the turn's own messages, not a rebuilt question: the user
    // row is one, and the assistant/tool state is the failed turn's.
    expect(backend.chatCalls.at(-1)!.messages).toEqual(failed.messages);
    expect(userRows()).toHaveLength(1);
    expect(input().value).toBe("add a bass line");
    // One shot: the row it came from no longer offers it.
    expect(retryButton()).toBeNull();
  });

  it("offers the turn's one Retry when the provider's tool arguments were not JSON", async () => {
    backend.chat = failingRound(2, {
      kind: "protocol",
      message: "Provider returned malformed tool arguments",
      retryable: true,
    });
    await mount();
    await send("arrange the chorus");

    await waitFor(() => expect(retryButton()).toBeTruthy());
    expect(document.querySelectorAll(".ai-turn-retry")).toHaveLength(1);
    expect(lastSystemRow()?.textContent).toContain("Error: Provider returned malformed tool arguments");
    expect(retryButton()!.textContent).toBe("Retry");
  });

  it("offers Retry for an empty reply that ran out of output budget", async () => {
    backend.chat = failingRound(2, {
      kind: "truncated",
      message: "provider returned an empty reply (finish_reason=length, no tool calls); the request may need a larger max_tokens because reasoning tokens count against it",
      retryable: true,
    });
    await mount();
    await send("transcribe the reference");

    await waitFor(() => expect(retryButton()).toBeTruthy());
    expect(document.querySelectorAll(".ai-turn-retry")).toHaveLength(1);
    expect(lastSystemRow()?.textContent).toContain("finish_reason=length");
  });

  it("offers no Retry when the provider layer calls the failure a configuration error", async () => {
    backend.chat = failingRound(1, {
      kind: "config",
      message: "No endpoint is saved for this provider.",
      retryable: false,
    });
    await mount();
    await send("transcribe the reference");

    await waitFor(() => expect(lastSystemRow()?.textContent).toContain("No endpoint is saved"));
    // Nothing to resend that could work, and no round ran, so the row has no
    // action at all: the user fixes the provider first.
    expect(retryButton()).toBeNull();
    expect(continueButton()).toBeNull();
    expect(document.querySelector(".ai-turn-actions")).toBeFalsy();
  });

  it("keeps a failed turn's edits and continues from the same project", async () => {
    const MUTATING = [{ name: "tracks_add", description: "Add a track", area: "tracks", mutation: true, inputSchema: {} }];
    backend.tools = MUTATING;
    let project = "snapshot";
    let snapshot: string | null = null;
    const undo: string[] = [];
    const commands: string[] = [];
    backend.tool = async ({ tool, args }) => {
      commands.push(tool);
      if (tool === "transaction_begin") { snapshot = project; return { active: true, label: args.label }; }
      if (tool === "transaction_commit") { undo.push(snapshot ?? project); snapshot = null; return { committed: true }; }
      if (tool === "transaction_rollback") { project = snapshot ?? project; snapshot = null; return { rolled_back: true }; }
      project = `${project}+${tool}`;
      return { revision: 1 };
    };
    let call = 0;
    backend.chat = async (): Promise<unknown> => {
      call += 1;
      if (call === 2) throw { kind: "server", message: "provider returned 500" };
      return {
        role: "assistant", content: null,
        tool_calls: [{ id: "call-1", type: "function", function: { name: "tracks_add", arguments: JSON.stringify({ name: "bass" }) } }],
      };
    };
    await mount();
    await send("add a bass track");
    await waitFor(() => expect(continueButton()).toBeTruthy());

    // The failed turn kept its edit as the entry a manual revert can restore.
    // Retry resends the failed request only; it does not replay the edit.
    expect(retryButton()).toBeTruthy();
    expect(document.querySelector(".ai-turn-note")?.textContent).toContain("last request");
    expect(project).toBe("snapshot+tracks_add");
    expect(undo).toEqual(["snapshot"]);
    expect(lastSystemRow()?.textContent).toContain("The turn failed. Applied edits kept: 1");
    expect(lastSystemRow()?.textContent).toContain("Right-click the user message to roll back this turn.");

    // Continue carries the same wire to an answer without applying the edit again.
    backend.chat = async (): Promise<unknown> => ({ role: "assistant", content: "Track added." });
    fireEvent.click(continueButton()!);
    await waitFor(() => expect(document.body.textContent).toContain("Track added."));
    expect(commands).toEqual(["transaction_begin", "tracks_add", "transaction_commit"]);
    expect(undo).toEqual(["snapshot"]);
  });

  it("keeps what landed at the round cap, so Continue resumes a project with the work in it", async () => {
    const MUTATING = [{ name: "tracks_add", description: "Add a track", area: "tracks", mutation: true, inputSchema: {} }];
    backend.tools = MUTATING;
    let project = "snapshot";
    let snapshot: string | null = null;
    const undo: string[] = [];
    const commands: string[] = [];
    backend.tool = async ({ tool, args }) => {
      commands.push(tool);
      if (tool === "transaction_begin") { snapshot = project; return { active: true, label: args.label }; }
      if (tool === "transaction_commit") { undo.push(snapshot ?? project); snapshot = null; return { committed: true }; }
      if (tool === "transaction_rollback") { project = snapshot ?? project; snapshot = null; return { rolled_back: true }; }
      project = `${project}+${tool}`;
      return { revision: commands.length };
    };
    let step = 0;
    backend.chat = async (): Promise<unknown> => ({
      role: "assistant", content: null,
      tool_calls: [{ id: `call-${step}`, type: "function", function: { name: "tracks_add", arguments: JSON.stringify({ name: `t${step++}` }) } }],
    });
    await mount();
    await send("add sixteen tracks");
    await waitFor(() => expect(continueButton()).toBeTruthy());
    await waitFor(() => expect(input().disabled).toBe(false));

    // The cap is a budget stop, not a failure: the batch is committed as the
    // turn's one undo entry, so the tracks the turn wrote are still there.
    expect(backend.chatCalls).toHaveLength(MAX_TOOL_ROUNDS);
    expect(commands.at(-1)).toBe("transaction_commit");
    expect(undo).toHaveLength(1);
    expect(project).not.toBe("snapshot");
    // Retry would apply the same edits twice, so only Continue is offered.
    expect(retryButton()).toBeNull();
    expect(lastSystemRow()?.textContent).toContain("stopped at the round limit");
    expect(lastSystemRow()?.textContent).toContain(`Applied edits kept: ${MAX_TOOL_ROUNDS}`);
    expect(document.querySelector(".ai-turn-note")?.textContent).toContain("Continue resumes after");
    expect(userRows()).toHaveLength(1);
    const capped = backend.chatCalls.at(-1)!;

    backend.chat = async () => ({ role: "assistant", content: "Sixteen rounds was enough." });
    fireEvent.click(continueButton()!);
    await waitFor(() => expect(document.body.textContent).toContain("Sixteen rounds was enough."));

    // Continue sent the conversation the stopped turn held: the last request
    // plus the round it never sent, and no second copy of the question.
    const resumed = backend.chatCalls.at(-1)!;
    expect(resumed.messages.slice(0, capped.messages.length)).toEqual(capped.messages);
    expect(resumed.messages.length).toBe(capped.messages.length + 2);
    expect(resumed.messages.at(-1)).toMatchObject({ role: "tool", tool_call_id: `call-${MAX_TOOL_ROUNDS - 1}` });
    expect(userRows()).toHaveLength(1);
    // The transcript says where it resumed.
    expect(lastSystemRow()?.textContent).toContain("Resumed after");
  });

  it("offers Continue but not Retry after a cancel, and closes the calls that never ran", async () => {
    backend.chat = async (): Promise<unknown> => ({
      role: "assistant", content: null,
      tool_calls: ["a", "b", "c"].map((name, index) => ({
        id: `call-${index}`, type: "function", function: { name: "tracks_list", arguments: JSON.stringify({ name }) },
      })),
    });
    let calls = 0;
    backend.tool = async () => {
      calls += 1;
      // Cancelling while the second call runs: two calls return, the third never does.
      if (calls === 2) fireEvent.click(cancelButton()!);
      return { tracks: [] };
    };
    await mount();
    await send("list the mix");
    await waitFor(() => expect(continueButton()).toBeTruthy());
    await waitFor(() => expect(input().disabled).toBe(false));

    // A cancel is deliberate: it can be continued, never resent as if it failed.
    expect(retryButton()).toBeNull();
    expect(userRows()).toHaveLength(1);

    backend.chat = async () => ({ role: "assistant", content: "The mix has no tracks." });
    fireEvent.click(continueButton()!);
    await waitFor(() => expect(document.body.textContent).toContain("The mix has no tracks."));

    const resumed = backend.chatCalls.at(-1)!;
    // The two results that landed stay; the call that never ran is answered as
    // "not run", so the model is not asked to trust a call that never happened.
    expect(resumed.messages.map(message => message.role)).toEqual(["system", "user", "assistant", "tool", "tool", "tool"]);
    expect(resumed.messages.filter(message => message.role === "tool").map(message => message.tool_call_id)).toEqual(["call-0", "call-1", "call-2"]);
    expect(JSON.parse(String(resumed.messages.at(-1)!.content)).error).toContain("stopped before this call ran");
    expect(userRows()).toHaveLength(1);
    expect(lastSystemRow()?.textContent).toContain("Resumed after 2 of 3 requested calls.");
  });

  it("has every retry and continue string in all five locales", () => {
    const keys: I18nKey[] = [
      "ai.retry.action",
      "ai.retry.note",
      "ai.retry.noteOne",
      "ai.retry.kept",
      "ai.retry.keptOne",
      "ai.continue.action",
      "ai.continue.note",
      "ai.continue.noteSaved",
      "ai.continue.resumed",
      "ai.continue.resumedSaved",
      "ai.cap.kept",
      "ai.cap.keptNone",
      "ai.timeout.kept",
      "ai.timeout.keptNone",
      "ai.cancel.kept",
      "ai.cancel.keptNone",
      "ai.failure.kept",
      "ai.failure.commitFailed",
    ];
    for (const key of keys) {
      for (const locale of i18n.locales) expect(i18n.has(locale, key), `${locale} ${key}`).toBe(true);
    }
  });
});
