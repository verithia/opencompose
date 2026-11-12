/**
 * The AI panel header: the workspace is the primary identity (its own name, with
 * the full path in the tooltip), the open conversation's title sits next to it,
 * and the model selector shows the whole `provider/model` string at a typical
 * panel width instead of a clipped fragment.
 *
 * jsdom has no layout, so "visible at 700 px" is asserted on the rendered text
 * of the trigger: the element is given the width the panel would have, and its
 * value must be the full identity. A truncated value keeps that identity in its
 * tooltip and in every dropdown option.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { TooltipLayer } from "../components/Tooltip";
import { workspaceStore } from "../stores/workspace";
import { aiSession } from "../stores/aiSession";
import { i18n } from "../stores/i18n";
import type { SessionMeta } from "../ai/sessions";

const CAPABILITIES = { text: true, audio_in: false, audio_out: false, native_tool_calling: true, streaming: true };

/** A panel width where the whole model identity has room. */
const PANEL_WIDTH = 700;

const CONFIG = {
  version: 2,
  active_provider: "xiaomi",
  providers: [{
    id: "xiaomi",
    name: "Xiaomi",
    kind: "openai_chat_completions",
    auth: "api_key",
    auth_provider: null,
    oauth_allowed_hosts: [],
    endpoint: "https://api.example.com/v1/chat/completions",
    model: "mimo-v2-flash",
    models: [],
    context_length: 128000,
    max_tokens: 8192,
    temperature: 0.3,
    thinking_level: "medium",
    capabilities: CAPABILITIES,
  }],
};

const CATALOG = {
  version: "fixture",
  providers: [{
    id: "xiaomi",
    name: "Xiaomi",
    kind: "openai_chat_completions",
    hosts: ["api.example.com"],
    auth_providers: [],
    models: [{
      id: "mimo-v2-flash",
      name: "MiMo v2 Flash",
      context: 128000,
      max_output: 8192,
      thinking_levels: ["low", "medium", "high"],
      capabilities: CAPABILITIES,
      status: "active",
    }],
  }],
};

function session(overrides: Partial<SessionMeta> = {}): SessionMeta {
  return {
    id: "s1",
    title: "punchy kick",
    created: "2026-10-28T09:00:00.000Z",
    updated: "2026-10-29T09:00:00.000Z",
    model: "mimo-v2-flash",
    provider: "xiaomi",
    workspace_key: "3f2a1b0c",
    workspace: "/Users/me/songs",
    turns: 2,
    tokens: 400,
    ...overrides,
  };
}

const backend = {
  workspacePath: "/Users/me/songs" as string | null,
  detached: false,
  sessions: [session()] as SessionMeta[],
};

function installBackend() {
  invokeMock.mockImplementation(async (command: string) => {
    switch (command) {
      case "load_ai_config": return CONFIG;
      case "ai_key_status": return true;
      case "ai_catalog": return { ...CATALOG };
      case "ai_model_limits": return { provider_id: "xiaomi", model_id: "mimo-v2-flash", bundled: null, overridden: null };
      case "ai_toolsets": return [];
      case "ai_tools": return [];
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n' };
      case "workspace_current": return {
        path: backend.workspacePath,
        name: backend.workspacePath ? "songs" : null,
        detached: backend.detached,
        projects: [],
      };
      case "workspaces_list": return [];
      case "sessions_list": return backend.sessions.map((entry) => ({ ...entry }));
      case "sessions_load": {
        const meta = backend.sessions.find((entry) => entry.id === "s1")!;
        return { meta: { ...meta }, messages: [{ role: "user", content: "make the kick punchier" }], skipped: 0 };
      }
      case "get_project": return makeSnapshot();
      default: return makeSnapshot();
    }
  });
}

const panel = () => <><AiPanel /><TooltipLayer /></>;
const modelTrigger = () => document.querySelector(".ai-model-picker .oc-dropdown-trigger") as HTMLElement;
const modelValue = () => modelTrigger().querySelector(".oc-dropdown-value")?.textContent ?? "";

