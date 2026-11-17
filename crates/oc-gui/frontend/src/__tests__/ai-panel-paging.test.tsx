/**
 * A saved conversation opens on its newest page. Older turns arrive one page
 * at a time, keep their whole-view wire indexes, and disappear when the view
 * changed shape or another conversation took the panel.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { aiConfigWire, invokeMock, makeSnapshot } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { TooltipLayer } from "../components/Tooltip";
import { ContextMenuLayer } from "../components/ContextMenu";
import { i18n } from "../stores/i18n";
import { aiSession } from "../stores/aiSession";
import { aiWorkspace } from "../stores/aiWorkspace";
import { resetContextMeter, textTokens } from "../stores/aiContext";
import { restoredTranscript } from "../ai/transcript";
import type { SessionPageRead } from "../ai/sessions";
import type { WireMessage } from "../ai/wire";
import { resetFakeTurns, runFakeTurn, type TurnStartArgs } from "./helpers/fakeTurn";

const META = {
  id: "s1",
  title: "paged conversation",
  created: "2026-11-01T09:00:00.000Z",
  updated: "2026-11-02T09:00:00.000Z",
  model: "test-model",
  provider: "test",
  workspace_key: "fixture",
  workspace: "/Users/me/songs",
  turns: 3,
  tokens: 300,
};

const SECOND = { ...META, id: "s2", title: "second conversation" };

const CONVERSATION: WireMessage[] = [
  { role: "user", content: "first question" },
  { role: "assistant", content: "first answer" },
  { role: "user", content: "middle question" },
  { role: "assistant", content: "middle answer" },
  { role: "user", content: "newest question" },
  { role: "assistant", content: "newest answer" },
];

interface OpenState {
  messages: WireMessage[];
  start: number;
  total: number;
  revision: string;
  older: number;
}

const backend = {
  sessions: [META, SECOND] as (typeof META)[],
  conversations: { s1: CONVERSATION, s2: [{ role: "user", content: "second only" }] } as Record<string, WireMessage[]>,
  open: {} as Record<string, OpenState>,
  page: null as SessionPageRead | null,
  holdPage: null as Promise<void> | null,
  stale: false,
  pageReads: [] as { id: string; before: number; revision: string }[],
  openReads: [] as string[],
  recalls: [] as { sessionId: string; index: number; undoEdits: boolean; confirmed: boolean }[],
  chat: async (): Promise<unknown> => ({ role: "assistant", content: "Alright." }),
};

function page(messages: WireMessage[], start: number, older: number, revision = "rev-1"): SessionPageRead {
  return {
    status: "page",
    messages,
    start,
    total: CONVERSATION.length,
    revision,
    older_tokens: older,
  };
}

function installBackend() {
  invokeMock.mockImplementation(async (command: string, args?: unknown) => {
    const typed = (args ?? {}) as Record<string, unknown>;
    switch (command) {
      case "load_ai_config": return aiConfigWire();
      case "ai_key_status": return true;
      case "ai_catalog": return { version: "fixture", providers: [] };
      case "ai_model_limits": return { provider_id: "test", model_id: "test-model", bundled: null, overridden: null };
      case "ai_toolsets": return [];
      case "ai_tools": return [];
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n  tempo: 128\n' };
      case "ai_batch_recover": return null;
      case "get_project": return makeSnapshot();
      case "sessions_list": return backend.sessions.map((entry) => ({ ...entry }));
      case "sessions_load": {
        const meta = backend.sessions.find((entry) => entry.id === typed.id);
        if (!meta) throw "no session";
        return { meta: { ...meta }, messages: backend.conversations[meta.id] ?? [], skipped: 0 };
      }
      case "sessions_open": {
        const id = String(typed.id);
        backend.openReads.push(id);
        const state = backend.open[id] ?? { messages: [], start: 0, total: 0, revision: "rev-1", older: 0 };
        return {
          meta: { ...(backend.sessions.find((entry) => entry.id === id) ?? META) },
          messages: state.messages,
          skipped: 0,
          start: state.start,
          total: state.total,
          revision: state.revision,
          older_tokens: state.older, state: [],
        };
      }
      case "sessions_page": {
        backend.pageReads.push({
          id: String(typed.id),
          before: Number(typed.before),
          revision: String(typed.revision),
        });
        if (backend.holdPage) await backend.holdPage;
        if (backend.stale) {
          backend.stale = false;
          return { status: "stale" };
        }
        return backend.page ?? { status: "stale" };
      }
      case "ai_turn_start":
        return runFakeTurn(typed as unknown as TurnStartArgs, { chat: async () => backend.chat() });
      case "ai_recall": {
        const index = Number(typed.index);
        const undoEdits = typed.undoEdits === true;
        backend.recalls.push({
          sessionId: String(typed.sessionId),
          index,
          undoEdits,
          confirmed: typed.confirmed === true,
        });
        return {
          status: "recalled",
          kept: index,
          message: { content: "middle question" },
          ...(undoEdits ? { snapshot: makeSnapshot() } : {}),
        };
      }
      default: return makeSnapshot();
    }
  });
}

const switcher = () => document.querySelector(".ai-session-current") as HTMLButtonElement;
const earlier = () => document.querySelector(".ai-transcript-earlier") as HTMLButtonElement | null;
const userRows = () => [...document.querySelectorAll(".ai-message.user")].map((row) => row.textContent ?? "");
const input = () => document.querySelector(".ai-input") as HTMLInputElement;
const sendButton = () => document.querySelector(".ai-send-btn") as HTMLButtonElement;

async function mountPanel() {
  await store.initProject();
  const view = render(() => <><AiPanel /><TooltipLayer /><ContextMenuLayer /></>);
  await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "sessions_list")).toBe(true));
  return view;
}

async function openConversation(title: string, expected: string) {
  fireEvent.click(switcher());
  await waitFor(() => expect(document.querySelector(".ai-session-row")).toBeTruthy());
  const row = [...document.querySelectorAll(".ai-session-row-title")]
    .find((node) => node.textContent === title) as HTMLElement;
  fireEvent.click(row);
  await waitFor(() => expect(document.body.textContent).toContain(expected));
}

beforeEach(() => {
  invokeMock.mockReset();
  backend.sessions = [META, SECOND];
  backend.conversations = { s1: CONVERSATION, s2: [{ role: "user", content: "second only" }] };
  backend.open = {
    s1: { messages: CONVERSATION.slice(4), start: 4, total: 6, revision: "rev-1", older: 123 },
    s2: { messages: [{ role: "user", content: "second only" }], start: 0, total: 1, revision: "rev-2", older: 0 },
  };
  backend.page = null;
  backend.holdPage = null;
  backend.stale = false;
  backend.pageReads = [];
  backend.openReads = [];
  backend.recalls = [];
  backend.chat = async () => ({ role: "assistant", content: "Alright." });
  resetFakeTurns();
  i18n.setLocale("en");
  aiSession.reset();
  resetContextMeter();
  installBackend();
});

afterEach(() => {
  cleanup();
  aiSession.reset();
  resetFakeTurns();
  vi.restoreAllMocks();
});

describe("opening a paged conversation", () => {
  it("shows the newest page and names the older turns behind the earlier button", async () => {
    await mountPanel();
    await openConversation("paged conversation", "newest answer");

    expect(document.body.textContent).not.toContain("first question");
    expect(document.body.textContent).not.toContain("middle question");
    expect(earlier()).toBeTruthy();
    expect(earlier()!.textContent).toBe(i18n.t("ai.transcript.earlierTurns"));
    expect(backend.openReads).toEqual(["s1"]);
  });

  it("loads the page before the first row and keeps the rows in order", async () => {
    backend.page = page(CONVERSATION.slice(2, 4), 2, 40);
    await mountPanel();
    await openConversation("paged conversation", "newest answer");

    fireEvent.click(earlier()!);
    await waitFor(() => expect(backend.pageReads).toEqual([{ id: "s1", before: 4, revision: "rev-1" }]));
    await waitFor(() => expect(userRows()).toEqual(["middle question", "newest question"]));
    expect(earlier()!.textContent).toBe(i18n.t("ai.transcript.earlierTurns"));

    backend.page = page(CONVERSATION.slice(0, 2), 0, 0);
    fireEvent.click(earlier()!);
    await waitFor(() => expect(backend.pageReads).toHaveLength(2));
    expect(backend.pageReads[1]).toEqual({ id: "s1", before: 2, revision: "rev-1" });
    await waitFor(() => expect(userRows()).toEqual(["first question", "middle question", "newest question"]));
    expect(earlier()).toBeNull();
  });

  it("recalls a row from an older page at its absolute wire index", async () => {
    backend.page = page(CONVERSATION.slice(2, 4), 2, 40);
    await mountPanel();
    await openConversation("paged conversation", "newest answer");
    fireEvent.click(earlier()!);
    await waitFor(() => expect(userRows()).toEqual(["middle question", "newest question"]));

    const row = [...document.querySelectorAll(".ai-message.user")]
      .find((element) => element.textContent === "middle question")!;
    fireEvent.contextMenu(row);
    const recall = screen
      .getAllByRole("menuitem")
      .find((item) => item.querySelector(".context-menu-label")?.textContent === "Recall message")!;
    fireEvent.click(recall);
    await waitFor(() =>
      expect(backend.recalls).toEqual([
        { sessionId: "s1", index: 2, undoEdits: false, confirmed: false },
      ]),
    );
  });

  it("repaints the conversation when the saved view changed shape", async () => {
    await mountPanel();
    await openConversation("paged conversation", "newest answer");
    backend.stale = true;
    backend.open.s1 = {
      messages: [{ role: "user", content: "repainted question" }, { role: "assistant", content: "repainted answer" }],
      start: 0,
      total: 2,
      revision: "rev-2",
      older: 0,
    };

    fireEvent.click(earlier()!);
    await waitFor(() => expect(backend.pageReads).toHaveLength(1));
    await waitFor(() => expect(document.body.textContent).toContain("repainted answer"));
    expect(backend.openReads).toEqual(["s1", "s1"]);
    expect(document.body.textContent).not.toContain("newest answer");
  });

  it("drops a page that arrives after another conversation took the panel", async () => {
    let release!: () => void;
    backend.holdPage = new Promise<void>((resolve) => { release = resolve; });
    backend.page = page(CONVERSATION.slice(2, 4), 2, 40);
    await mountPanel();
    await openConversation("paged conversation", "newest answer");

    fireEvent.click(earlier()!);
    await waitFor(() => expect(backend.pageReads).toHaveLength(1));
    await openConversation("second conversation", "second only");

    release();
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(document.body.textContent).toContain("second only");
    expect(document.body.textContent).not.toContain("middle question");
    expect(backend.pageReads).toHaveLength(1);
  });

  it("drops an older page that arrives after a turn started", async () => {
    let releasePage!: () => void;
    backend.holdPage = new Promise<void>((resolve) => { releasePage = resolve; });
    backend.page = page(CONVERSATION.slice(2, 4), 2, 40);
    await mountPanel();
    await openConversation("paged conversation", "newest answer");
    fireEvent.click(earlier()!);
    await waitFor(() => expect(backend.pageReads).toHaveLength(1));

    let releaseChat!: () => void;
    backend.chat = () => new Promise((resolve) => {
      releaseChat = () => resolve({ role: "assistant", content: "answered later" });
    });
    fireEvent.input(input(), { target: { value: "ask during the page" } });
    fireEvent.click(sendButton());
    await waitFor(() => expect(document.body.textContent).toContain("ask during the page"));
    await waitFor(() => expect(document.querySelector(".ai-pending")).not.toBeNull());

    backend.stale = true;
    releasePage();
    await new Promise((resolve) => setTimeout(resolve, 20));
    expect(backend.openReads).toEqual(["s1"]);
    expect(document.body.textContent).toContain("ask during the page");

    releaseChat();
    await waitFor(() => expect(document.querySelector(".ai-pending")).toBeNull());
  });

  it("keeps the reader's scroll position when the older page is prepended", async () => {
    backend.page = page(CONVERSATION.slice(2, 4), 2, 40);
    await mountPanel();
    await openConversation("paged conversation", "newest answer");
    const transcript = document.querySelector(".ai-messages") as HTMLElement;
    let top = 10;
    Object.defineProperties(transcript, {
      scrollHeight: {
        configurable: true,
        get: () => 1000 + transcript.querySelectorAll(".ai-message").length * 20,
      },
      scrollTop: {
        configurable: true,
        get: () => top,
        set: (value: number) => { top = value; },
      },
    });
    // Let the transcript's own follow settle, then place the reader at the top
    // so prepending does not trigger a scroll to the bottom.
    fireEvent.scroll(transcript);
    await Promise.resolve();
    top = 10;
    fireEvent.scroll(transcript);
    const beforeHeight = transcript.scrollHeight;

    fireEvent.click(earlier()!);
    await waitFor(() => expect(userRows()).toEqual(["middle question", "newest question"]));
    await new Promise((resolve) => setTimeout(resolve, 0));
    const afterHeight = transcript.scrollHeight;
    expect(afterHeight).toBeGreaterThan(beforeHeight);
    expect(top).toBe(10 + (afterHeight - beforeHeight));
  });

  it("counts the unloaded older turns in the context estimate", async () => {
    await mountPanel();
    await openConversation("paged conversation", "newest answer");
    const pageMessages = backend.open.s1.messages.map((message) => String(message.content)).join("\n");
    const expected = textTokens(`${pageMessages}\n${store.dslSource()}`) + backend.open.s1.older;
    await waitFor(() => expect(aiWorkspace.contextEstimate()).toBe(expected));
  });

  it("forgets the older turns of a conversation the person left for a new one", async () => {
    await mountPanel();
    await openConversation("paged conversation", "newest answer");
    expect(earlier()).toBeTruthy();

    fireEvent.click(document.querySelector(".ai-session-new") as HTMLButtonElement);
    await waitFor(() => expect(document.body.textContent).not.toContain("newest answer"));
    expect(earlier()).toBeNull();
    expect(backend.pageReads).toEqual([]);
  });

  it("exports only the rows it can still address when the saved view changed shape", async () => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText: vi.fn(async () => {}) },
    });
    await mountPanel();
    await openConversation("paged conversation", "newest answer");

    backend.conversations.s1 = [
      { role: "user", content: "rewritten question" },
      { role: "assistant", content: "rewritten answer" },
    ];
    backend.open.s1 = {
      messages: backend.conversations.s1,
      start: 0,
      total: 2,
      revision: "rev-2",
      older: 0,
    };

    fireEvent.click(document.querySelector(".ai-session-export") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".ai-export-menu")).toBeTruthy());
    fireEvent.click(document.querySelector(".ai-export-copy-markdown") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".ai-export-note")).toBeTruthy());

    const copied = (navigator.clipboard.writeText as unknown as { mock: { calls: string[][] } })
      .mock.calls.at(-1)![0];
    expect(copied).toContain("newest answer");
    expect(copied).not.toContain("rewritten answer");
    expect(copied).not.toContain("first question");
  });

  it("exports the turns the panel has not loaded", async () => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText: vi.fn(async () => {}) },
    });
    await mountPanel();
    await openConversation("paged conversation", "newest answer");
    expect(document.body.textContent).not.toContain("first question");

    fireEvent.click(document.querySelector(".ai-session-export") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".ai-export-menu")).toBeTruthy());
    fireEvent.click(document.querySelector(".ai-export-copy-markdown") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".ai-export-note")).toBeTruthy());

    const copied = (navigator.clipboard.writeText as unknown as { mock: { calls: string[][] } })
      .mock.calls.at(-1)![0];
    expect(copied).toContain("first question");
    expect(copied).toContain("middle question");
    expect(copied).toContain("newest answer");
  });
});

describe("restored transcript page offsets", () => {
  it("names every row by its absolute wire index", () => {
    const rows = restoredTranscript([
      { role: "user", content: "question" },
      { role: "assistant", content: "answer" },
    ], 7);
    expect(rows.map((row) => row.wireIndex)).toEqual([7, 8]);
  });
});
