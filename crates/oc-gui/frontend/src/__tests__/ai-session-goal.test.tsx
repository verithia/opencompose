/**
 * The goal and plan are saved session rows: the composer reads the newest
 * rows, `/goal` and the chip clear write them, and a goal left in the
 * window's own store migrates once.
 */
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { aiConfigWire, invokeMock, makeSnapshot } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { TooltipLayer } from "../components/Tooltip";
import { ContextMenuLayer } from "../components/ContextMenu";
import { i18n } from "../stores/i18n";
import { aiSession } from "../stores/aiSession";
import { resetContextMeter } from "../stores/aiContext";

if (!window.localStorage) {
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: (() => {
      const store = new Map<string, string>();
      return {
        getItem: (key: string) => store.get(key) ?? null,
        setItem: (key: string, value: string) => { store.set(key, value); },
        removeItem: (key: string) => { store.delete(key); },
        clear: () => { store.clear(); },
        key: (index: number) => [...store.keys()][index] ?? null,
        get length() { return store.size; },
      };
    })(),
  });
}

const META = {
  id: "s1",
  title: "planned conversation",
  created: "2026-11-01T09:00:00.000Z",
  updated: "2026-11-02T09:00:00.000Z",
  model: "test-model",
  provider: "test",
  workspace_key: "fixture",
  workspace: "/songs",
  project_path: null as string | null,
  turns: 1,
  tokens: 10,
};

const stateRow = (state: unknown, content: string) => ({
  role: "user",
  content,
  message_meta: { version: 1, kind: "session_state", state },
});

const backend = {
  messages: [] as unknown[],
  goals: [] as { id: string; text: string | null }[],
};

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
      case "ai_batch_recover": return null;
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n' };
      case "ai_turn_start": throw new Error("the fixture runs no turn");
      case "get_project": return { ...makeSnapshot(), file_path: null };
      case "sessions_list": return [META];
      case "sessions_open":
        return {
          meta: META,
          messages: backend.messages,
          skipped: 0,
          start: 0,
          total: backend.messages.length,
          revision: "rev-1",
          older_tokens: 0,
        };
      case "sessions_binding":
        return { bound: null, open: null, matches: true };
      case "sessions_set_goal":
        backend.goals.push({ id: String(typed.id), text: (typed.text as string | null) ?? null });
        return META;
      default: return makeSnapshot();
    }
  });
}

const switcher = () => document.querySelector(".ai-session-current") as HTMLButtonElement;
const input = () => document.querySelector(".ai-input") as HTMLTextAreaElement;

async function mountWithConversation() {
  await store.initProject();
  render(() => <><AiPanel /><TooltipLayer /><ContextMenuLayer /></>);
  await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "sessions_list")).toBe(true));
  fireEvent.click(switcher());
  await waitFor(() => expect(document.querySelector(".ai-session-row")).toBeTruthy());
  fireEvent.click(document.querySelector(".ai-session-row-title") as HTMLElement);
  await waitFor(() => expect(document.body.textContent).toContain("earlier question"));
}

beforeEach(() => {
  invokeMock.mockReset();
  backend.messages = [
    stateRow(
      { kind: "goal_set", text: "finish the bridge", author: "person" },
      "Goal: finish the bridge",
    ),
    stateRow(
      {
        kind: "plan_set",
        items: [
          { text: "write the part", status: "done" },
          { text: "mix it", status: "in_progress" },
        ],
      },
      "Plan:\n1. [done] write the part\n2. [in progress] mix it",
    ),
    { role: "user", content: "earlier question" },
  ];
  backend.goals = [];
  i18n.setLocale("en");
  aiSession.reset();
  resetContextMeter();
  installBackend();
});

afterEach(() => {
  cleanup();
  aiSession.reset();
  vi.restoreAllMocks();
});

describe("goal and plan are saved session rows", () => {
  it("paints the derived goal, plan strip and state rows", async () => {
    await mountWithConversation();
    await waitFor(() => expect(document.querySelector(".ai-plan-strip")).toBeTruthy());
    const strip = document.querySelector(".ai-plan-strip")!;
    expect(strip.textContent).toContain("write the part");
    expect(strip.textContent).toContain("In progress");
    expect(document.querySelector(".ai-goal-text")?.textContent).toBe("finish the bridge");
    const rows = [...document.querySelectorAll(".ai-state-row")].map(row => row.textContent);
    expect(rows).toContain("Goal set: finish the bridge");
    expect(rows).toContain("Plan updated (2 steps)");
  });

  it("writes /goal as one command and shows it at once", async () => {
    await mountWithConversation();
    await waitFor(() => expect(document.querySelector(".ai-goal-chip")).toBeTruthy());
    fireEvent.input(input(), { target: { value: "/goal keep the low end steady" } });
    await waitFor(() => expect(document.querySelector(".ai-command-row")).toBeTruthy());
    fireEvent.click(document.querySelector(".ai-command-row") as HTMLElement);
    await waitFor(() => expect(backend.goals).toEqual([{ id: "s1", text: "keep the low end steady" }]));
    await waitFor(() => expect(document.querySelector(".ai-goal-text")?.textContent).toBe("keep the low end steady"));
  });

  it("clears the goal with the chip", async () => {
    await mountWithConversation();
    await waitFor(() => expect(document.querySelector(".ai-goal-clear")).toBeTruthy());
    fireEvent.click(document.querySelector(".ai-goal-clear") as HTMLElement);
    await waitFor(() => expect(backend.goals).toEqual([{ id: "s1", text: null }]));
    await waitFor(() => expect(document.querySelector(".ai-goal-chip")).toBeFalsy());
    expect(document.querySelector(".ai-plan-strip")).toBeTruthy();
  });

  it("migrates a goal kept in the window store once", async () => {
    backend.messages = [{ role: "user", content: "earlier question" }];
    window.localStorage.setItem("opencompose.ai.panel.v1", JSON.stringify({
      v: 1,
      open: null,
      openScope: null,
      openByScope: {},
      conversations: { s1: { goal: "legacy goal" } },
      turn: null,
    }));
    aiSession.reload();
    await mountWithConversation();
    await waitFor(() => expect(backend.goals).toEqual([{ id: "s1", text: "legacy goal" }]));
    const stored = JSON.parse(window.localStorage.getItem("opencompose.ai.panel.v1") ?? "{}");
    expect(stored.conversations?.s1?.goal).toBeUndefined();
    // A second paint reads the cleared store, not the old goal.
    await waitFor(() => expect(document.querySelector(".ai-goal-text")?.textContent).toBe("legacy goal"));
    expect(backend.goals).toHaveLength(1);
  });
});
