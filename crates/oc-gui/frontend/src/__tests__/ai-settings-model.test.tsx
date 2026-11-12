/**
 * The settings dialog carries the model selector: same grouped list, same
 * update path as the panel's composer footer, so the operator never has to
 * close the dialog to change model. The provider defaults are labelled and sit under the model
 * group, and the status line says whether it reports a success or a failure.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor } from "@solidjs/testing-library";
import { createSignal } from "solid-js";
import { invokeMock, makeSnapshot } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { PreferencesHost } from "../components/PreferencesModal";
import { AiSettingsModal, type NoticeTone } from "../components/AiSettingsModal";
import { aiConfig, getActiveProvider, loadConfig, type AiProvider } from "../ai/config";
import type { Catalog } from "../ai/catalog";

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
        { id: "deepseek-v4-pro", name: "DeepSeek V4 Pro", context: 500000, max_output: 4096, capabilities: CAPABILITIES, status: "active" },
      ],
    },
    {
      id: "google",
      name: "Google",
      kind: "gemini_interactions",
      hosts: ["generativelanguage.googleapis.com"],
      auth_providers: [],
      models: [
        {
          id: "gemini-4-argon",
          name: "Gemini 4 Argon",
          context: 200000,
          max_output: 32000,
          thinking_levels: ["low", "medium", "high"],
          capabilities: CAPABILITIES,
          status: "active",
        },
      ],
    },
  ],
} as unknown as Catalog;

/** A provider the catalog does not serve: its own ids are all it lists. */
const GATEWAY: AiProvider = {
  id: "gateway",
  name: "Gateway",
  kind: "openai_chat_completions",
  auth: "api_key",
  endpoint: "https://api.gateway.example/v1/chat/completions",
  model: "gateway-70b",
  models: ["gateway-70b", "gateway-8b"],
  contextLength: 128000,
  maxTokens: 8192,
  temperature: 0.3,
  thinkingLevel: "medium",
  capabilities: { text: true, audioIn: false, audioOut: false, nativeToolCalling: true, streaming: true },
};

/** A saved provider on a catalog host; the catalog's entry supplies the ids. */
const DEEPSEEK: AiProvider = {
  id: "deepseek-pro",
  name: "DeepSeek Pro",
  kind: "openai_chat_completions",
  auth: "api_key",
  endpoint: "https://api.deepseek.com/chat/completions",
  model: "deepseek-v4-pro",
  contextLength: 500000,
  maxTokens: 4096,
  temperature: 0.3,
  thinkingLevel: "medium",
  capabilities: { text: true, audioIn: false, audioOut: false, nativeToolCalling: true, streaming: true },
};

const GOOGLE: AiProvider = {
  id: "google-pro",
  name: "Google Pro",
  kind: "gemini_interactions",
  auth: "api_key",
  endpoint: "https://generativelanguage.googleapis.com/v1beta/interactions",
  model: "gemini-4-argon",
  contextLength: 200000,
  maxTokens: 32000,
  temperature: 0.3,
  thinkingLevel: "medium",
  capabilities: { text: true, audioIn: false, audioOut: false, nativeToolCalling: true, streaming: true },
};

