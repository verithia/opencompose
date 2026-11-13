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
import { aiLimits } from "../stores/aiLimits";
import { resetStats } from "../stores/aiStats";
import { aiSession } from "../stores/aiSession";
import { i18n, type I18nKey } from "../stores/i18n";
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
    const typed = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "load_ai_config": return aiConfigWire();
      case "save_ai_config": return { path: "/tmp/ai-providers.yaml", warnings: [] };
      case "ai_key_status": return true;
      case "ai_catalog": return CATALOG;
      case "ai_model_limits": return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
      case "ai_toolsets": return SETS;
      case "ai_tools": return backend.tools;
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
      case "ai_turn_start": {
        return runFakeTurn(typed as unknown as TurnStartArgs, {
          chat: async (round) => {
            // The fake keeps appending to its own array, so the request is copied here.
            backend.chatCalls.push({
              ...round,
              messages: JSON.parse(JSON.stringify(round.messages)),
            } as unknown as { messages: { role: string; content?: unknown; tool_call_id?: string }[]; [key: string]: unknown });
            return backend.chat();
          },
          tool: (call) => backend.tool(call),
          mutation: (tool) => backend.tools.some((entry) => entry.name === tool && entry.mutation),
        });
      }
      case "ai_turn_answer": return answerFakeTurn(String(typed.requestId), String(typed.callId), typed.decision as never);
      case "ai_turn_questions": return [];
      case "ai_cancel": cancelFakeTurn(String(typed.requestId)); return undefined;
      case "ai_batch_recover": return null;
      case "ai_attach_media": return ATTACHMENT;
      case "sessions_new": return { id: "s1", title: String(typed.title ?? "new") };
      case "sessions_list": return [];
      case "sessions_load": return { meta: { id: "s1", title: "new" }, messages: [], skipped: 0 };
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
  resetFakeTurns();
  installBackend();
});

afterEach(() => {
  cleanup();
  resetFakeTurns();
  aiSession.reset();
  aiLimits.setActiveContext(null);
  resetStats();
});

