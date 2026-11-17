/**
 * The open conversation follows the project file on screen: opening another
 * file rebinds it, so the next message carries the binding notice instead of
 * being refused.
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

const FIRST = "/songs/first.ocp";
const SECOND = "/songs/second.ocp";

const META = {
  id: "s1",
  title: "bound conversation",
  created: "2026-11-01T09:00:00.000Z",
  updated: "2026-11-02T09:00:00.000Z",
  model: "test-model",
  provider: "test",
  workspace_key: "fixture",
  workspace: "/songs",
  project_path: FIRST,
  turns: 1,
  tokens: 10,
};

const backend = {
  bound: FIRST,
  open: FIRST,
  binds: [] as string[],
  order: [] as string[],
  holdTurn: false,
  releaseTurn: null as ((value: unknown) => void) | null,
};

function installBackend() {
  invokeMock.mockImplementation(async (command: string, args?: unknown) => {
    const typed = (args ?? {}) as Record<string, unknown>;
    if (command === "sessions_bind" || command === "ai_turn_start") backend.order.push(command);
    switch (command) {
      case "load_ai_config": return aiConfigWire();
      case "ai_key_status": return true;
      case "ai_catalog": return { version: "fixture", providers: [] };
      case "ai_model_limits": return { provider_id: "test", model_id: "test-model", bundled: null, overridden: null };
      case "ai_toolsets": return [];
      case "ai_tools": return [];
      case "ai_batch_recover": return null;
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n' };
      case "ai_turn_start":
        if (backend.holdTurn) {
          return new Promise((resolve) => { backend.releaseTurn = resolve; });
        }
        throw new Error("the fixture runs no turn");
      case "get_project": return { ...makeSnapshot(), file_path: backend.open };
      case "sessions_list": return [{ ...META, project_path: backend.bound }];
      case "sessions_open":
        return {
          meta: { ...META, project_path: backend.bound },
          messages: [{ role: "user", content: "earlier question" }],
          skipped: 0,
          start: 0,
          total: 1,
          revision: "rev-1",
          older_tokens: 0,
        };
      case "sessions_binding":
        return { bound: backend.bound, open: backend.open, matches: backend.bound === backend.open };
      case "sessions_bind":
        backend.binds.push(String(typed.id));
        backend.bound = backend.open;
        return { ...META, project_path: backend.bound };
      default: return makeSnapshot();
    }
  });
}

const switcher = () => document.querySelector(".ai-session-current") as HTMLButtonElement;

async function mountWithConversation() {
  await store.initProject();
  render(() => <><AiPanel /><TooltipLayer /><ContextMenuLayer /></>);
  await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "sessions_list")).toBe(true));
  fireEvent.click(switcher());
  await waitFor(() => expect(document.querySelector(".ai-session-row")).toBeTruthy());
  fireEvent.click(document.querySelector(".ai-session-row-title") as HTMLElement);
  await waitFor(() => expect(document.body.textContent).toContain("earlier question"));
}

function openFile(path: string) {
  backend.open = path;
  store.applyExternalSnapshot({ ...makeSnapshot(), file_path: path, revision: 1_000_000 }, true);
}

beforeEach(() => {
  invokeMock.mockReset();
  backend.bound = FIRST;
  backend.open = FIRST;
  backend.binds = [];
  backend.order = [];
  backend.holdTurn = false;
  backend.releaseTurn = null;
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

describe("a conversation follows the open project", () => {
  it("rebinds the open conversation when another file opens", async () => {
    await mountWithConversation();
    expect(backend.binds).toEqual([]);

    openFile(SECOND);

    await waitFor(() => expect(backend.binds).toEqual(["s1"]));
    expect(backend.bound).toBe(SECOND);
  });

  it("leaves a conversation alone when the file that opens is its own", async () => {
    await mountWithConversation();
    backend.bound = SECOND;

    openFile(SECOND);
    await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "sessions_binding")).toBe(true));

    expect(backend.binds).toEqual([]);
  });

  it("rebinds a conversation bound to another file only when the person sends", async () => {
    backend.bound = SECOND;
    await mountWithConversation();
    await waitFor(() => expect(invokeMock.mock.calls.some(([command]) => command === "sessions_binding")).toBe(true));
    expect(backend.binds).toEqual([]);

    const input = document.querySelector(".ai-input") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "keep going" } });
    fireEvent.click(document.querySelector(".ai-send-btn") as HTMLButtonElement);

    await waitFor(() => expect(backend.order).toEqual(["sessions_bind", "ai_turn_start"]));
    expect(backend.bound).toBe(FIRST);
  });

  it("waits for a running turn before rebinding to a file opened meanwhile", async () => {
    await mountWithConversation();
    backend.holdTurn = true;
    const input = document.querySelector(".ai-input") as HTMLInputElement;
    fireEvent.input(input, { target: { value: "keep going" } });
    fireEvent.click(document.querySelector(".ai-send-btn") as HTMLButtonElement);
    await waitFor(() => expect(backend.order).toEqual(["ai_turn_start"]));

    openFile(SECOND);
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(backend.binds, "the running turn keeps its binding").toEqual([]);
    expect(backend.bound).toBe(FIRST);

    backend.releaseTurn?.({ text: "done", failed: false });
    await waitFor(() => expect(backend.binds).toEqual(["s1"]));
    expect(backend.bound).toBe(SECOND);
  });
});