async function mountPanel() {
  await store.initProject();
  await workspaceStore.init();
  const view = render(panel);
  await waitFor(() => expect(modelTrigger()).toBeTruthy());
  // The width the panel has in the app; the assertion below reads its text.
  (view.container.querySelector(".ai-panel") as HTMLElement).style.width = `${PANEL_WIDTH}px`;
  return view;
}

beforeEach(() => {
  invokeMock.mockReset();
  backend.workspacePath = "/Users/me/songs";
  backend.detached = false;
  backend.sessions = [session()];
  i18n.setLocale("en");
  workspaceStore.reset();
  aiSession.reset();
  installBackend();
});

afterEach(cleanup);

describe("the AI panel header", () => {
  it("shows the workspace, the conversation title and the full provider/model at 700 px", async () => {
    const view = await mountPanel();
    const header = view.container.querySelector(".ai-panel-header") as HTMLElement;

    // The workspace's own name is the identity; the path is the tooltip.
    expect(header.querySelector(".ai-header-scope-name")?.textContent).toBe("songs");
    fireEvent.focusIn(header.querySelector(".ai-header-scope") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".oc-tooltip")?.textContent).toContain("/Users/me/songs"));

    // Opening a saved conversation puts its title next to the workspace.
    fireEvent.click(view.container.querySelector(".ai-session-current") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".ai-session-row-title")).toBeTruthy());
    fireEvent.click(document.querySelector(".ai-session-row-main") as HTMLElement);
    await waitFor(() =>
      expect(view.container.querySelector(".ai-header-title")?.textContent).toBe("punchy kick"));

    // The model shows provider and id in full, not a clipped fragment.
    expect(modelValue()).toBe("xiaomi/mimo-v2-flash");
    expect(modelTrigger().getAttribute("aria-describedby")).toBeTruthy();
    fireEvent.focusIn(modelTrigger());
    await waitFor(() => expect(document.querySelector(".oc-tooltip")?.textContent).toContain("xiaomi/mimo-v2-flash"));
  });

  it("keeps the full provider/model in every dropdown option", async () => {
    await mountPanel();
    fireEvent.click(modelTrigger());
    const listbox = await waitFor(() => {
      const list = document.querySelector('[role="listbox"]');
      expect(list).toBeTruthy();
      return list as HTMLElement;
    });
    const labels = [...listbox.querySelectorAll('[role="option"]')]
      .map((option) => option.querySelector(".oc-dropdown-option-label")?.textContent);
    expect(labels).toEqual(["xiaomi/mimo-v2-flash"]);
  });

  it("says No workspace when the user continued without one", async () => {
    backend.workspacePath = null;
    backend.detached = true;
    backend.sessions = [];
    const view = await mountPanel();

    const scope = view.container.querySelector(".ai-header-scope-name");
    expect(scope?.textContent).toBe("No workspace");
    fireEvent.focusIn(view.container.querySelector(".ai-header-scope") as HTMLElement);
    await waitFor(() => expect(document.querySelector(".oc-tooltip")?.textContent).toBe("No workspace"));
    // The scope line in the session list carries the same answer.
    fireEvent.click(view.container.querySelector(".ai-session-current") as HTMLElement);
    await waitFor(() => expect(view.container.querySelector(".ai-session-scope-path")?.textContent).toBe("No workspace"));
  });

  it("opens the workspace chooser from the header and from the scope line", async () => {
    const view = await mountPanel();
    fireEvent.click(view.container.querySelector(".ai-header-scope") as HTMLElement);
    expect(workspaceStore.chooserOpen()).toBe(true);
    workspaceStore.closeChooser();

    fireEvent.click(view.container.querySelector(".ai-session-current") as HTMLElement);
    await waitFor(() => expect(view.container.querySelector(".ai-session-scope")).toBeTruthy());
    fireEvent.click(view.container.querySelector(".ai-session-scope") as HTMLElement);
    expect(workspaceStore.chooserOpen()).toBe(true);
  });
});