describe("a turn that stops early", () => {
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

describe("the stop facts the backend reports", () => {
  /** A turn whose outcome is scripted, as the loop would report it. */
  function outcome(value: unknown) {
    const base = invokeMock.getMockImplementation()!;
    invokeMock.mockImplementation(async (command: string, args?: unknown) =>
      command === "ai_turn_start" ? value : base(command, args));
  }
  const lastRow = () => ([...document.querySelectorAll(".ai-message.system")] as HTMLElement[]).at(-1);

  it.each([
    [{ cancelled: false, done: 64, planned: 64, edits: 3, committed: true }, "ai.cap.kept", false],
    [{ cancelled: false, done: 2, planned: 2, edits: 0 }, "ai.cap.keptNone", true],
    [{ cancelled: false, done: 2, planned: 2, edits: 2, committed: true, timed_out: true }, "ai.timeout.kept", true],
    [{ cancelled: false, done: 1, planned: 2, edits: 1, committed: true, failure_kept: true }, "ai.failure.kept", true],
    [{ cancelled: true, done: 1, planned: 3, not_run: 2, edits: 1, committed: true }, "ai.cancel.kept", false],
  ] as const)("words %o from the locale table and offers what it allows", async (stop, key, retry) => {
    outcome({ text: "stopped", failed: true, stop });
    await mount();
    await send("build the arrangement");
    await settle();
    expect(lastRow()?.textContent).toContain(i18n.t(key as I18nKey, { edits: stop.edits }));
    expect(!!document.querySelector(".ai-turn-retry")).toBe(retry);
    expect(document.querySelector(".ai-turn-continue")).toBeTruthy();
  });

  it("puts a commit failure first and offers neither Retry nor a silent success", async () => {
    outcome({
      text: "stopped", failed: true,
      stop: { cancelled: false, done: 1, planned: 1, edits: 1, commit_error: "disk full" },
    });
    await mount();
    await send("rename the bass");
    await settle();
    expect(lastRow()?.textContent).toContain(i18n.t("ai.failure.commitFailed", { error: "disk full" }));
    expect(document.querySelector(".ai-turn-retry")).toBeNull();
    expect(answers()).toHaveLength(0);
  });

  it("names a provider failure and withholds Retry when resending cannot help", async () => {
    outcome({ text: "bad key", failed: true, error: { kind: "auth", message: "bad key", retryable: false } });
    await mount();
    await send("describe the mix");
    await settle();
    expect(lastRow()?.textContent).toContain("Error: bad key");
    expect(document.querySelector(".ai-turn-retry")).toBeNull();
    // The message goes back to the composer so nothing typed is lost.
    expect(input().value).toBe("describe the mix");
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
    const dispatched: string[] = [];
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
    backend.tool = async ({ tool }) => {
      dispatched.push(tool);
      // The user clicks Cancel while the second call is still running.
      if (dispatched.length === 2) fireEvent.click(cancelButton()!);
      return { revision: 1 };
    };
    await mount();
    await send("add five tracks");

    await waitFor(() => expect(document.body.textContent).toContain("Cancelled. Applied edits kept: 2"));
    await waitFor(() => expect(input().disabled).toBe(false));
    expect(systemRows()).toHaveLength(1);
    const notice = systemRows()[0].textContent ?? "";
    expect(notice).toContain("Cancelled. Applied edits kept: 2");
    // A cancelled turn is a notice, not an answer, and not an error bubble.
    expect(answers()).toHaveLength(0);
    expect(document.body.textContent).not.toContain("Request cancelled");
    // The two calls that returned are the edits the notice counts; the rest
    // never ran.
    expect(dispatched).toEqual(["tracks_add", "tracks_add"]);
  });

  it("cancels before any call runs and says the project is unchanged", async () => {
    const dispatched: string[] = [];
    let providerStarted!: () => void;
    const provider = new Promise<void>((resolve) => { providerStarted = resolve; });
    backend.chat = () => {
      providerStarted();
      return new Promise(() => {});
    };
    backend.tool = async ({ tool }) => {
      dispatched.push(tool);
      return { tracks: [] };
    };
    await mount();
    await send("describe the mix");
    const cancel = await waitFor(() => {
      const button = cancelButton();
      expect(button).toBeTruthy();
      return button!;
    });
    // The turn owns the composer before it reaches the provider; wait for the
    // request so Stop names a turn the backend holds.
    await provider;
    fireEvent.click(cancel);

    await waitFor(() => expect(document.body.textContent).toContain("Cancelled before any edit was applied"));
    await waitFor(() => expect(input().disabled).toBe(false));
    expect(systemRows()).toHaveLength(1);
    expect(systemRows()[0].querySelector(".ai-message-content")?.textContent).toBe("Cancelled before any edit was applied; the project is unchanged.");
    expect(answers()).toHaveLength(0);
    expect(document.body.textContent).not.toContain("Request cancelled");
    expect(dispatched).toEqual([]);
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
  it("offers one Retry on a transport failure, restores the draft and the attachment, and sends the system prompt again", async () => {
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

    backend.chat = async () => ({ role: "assistant", content: "The bass line is in." });
    fireEvent.click(retryButton()!);
    await waitFor(() => expect(document.body.textContent).toContain("The bass line is in."));

    // A resumed turn sends only the system prompt; the backend replays the
    // saved conversation, so the question is not sent twice.
    expect(backend.chatCalls.at(-1)!.messages.map((message) => message.role)).toEqual(["system"]);
    expect(userRows()).toHaveLength(1);
    expect(input().value).toBe("add a bass line");
    // One shot: the row it came from stops offering it.
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

  it("keeps a failed turn's edits and continues without replaying the call", async () => {
    const MUTATING = [{ name: "tracks_add", description: "Add a track", area: "tracks", mutation: true, inputSchema: {} }];
    backend.tools = MUTATING;
    const dispatched: string[] = [];
    backend.tool = async ({ tool }) => {
      dispatched.push(tool);
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
    expect(dispatched).toEqual(["tracks_add"]);
    expect(lastSystemRow()?.textContent).toContain("The turn failed. Applied edits kept: 1");
    expect(lastSystemRow()?.textContent).toContain("Right-click the user message to roll back this turn.");

    // Continue sends only the system prompt; the backend replays the saved
    // conversation, so the edit is not applied a second time.
    backend.chat = async (): Promise<unknown> => ({ role: "assistant", content: "Track added." });
    fireEvent.click(continueButton()!);
    await waitFor(() => expect(document.body.textContent).toContain("Track added."));
    expect(backend.chatCalls.at(-1)!.messages.map((message) => message.role)).toEqual(["system"]);
    expect(dispatched).toEqual(["tracks_add"]);
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

    // Continue sends only the system prompt; the backend replays the saved
    // conversation, including the call that never ran.
    const resumed = backend.chatCalls.at(-1)!;
    expect(resumed.messages.map(message => message.role)).toEqual(["system"]);
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
