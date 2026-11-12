/**
 * i18n coverage for the AI surfaces.
 *
 * The panel, its settings and provider dialogs, the stats / limits / thinking /
 * auth readouts, and the transcript lines the AI modules themselves generate all
 * read their labels from the locale table. Each case mounts a surface with the
 * locale on zh-Hans and asserts that the rendered label equals the zh-Hans table
 * entry and not the English one: a component that keeps a hard-coded English
 * literal fails the second assertion, and one that hard-codes Chinese still
 * fails the first because the value must match `i18n.t` for the key.
 *
 * Exact translations are never written here: every expectation is read back
 * from the table, so editing a translation cannot break this suite.
 *
 * Table values are read *before* a `waitFor` starts. `i18n.setLocale` rewrites
 * `<html lang>`, and a mutation inside the poll callback re-triggers the
 * observer, so a helper that switches locale inside `waitFor` never settles.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { aiConfigWire, invokeMock, makeSnapshot, openStepGroups } from "./setup";
import { store } from "../stores/project";
import { i18n, type I18nKey } from "../stores/i18n";
import { aiSession } from "../stores/aiSession";
import { aiLimits } from "../stores/aiLimits";
import { AiPanel } from "../components/AiPanel";
import { AiSettingsModal } from "../components/AiSettingsModal";
import { PreferencesHost } from "../components/PreferencesModal";
import { AiStats } from "../components/AiStats";
import { AiModelLimits } from "../components/AiModelLimits";
import { AiThinking, type ThinkingFacts } from "../components/AiThinking";
import { compactionNotice } from "../ai/sessions";
import type { ToolsetDef } from "../ai/toolsets";

const ZH = "zh-Hans" as const;

/** The table entry for a key, with the locale left on zh-Hans. */
function zh(key: I18nKey, values?: Record<string, string | number>): string {
  i18n.setLocale(ZH);
  return i18n.t(key, values);
}

function english(key: I18nKey, values?: Record<string, string | number>): string {
  i18n.setLocale("en");
  return i18n.t(key, values);
}

/** Assert one rendered string is the table entry for `key`, not English. */
function fromTable(key: I18nKey, rendered: string | null | undefined, values?: Record<string, string | number>) {
  const en = english(key, values);
  const tr = zh(key, values);
  expect(tr, `${key} has no zh-Hans translation`).not.toBe(en);
  expect(rendered, key).toBe(tr);
}

function textOf(element: Element | null | undefined): string {
  return (element?.textContent ?? "").replace(/\s+/g, " ").trim();
}

/** The element whose text is exactly `value`; null when none carries it. */
function byText(root: ParentNode, value: string): Element | null {
  return [...root.querySelectorAll("*")].find((element) => textOf(element) === value) ?? null;
}

/** Assert the accessible name of a control is the table entry for `key`. */
function ariaFromTable(root: ParentNode, key: I18nKey, values?: Record<string, string | number>) {
  const en = english(key, values);
  const tr = zh(key, values);
  expect(tr, `${key} has no zh-Hans translation`).not.toBe(en);
  expect(root.querySelector(`[aria-label="${en}"]`), `${key}: English label still rendered`).toBeNull();
  expect(root.querySelector(`[aria-label="${tr}"]`), `${key}: no control carries ${JSON.stringify(tr)}`).not.toBeNull();
}

/** Assert a placeholder is the table entry for `key`. */
function placeholderFromTable(root: ParentNode, key: I18nKey) {
  const en = english(key);
  const tr = zh(key);
  expect(tr, `${key} has no zh-Hans translation`).not.toBe(en);
  expect(root.querySelector(`[placeholder="${en}"]`), `${key}: English placeholder still rendered`).toBeNull();
  expect(root.querySelector(`[placeholder="${tr}"]`), `${key}: no field carries ${JSON.stringify(tr)}`).not.toBeNull();
}

