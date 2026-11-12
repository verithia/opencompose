/**
 * Interaction matrix for the AI settings dialog: provider/protocol/endpoint,
 * authentication, the key row, Test Connection, Save, the provider defaults and
 * the edit target. Every test drives the control and checks the resulting
 * state, including what the dialog refuses to do while a save or a test runs.
 */
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@solidjs/testing-library";
import { askDialogMock, chooseDropdown, invokeMock, makeSnapshot, makeTrack, aiConfigWire } from "./setup";
import { store } from "../stores/project";
import { AiPanel } from "../components/AiPanel";
import { PreferencesHost } from "../components/PreferencesModal";
import { aiConfig, getActiveProvider, hasUnsavedConfig } from "../ai/config";
import { i18n } from "../stores/i18n";
import { preferencesUi } from "../stores/preferencesUi";
import { aiWorkspace } from "../stores/aiWorkspace";

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

function deferred<T = unknown>() {
  let resolve!: (value: T) => void;
  let reject!: (reason?: unknown) => void;
  const promise = new Promise<T>((ok, fail) => { resolve = ok; reject = fail; });
  return { promise, resolve, reject };
}

const backend = {
  keyStatus: async (): Promise<boolean> => true,
  readKeyCalls: 0,
  save: async (): Promise<unknown> => ({ path: "/tmp/ai-providers.yaml", warnings: [] }),
  setKey: async (): Promise<unknown> => undefined,
  removeKey: async (): Promise<unknown> => undefined,
  chat: async (): Promise<unknown> => ({ role: "assistant", content: "hello" }),
  saveCalls: [] as Record<string, unknown>[],
  setKeyCalls: [] as Record<string, unknown>[],
  /** The sign-in is a background attempt: the dialog starts it, polls it, and
   * can cancel it. */
  oauthPoll: async (): Promise<unknown> => ({ state: "pending", message: "" }),
  oauthBeginCalls: 0,
  oauthCancelCalls: [] as string[],
};

function installBackend() {
  invokeMock.mockImplementation(async (cmd: string, args?: unknown) => {
    const typed = (args ?? {}) as Record<string, unknown>;
    switch (cmd) {
      case "ai_batch_recover": return null;
      case "load_ai_config": return aiConfigWire();
      case "save_ai_config": backend.saveCalls.push(typed); return backend.save();
      case "ai_key_status": return backend.keyStatus();
      case "ai_read_key": backend.readKeyCalls += 1; return backend.keyStatus();
      case "ai_warm_key": return undefined;
      case "ai_set_key": backend.setKeyCalls.push(typed); return backend.setKey();
      case "ai_remove_key": return backend.removeKey();
      case "ai_catalog": return CATALOG;
      case "ai_model_limits": return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
      case "ai_toolsets": return [];
      case "ai_tools": return [];
      case "ai_build_context": return { dsl_context: '@project "Test Project"\n' };
      case "ai_chat": return backend.chat();
      case "ai_oauth_begin":
        backend.oauthBeginCalls += 1;
        // Distinct ids, so a test can tell a retry from the attempt it replaced.
        return {
          attempt_id: `attempt-${backend.oauthBeginCalls}`,
          url: "https://auth.example.test/authorize?client_id=fixture",
          mode: "auto",
          expires_at: Date.now() + 300_000,
          opened: true,
        };
      case "ai_oauth_poll": return backend.oauthPoll();
      case "ai_oauth_cancel":
        backend.oauthCancelCalls.push(String(typed.attemptId ?? ""));
        return undefined;
      default: return makeSnapshot();
    }
  });
}

async function mount() {
  await store.initProject();
  const view = render(() => <><AiPanel /><PreferencesHost /></>);
  await waitFor(() => expect(view.container.querySelector(".ai-save-notice")).toBeFalsy());
  // Installing the defaults saves once; tests count only their own saves.
  backend.saveCalls = [];
  return view;
}

