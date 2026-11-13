/**
 * The project instructions area in AI settings: it loads the note of the open
 * project, counts it against the store's cap, saves it, and refuses an
 * over-limit note with a readable message instead of truncating it.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { AiSettingsModal, type NoticeTone } from "../components/AiSettingsModal";
import { loadConfig } from "../ai/config";
import { noteIsOverLimit } from "../ai/projectInstructions";
import { i18n } from "../stores/i18n";
import type { Catalog } from "../ai/catalog";
import { resetFakeTurns, runFakeTurn, type TurnStartArgs } from "./helpers/fakeTurn";

const PROJECT = "/tmp/panelg9/song.ocp";
const SAVED = "Always print stems at -14 LUFS; this song is in D minor.";

/** What the store holds; the mocked commands read and write it. */
let stored: { text: string; limit: number | null };
/** The texts the store was asked to save. */
let writes: string[] = [];
/** When set, the store refuses a write with this message. */
let refuseWrite: string | null = null;
/** Every `ai_chat` payload the panel sent. */
let chats: Record<string, any>[] = [];

const SETS = [
  { id: "dsl-edit", title: "DSL edit", description: "Read a subtree and propose one replacement", tools: ["edit_subtree"], default_on: true },
  { id: "project-edit", title: "Project edit", description: "Tracks, presets and buses", tools: ["tracks_list"], default_on: true },
];

/** The registry rows the panel advertises; no destructive tool is needed. */
const TOOLS = [
  { name: "edit_subtree", description: "Propose one complete subtree replacement", area: "project", inputSchema: {} },
  { name: "tracks_list", description: "List tracks and their mix", area: "project", inputSchema: {} },
];

const CATALOG = {
  version: "fixture",
  providers: [
    {
      id: "deepseek",
      name: "DeepSeek",
      kind: "openai_chat_completions",
      hosts: ["api.deepseek.com"],
      auth_providers: [],
      models: [{
        id: "deepseek-flash", name: "DeepSeek Flash", context: 1000000, max_output: 8192,
        capabilities: { text: true, audio_in: false, audio_out: false, native_tool_calling: true, streaming: true },
        status: "active",
      }],
    },
  ],
} as unknown as Catalog;

function mockBackend() {
  invokeMock.mockImplementation(async (cmd: string, args?: any) => {
    // The panel asks once at mount whether a previous turn left a batch open.
    if (cmd === "ai_batch_recover") return null;
    if (cmd === "load_ai_config") {
      return {
        version: 2,
        active_provider: "deepseek",
        providers: [{
          id: "deepseek", name: "DeepSeek", kind: "openai_chat_completions", auth: "api_key",
          endpoint: "https://api.deepseek.com/v1/chat/completions", model: "deepseek-flash",
          max_tokens: 4096, temperature: 0.3, context_length: 1000000,
          capabilities: { text: true, audio_in: false, audio_out: false, native_tool_calling: true, streaming: true },
        }],
      };
    }
    if (cmd === "ai_key_status") return true;
    if (cmd === "ai_catalog") return CATALOG;
    if (cmd === "ai_model_limits") return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
    if (cmd === "save_ai_config") return { path: "/tmp/ai-providers.yaml", warnings: [] };
    if (cmd === "ai_toolsets") return SETS;
    if (cmd === "ai_tools") return TOOLS;
    if (cmd === "ai_build_context") return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
    if (cmd === "ai_turn_start") {
      const typed = (args ?? {}) as Record<string, any>;
      chats.push(typed);
      return runFakeTurn(typed as TurnStartArgs, {
        chat: async () => ({ role: "assistant", content: "Hello." }),
      });
    }
    if (cmd === "ai_turn_questions") return [];
    if (cmd === "ai_cancel") return undefined;
    if (cmd === "sessions_list") return [];
    if (cmd === "sessions_new") return {
      id: "s1", title: "Test conversation", created: "2026-10-28T09:00:00.000Z",
      updated: "2026-10-28T09:00:00.000Z", model: "deepseek-flash", provider: "deepseek",
      workspace_key: "3f2a1b0c", workspace: "/tmp/panelg9", turns: 1, tokens: 20,
    };
    if (cmd === "get_project") return { ...makeSnapshot(), file_path: PROJECT };
    if (cmd === "ai_project_instructions_get") {
      return {
        project: stored.limit === null ? null : PROJECT,
        text: stored.text,
        ...(stored.limit === null ? {} : { max_chars: stored.limit }),
      };
    }
    if (cmd === "ai_project_instructions_set") {
      if (refuseWrite) throw new Error(refuseWrite);
      writes.push(String(args?.text ?? ""));
      return { project: PROJECT, text: String(args?.text ?? ""), max_chars: stored.limit ?? 4000 };
    }
    return makeSnapshot();
  });
}