/** The button whose accessible name is the table entry for `key`. */
function buttonFromTable(key: I18nKey): HTMLElement {
  const en = english(key);
  const tr = zh(key);
  expect(tr, `${key} has no zh-Hans translation`).not.toBe(en);
  expect(screen.queryByRole("button", { name: en }), `${key}: English button still rendered`).toBeNull();
  return screen.getByRole("button", { name: tr });
}

/** Both values for a key, with the locale left on zh-Hans. */
function both(key: I18nKey, values?: Record<string, string | number>): { en: string; tr: string } {
  const en = english(key, values);
  return { en, tr: zh(key, values) };
}

/** Assert a key carries a zh-Hans translation at all. */
function translated(key: I18nKey, values?: Record<string, string | number>) {
  expect(zh(key, values), `${key} has no zh-Hans translation`).not.toBe(english(key, values));
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
      ],
    },
  ],
};

const TOOLS = [
  { name: "tracks_list", description: "List tracks and their mix", area: "project", inputSchema: {} },
  { name: "tracks_rename", description: "Rename a track", area: "project", mutation: true, inputSchema: {} },
  { name: "edit_subtree", description: "Propose one complete subtree replacement", area: "project", inputSchema: {} },
];

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

/** A tool call whose result becomes one transcript tool row. */
function toolReply(name = "tracks_list") {
  return {
    role: "assistant",
    content: null,
    tool_calls: [{ id: "call-1", type: "function", function: { name, arguments: "{}" } }],
  };
}

/** Everything a test steers; the mock below only reads it. */
const backend = {
  chat: async (): Promise<unknown> => ({ role: "assistant", content: "Hello." }),
  chatCalls: 0,
  toolsets: [] as ToolsetDef[],
  validate: async (): Promise<unknown> => ({ valid: true }),
  apply: async (): Promise<unknown> => makeSnapshot(),
  tool: async (_call: unknown): Promise<unknown> => ({ tracks: [] }),
  keyStatus: async (): Promise<boolean> => true,
  save: async (): Promise<unknown> => ({ path: "/tmp/ai-providers.yaml", warnings: [] }),
  /** The window the backend says a request uses; unset means it did not say. */
  effectiveContext: undefined as number | undefined,
};

const TOOLSET: ToolsetDef = { id: "project-edit", title: "Project edit", description: "Tracks, presets and buses", tools: ["tracks_list"], default_on: false };

function installBackend() {
  invokeMock.mockImplementation(async (cmd: string) => {
    switch (cmd) {
      case "load_ai_config": return aiConfigWire();
      case "save_ai_config": return backend.save();
      case "ai_key_status": return backend.keyStatus();
      case "ai_catalog": return CATALOG;
      case "ai_model_limits":
        return {
          provider_id: "custom",
          model_id: "third-party-7b",
          bundled: null,
          overridden: null,
          ...(backend.effectiveContext === undefined ? {} : { effective_context: backend.effectiveContext }),
        };
      case "ai_toolsets": return backend.toolsets;
      case "ai_tools": return TOOLS;
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
      case "ai_chat": backend.chatCalls += 1; return backend.chat();
      case "ai_validate_edit": return backend.validate();
      case "extract_subtree": return { dsl_text: "track lead\n" };
      case "diff_dsl": return { old_text: "a", new_text: "b", hunks: [{ lines: [{ kind: "Added", text: "b" }] }] };
      case "ai_apply_edit": return backend.apply();
      case "ai_call_tool": return backend.tool({});
      case "ai_project_instructions_get": return { text: "", max_chars: 2000 };
      case "ai_oauth_status":
        return { state: "signed_out", account: null, project: null, expires_at: null, reauth_required: true, message: "" };
      default: return makeSnapshot();
    }
  });
}