const dialog = () => screen.getByRole("dialog", { name: /AI settings|Preferences/ });
const settingsButton = () => screen.getByRole("button", { name: "Provider settings" });

async function openSettings() {
  fireEvent.click(settingsButton());
  await waitFor(() => expect(dialog()).toBeTruthy());
}

function selectAfterLabel(label: string): HTMLSelectElement {
  const node = screen.getByText(label).nextElementSibling;
  const select = node?.tagName === "SELECT" ? node : node?.querySelector("select");
  if (!(select instanceof HTMLSelectElement)) throw new Error(`no select after ${label}`);
  return select;
}

const protocolSelect = () => selectAfterLabel("Provider protocol");
const targetSelect = () => selectAfterLabel("Edit Target");
const endpointInput = () => screen.getByPlaceholderText("https://api.openai.com/v1/chat/completions") as HTMLInputElement;
const keyInput = () => screen.getByPlaceholderText(/Enter a new key/) as HTMLInputElement;
const saveButton = () => screen.getByRole("button", { name: /Save settings/ }) as HTMLButtonElement;
const removeKeyButton = () => screen.getByRole("button", { name: "Remove saved key" }) as HTMLButtonElement;
const testButton = () => screen.getByRole("button", { name: "Test Connection" }) as HTMLButtonElement;
const doneButton = () => document.querySelector(".preferences-modal-close") as HTMLButtonElement;
beforeEach(() => {
  invokeMock.mockReset();
  // The draft store is module state, so a key typed by an earlier test would
  // otherwise be on screen here.
  for (const id of ["deepseek-v4-flash", "anthropic-claude", "acme-gateway"]) {
    aiConfig.setKeyDraftFor(id, "");
  }
  backend.keyStatus = async () => true;
  backend.readKeyCalls = 0;
  backend.save = async () => ({ path: "/tmp/ai-providers.yaml", warnings: [] });
  backend.setKey = async () => undefined;
  backend.removeKey = async () => undefined;
  backend.chat = async () => ({ role: "assistant", content: "hello" });
  backend.saveCalls = [];
  backend.setKeyCalls = [];
  backend.oauthBeginCalls = 0;
  backend.oauthCancelCalls = [];
  backend.oauthPoll = async () => ({ state: "pending", message: "" });
  i18n.setLocale("en");
  preferencesUi.close();
  aiWorkspace.notify(null);
  aiWorkspace.setPanelBusy(false);
  askDialogMock.mockResolvedValue(true);
  installBackend();
});

afterEach(cleanup);