function modalProps(overrides: Partial<Parameters<typeof AiSettingsModal>[0]> = {}) {
  return {
    catalog: CATALOG,
    loading: false,
    contextEstimate: 1200,
    testResult: null,
    noticeTone: "idle" as NoticeTone,
    targetPath: "project",
    toolsets: [],
    selectedSets: [] as string[],
    toolDescriptions: new Map<string, string>(),
    onClose: () => {},
    onTestResult: () => {},
    onTargetPath: () => {},
    onToggleSet: () => {},
    onTestConnection: () => {},
    onCatalogReload: () => {},
    onAddProvider: () => {},
    ...overrides,
  };
}

async function mountDialog() {
  await store.initProject();
  await loadConfig();
  render(() => <AiSettingsModal {...modalProps()} />);
  return await screen.findByLabelText("Project instructions") as HTMLTextAreaElement;
}

const count = () => screen.getByTestId("ai-instructions-count");
const input = () => document.querySelector(".ai-input") as HTMLInputElement;
const sendButton = () => document.querySelector(".ai-send-btn") as HTMLButtonElement;

/** Sends one message through the panel and returns the system prompt it sent. */
async function sentSystemPrompt(note: string): Promise<string> {
  stored.text = note;
  await store.initProject();
  const view = render(() => <AiPanel />);
  await waitFor(() => expect(view.container.querySelector(".ai-save-notice")).toBeFalsy());
  const before = chats.length;
  fireEvent.input(input(), { target: { value: "check the mix" } });
  fireEvent.click(sendButton());
  await waitFor(() => expect(chats.length).toBe(before + 1));
  return String(chats[before].messages[0].content);
}
const saveButton = () => screen.getByRole("button", { name: "Save instructions" });
const setCalls = () => invokeMock.mock.calls.filter(([command]) => command === "ai_project_instructions_set");

beforeEach(() => {
  invokeMock.mockReset();
  stored = { text: SAVED, limit: 4000 };
  writes = [];
  chats = [];
  refuseWrite = null;
  mockBackend();
});

afterEach(() => {
  cleanup();
  resetFakeTurns();
});