/** A local endpoint that lists no ids: the dialog has to take a typed one. */
const LOCAL: AiProvider = {
  id: "local-llm",
  name: "Local",
  kind: "openai_chat_completions",
  auth: "api_key",
  endpoint: "http://localhost:8080/v1/chat/completions",
  model: "",
  contextLength: 8192,
  maxTokens: 4096,
  temperature: 0.3,
  thinkingLevel: "medium",
  capabilities: { text: true, audioIn: false, audioOut: false, nativeToolCalling: false, streaming: true },
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

function mockBackend(loaded: AiProvider[], effectiveContext?: number) {
  invokeMock.mockImplementation(async (cmd: string) => {
    if (cmd === "load_ai_config") {
      return {
        version: 2,
        active_provider: loaded[0].id,
        providers: loaded.map(rustProvider),
        warnings: [],
      };
    }
    if (cmd === "save_ai_config") return { path: "/tmp/ai-providers.yaml", warnings: [] };
    if (cmd === "ai_key_status") return true;
    if (cmd === "ai_catalog") return CATALOG;
    if (cmd === "ai_model_limits") {
      return {
        provider_id: "x",
        model_id: "y",
        bundled: null,
        overridden: null,
        ...(effectiveContext === undefined ? {} : { effective_context: effectiveContext }),
      };
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

/** Loads the stored config and opens the dialog on its first provider. */
async function mountDialog(overrides: Partial<Parameters<typeof AiSettingsModal>[0]> = {}) {
  await store.initProject();
  await loadConfig();
  const view = render(() => <AiSettingsModal {...modalProps(overrides)} />);
  await waitFor(() => expect(dialog()).toBeTruthy());
  return view;
}

const dialog = () => screen.queryByRole("dialog", { name: /AI settings|Preferences/ });
const modelTrigger = () => document.querySelector(".ai-settings-model .oc-dropdown-trigger") as HTMLElement;
const effortTrigger = () => screen.queryByRole("button", { name: "Thinking effort" });

function optionLabels(listbox: HTMLElement): string[] {
  return [...listbox.querySelectorAll('[role="option"]')]
    .map((option) => option.querySelector(".oc-dropdown-option-label")?.textContent ?? "");
}

async function pickModel(id: string) {
  fireEvent.click(modelTrigger());
  const listbox = await screen.findByRole("listbox");
  const option = [...listbox.querySelectorAll('[role="option"]')]
    .find((entry) => entry.textContent?.includes(id)) as HTMLElement;
  fireEvent.click(option);
}

beforeEach(() => {
  invokeMock.mockReset();
});

afterEach(cleanup);

describe("the settings dialog's model selector", () => {
  it("lists the provider's own ids and the catalog ids in one grouped list", async () => {
    mockBackend([GATEWAY, DEEPSEEK, GOOGLE]);
    await mountDialog();

    fireEvent.click(modelTrigger());
    const listbox = await screen.findByRole("listbox");
    expect(optionLabels(listbox)).toEqual([
      "gateway/gateway-70b",
      "gateway/gateway-8b",
      "deepseek-pro/deepseek-flash",
      "deepseek-pro/deepseek-v4-pro",
      "google-pro/gemini-4-argon",
    ]);
  });

  it("stores the picked model and applies its catalog limits", async () => {
    mockBackend([GATEWAY, DEEPSEEK, GOOGLE]);
    await mountDialog();

    await pickModel("deepseek-v4-pro");
    await waitFor(() => expect(getActiveProvider().model).toBe("deepseek-v4-pro"));
    // The same update path as the footer's picker: provider, limits and capabilities move.
    expect(getActiveProvider().id).toBe("deepseek-pro");
    expect(getActiveProvider().contextLength).toBe(500000);
    expect(getActiveProvider().maxTokens).toBe(4096);
    expect(modelTrigger().textContent).toContain("deepseek-v4-pro");
  });

  it("keeps the not-in-catalog marker for an id the catalog does not describe", async () => {
    mockBackend([{ ...DEEPSEEK, model: "deepseek-v9-ghost" }]);
    await mountDialog();

    await waitFor(() => expect(modelTrigger().getAttribute("data-model-known")).toBe("false"));
    expect(modelTrigger().textContent).toContain("deepseek-v9-ghost");
    expect(modelTrigger().textContent).toContain("not in catalog");

    fireEvent.click(modelTrigger());
    const listbox = await screen.findByRole("listbox");
    expect(optionLabels(listbox)).toContain("deepseek-pro/deepseek-v9-ghost · not in catalog");
  });

  it("offers the thinking effort only for a model that declares levels", async () => {
    mockBackend([GOOGLE, DEEPSEEK]);
    await mountDialog();

    await waitFor(() => expect(effortTrigger()).toBeTruthy());
    expect(effortTrigger()!.textContent).toContain("medium");

    await pickModel("deepseek-v4-pro");
    await waitFor(() => expect(getActiveProvider().model).toBe("deepseek-v4-pro"));
    expect(effortTrigger()).toBeNull();
  });

  it("takes a typed id when the provider lists nothing", async () => {
    mockBackend([LOCAL]);
    await mountDialog();

    fireEvent.click(modelTrigger());
    const listbox = await screen.findByRole("listbox");
    expect(optionLabels(listbox)).toContain("Type an id…");
    const option = [...listbox.querySelectorAll('[role="option"]')]
      .find((entry) => entry.textContent?.includes("Type an id…")) as HTMLElement;
    fireEvent.click(option);

    const input = await screen.findByLabelText("Type an id…");
    fireEvent.input(input, { target: { value: "llama-3.2-8b" } });
    fireEvent.keyDown(input, { key: "Enter" });

    await waitFor(() => expect(getActiveProvider().model).toBe("llama-3.2-8b"));
    // The catalog knows nothing about the id, so the provider's own values stay.
    expect(getActiveProvider().contextLength).toBe(8192);
    expect(modelTrigger().textContent).toContain("llama-3.2-8b");
  });
});

describe("the settings dialog's model column", () => {
  it("labels the provider defaults and keeps them out of the connection column", async () => {
    mockBackend([DEEPSEEK, GOOGLE]);
    await mountDialog();

    const columns = document.querySelectorAll<HTMLElement>(".ai-settings-column");
    const [connection, model] = [columns[0], columns[1]];
    expect(model.querySelector(".ai-settings-column-title")!.textContent).toBe("Model");
    expect(screen.getByText(/Provider defaults/)).toBeTruthy();
    expect(screen.getByText(/Provider defaults/).closest(".ai-settings-column")).toBe(model);

    const context = screen.getByLabelText("Context (tokens)");
    expect(model.contains(context)).toBe(true);
    expect(connection.contains(context)).toBe(false);
    expect(connection.textContent).not.toContain("Context (tokens)");
    expect(connection.textContent).not.toContain("Temperature");

    // The model group holds the selector, the effort and the active model's limits.
    expect(model.contains(modelTrigger())).toBe(true);
    expect(model.contains(screen.getByRole("button", { name: "Edit limits" }))).toBe(true);
    // Then the three sections the column title names.
    expect(model.textContent).toContain("Capabilities");
    expect(model.textContent).toContain("Edit Target");
    expect(model.textContent).toContain("Toolsets");
  });

  it("says when the model's own window overrides the stored context default", async () => {
    // The stored provider default says 64K, but the model resolves to a 1M
    // window, which is the number a request and auto-compaction use.
    mockBackend([{ ...GATEWAY, contextLength: 65536 }], 1_048_576);
    await mountDialog();

    const line = await screen.findByText(/1048576-token window instead of this number/);
    expect(line).toBeTruthy();
    expect(line.closest(".ai-settings-defaults")).toBeTruthy();
  });

  it("stays quiet when the stored context default is the window in use", async () => {
    mockBackend([{ ...GATEWAY, contextLength: 65536 }], 65_536);
    await mountDialog();

    await waitFor(() => expect(dialog()).toBeTruthy());
    expect(screen.queryByText(/instead of this number/)).toBeNull();
  });

  it("renders a success and an error notice in different classes", async () => {
    mockBackend([DEEPSEEK]);
    const [notice, setNotice] = createSignal<{ text: string; tone: NoticeTone }>({
      text: "Added Gateway.",
      tone: "success",
    });
    render(() => (
      <AiSettingsModal
        {...modalProps({
          testResult: notice().text,
          noticeTone: notice().tone,
        })}
      />
    ));

    const line = await waitFor(() => {
      const element = document.querySelector(".ai-settings-notice");
      expect(element).toBeTruthy();
      return element as HTMLElement;
    });
    expect(line.textContent).toBe("Added Gateway.");
    expect(line.classList.contains("ai-settings-notice-success")).toBe(true);
    expect(line.classList.contains("ai-settings-notice-error")).toBe(false);

    setNotice({ text: "Test failed: 401 Unauthorized", tone: "error" });
    await waitFor(() =>
      expect(document.querySelector(".ai-settings-notice")!.classList.contains("ai-settings-notice-error")).toBe(true),
    );
    expect(document.querySelector(".ai-settings-notice")!.classList.contains("ai-settings-notice-success")).toBe(false);
    // The text is untouched: only the tone says how to read it.
    expect(document.querySelector(".ai-settings-notice")!.textContent).toBe("Test failed: 401 Unauthorized");
  });
});

describe("the dialog and the panel's model selector", () => {
  it("share one list and one update path", async () => {
    mockBackend([GATEWAY, DEEPSEEK, GOOGLE]);
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "load_ai_config") {
        return { version: 2, active_provider: "gateway", providers: [GATEWAY, DEEPSEEK, GOOGLE].map(rustProvider), warnings: [] };
      }
      if (cmd === "save_ai_config") return { path: "/tmp/ai-providers.yaml", warnings: [] };
      if (cmd === "ai_key_status") return true;
      if (cmd === "ai_catalog") return CATALOG;
      if (cmd === "ai_model_limits") return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
      if (cmd === "ai_tools") return [];
      if (cmd === "ai_toolsets") return [];
      return makeSnapshot();
    });

    await store.initProject();
    const view = render(() => <><AiPanel /><PreferencesHost /></>);
    await waitFor(() => expect(view.container.querySelector(".ai-save-notice")).toBeFalsy());

    const picker = view.container.querySelector(".ai-model-picker .oc-dropdown-trigger") as HTMLElement;
    expect(picker.textContent).toContain("gateway-70b");

    fireEvent.click(screen.getByRole("button", { name: "Provider settings" }));
    await waitFor(() => expect(dialog()).toBeTruthy());
    expect(modelTrigger().textContent).toContain("gateway-70b");

    // Both lists are the same list, so the dialog's pick reaches the panel.
    fireEvent.click(modelTrigger());
    const dialogList = await screen.findByRole("listbox");
    const dialogLabels = optionLabels(dialogList);
    fireEvent.click([...dialogList.querySelectorAll('[role="option"]')]
      .find((entry) => entry.textContent?.includes("deepseek-flash")) as HTMLElement);
    await waitFor(() => expect(getActiveProvider().model).toBe("deepseek-flash"));

    fireEvent.click(picker);
    const panelList = await screen.findByRole("listbox");
    expect(optionLabels(panelList)).toEqual(dialogLabels);
    fireEvent.pointerDown(document.body);

    expect(picker.textContent).toContain("deepseek-flash");
    expect(modelTrigger().textContent).toContain("deepseek-flash");
  });
});

/** The default providers are merged in; the fixture ids must survive them. */
it("keeps the saved providers in file order and adds none", async () => {
  mockBackend([GATEWAY, DEEPSEEK, GOOGLE]);
  await loadConfig();
  expect(aiConfig.config.providers.map((provider) => provider.id)).toEqual([
    "gateway",
    "deepseek-pro",
    "google-pro",
  ]);
});