describe("provider and protocol", () => {
  it("follows the provider picker and keeps each provider's unsaved edit", async () => {
    await mount();
    await openSettings();

    fireEvent.input(endpointInput(), { target: { value: "https://api.deepseek.com/v2/chat" } });
    await waitFor(() => expect(hasUnsavedConfig()).toBe(true));

    chooseDropdown(screen.getByRole("button", { name: "Provider" }), "Anthropic Claude");
    await waitFor(() => expect(getActiveProvider().id).toBe("anthropic-claude"));
    expect(protocolSelect().value).toBe("anthropic_messages");
    expect(endpointInput().value).toBe("https://api.anthropic.com/v1/messages");

    // Coming back shows the draft the other provider still holds: it was kept,
    // not silently dropped.
    chooseDropdown(screen.getByRole("button", { name: "Provider" }), "DeepSeek Flash");
    await waitFor(() => expect(getActiveProvider().id).toBe("deepseek-v4-flash"));
    expect(endpointInput().value).toBe("https://api.deepseek.com/v2/chat");
    expect(hasUnsavedConfig()).toBe(true);
  });

  it("clears an unparseable token draft when the edited provider changes", async () => {
    await mount();
    await openSettings();
    const context = screen.getByLabelText("Context (tokens)") as HTMLInputElement;
    expect(context.value).toBe("1M");
    fireEvent.input(context, { target: { value: "abc" } });
    await waitFor(() => expect(saveButton().disabled).toBe(true));

    // Both providers carry the same 1M context, so only the provider change can
    // reset the field; its draft belongs to the provider it was typed for.
    chooseDropdown(screen.getByRole("button", { name: "Provider" }), "DeepSeek V4 Pro");
    await waitFor(() => expect(getActiveProvider().id).toBe("deepseek-v4-pro"));
    expect(context.value).toBe("1M");
    expect(screen.queryByText("Enter a token count such as 128K, 1M or 32000.")).toBeNull();
  });

  it("moves the wire with the protocol and re-marks the header model", async () => {
    await mount();
    await openSettings();
    fireEvent.change(protocolSelect(), { target: { value: "anthropic_messages" } });
    const provider = aiConfig.config.providers.find((entry) => entry.id === getActiveProvider().id)!;
    expect(provider.kind).toBe("anthropic_messages");
    expect(provider.wire).toBe("anthropic-messages");
    expect(hasUnsavedConfig()).toBe(true);

    const trigger = document.querySelector(".ai-model-picker .oc-dropdown-trigger") as HTMLElement;
    await waitFor(() => expect(trigger.getAttribute("data-model-known")).toBe("false"));
    expect(trigger.textContent).toContain("not in catalog");

    fireEvent.click(trigger);
    const listbox = await screen.findByRole("listbox");
    const labels = [...listbox.querySelectorAll('[role="option"]')]
      .map((option) => option.querySelector(".oc-dropdown-option-label")?.textContent ?? "");
    expect(labels.some((label) => label.includes("not in catalog"))).toBe(true);
    fireEvent.pointerDown(document.body);
  });

  it("surfaces a rejected endpoint instead of pretending it saved", async () => {
    await mount();
    await openSettings();
    backend.save = async () => { throw new Error("Use HTTPS (HTTP only on loopback), with no URL credentials, query or fragment."); };
    fireEvent.input(endpointInput(), { target: { value: "http://example.com/v1" } });
    fireEvent.click(saveButton());
    await waitFor(() => expect(document.body.textContent).toContain("Use HTTPS (HTTP only on loopback)"));
    expect(hasUnsavedConfig()).toBe(true);
    expect(dialog()).toBeTruthy();
  });

});

