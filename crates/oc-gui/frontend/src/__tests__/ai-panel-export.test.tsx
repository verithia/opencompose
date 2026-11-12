/**
 * Transcript export: the Markdown document a person reads, the JSON document a
 * tool reads, and the two ways the panel hands them over.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot, openStepGroups } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { TooltipLayer } from "../components/Tooltip";
import { i18n } from "../stores/i18n";
import { aiSession } from "../stores/aiSession";
import type { WireMessage } from "../ai/agent";
import type { MediaAttachment } from "../ai/media";
import type { SessionMeta } from "../ai/sessions";
import type { ChatMessage } from "../ai/transcript";
import {
  RESULT_LIMIT_BYTES,
  boundedResult,
  exportFileName,
  exportJson,
  exportMarkdown,
} from "../ai/export";

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

const AT = Date.UTC(2026, 9, 28, 9, 0, 12);
const ISO = new Date(AT).toISOString();

const META: SessionMeta = {
  id: "s2",
  title: "punchier kick",
  created: "2026-10-28T09:00:00.000Z",
  updated: "2026-10-28T09:05:00.000Z",
  model: "gemini-4-argon",
  provider: "google",
  workspace_key: "3f2a1b0c",
  workspace: "/Users/me/songs",
  project_path: "/Users/me/songs/song.ocp",
  turns: 2,
  tokens: 400,
};

const ATTACHMENT: MediaAttachment = {
  kind: "audio", name: "take.wav", path: "/tmp/take.wav", mime: "audio/wav",
  bytes: 320000, duration_secs: 10, sha256: "a".repeat(64),
};

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

const LARGE_RESULT = `{"ok":true,"text":"${"a".repeat(RESULT_LIMIT_BYTES + 4000)}"}`;
const IMAGE_RESULT = `{"image":"data:image/png;base64,${"A".repeat(200)}","ok":true}`;

function rows(): ChatMessage[] {
  return [
    { role: "user", content: "make the kick punchier", timestamp: AT, media: [ATTACHMENT] },
    { role: "assistant", content: "Trying a gain cut.", timestamp: AT },
    {
      role: "tool", content: '{"ok":true,"notes":"gain -2 dB"}', timestamp: AT,
      tool: { name: "tracks_mix", args: '{"track":"kick","gain_db":-2}', result: '{"ok":true,…}' },
    },
    { role: "system", content: "Context compacted: 24 messages became a summary (12400 → 1800 tokens); 6 recent tool calls kept as text.", timestamp: AT },
    { role: "user", content: "Conversation summary written 2026-10-29T07:52:58.782Z. Everything older is replaced by this message.", timestamp: AT, checkpoint: true },
    { role: "tool", content: IMAGE_RESULT, timestamp: AT, tool: { name: "render_range", args: '{"bars":"1-4"}' } },
    { role: "tool", content: LARGE_RESULT, timestamp: AT, tool: { name: "render_range", args: '{"bars":"5-8"}' } },
    { role: "assistant", content: "Done: the kick is 2 dB quieter.", timestamp: AT },
  ];
}

// ── the documents themselves ───────────────────────────────────────────────

describe("the Markdown transcript", () => {
  it("carries every row in order with its time, its tool call and its result", () => {
    const markdown = exportMarkdown(META, rows());
    const order = [
      "# punchier kick",
      "- Workspace: /Users/me/songs",
      "- Provider: google · gemini-4-argon",
      `## User · ${ISO}`,
      "make the kick punchier",
      "## Assistant",
      "Trying a gain cut.",
      "## Tool · tracks_mix",
      "Arguments:",
      '{"track":"kick","gain_db":-2}',
      "Result:",
      '{"ok":true,"notes":"gain -2 dB"}',
      "## System",
      "Context compacted: 24 messages",
      "## Checkpoint",
      "Conversation summary written",
      "Done: the kick is 2 dB quieter.",
    ];
    let cursor = -1;
    for (const fragment of order) {
      const at = markdown.indexOf(fragment);
      expect(at, fragment).toBeGreaterThan(cursor);
      cursor = at;
    }
  });

  it("names an attachment instead of inlining its bytes", () => {
    const markdown = exportMarkdown(META, rows());
    expect(markdown).toContain("- take.wav (audio/wav, 312.5 KiB)");
    expect(markdown).not.toContain("data:image");
    expect(markdown).not.toContain("base64,");
    expect(markdown).not.toContain("A".repeat(200));
    // The one inline blob in the fixture is replaced by a visible marker.
    expect(markdown).toContain("[inline data omitted]");
  });

  it("clips only the result that is over the limit, and says so", () => {
    const markdown = exportMarkdown(META, rows());
    expect(markdown).toContain("[truncated: ");
    expect(markdown).toContain("first 8.0 KiB shown");
    // The shorter result is whole.
    expect(markdown).toContain('{"ok":true,"notes":"gain -2 dB"}');
    // Nothing near the whole large body survives.
    expect(markdown.length).toBeLessThan(RESULT_LIMIT_BYTES + 6000);
  });

  it("says that a restored row has no recorded time", () => {
    const markdown = exportMarkdown(META, [{ role: "user", content: "restored" }]);
    expect(markdown).toContain("A row without a time was loaded from the saved conversation");
    expect(markdown).toContain("## User\n\nrestored");
  });

  it("omits an inline attachment body before clipping the result around it", () => {
    // A body this large is clipped, so the marker has to be applied first or the
    // surviving half of the base64 has no closing quote left to match.
    const body = "A".repeat(40_000);
    const markdown = exportMarkdown(META, [
      { role: "tool", content: `{"base64":"${body}","ok":true}`, timestamp: AT, tool: { name: "render_range" } },
    ]);
    expect(markdown).not.toContain("A".repeat(400));
    expect(markdown).toContain("[omitted]");
    expect(markdown).toContain('"ok":true');
    expect(markdown.length).toBeLessThan(RESULT_LIMIT_BYTES);
  });

  it("marks an attachment whose file is gone", () => {
    const markdown = exportMarkdown(META, [
      { role: "user", content: "what do you hear?", timestamp: AT, media: [{ ...ATTACHMENT, missing: true }] },
    ]);
    expect(markdown).toContain("- take.wav (audio/wav, 312.5 KiB) — the file is missing");
  });

  it("prints a row whose recorded time cannot be read without a time", () => {
    // A time past the range a date can hold must not fail the whole export.
    const markdown = exportMarkdown(META, [
      { role: "user", content: "no time", timestamp: Number.MAX_SAFE_INTEGER },
    ]);
    expect(markdown).toContain("## User\n\nno time");
    expect(markdown).not.toContain("## User · ");
  });
});

describe("a clipped result", () => {
  it("cuts on a character boundary instead of splitting a sequence", () => {
    // The last character starts one byte before the cut: a raw byte slice would
    // decode half of it and write a replacement glyph into the export.
    const text = `${"a".repeat(RESULT_LIMIT_BYTES - 1)}中文`;
    const bounded = boundedResult(text);
    expect(bounded).not.toContain("\uFFFD");
    expect(bounded.startsWith("a".repeat(RESULT_LIMIT_BYTES - 1))).toBe(true);
    expect(bounded).toContain("[truncated: ");
  });
});

describe("the JSON transcript", () => {
  it("round-trips through JSON.parse and keeps the message order", () => {
    const parsed = JSON.parse(exportJson(META, CONVERSATION));
    expect(parsed.version).toBe(1);
    expect(parsed.meta.title).toBe("punchier kick");
    expect(parsed.meta.workspace).toBe("/Users/me/songs");
    expect(parsed.meta.project_path).toBe("/Users/me/songs/song.ocp");
    expect(parsed.meta.provider).toBe("google");
    expect(parsed.meta.model).toBe("gemini-4-argon");
    expect(parsed.meta.created).toBe(META.created);
    expect(parsed.meta.updated).toBe(META.updated);
    expect(parsed.messages.map((message: WireMessage) => message.role))
      .toEqual(["user", "assistant", "tool", "assistant"]);
    // The raw envelope survives, arguments included.
    expect(parsed.messages[1].tool_calls[0].function).toEqual({
      name: "tracks_mix", arguments: '{"track":"kick","gain_db":-2}',
    });
  });

  it("replaces inline audition audio with its size", () => {
    const clip = "A".repeat(4_000);
    const text = exportJson(META, [
      { role: "user", content: "hear this", input_audio: { data: clip, format: "wav" } },
    ]);
    const parsed = JSON.parse(text);
    expect(text).not.toContain(clip.slice(0, 200));
    // The message keeps the clip's format, and the document says how much it dropped.
    expect(parsed.messages[0].input_audio).toEqual({ data: "[omitted: 2.9 KiB of audio]", format: "wav" });
  });
});

describe("the export file name", () => {
  it("is the conversation's title, safe on every platform", () => {
    expect(exportFileName(META, "md")).toBe("punchier kick.md");
    expect(exportFileName(META, "json")).toBe("punchier kick.json");
    expect(exportFileName({ ...META, title: 'a/b:c*"d"' }, "json")).toBe("a b c d.json");
    expect(exportFileName({ ...META, title: "   " }, "md")).toBe("session.md");
  });
});

// ── the panel's two actions ────────────────────────────────────────────────

const backend = {
  sessions: [META] as SessionMeta[],
  conversations: { s2: CONVERSATION } as Record<string, WireMessage[]>,
  /** The provider's answer; a held promise keeps a turn in flight. */
  chat: async (): Promise<unknown> => ({ role: "assistant", content: "Alright." }),
};

