import { describe, expect, it, vi } from "vitest";
import { invoke } from "@tauri-apps/api/core";
import { DEFAULT_PROVIDERS, aiConfig, authProvidersFor, normaliseSaved, saveConfig, loadConfig, wireOf, type AiProvider } from "../ai/config";
import { rustAiProvider } from "./setup";
import { modelFor, type Catalog } from "../ai/catalog";
import { applyModelChoice, modelKey } from "../ai/modelPicker";

const CAPS = { text: true, audio_in: false, audio_out: false, native_tool_calling: true, streaming: true };

/** One provider in the saved-file shape, for a load fixture. */
const rustOf = rustAiProvider;

/** Seed the store as a configured install: the catalogue is not installed for
 * the user, so every test that saves or switches a provider says so first. */
async function seed(providers: AiProvider[] = DEFAULT_PROVIDERS) {
  vi.mocked(invoke).mockResolvedValueOnce({
    version: 2,
    active_provider: providers[0]?.id ?? "",
    providers: providers.map(rustOf),
  });
  await loadConfig();
}

describe("provider upgrades", () => {
  it("saves metadata only and propagates backend failure without plaintext fallback", async () => {
    const mock=vi.mocked(invoke);
    await seed();
    mock.mockResolvedValueOnce("fixture-path");
    await saveConfig();
    const saved=mock.mock.calls.findLast(c=>c[0]==="save_ai_config")?.[1];
    expect(JSON.stringify(saved)).not.toContain("apiKey");
    expect(JSON.stringify(saved)).not.toMatch(/sk-[A-Za-z0-9]/);
    expect((saved as { config: { providers: { auth: string }[] } }).config.providers[0].auth).toBe("api_key");
    mock.mockRejectedValueOnce(new Error("store unavailable"));
    await expect(saveConfig()).rejects.toThrow("store unavailable");
    // A failed load keeps the backend's own words: a corrupt file has to name
    // the parse fault, not blame a missing desktop backend.
    mock.mockRejectedValueOnce(new Error("Failed to parse YAML: bad indentation"));
    await expect(loadConfig()).rejects.toThrow("Failed to parse YAML: bad indentation");
  });
  const current = DEFAULT_PROVIDERS.find(p => p.id === "openai-gpt4o")!;
  it("keeps a saved model id while still normalising an untouched built-in", () => {
    const result = normaliseSaved({ activeProviderId: current.id,
      providers: [{ ...current, model: "gpt-4o" }] });
    expect(result.activeProviderId).toBe(current.id);
    // The catalog is metadata: no substitution behind the user's back.
    expect(result.providers[0].model).toBe("gpt-4o");
    expect(result.providers[0].name).toBe(current.name);
    expect(result.providers[0].kind).toBe("openai_responses");
    expect(result.providers[0]).not.toHaveProperty("apiKey");
    expect(normaliseSaved(result)).toEqual(result);

    const gemini = DEFAULT_PROVIDERS.find(p => p.id === "gemini-2.5-pro")!;
    const legacy = normaliseSaved({ activeProviderId: gemini.id,
      providers: [{ ...gemini, model: "gemini-2.5-pro-preview-06-05" }] });
    expect(legacy.providers[0].model).toBe("gemini-2.5-pro-preview-06-05");
  });
  it("preserves custom models and proxy endpoints", () => {
    for (const change of [{ model: "my-model" }, { endpoint: "https://example.test/v1/chat/completions", model: "gpt-4o" }]) {
      const provider = { ...current, ...change };
      expect(normaliseSaved({ activeProviderId: provider.id, providers: [provider] }).providers[0]).toEqual(provider);
    }
  });
  it("unlocks the key when the load or a pick switches provider", async () => {
    const mock = vi.mocked(invoke);
    const catalog: Catalog = {
      version: "fixture",
      providers: [
        {
          id: "deepseek", name: "DeepSeek", wire: "openai-chat-completions",
          hosts: ["api.deepseek.com"], auth_providers: [],
          models: [{ id: "deepseek-flash", name: "Flash", context: 1000000, max_output: 8192, capabilities: CAPS, status: "active" }],
        },
        {
          id: "openai", name: "OpenAI", wire: "openai-responses",
          hosts: ["api.openai.com"], auth_providers: [],
          models: [{ id: "gpt-6.1-sol", name: "Sol", context: 1050000, max_output: 128000, capabilities: CAPS, status: "active" }],
        },
      ],
    };
    const openai = DEFAULT_PROVIDERS.find(p => p.id === "openai-gpt4o")!;
    mock.mockResolvedValueOnce({
      version: 2,
      active_provider: DEFAULT_PROVIDERS[0].id,
      providers: [DEFAULT_PROVIDERS[0], openai].map(rustOf),
    });
    await loadConfig();
    const warmed = () => mock.mock.calls
      .filter(([command]) => command === "ai_warm_key")
      .map(([, args]) => (args as { providerId: string }).providerId);
    // The key is unlocked as the panel loads, before anything asks for a turn.
    expect(warmed()).toContain(DEFAULT_PROVIDERS[0].id);

    mock.mockClear();
    expect(applyModelChoice(catalog, modelKey("catalog:openai", "gpt-6.1-sol"))).toBe(true);
    // A pick that changes provider unlocks that provider too, so the first send
    // never opens the OS prompt mid-turn.
    expect(warmed()).toContain("openai-gpt4o");

    // Re-picking the model already shown changes nothing and asks for nothing.
    mock.mockClear();
    expect(applyModelChoice(catalog, modelKey("catalog:openai", "gpt-6.1-sol"))).toBe(true);
    expect(warmed()).toHaveLength(0);
  });

  it("offers no sign-in in a build without a client registration", () => {
    expect(authProvidersFor("openai-responses")).toEqual([]);
    expect(authProvidersFor("openai-chat-completions")).toEqual([]);
    expect(authProvidersFor("anthropic-messages")).toEqual([]);
    expect(authProvidersFor("gemini-interactions")).toEqual([]);
    expect(authProvidersFor("gemini-generate")).toEqual([]);
    expect(DEFAULT_PROVIDERS.filter(p => p.auth === "oauth")).toEqual([]);
  });

  it("keeps a saved file to the providers it lists", async () => {
    // A saved file that lists only DeepSeek keeps only DeepSeek: the catalogue
    // is a template for Add provider, not rows the user owns.
    vi.mocked(invoke).mockResolvedValueOnce({
      version: 2, active_provider: "deepseek-v4-flash",
      providers: [rustOf(DEFAULT_PROVIDERS.find(p => p.id === "deepseek-v4-flash")!)],
    });
    await loadConfig();
    expect(aiConfig.config.providers.map(p => p.id)).toEqual(["deepseek-v4-flash"]);
  });

  it("ignores saved entries whose protocol or sign-in this build does not know", async () => {
    const deepseek = rustOf(DEFAULT_PROVIDERS.find(p => p.id === "deepseek-v4-flash")!);
    const chatgpt = rustOf({
      ...DEFAULT_PROVIDERS.find(p => p.id === "openai-gpt4o")!,
      auth: "oauth",
      authProvider: "openai_chat_gpt",
    });
    // Tags of integrations this build no longer ships: an unknown wire, an
    // unknown legacy kind with no wire, and an unknown sign-in on a known wire.
    const retired = [
      { ...deepseek, id: "old-wire", wire: "antigravity", kind: "antigravity", auth: "oauth", auth_provider: "antigravity" },
      { ...deepseek, id: "old-kind", wire: undefined, kind: "antigravity" },
      { ...chatgpt, id: "old-sign-in", auth_provider: "github_copilot" },
    ];
    // A tag the backend reports as null is absent, not unknown.
    const nulls = { ...deepseek, id: "null-tags", wire: null, auth_provider: null };
    vi.mocked(invoke).mockResolvedValueOnce({
      version: 2, active_provider: "old-wire", providers: [...retired, deepseek, chatgpt, nulls],
    });
    await loadConfig();
    expect(aiConfig.config.providers.map(p => p.id)).toEqual(["deepseek-v4-flash", "null-tags"]);
    expect(aiConfig.config.providers[1].wire).toBe("openai-chat-completions");
    expect(aiConfig.config.providers[1].authProvider).toBeUndefined();
    // The active id named a dropped entry, so the panel falls back to a real one
    // instead of describing a provider it cannot speak to.
    expect(aiConfig.getActiveProvider().id).toBe("deepseek-v4-flash");
    expect(aiConfig.getActiveProvider().wire).toBe("openai-chat-completions");
  });

});