describe("key row", () => {
  it("saves a key without echoing it and clears the draft", async () => {
    await mount();
    await openSettings();
    fireEvent.input(keyInput(), { target: { value: "sk-secret" } });
    fireEvent.click(saveButton());
    await waitFor(() => expect(backend.setKeyCalls).toHaveLength(1));
    expect(backend.setKeyCalls[0]).toMatchObject({ providerId: "deepseek-v4-flash", key: "sk-secret" });
    expect(JSON.stringify(backend.saveCalls)).not.toContain("sk-secret");
    await waitFor(() => expect(keyInput().value).toBe(""));
    expect(dialog().textContent).toContain("Key saved in the OS credential store.");
  });

  it("disables Remove saved key when there is no key to remove", async () => {
    backend.keyStatus = async () => false;
    await mount();
    await openSettings();
    await waitFor(() => expect(dialog().textContent).toContain("No saved key."));
    expect(removeKeyButton().disabled).toBe(true);
  });

  it("removes a saved key and disables itself afterwards", async () => {
    let saved = true;
    backend.keyStatus = async () => saved;
    backend.removeKey = async () => { saved = false; };
    await mount();
    await openSettings();
    await waitFor(() => expect(removeKeyButton().disabled).toBe(false));
    fireEvent.click(removeKeyButton());
    await waitFor(() => expect(dialog().textContent).toContain("No saved key."));
    expect(removeKeyButton().disabled).toBe(true);
  });

  it("shows the store's own refusal and reads the key again on demand", async () => {
    const refused = "The OS credential store did not release the saved key";
    backend.keyStatus = async () => { throw new Error(refused); };
    await mount();
    await openSettings();
    // The store's words name the fix; a generic line would hide it.
    await waitFor(() => expect(dialog().textContent).toContain(refused));

    // "Read key again" is an explicit OS read, so a fixed prompt shows at once.
    backend.keyStatus = async () => true;
    fireEvent.click(screen.getByRole("button", { name: "Read key again" }));
    await waitFor(() => expect(dialog().textContent).toContain("Key saved in the OS credential store."));
    expect(backend.readKeyCalls).toBe(1);

    backend.setKey = async () => { throw new Error("keychain locked"); };
    fireEvent.input(keyInput(), { target: { value: "sk-secret" } });
    fireEvent.click(saveButton());
    await waitFor(() => expect(document.body.textContent).toContain("keychain locked"));
  });

  it("keeps a typed key for its own provider until it is saved", async () => {
    await mount();
    await openSettings();
    fireEvent.input(keyInput(), { target: { value: "sk-draft" } });
    // The status line has to say the digits on screen are not in effect yet.
    await waitFor(() => expect(dialog().textContent).toContain("This key is not saved yet."));

    fireEvent.click(doneButton());
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /AI settings|Preferences/ })).toBeNull());
    await openSettings();
    // Closing the dialog is not a silent loss: the draft comes back.
    expect(keyInput().value).toBe("sk-draft");

    // It belongs to the provider it was typed for, never to another one.
    chooseDropdown(screen.getByRole("button", { name: "Provider" }), "Anthropic Claude");
    await waitFor(() => expect(getActiveProvider().id).toBe("anthropic-claude"));
    expect(keyInput().value).toBe("");

    chooseDropdown(screen.getByRole("button", { name: "Provider" }), "DeepSeek Flash");
    await waitFor(() => expect(getActiveProvider().id).toBe("deepseek-v4-flash"));
    expect(keyInput().value).toBe("sk-draft");
    expect(backend.setKeyCalls).toHaveLength(0);
  });

  it("shows a failed automatic save instead of pretending the pick applied", async () => {
    await mount();
    await openSettings();
    backend.save = async () => { throw new Error('Provider id "fixture" is saved twice.'); };
    // A model or effort pick writes at once, so its failure has to surface in
    // the dialog that made it and not only in the panel behind this dialog.
    expect(await aiConfig.flushSave()).toBe(false);
    await waitFor(() => expect(dialog().textContent).toContain("Settings could not be saved"));
    expect(dialog().textContent).toContain("saved twice");
  });

  it("removes a provider the operator added and writes the removal", async () => {
    await mount();
    aiConfig.addProvider({
      id: "acme-gateway",
      name: "Acme Gateway",
      kind: "openai_chat_completions",
      auth: "api_key",
      endpoint: "https://llm.acme.test/v1/chat/completions",
      model: "acme-large",
      contextLength: 64000,
      maxTokens: 4096,
      temperature: 0.3,
      thinkingLevel: "medium",
      capabilities: { text: true, audioIn: false, audioOut: false, nativeToolCalling: false, streaming: true },
    });
    await openSettings();
    chooseDropdown(screen.getByRole("button", { name: "Provider" }), "Acme Gateway");
    await waitFor(() => expect(getActiveProvider().id).toBe("acme-gateway"));
    fireEvent.click(screen.getByRole("button", { name: "Remove provider" }));
    await waitFor(() =>
      expect(aiConfig.config.providers.some((entry) => entry.id === "acme-gateway")).toBe(false));
    // The removal is written, so the next load does not bring the entry back.
    expect(JSON.stringify(backend.saveCalls.at(-1))).not.toContain("acme-gateway");
    // A built-in has no Remove button.
    expect(screen.queryByRole("button", { name: "Remove provider" })).toBeNull();
  });
});