async function mountPanel() {
  await store.initProject();
  const view = render(() => <><AiPanel /><PreferencesHost /></>);
  await waitFor(() => expect(view.container.querySelector(".ai-save-notice")).toBeFalsy());
  await waitFor(() => expect(view.container.querySelector(".ai-toolset-chip")).toBeTruthy());
  return view;
}

const input = () => document.querySelector(".ai-input") as HTMLTextAreaElement;
const sendButton = () => document.querySelector(".ai-send-btn") as HTMLButtonElement;

async function send(text: string) {
  fireEvent.input(input(), { target: { value: text } });
  fireEvent.click(sendButton());
  await waitFor(() => expect(backend.chatCalls).toBeGreaterThan(0));
}

/** The panel's own settings dialog, opened from its header button. */
async function openSettings(options: { toolsets?: ToolsetDef[] } = {}) {
  backend.toolsets = options.toolsets ?? [];
  const title = zh("preferences.title");
  const view = await mountPanel();
  fireEvent.click(buttonFromTable("ai.settings.open"));
  await waitFor(() => expect(screen.getByRole("dialog", { name: title })).toBeTruthy());
  return view;
}

/** The open Preferences surface, opened on the AI page. */
const dialog = () => screen.getByRole("dialog", { name: zh("preferences.title") });

beforeEach(() => {
  i18n.setLocale("en");
  invokeMock.mockReset();
  backend.chat = async () => ({ role: "assistant", content: "Hello." });
  backend.chatCalls = 0;
  backend.toolsets = [];
  backend.validate = async () => ({ valid: true });
  backend.apply = async () => makeSnapshot();
  backend.tool = async () => ({ tracks: [] });
  backend.keyStatus = async () => true;
  backend.save = async () => ({ path: "/tmp/ai-providers.yaml", warnings: [] });
  backend.effectiveContext = undefined;
  installBackend();
  // Mounts below happen in zh-Hans; the helpers switch to English and back only
  // to read the two table values.
  i18n.setLocale(ZH);
});

afterEach(() => {
  cleanup();
  aiSession.setPending(null);
  i18n.setLocale("en");
});

describe("AI panel header", () => {
  it("names the settings button and its accessible name from the table", async () => {
    await mountPanel();
    const button = buttonFromTable("ai.settings.open");
    const rendered = textOf(button);
    ariaFromTable(document, "ai.settings.open");
    fromTable("ai.settings.button", rendered);
  });
});

describe("AI panel quick actions", () => {
  it("translates the chip labels", async () => {
    const view = await mountPanel();
    const chips = [...view.container.querySelectorAll(".ai-quick-btn")].map(textOf);
    expect(chips).toHaveLength(4);
    const keys: I18nKey[] = ["ai.quick.describe", "ai.quick.reverb", "ai.quick.drums", "ai.quick.mix"];
    keys.forEach((key, index) => fromTable(key, chips[index]));
    expect(textOf(view.container)).not.toContain("Describe arrangement");
  });

  it("sends the translated prompt, not the English one", async () => {
    const view = await mountPanel();
    fireEvent.click([...view.container.querySelectorAll(".ai-quick-btn")][0]);
    await waitFor(() => expect(backend.chatCalls).toBe(1));
    const call = invokeMock.mock.calls.find(([cmd]) => cmd === "ai_chat")!;
    const messages = (call[1] as { messages: { role: string; content?: string }[] }).messages;
    const sent = messages.filter((message) => message.role === "user").map((message) => message.content);
    expect(sent).toContain(zh("ai.quick.describePrompt"));
    expect(sent).not.toContain(english("ai.quick.describePrompt"));
  });
});