describe("legacy wire entries", () => {
  it("defaults a missing protocol to Chat Completions and api_key", async () => {
    vi.mocked(invoke).mockResolvedValueOnce({
      active_provider: "legacy",
      providers: [{
        id: "legacy", name: "Legacy", endpoint: "https://example.test/v1/chat/completions",
        model: "gpt-4o", context_length: 32000, max_tokens: 1024, temperature: 0.3,
        capabilities: { text: true, audio_in: false, audio_out: false, native_tool_calling: false, streaming: true },
      }],
    });
    await loadConfig();
    expect(aiConfig.config.providers[0].kind).toBe("openai_chat_completions");
    expect(aiConfig.config.providers[0].auth).toBe("api_key");
    expect(aiConfig.config.providers[0].authProvider).toBeUndefined();
  });
});

describe("saved model ids", () => {
  it("loads a saved legacy id unchanged and leaves it for the not-in-catalog path", async () => {
    const saved = DEFAULT_PROVIDERS.find(p => p.id === "openai-gpt4o")!;
    vi.mocked(invoke).mockResolvedValueOnce({
      active_provider: saved.id,
      providers: [{
        id: saved.id, name: saved.name, kind: "openai_responses", auth: "api_key",
        endpoint: saved.endpoint, model: "gpt-4o",
        context_length: saved.contextLength, max_tokens: saved.maxTokens, temperature: 0.3,
        capabilities: { text: true, audio_in: false, audio_out: false,
          native_tool_calling: true, streaming: true },
      }],
    });
    await loadConfig();
    const loaded = aiConfig.config.providers.find(p => p.id === saved.id)!;
    expect(loaded.model).toBe("gpt-4o");
    // The lookup the panel marks "not in catalog" against has no gpt-4o, so the
    // entry stays visible as the user's model (and the backend warning names it)
    // instead of being healed to the shipped id.
    const catalog: Catalog = {
      version: "fixture",
      providers: [{
        id: "openai", name: "OpenAI", kind: "openai_responses",
        hosts: ["api.openai.com"], auth_providers: [],
        models: [{
          id: "gpt-6.1-sol", name: "GPT-6.1 Sol", context: 1050000, max_output: 128000,
          capabilities: { text: true, audio_in: false, audio_out: false,
            native_tool_calling: true, streaming: true },
          status: "active",
        }],
      }],
    };
    expect(modelFor(catalog, wireOf(loaded), loaded.model)).toBeUndefined();
    expect(modelFor(catalog, wireOf(loaded), "gpt-6.1-sol")).toBeDefined();
  });
});