describe("Test Connection", () => {
  it("is disabled before a save and reports success and failure tones after one", async () => {
    await mount();
    await openSettings();
    expect(testButton().disabled).toBe(false);
    // An unsaved key draft is also a reason to save before testing.
    fireEvent.input(keyInput(), { target: { value: "sk-draft" } });
    expect(testButton().disabled).toBe(true);
    fireEvent.input(keyInput(), { target: { value: "" } });
    await waitFor(() => expect(testButton().disabled).toBe(false));

    fireEvent.input(endpointInput(), { target: { value: "https://api.deepseek.com/v1/chat/completions" } });
    await waitFor(() => expect(testButton().disabled).toBe(true));
    expect(dialog().textContent).toContain("Tests the saved provider and model. Save your edits first.");

    fireEvent.click(saveButton());
    await waitFor(() => expect(testButton().disabled).toBe(false));
    fireEvent.click(testButton());
    await waitFor(() => expect(document.body.textContent).toContain("Connected. Response: hello"));
    const success = [...document.querySelectorAll<HTMLElement>(".ai-settings-column .selectable")]
      .find((element) => element.textContent?.includes("Connected. Response"))!;
    // Tone travels as a class so the stylesheet owns the colour.
    expect(success.classList.contains("ai-settings-notice-success")).toBe(true);

    backend.chat = async () => { throw new Error("network down"); };
    fireEvent.click(testButton());
    await waitFor(() => expect(document.body.textContent).toContain("network down"));
    const failure = [...document.querySelectorAll<HTMLElement>(".ai-settings-column .selectable")]
      .find((element) => element.textContent?.includes("network down"))!;
    expect(failure.classList.contains("ai-settings-notice-error")).toBe(true);
  });

  it("refuses to close the dialog while the test request runs", async () => {
    await mount();
    await openSettings();
    const pending = deferred();
    backend.chat = () => pending.promise;
    fireEvent.click(testButton());
    await waitFor(() => expect(testButton().disabled).toBe(true));

    fireEvent.click(doneButton());
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    fireEvent.click(document.querySelector(".preferences-overlay") as HTMLElement);
    expect(screen.queryByRole("dialog", { name: /AI settings|Preferences/ })).toBeTruthy();

    pending.resolve({ role: "assistant", content: "hello" });
    await waitFor(() => expect(testButton().disabled).toBe(false));
    fireEvent.click(doneButton());
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /AI settings|Preferences/ })).toBeNull());
  });
});

describe("Save settings", () => {
  it("persists, clears the unsaved state and keeps the dialog open", async () => {
    await mount();
    await openSettings();
    fireEvent.input(endpointInput(), { target: { value: "https://api.deepseek.com/v2/chat" } });
    await waitFor(() => expect(hasUnsavedConfig()).toBe(true));
    fireEvent.click(saveButton());
    await waitFor(() => expect(hasUnsavedConfig()).toBe(false));
    expect(savedEndpoint()).toBe("https://api.deepseek.com/v2/chat");
    expect(dialog()).toBeTruthy();
    await waitFor(() => expect(document.body.textContent).toContain("Saved settings. Keys are stored only in the OS credential store."));
  });

  it("refuses to close while the save is in flight", async () => {
    const pending = deferred();
    await mount();
    await openSettings();
    backend.save = () => pending.promise;
    fireEvent.input(endpointInput(), { target: { value: "https://api.deepseek.com/v2/chat" } });
    fireEvent.click(saveButton());
    await waitFor(() => expect(backend.saveCalls).toHaveLength(1));

    fireEvent.click(doneButton());
    fireEvent.keyDown(document.activeElement ?? document.body, { key: "Escape" });
    fireEvent.click(document.querySelector(".preferences-overlay") as HTMLElement);
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    expect(screen.queryByRole("dialog", { name: /AI settings|Preferences/ })).toBeTruthy();

    pending.resolve({ path: "/tmp/ai-providers.yaml", warnings: [] });
    await waitFor(() => expect(hasUnsavedConfig()).toBe(false));
    await waitFor(() => expect(testButton().disabled).toBe(false));
    fireEvent.click(doneButton());
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /AI settings|Preferences/ })).toBeNull());
  });

  it("saves with an empty key draft without touching the key store", async () => {
    await mount();
    await openSettings();
    fireEvent.input(endpointInput(), { target: { value: "https://api.deepseek.com/v2/chat" } });
    fireEvent.click(saveButton());
    await waitFor(() => expect(hasUnsavedConfig()).toBe(false));
    expect(backend.setKeyCalls).toHaveLength(0);
  });
});

