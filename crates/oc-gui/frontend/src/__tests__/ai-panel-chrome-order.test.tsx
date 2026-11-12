/**
 * The AI panel's reading order: the conversation's identity and its settings
 * come first, the transcript fills the middle, and the model controls are the
 * composer's footer at the bottom.
 *
 * jsdom has no layout, so the order is asserted on document position: the model
 * selector must follow the composer, and nothing but the stats line may follow
 * it. Geometry (the row fits a 240 px panel, the composer stays on screen) is
 * measured in `layout/ai-conversation.layout.tsx`.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { TooltipLayer } from "../components/Tooltip";
import { workspaceStore } from "../stores/workspace";
import { aiSession } from "../stores/aiSession";
import { aiConfig, getActiveProvider } from "../ai/config";
import { i18n } from "../stores/i18n";
import type { SessionMeta } from "../ai/sessions";

const CAPABILITIES = { text: true, audio_in: false, audio_out: false, native_tool_calling: true, streaming: true };

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
        path: "/Users/me/songs",
        name: "songs",
        detached: false,
        projects: [],
      };
      case "workspaces_list": return [];
      case "sessions_list": return [session()];
      case "get_project": return makeSnapshot();
      default: return makeSnapshot();
    }
  });
}

/** True when `first` comes before `second` in document order. */
function precedes(first: Element, second: Element): boolean {
  return (first.compareDocumentPosition(second) & Node.DOCUMENT_POSITION_FOLLOWING) !== 0;
}

async function mountPanel() {
  await store.initProject();
  await workspaceStore.init();
  const view = render(() => <><AiPanel /><TooltipLayer /></>);
  await waitFor(() => expect(view.container.querySelector(".ai-model-bar")).toBeTruthy());
  return view;
}

beforeEach(() => {
  invokeMock.mockReset();
  i18n.setLocale("en");
  workspaceStore.reset();
  aiSession.reset();
  installBackend();
});

afterEach(cleanup);

describe("the AI panel's row order", () => {
  it("keeps the session name and the settings control above the transcript", async () => {
    const view = await mountPanel();
    const panel = view.container.querySelector(".ai-panel") as HTMLElement;
    const header = panel.querySelector(".ai-panel-header")!;
    const sessionBar = panel.querySelector(".ai-session-bar")!;
    const sessionName = panel.querySelector(".ai-session-current-title")!;
    const transcript = panel.querySelector(".ai-messages")!;

    expect(sessionName.textContent).toBeTruthy();
    expect(precedes(header, sessionBar)).toBe(true);
    expect(precedes(sessionName, transcript)).toBe(true);
    expect(precedes(sessionBar, transcript)).toBe(true);
    // The settings control is part of that first block, not the footer.
    const settings = panel.querySelector(`[aria-label="${i18n.t("ai.settings.open")}"]`);
    expect(settings).toBeTruthy();
    expect(precedes(settings!, transcript)).toBe(true);
    // The model is not a header control any more.
    expect(header.querySelector(".oc-dropdown")).toBeNull();
  });

  it("puts the model selector after the composer as the last row", async () => {
    const view = await mountPanel();
    const panel = view.container.querySelector(".ai-panel") as HTMLElement;
    const transcript = panel.querySelector(".ai-messages")!;
    const composer = panel.querySelector(".ai-input-bar")!;
    const modelBar = panel.querySelector(".ai-model-bar")!;

    expect(precedes(transcript, composer)).toBe(true);
    expect(precedes(composer, modelBar)).toBe(true);
    // The composer is the row above the model, not a wrapping sibling.
    expect(composer.nextElementSibling).toBe(modelBar);
    // Only the stats line follows the model controls.
    const rows = [...panel.children];
    expect(rows.slice(rows.indexOf(modelBar) + 1).map((row) => row.className)).toEqual(["ai-stats"]);
    // The selector is still the model control it was, in its new place.
    const picker = modelBar.querySelector(".ai-model-picker .oc-dropdown-trigger") as HTMLElement;
    expect(picker.getAttribute("aria-label")).toBe(i18n.t("ai.header.model"));
    expect(picker.textContent).toContain("mimo-v2-flash");
  });

  it("keeps the model selector usable from the composer footer", async () => {
    const view = await mountPanel();
    const trigger = view.container.querySelector(".ai-model-picker .oc-dropdown-trigger") as HTMLElement;
    fireEvent.click(trigger);
    const listbox = await waitFor(() => {
      const list = document.querySelector('[role="listbox"]');
      expect(list).toBeTruthy();
      return list as HTMLElement;
    });
    const labels = [...listbox.querySelectorAll('[role="option"]')]
      .map((option) => option.querySelector(".oc-dropdown-option-label")?.textContent ?? "");
    expect(labels.join("\n")).toContain("mimo-v2-flash");
    const option = [...listbox.querySelectorAll('[role="option"]')]
      .find((entry) => entry.textContent?.includes("mimo-v2-flash")) as HTMLElement;
    fireEvent.click(option);
    await waitFor(() => expect(document.querySelector('[role="listbox"]')).toBeNull());
    expect(getActiveProvider().model).toBe("mimo-v2-flash");
    expect(trigger.textContent).toContain("mimo-v2-flash");
  });

  it("moves the thinking level with the model, not with the header", async () => {
    const view = await mountPanel();
    const header = view.container.querySelector(".ai-panel-header")!;
    const modelBar = view.container.querySelector(".ai-model-bar") as HTMLElement;
    // MiMo v2 Flash declares levels, so the effort selector exists at the
    // footer and nowhere in the header. The catalog that carries the levels
    // loads after the panel's first paint.
    const effort = await waitFor(() => {
      const trigger = modelBar.querySelector(".ai-model-effort .oc-dropdown-trigger");
      expect(trigger).toBeTruthy();
      return trigger as HTMLElement;
    });
    expect(effort.textContent).toContain("medium");
    expect(header.querySelector(".ai-model-effort")).toBeNull();
    fireEvent.click(effort);
    const levels = await waitFor(() => {
      const list = document.querySelector('[role="listbox"]');
      expect(list).toBeTruthy();
      return list as HTMLElement;
    });
    const high = [...levels.querySelectorAll('[role="option"]')]
      .find((entry) => entry.textContent?.includes("high")) as HTMLElement;
    fireEvent.click(high);
    await waitFor(() => expect(aiConfig.config.providers[0].thinkingLevel).toBe("high"));
    expect(effort.textContent).toContain("high");
  });
});