function installBackend() {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const typed = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "load_ai_config": return CONFIG;
      case "ai_key_status": return true;
      case "ai_catalog": return CATALOG;
      case "ai_model_limits": return { provider_id: "google", model_id: "gemini-4-argon", bundled: null, overridden: null };
      case "ai_toolsets": return [];
      case "ai_tools": return [];
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
      case "ai_chat": return backend.chat();
      case "get_project": return { ...makeSnapshot(), file_path: null };
      case "sessions_list": return backend.sessions.map((entry) => ({ ...entry }));
      case "sessions_load": {
        const meta = backend.sessions.find((entry) => entry.id === typed.id);
        if (!meta) throw "no session";
        return { meta: { ...meta }, messages: backend.conversations[meta.id] ?? [], skipped: 0 };
      }
      default: return makeSnapshot();
    }
  });
}

const panel = () => <><AiPanel /><TooltipLayer /></>;
const switcher = () => document.querySelector(".ai-session-current") as HTMLButtonElement;
const menu = () => document.querySelector(".ai-export-menu");

async function mountPanel() {
  await store.initProject();
  const view = render(panel);
  await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "sessions_list")).toBe(true));
  return view;
}

async function openConversation() {
  fireEvent.click(switcher());
  await waitFor(() => expect(document.querySelector(".ai-session-row")).toBeTruthy());
  fireEvent.click(document.querySelector(".ai-session-row-title") as HTMLElement);
  await waitFor(() => expect(document.querySelector(".ai-step-group")).toBeTruthy());
  openStepGroups();
  await waitFor(() => expect(document.querySelector(".ai-message.tool")).toBeTruthy());
}