function savedEndpoint(): string | undefined {
  const config = backend.saveCalls.at(-1)?.config as { providers: { id: string; endpoint: string }[] } | undefined;
  return config?.providers.find((provider) => provider.id === "deepseek-v4-flash")?.endpoint;
}

describe("provider defaults", () => {
  const contextInput = () => screen.getByLabelText("Context (tokens)") as HTMLInputElement;
  const outputInput = () => screen.getByLabelText("Maximum output tokens") as HTMLInputElement;
  const temperatureInput = () => screen.getByDisplayValue("0.3") as HTMLInputElement;

  it("blocks Save while a token field is unparseable instead of saving the old value silently", async () => {
    await mount();
    await openSettings();
    fireEvent.input(contextInput(), { target: { value: "abc" } });
    await waitFor(() => expect(screen.getByText("Enter a token count such as 128K, 1M or 32000.")).toBeTruthy());
    expect(saveButton().disabled).toBe(true);
    fireEvent.click(saveButton());
    expect(backend.saveCalls).toHaveLength(0);

    fireEvent.input(contextInput(), { target: { value: "128K" } });
    await waitFor(() => expect(saveButton().disabled).toBe(false));
    fireEvent.input(outputInput(), { target: { value: "abc" } });
    await waitFor(() => expect(saveButton().disabled).toBe(true));
  });

  it("names an output limit above the context and refuses to save it", async () => {
    await mount();
    await openSettings();
    fireEvent.input(contextInput(), { target: { value: "1000" } });
    fireEvent.input(outputInput(), { target: { value: "2000" } });
    await waitFor(() => expect(screen.getByText("Output limit must not exceed the context length.")).toBeTruthy());
    expect(saveButton().disabled).toBe(true);
    fireEvent.click(saveButton());
    expect(backend.saveCalls).toHaveLength(0);
  });

  it("keeps a temperature of 0 and blocks an out-of-range one", async () => {
    await mount();
    await openSettings();
    const temperature = temperatureInput();
    fireEvent.input(temperature, { target: { value: "0" } });
    await waitFor(() => expect(getActiveProvider().temperature).toBe(0));

    fireEvent.input(temperature, { target: { value: "5" } });
    await waitFor(() => expect(screen.getByText("Temperature must be between 0 and 2.")).toBeTruthy());
    expect(saveButton().disabled).toBe(true);
    expect(getActiveProvider().temperature).toBe(0);
  });

  it("saves valid defaults with the numbers the fields show", async () => {
    await mount();
    await openSettings();
    fireEvent.input(contextInput(), { target: { value: "128K" } });
    fireEvent.input(outputInput(), { target: { value: "4096" } });
    fireEvent.input(temperatureInput(), { target: { value: "0.7" } });
    await waitFor(() => expect(saveButton().disabled).toBe(false));
    fireEvent.click(saveButton());
    await waitFor(() => expect(backend.saveCalls).toHaveLength(1));
    const provider = (backend.saveCalls[0].config as { providers: Record<string, unknown>[] }).providers.find((entry) => entry.id === "deepseek-v4-flash")!;
    expect(provider.context_length).toBe(128000);
    expect(provider.max_tokens).toBe(4096);
    expect(provider.temperature).toBe(0.7);
  });
});