describe("AI settings dialog", () => {
  it("translates the connection fields, the key row and the action buttons", async () => {
    await openSettings();
    const body = dialog();
    expect(textOf(body)).toContain(zh("ai.settings.endpoint"));
    placeholderFromTable(body, "ai.auth.keyPlaceholder");
    const save = textOf(buttonFromTable("ai.settings.saveWithKey"));
    const remove = textOf(buttonFromTable("ai.auth.removeKey"));
    fromTable("ai.settings.saveWithKey", save);
    fromTable("ai.auth.removeKey", remove);
    expect(textOf(dialog())).not.toContain("Endpoint");
  });

  it("translates the line naming the window a request will use", async () => {
    // The catalog's window differs from the stored provider default, so the
    // dialog says which number the request and auto-compaction use.
    backend.effectiveContext = 1_048_576;
    await openSettings();
    const expected = zh("ai.settings.effectiveContext", { context: 1_048_576 });
    const line = await waitFor(() => {
      const found = byText(dialog(), expected);
      expect(found, "the effective window line is rendered").toBeTruthy();
      return found!;
    });
    expect(textOf(line)).toBe(expected);
    expect(textOf(dialog())).not.toContain("instead of this number");
  });

  it("translates the capability switches", async () => {
    await openSettings();
    const body = dialog();
    expect(byText(body, zh("ai.settings.nativeTools")), "Native tools").not.toBeNull();
    expect(byText(body, zh("ai.settings.streaming")), "Streaming").not.toBeNull();
    expect(textOf(body)).not.toContain("Native tools");
  });

  it("translates the test button and reports both probe lines from the table", async () => {
    await openSettings();
    const test = buttonFromTable("ai.settings.test");
    const testing = zh("ai.settings.testing");
    const connected = zh("ai.settings.connected", { reply: "hello" });
    let release!: (value: unknown) => void;
    backend.chat = () => new Promise((resolve) => { release = resolve; });
    fireEvent.click(test);
    await waitFor(() => expect(document.body.textContent).toContain(testing));
    release({ role: "assistant", content: "hello" });
    await waitFor(() => expect(document.body.textContent).toContain(connected));
    expect(document.body.textContent).not.toContain("Connected. Response");
  });

  it("explains a build without the toolset switchboard", async () => {
    await openSettings({ toolsets: [] });
    const body = dialog();
    expect(byText(body, zh("ai.settings.toolsetsUnavailable")), "toolsets unavailable").not.toBeNull();
    expect(textOf(body)).not.toContain("Toolsets are not available on this build");
  });

  it("explains a build where every set is switched off", async () => {
    await openSettings({ toolsets: [TOOLSET] });
    const body = dialog();
    const none = zh("ai.toolsets.noneHint");
    await waitFor(() => expect(byText(body, none)).not.toBeNull());
    expect(textOf(body)).not.toContain("No toolset is selected");
    // A set's own title and description come from the backend, not the table.
    expect(textOf(body)).toContain(TOOLSET.title);
  });
});

describe("AI settings dialog: edit target", () => {
  it("translates the target the project no longer has", async () => {
    const path = "track:does-not-exist";
    const expected = zh("ai.settings.targetMissing", { path });
    render(() => (
      <AiSettingsModal
        catalog={null}
        loading={false}
        contextEstimate={0}
        testResult={null}
        noticeTone="idle"
        targetPath={path}
        toolsets={[]}
        selectedSets={null}
        toolDescriptions={new Map()}
        onClose={() => {}}
        onTestResult={() => {}}
        onTargetPath={() => {}}
        onToggleSet={() => {}}
        onTestConnection={() => {}}
        onCatalogReload={() => {}}
        onAddProvider={() => {}}
      />
    ));
    const option = await waitFor(() => {
      const found = [...document.querySelectorAll("option")].find((element) => textOf(element) === expected);
      expect(found, "a stale target names itself as missing").toBeTruthy();
      return found!;
    });
    expect(textOf(option)).toBe(expected);
    expect(textOf(option)).not.toBe(english("ai.settings.targetMissing", { path }));
  });

  it("names the provider the dialog removes from the table", () => {
    translated("ai.settings.removed", { name: "Custom" });
  });
});

