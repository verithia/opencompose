/**
 * The catalog is metadata, not a gate: a saved model it has no entry for gets
 * one informational line above the panel, and the model selector marks the id
 * instead of looking fine. Nothing is refused and nothing is rewritten.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { invokeMock, makeSnapshot } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { PreferencesHost } from "../components/PreferencesModal";
import { DEFAULT_PROVIDERS, aiConfig, saveConfig, type AiProvider } from "../ai/config";

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
        { id: "deepseek-flash", name: "DeepSeek Flash", context: 1000000, max_output: 8192, capabilities: CAPABILITIES, status: "active" },
      ],
    },
    {
      id: "openai",
      name: "OpenAI",
      kind: "openai_responses",
      hosts: ["api.openai.com"],
      auth_providers: [],
      models: [
        { id: "gpt-6.1-sol", name: "GPT-6.1 Sol", context: 1050000, max_output: 128000, capabilities: CAPABILITIES, status: "active" },
      ],
    },
  ],
};

/** An id no default provider uses, so the legacy built-in upgrade leaves it alone. */
const ENTERPRISE: AiProvider = {
  id: "openai-enterprise",
  name: "OpenAI Enterprise",
  kind: "openai_responses",
  auth: "api_key",
  endpoint: "https://api.openai.com/v1/responses",
  model: "gpt-4o",
  contextLength: 128000,
  maxTokens: 16384,
  temperature: 0.3,
  thinkingLevel: "medium",
  capabilities: { text: true, audioIn: false, audioOut: false, nativeToolCalling: true, streaming: true },
};

const WARNING = {
  provider_id: "openai-enterprise",
  provider_name: "OpenAI Enterprise",
  model: "gpt-4o",
  message: '"OpenAI Enterprise" uses model "gpt-4o", which the OpenAI Responses catalog does not describe; limits default to 1M context and 16K output. Pick a model in the provider\'s settings.',
};

function rustProvider(provider: AiProvider) {
  return {
    id: provider.id,
    name: provider.name,
    kind: provider.kind,
    ...(provider.wire ? { wire: provider.wire } : {}),
    auth: provider.auth,
    ...(provider.authProvider ? { auth_provider: provider.authProvider } : {}),
    endpoint: provider.endpoint,
    model: provider.model,
    ...(provider.models?.length ? { models: provider.models } : {}),
    context_length: provider.contextLength,
    max_tokens: provider.maxTokens,
    temperature: provider.temperature,
    thinking_level: provider.thinkingLevel,
    capabilities: {
      text: provider.capabilities.text,
      audio_in: provider.capabilities.audioIn,
      audio_out: provider.capabilities.audioOut,
      native_tool_calling: provider.capabilities.nativeToolCalling,
      streaming: provider.capabilities.streaming,
    },
  };
}

function config(model: string, warnings: unknown[]) {
  return {
    version: 2,
    active_provider: "openai-enterprise",
    providers: [...DEFAULT_PROVIDERS.map(rustProvider), rustProvider({ ...ENTERPRISE, model })],
    warnings,
  };
}

function mockBackend(loaded = config("gpt-4o", [WARNING])) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "load_ai_config") return loaded;
    if (cmd === "save_ai_config") return { path: "/tmp/ai-providers.yaml", warnings: [WARNING] };
    if (cmd === "ai_key_status") return true;
    if (cmd === "ai_catalog") return CATALOG;
    if (cmd === "ai_model_limits") return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
    return makeSnapshot();
  });
}

async function mount() {
  await store.initProject();
  const view = render(() => <><AiPanel /><PreferencesHost /></>);
  await waitFor(() => expect(view.container.querySelector(".ai-save-notice")).toBeFalsy());
  await waitFor(() => expect(view.container.querySelector(".ai-panel-header")).toBeTruthy());
  return view;
}

function warningLine(): HTMLElement | null {
  return document.querySelector(".ai-catalog-warning");
}

function modelTrigger(): HTMLElement {
  return document.querySelector(".ai-model-picker .oc-dropdown-trigger") as HTMLElement;
}

beforeEach(() => {
  invokeMock.mockReset();
});

afterEach(cleanup);

describe("a model the catalog cannot describe", () => {
  it("shows one compact line and opens settings on that provider", async () => {
    mockBackend();
    const view = await mount();
    await waitFor(() => expect(warningLine()).toBeTruthy());
    expect(warningLine()!.textContent).toContain(
      "1 provider uses a model the catalog does not describe",
    );
    expect(warningLine()!.textContent).toContain("1M/16K");
    // One line: the provider is not listed in the panel, only in the tooltip.
    expect(warningLine()!.textContent).not.toContain("OpenAI Enterprise");

    fireEvent.click(screen.getByRole("button", { name: /provider uses a model/ }));
    await waitFor(() =>
      expect(screen.getByRole("dialog", { name: /AI settings|Preferences/ })).toBeTruthy(),
    );
    expect(aiConfig.config.activeProviderId).toBe("openai-enterprise");
    expect(view.baseElement.textContent).toContain("OpenAI Enterprise");
  });

  it("dismissing the line does not silence the next save", async () => {
    mockBackend();
    await mount();
    await waitFor(() => expect(warningLine()).toBeTruthy());
    fireEvent.click(screen.getByRole("button", { name: "Dismiss" }));
    expect(warningLine()).toBeFalsy();

    await saveConfig();
    await waitFor(() => expect(warningLine()).toBeTruthy());
  });

  it("marks the selected id instead of looking fine", async () => {
    mockBackend();
    await mount();
    await waitFor(() => expect(modelTrigger().getAttribute("data-model-known")).toBe("false"));
    expect(modelTrigger().textContent).toContain("gpt-4o");
    expect(modelTrigger().textContent).toContain("not in catalog");
    expect(
      document.querySelector(".ai-catalog-warning-text")!.getAttribute("title"),
    ).toContain(WARNING.message);
  });

  it("leaves a catalog model unmarked and shows no line", async () => {
    mockBackend(config("gpt-6.1-sol", []));
    await mount();
    await waitFor(() => expect(modelTrigger().getAttribute("data-model-known")).toBe("true"));
    expect(modelTrigger().textContent).toContain("gpt-6.1-sol");
    expect(modelTrigger().textContent).not.toContain("not in catalog");
    expect(warningLine()).toBeFalsy();
  });
});