describe("edit target and limits", () => {
  it("shows a stale target instead of silently showing another one", async () => {
    invokeMock.mockImplementation(async (cmd: string) => {
      if (cmd === "ai_batch_recover") return null;
      if (cmd === "load_ai_config") return aiConfigWire();
      if (cmd === "save_ai_config") return { path: "/tmp/ai-providers.yaml", warnings: [] };
      if (cmd === "ai_key_status") return true;
      if (cmd === "ai_catalog") return CATALOG;
      if (cmd === "ai_model_limits") return { provider_id: "x", model_id: "y", bundled: null, overridden: null };
      if (cmd === "ai_toolsets") return [];
      if (cmd === "ai_tools") return [];
      if (cmd === "ai_build_context") return { dsl_context: "" };
      if (cmd === "ai_chat") return { role: "assistant", content: "ok" };
      if (cmd === "get_project") return makeSnapshot({ tracks: [makeTrack("lead", 0)] });
      return makeSnapshot();
    });
    await mount();
    await openSettings();
    fireEvent.change(targetSelect(), { target: { value: "track:lead" } });
    expect(targetSelect().value).toBe("track:lead");

    // The track disappears while the dialog is closed.
    store.applyAiSnapshot(makeSnapshot());
    fireEvent.click(doneButton());
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /AI settings|Preferences/ })).toBeNull());
    await openSettings();
    await waitFor(() => expect(targetSelect()).toBeTruthy());
    expect(targetSelect().value).toBe("track:lead");
    expect(targetSelect().selectedOptions[0]?.textContent).toContain("track:lead");
  });

  it("shows the custom-endpoint placeholder with no model and no limits to edit", async () => {
    await mount();
    await openSettings();
    chooseDropdown(screen.getByRole("button", { name: "Provider" }), "Custom Endpoint");
    await waitFor(() => expect(getActiveProvider().id).toBe("custom"));
    expect(endpointInput().value).toBe("");
    expect(screen.getByRole("button", { name: "Edit limits" }).hasAttribute("disabled")).toBe(true);

    // The empty endpoint is a draft, not a saved provider: closing discards it
    // and the panel keeps the provider it had.
    fireEvent.click(doneButton());
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /AI settings|Preferences/ })).toBeNull());
    const trigger = document.querySelector(".ai-model-picker .oc-dropdown-trigger") as HTMLElement;
    expect(trigger.textContent).toContain("deepseek-flash");
  });

  it("disables Edit limits without a model and opens the limits dialog with one", async () => {
    await mount();
    await openSettings();
    const editLimits = screen.getByRole("button", { name: "Edit limits" }) as HTMLButtonElement;
    expect(editLimits.disabled).toBe(false);
    fireEvent.click(editLimits);
    const limits = await waitFor(() => screen.getByRole("dialog", { name: "Model limits" }));
    fireEvent.click(within(limits).getByRole("button", { name: "Close" }));
    await waitFor(() => expect(screen.queryByRole("dialog", { name: "Model limits" })).toBeNull());
  });

  it("guards a provider draft after switching to another page", async () => {
    await mount();
    await openSettings();
    fireEvent.input(endpointInput(), { target: { value: "https://api.deepseek.com/v2/chat" } });
    await waitFor(() => expect(hasUnsavedConfig()).toBe(true));

    fireEvent.click(screen.getByRole("button", { name: i18n.t("preferences.page.display") }));
    let closed = false;
    askDialogMock.mockResolvedValueOnce(false);
    await preferencesUi.requestClose(() => { closed = true; });
    expect(closed).toBe(false);
    expect(screen.queryByRole("dialog", { name: /AI settings|Preferences/ })).toBeTruthy();

    askDialogMock.mockResolvedValueOnce(true);
    await preferencesUi.requestClose(() => { closed = true; });
    expect(closed).toBe(true);
  });

  it("returns focus to the Settings button when the dialog closes", async () => {
    await mount();
    settingsButton().focus();
    await openSettings();
    fireEvent.click(doneButton());
    await waitFor(() => expect(screen.queryByRole("dialog", { name: /AI settings|Preferences/ })).toBeNull());
    expect(document.activeElement).toBe(settingsButton());
  });
});