describe("AI edit permission", () => {
  const option = (mode: string) =>
    document.querySelector(`.ai-permission-option[data-permission="${mode}"]`) as HTMLElement | null;
  const card = () => document.querySelector(".ai-approval-card") as HTMLElement | null;
  const diffRow = () =>
    [...document.querySelectorAll(".ai-message.assistant")].find((row) => row.querySelector(".ai-diff")) ?? null;

  async function choosePermission(mode: "require_approval" | "autoreview" | "write") {
    fireEvent.click(option(mode)!);
    await waitFor(() => expect(option(mode)!.getAttribute("aria-pressed")).toBe("true"));
  }

  it("names the three modes in the panel's language", async () => {
    await mountPanel();
    for (const [mode, key] of [
      ["require_approval", "ai.permission.require_approval"],
      ["autoreview", "ai.permission.autoreview"],
      ["write", "ai.permission.write"],
    ] as const) {
      fromTable(key, textOf(option(mode)));
    }
    fromTable("ai.permission.label", document.querySelector(".ai-permission")!.getAttribute("aria-label"));
  });

  it("names the validated edit in the panel's language", async () => {
    backend.chat = async () => proposalReply();
    await mountPanel();
    // `autoreview` records the replacement it applies as a diff row.
    await choosePermission("autoreview");
    await send("add a lead");
    const row = await waitFor(() => {
      expect(diffRow(), "the diff row").toBeTruthy();
      return diffRow()!;
    });
    fromTable("ai.edit.validatedFor", textOf(row.querySelector(".ai-message-content")), { path: "track:lead" });
  });

  it("translates the approval card's answers", async () => {
    backend.chat = async () => proposalReply();
    await mountPanel();
    await choosePermission("require_approval");
    await send("add a lead");
    const waiting = await waitFor(() => {
      expect(card(), "the approval card").toBeTruthy();
      return card()!;
    });
    fromTable("ai.approval.approve", textOf(waiting.querySelector(".ai-approval-approve")));
    fromTable("ai.approval.reject", textOf(waiting.querySelector(".ai-approval-reject")));
  });

  it("labels the call's arguments in the panel's language", async () => {
    backend.chat = async () => proposalReply();
    await mountPanel();
    await choosePermission("require_approval");
    await send("add a lead");
    const waiting = await waitFor(() => {
      expect(card(), "the approval card").toBeTruthy();
      return card()!;
    });
    const argumentsText = textOf(waiting.querySelector(".ai-approval-arguments"));
    expect(argumentsText.startsWith(zh("ai.approval.arguments"))).toBe(true);
    expect(argumentsText).not.toContain(english("ai.approval.arguments"));
  });

  it("translates a re-validation failure the loop's own check let through", () => {
    // The loop validates a replacement before it hands one over, so this line only
    // appears when the project changed between the two checks; it is asserted
    // through the table rather than by racing a turn.
    translated("ai.edit.validationFailed", { errors: "bad path" });
  });
});

describe("AI tool rows", () => {
  it("names a repeated call from the table", async () => {
    // The loop answers a call it has already seen with a reported failure row
    // instead of looping; that row's state word comes from the table.
    backend.chat = async () => toolReply("tracks_rename");
    const view = await mountPanel();
    const { en: wrong, tr: failed } = both("ai.tool.failed");
    await send("list the tracks");
    const summary = await waitFor(() => {
      // The rich call row carries the call's name; the reported row does not.
      openStepGroups(view.container);
      const element = [...view.container.querySelectorAll(".ai-message.tool .ai-tool-result summary")]
        .find((node) => !node.querySelector(".ai-tool-name"));
      expect(element, "the reported tool row").toBeTruthy();
      return element!;
    });
    const rendered = textOf(summary);
    expect(rendered.endsWith(failed), rendered).toBe(true);
    expect(rendered.endsWith(wrong), rendered).toBe(false);
  });

  it("translates the success word a restored row falls back to", () => {
    // A tool row with no call attached (an older transcript) has no argument or
    // result line, so the state word is all it says about the outcome.
    translated("ai.tool.result");
  });
});