beforeEach(() => {
  invokeMock.mockReset();
  backend.sessions = [{ ...META }];
  backend.conversations = { s2: CONVERSATION };
  backend.chat = async () => ({ role: "assistant", content: "Alright." });
  i18n.setLocale("en");
  aiSession.reset();
  installBackend();
  Object.defineProperty(navigator, "clipboard", {
    configurable: true,
    value: { writeText: vi.fn(async () => {}) },
  });
});

afterEach(() => {
  cleanup();
  aiSession.reset();
  vi.restoreAllMocks();
});

describe("the export menu", () => {
  it("copies the Markdown transcript to the clipboard", async () => {
    await mountPanel();
    await openConversation();
    fireEvent.click(document.querySelector(".ai-session-export") as HTMLElement);
    await waitFor(() => expect(menu()).toBeTruthy());
    expect(document.querySelector(".ai-export-format")?.textContent).toBe("Markdown");
    expect(menu()!.textContent).toContain("JSON");

    fireEvent.click(document.querySelector(".ai-export-copy-markdown") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".ai-export-note")).toBeTruthy());
    expect(document.querySelector(".ai-export-note")?.textContent).toBe("Copied punchier kick.md to the clipboard.");

    const copied = (navigator.clipboard.writeText as unknown as { mock: { calls: string[][] } }).mock.calls.at(-1)![0];
    expect(copied).toContain("# punchier kick");
    expect(copied).toContain("make the kick punchier");
    expect(copied).toContain("tracks_mix");
    expect(copied).toContain('"gain_db":-2');
    expect(copied).toContain('"notes":"gain -2 dB"');
    expect(copied).toContain("Done: the kick is 2 dB quieter.");
    expect(copied).not.toContain("data:image");
  });

  it("downloads the JSON document under the conversation's name", async () => {
    let blob: Blob | null = null;
    const createObjectURL = vi.fn((value: Blob) => { blob = value; return "blob:doc"; });
    Object.assign(URL, { createObjectURL, revokeObjectURL: vi.fn() });
    let anchor: HTMLAnchorElement | null = null;
    vi.spyOn(HTMLAnchorElement.prototype, "click").mockImplementation(function (this: HTMLAnchorElement) {
      anchor = this;
    });

    await mountPanel();
    await openConversation();
    fireEvent.click(document.querySelector(".ai-session-export") as HTMLElement);
    await waitFor(() => expect(menu()).toBeTruthy());
    fireEvent.click(document.querySelector(".ai-export-download-json") as HTMLElement);

    await waitFor(() => expect(document.querySelector(".ai-export-note")).toBeTruthy());
    expect(document.querySelector(".ai-export-note")?.textContent).toBe("Downloaded punchier kick.json.");
    expect(anchor!.download).toBe("punchier kick.json");
    expect(anchor!.href).toBe("blob:doc");
    expect(blob!.type).toBe("application/json");

    const text = await new Promise<string>((resolve) => {
      const reader = new FileReader();
      reader.onload = () => resolve(String(reader.result));
      reader.readAsText(blob!);
    });
    const parsed = JSON.parse(text);
    expect(parsed.meta.title).toBe("punchier kick");
    expect(parsed.messages.map((message: WireMessage) => message.role))
      .toEqual(["user", "assistant", "tool", "assistant"]);
    expect(parsed.messages[0].content).toBe("make the kick punchier");
  });

  it("exports the transcript so far while a turn runs", async () => {
    let release!: (value: unknown) => void;
    const pending = new Promise((resolve) => { release = resolve; });
    backend.chat = () => pending;
    await mountPanel();
    await openConversation();

    const field = document.querySelector(".ai-input") as HTMLTextAreaElement;
    fireEvent.input(field, { target: { value: "make the bass mono" } });
    fireEvent.click(document.querySelector(".ai-send-btn") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".ai-pending")).toBeTruthy());

    // The transcript up to now is exportable; the running turn is not a reason
    // to withhold it.
    const button = document.querySelector(".ai-session-export") as HTMLButtonElement;
    expect(button.disabled).toBe(false);
    fireEvent.click(button);
    await waitFor(() => expect(menu()).toBeTruthy());
    fireEvent.click(document.querySelector(".ai-export-copy-markdown") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".ai-export-note")).toBeTruthy());

    const copied = (navigator.clipboard.writeText as unknown as { mock: { calls: string[][] } }).mock.calls.at(-1)![0];
    expect(copied).toContain("make the bass mono");
    expect(copied).toContain("make the kick punchier");
    release({ role: "assistant", content: "Done." });
    await waitFor(() => expect(document.querySelector(".ai-pending")).toBeFalsy());
  });

  it("offers nothing before a conversation exists", async () => {
    backend.sessions = [];
    await mountPanel();
    expect((document.querySelector(".ai-session-export") as HTMLButtonElement).disabled).toBe(true);
  });
});