describe("project instructions in AI settings", () => {
  it("loads the open project's note and counts its characters", async () => {
    const field = await mountDialog();
    await waitFor(() => expect(field.value).toBe(SAVED));
    expect(count().textContent).toBe(`${SAVED.trim().length} / 4000 characters`);
    expect(count().classList.contains("ai-instructions-over")).toBe(false);
    const read = invokeMock.mock.calls.find(([command]) => command === "ai_project_instructions_get");
    expect(read?.[1]).toEqual({ project: PROJECT });
  });

  it("saves the note and reloads what the store kept", async () => {
    const field = await mountDialog();
    await waitFor(() => expect(field.value).toBe(SAVED));
    const edited = "This song is in D minor; do not modulate.";
    fireEvent.input(field, { target: { value: edited } });
    expect(count().textContent).toBe(`${edited.length} / 4000 characters`);

    fireEvent.click(saveButton());
    await waitFor(() => expect(screen.getByText("Saved. The next turn uses these instructions.")).toBeTruthy());
    expect(writes).toEqual([edited]);
    expect(setCalls()[0][1]).toEqual({ project: PROJECT, text: edited });

    // The note survives the dialog: reopening reads the store's copy.
    stored.text = edited;
    cleanup();
    const reopened = await mountDialog();
    await waitFor(() => expect(reopened.value).toBe(edited));
  });

  it("refuses a note over the cap, names both numbers, and saves nothing", async () => {
    const field = await mountDialog();
    await waitFor(() => expect(field.value).toBe(SAVED));
    fireEvent.input(field, { target: { value: "x".repeat(4001) } });
    expect(count().textContent).toBe("4001 / 4000 characters");
    expect(count().classList.contains("ai-instructions-over")).toBe(true);

    fireEvent.click(saveButton());
    const refusal = await screen.findByText(/The note is 4001 characters; the limit is 4000/);
    expect(refusal.textContent).toContain("about 1000 prompt tokens");
    expect(setCalls().length).toBe(0);
    expect(writes).toEqual([]);
  });

  it("uses the cap the store reports, not a number of its own", async () => {
    stored.limit = 10;
    const field = await mountDialog();
    await waitFor(() => expect(field.value).toBe(SAVED));
    expect(count().textContent).toBe(`${SAVED.trim().length} / 10 characters`);
    expect(count().classList.contains("ai-instructions-over")).toBe(true);

    fireEvent.click(saveButton());
    await screen.findByText(new RegExp(`The note is ${SAVED.trim().length} characters; the limit is 10`));
    expect(setCalls().length).toBe(0);
  });

  it("shows the store's own refusal when the project has no file", async () => {
    stored = { text: "", limit: null };
    refuseWrite = "this project has no file yet; save the project first, then write its instructions";
    const field = await mountDialog();
    fireEvent.input(field, { target: { value: "no reverb on the lead" } });
    fireEvent.click(saveButton());
    const refusal = await screen.findByText(/this project has no file yet/);
    expect(refusal.textContent).toContain("save the project first");
  });

  it("sends the open project's note in every request, and never another project's", async () => {
    const a = "Always print stems at -14 LUFS.";
    const b = "This song is in D minor; do not modulate.";
    const systemA = await sentSystemPrompt(a);
    expect(systemA.split(a).length - 1).toBe(1);
    expect(systemA).toContain("## Project instructions");
    expect(systemA).not.toContain("do not modulate");

    cleanup();
    const systemB = await sentSystemPrompt(b);
    expect(systemB.split(b).length - 1).toBe(1);
    expect(systemB).not.toContain("-14 LUFS");
  });

  it("sends the same prompt as a project with no note when the note is empty", async () => {
    const plain = await sentSystemPrompt("");
    cleanup();
    expect(plain).not.toContain("## Project instructions");
    const blank = await sentSystemPrompt("   ");
    expect(blank).toBe(plain);
  });

  it("translates the area's labels and its refusal", async () => {
    for (const key of i18n.keys().filter((name) => name.startsWith("ai.settings.instructions"))) {
      expect(i18n.has("zh-Hans", key), `zh-Hans ${key}`).toBe(true);
      expect(i18n.has("zh-Hant", key), `zh-Hant ${key}`).toBe(true);
    }
  });
});

describe("the note's character count", () => {
  it("counts characters the way the store does, not UTF-16 units", () => {
    const limit = 4000;
    // Two units in JavaScript, one character to the store.
    expect(noteIsOverLimit("😀".repeat(3000), limit)).toBe(false);
    expect(noteIsOverLimit("😀".repeat(4001), limit)).toBe(true);
    expect(noteIsOverLimit("x".repeat(4000), limit)).toBe(false);
    expect(noteIsOverLimit("x".repeat(4001), limit)).toBe(true);
    // The count is of the trimmed text, as the store trims before it measures.
    expect(noteIsOverLimit(`  ${"x".repeat(4000)}\n`, limit)).toBe(false);
  });

  it("shows an emoji note's code points, not its UTF-16 units", async () => {
    const field = await mountDialog();
    await waitFor(() => expect(field.value).toBe(SAVED));
    // 2,500 emoji: 2,500 characters to the store, 5,000 UTF-16 units.
    const emoji = "😀".repeat(2500);
    fireEvent.input(field, { target: { value: emoji } });

    expect(count().textContent).toBe("2500 / 4000 characters");
    expect(count().classList.contains("ai-instructions-over")).toBe(false);
    // The number the field shows is the number the store's gate uses, so a note
    // the store accepts is never refused by what the field displays.
    fireEvent.click(saveButton());
    await waitFor(() => expect(writes).toEqual([emoji]));
  });
});