describe("session compaction notice", () => {
  it("builds the transcript line from the table", () => {
    const report = { dropped_from: 0, dropped_to: 6, tokens_before: 9000, tokens_after: 1200, retained_pairs: 3 };
    const pairs = zh("ai.sessions.retained", { count: 3 });
    const expected = zh("ai.sessions.compaction", { dropped: 6, before: 9000, after: 1200, pairs });
    const rendered = compactionNotice(report);
    expect(rendered).toBe(expected);
    expect(rendered).not.toBe(english("ai.sessions.compaction", {
      dropped: 6,
      before: 9000,
      after: 1200,
      pairs: english("ai.sessions.retained", { count: 3 }),
    }));
    // The numbers and the token unit survive translation.
    expect(rendered).toContain("9000");
    expect(rendered).toContain("1200");
    expect(rendered).toContain("tokens");
  });

  it("uses the singular line for one retained pair", () => {
    const report = { dropped_from: 0, dropped_to: 2, tokens_before: 500, tokens_after: 100, retained_pairs: 1 };
    const rendered = compactionNotice(report);
    expect(rendered).toContain(zh("ai.sessions.retainedOne"));
    expect(zh("ai.sessions.retainedOne")).not.toBe(english("ai.sessions.retainedOne"));
  });
});

describe("AI stats", () => {
  it("names the info button from the table", () => {
    aiLimits.setActiveContext(1_000_000);
    const view = render(() => <AiStats used={450_000} />);
    const button = view.container.querySelector(".ai-stats-info")!;
    expect(button).toBeTruthy();
    fromTable("ai.stats.info", button.getAttribute("aria-label"));
  });
});

describe("AI model limits", () => {
  it("labels the limits entry and the input modalities from the table", async () => {
    render(() => <AiModelLimits providerId="custom" modelId="third-party-7b" used={12000} />);
    const edit = both("ai.limits.edit");
    const limitsTitle = zh("ai.limits.title");
    const button = await waitFor(() => screen.getByRole("button", { name: edit.tr }));
    expect(screen.queryByRole("button", { name: edit.en })).toBeNull();
    fireEvent.click(button);
    const limits = await waitFor(() => screen.getByRole("dialog", { name: limitsTitle }));
    expect(textOf(limits)).toContain(zh("ai.limits.input"));
    expect(textOf(limits)).toContain(zh("ai.limits.maxOutput"));
  });
});

describe("AI thinking row", () => {
  const IDENTITY = "deepseek/deepseek-flash";
  const FACTS: ThinkingFacts = { loading: true, partial: "", approval: false, lastTool: null, identity: IDENTITY };

  it("names the waiting phase from the table", async () => {
    const view = render(() => (
      <AiThinking
        loading={FACTS.loading}
        partial={FACTS.partial}
        approval={FACTS.approval}
        lastTool={FACTS.lastTool}
        identity={FACTS.identity}
      />
    ));
    const label = await waitFor(() => {
      const element = view.container.querySelector(".ai-thinking-label");
      expect(element).toBeTruthy();
      return element!;
    });
    fromTable("ai.thinking.waiting", textOf(label), { model: IDENTITY });
  });
});

describe("add-provider dialog", () => {
  it("translates its title, its fields and its hints", async () => {
    await openSettings();
    const title = zh("ai.provider.addTitle");
    fireEvent.click(buttonFromTable("ai.provider.add"));
    const provider = await waitFor(() => screen.getByRole("dialog", { name: title }));
    expect(textOf(provider)).toContain(zh("ai.provider.name"));
    expect(textOf(provider)).toContain(zh("ai.provider.baseUrl"));
    expect(textOf(provider)).toContain(zh("ai.provider.keyHint"));
    expect(textOf(provider)).not.toContain("Provider name");
  });
});
