/**
 * AI Provider Configuration
 *
 * Config file: ~/.opencompose/ai-providers.yaml
 * Backend (Rust) handles file I/O. Frontend loads/displays/saves via IPC.
 *
 * Each provider declares connection info, model capabilities,
 * and context limits. The system prompt adapts based on capabilities.
 */
import { createSignal } from "solid-js";
import { createStore } from "solid-js/store";
import { invoke } from "@tauri-apps/api/core";
import { log } from "../stores/log";

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

export interface ModelCapabilities {
  text: boolean;
  audioIn: boolean;
  audioOut: boolean;
  nativeToolCalling: boolean;
  streaming: boolean;
}

export type ProviderKind =
  | "openai_chat_completions"
  | "openai_responses"
  | "anthropic_messages"
  | "gemini_interactions"
  | "gemini_generate";

export const PROVIDER_KINDS: ProviderKind[] = [
  "openai_chat_completions",
  "openai_responses",
  "anthropic_messages",
  "gemini_interactions",
  "gemini_generate",
];

/** Catalog protocol id. The backend selects the adapter from this value. */
export type Wire =
  | "openai-chat-completions"
  | "openai-responses"
  | "anthropic-messages"
  | "gemini-interactions"
  | "gemini-generate";

/** Keeps the saved wire in step when the settings protocol changes. */
export const WIRE_FOR_KIND: Record<ProviderKind, Wire> = {
  openai_chat_completions: "openai-chat-completions",
  openai_responses: "openai-responses",
  anthropic_messages: "anthropic-messages",
  gemini_interactions: "gemini-interactions",
  gemini_generate: "gemini-generate",
};

/** Legacy selector for a saved wire, so an older config's control shows the
 * protocol it actually speaks. */
export const KIND_FOR_WIRE: Record<Wire, ProviderKind> = {
  "openai-chat-completions": "openai_chat_completions",
  "openai-responses": "openai_responses",
  "anthropic-messages": "anthropic_messages",
  "gemini-interactions": "gemini_interactions",
  "gemini-generate": "gemini_generate",
};

export type AuthKind = "api_key" | "oauth";

export type AuthProvider =
  | "openai_chat_gpt"
  | "anthropic"
  | "gemini_cli"
  | "xai";

/** Sign-in tags this build can speak to. None ship with a client registration,
 * so a saved OAuth entry is dropped with a warning instead of shown. */
export const AUTH_PROVIDERS: AuthProvider[] = [];

/** The protocol a saved provider speaks. `wire` is authoritative; `kind` is the
 * field older configs carry, and the settings dropdown still edits both. */
export function wireOf(provider: { kind?: ProviderKind; wire?: Wire }): Wire {
  return provider.wire ?? WIRE_FOR_KIND[provider.kind ?? "openai_chat_completions"];
}

/** No OAuth sign-in ships in this build; saved OAuth entries stay readable so
 * they can be dropped with a warning when the configuration loads. */
export function authProvidersFor(_wire: Wire): AuthProvider[] {
  return [];
}

const WIRES = Object.keys(KIND_FOR_WIRE) as Wire[];

function knownKind(value: unknown): ProviderKind | undefined {
  return PROVIDER_KINDS.find((kind) => kind === value);
}

function knownWire(value: unknown): Wire | undefined {
  return WIRES.find((wire) => wire === value);
}

function knownAuthProvider(value: unknown): AuthProvider | undefined {
  return AUTH_PROVIDERS.find((provider) => provider === value);
}

export interface AiProvider {
  id: string;
  name: string;
  kind: ProviderKind;
  wire?: Wire;
  auth: AuthKind;
  authProvider?: AuthProvider;
  oauthAllowedHosts?: string[];
  endpoint: string;
  model: string;
  /** Extra model ids a saved provider declares; empty for catalog providers. */
  models?: string[];
  contextLength: number;
  maxTokens: number;
  temperature: number;
  /** Effort sent to protocols that declare thinking levels for the model. */
  thinkingLevel: string;
  capabilities: ModelCapabilities;
  builtin?: boolean;
}

export interface AiConfig {
  activeProviderId: string;
  providers: AiProvider[];
}

// Wire format from Rust (snake_case YAML)
interface RustAiConfig {
  version?: number;
  active_provider: string;
  providers: RustProvider[];
  /** Entries the catalog cannot describe, from the last load or save. */
  warnings?: ProviderWarning[];
}

/** An entry the catalog has no metadata for. Informational: the panel shows one
 * line, and nothing about the provider is refused or rewritten because of it. */
export interface ProviderWarning {
  provider_id: string;
  provider_name: string;
  model: string;
  message: string;
}

interface SaveResult {
  path: string;
  warnings?: ProviderWarning[];
}

/** One saved entry as the backend reports it. The tags stay untyped strings
 * here: `fromRust` is the boundary that turns them into the unions. */
interface RustProvider {
  id: string;
  name: string;
  kind?: string | null;
  wire?: string | null;
  auth?: string | null;
  auth_provider?: string | null;
  oauth_allowed_hosts?: string[];
  endpoint: string;
  model: string;
  models?: string[];
  context_length: number;
  max_tokens: number;
  temperature: number;
  thinking_level?: string;
  capabilities: {
    text: boolean;
    audio_in: boolean;
    audio_out: boolean;
    native_tool_calling: boolean;
    streaming: boolean;
  };
}

// ---------------------------------------------------------------------------
// Default Providers
// ---------------------------------------------------------------------------

/** Level an existing config gets: the adapters sent `medium` before the
 * setting existed, so the default request is unchanged. */
export const DEFAULT_THINKING_LEVEL = "medium";

/** Keep the saved level when the model declares it, otherwise fall back to
 * medium (or the model's first level). A model with no levels clears it. */
export function clampThinkingLevel(level: string | undefined, levels: string[] | undefined): string {
  if (!levels?.length) return DEFAULT_THINKING_LEVEL;
  if (level && levels.includes(level)) return level;
  return levels.includes(DEFAULT_THINKING_LEVEL) ? DEFAULT_THINKING_LEVEL : levels[0];
}

export const DEFAULT_PROVIDERS: AiProvider[] = [
  {
    id: "deepseek-v4-flash",
    name: "DeepSeek Flash",
    kind: "openai_chat_completions",
    auth: "api_key",
    endpoint: "https://api.deepseek.com/chat/completions",
    model: "deepseek-flash",
    contextLength: 1000000,
    maxTokens: 8192,
    temperature: 0.3,
    thinkingLevel: "medium",
    capabilities: { text: true, audioIn: false, audioOut: false, nativeToolCalling: true, streaming: true },
    builtin: true,
  },
  {
    id: "deepseek-v4-pro",
    name: "DeepSeek V4 Pro",
    kind: "openai_chat_completions",
    auth: "api_key",
    endpoint: "https://api.deepseek.com/chat/completions",
    model: "deepseek-v4-pro",
    contextLength: 1000000,
    maxTokens: 8192,
    temperature: 0.3,
    thinkingLevel: "medium",
    capabilities: { text: true, audioIn: false, audioOut: false, nativeToolCalling: true, streaming: true },
    builtin: true,
  },
  {
    id: "openai-gpt4o",
    name: "OpenAI GPT-6.1 Sol",
    kind: "openai_responses",
    auth: "api_key",
    endpoint: "https://api.openai.com/v1/responses",
    model: "gpt-6.1-sol",
    contextLength: 128000,
    maxTokens: 16384,
    temperature: 0.3,
    thinkingLevel: "medium",
    capabilities: { text: true, audioIn: false, audioOut: false, nativeToolCalling: true, streaming: true },
    builtin: true,
  },
  {
    id: "anthropic-claude",
    name: "Anthropic Claude",
    kind: "anthropic_messages",
    auth: "api_key",
    endpoint: "https://api.anthropic.com/v1/messages",
    model: "claude-opus-4-6",
    contextLength: 200000,
    maxTokens: 8192,
    temperature: 0.3,
    thinkingLevel: "medium",
    capabilities: { text: true, audioIn: false, audioOut: false, nativeToolCalling: true, streaming: true },
    builtin: true,
  },
  {
    id: "gemini-2.5-pro",
    name: "Gemini 4 Argon",
    kind: "gemini_interactions",
    auth: "api_key",
    endpoint: "https://generativelanguage.googleapis.com/v1beta/interactions",
    model: "gemini-4-argon",
    contextLength: 1048576,
    maxTokens: 65536,
    temperature: 0.3,
    thinkingLevel: "medium",
    capabilities: { text: true, audioIn: true, audioOut: false, nativeToolCalling: true, streaming: true },
    builtin: true,
  },
  {
    id: "opencode-go",
    name: "OpenCode Go",
    kind: "gemini_generate",
    wire: "gemini-generate",
    auth: "api_key",
    endpoint: "https://opencode.ai/inference/go/google/v1beta",
    model: "gemini-4-argon",
    contextLength: 1048576,
    maxTokens: 65536,
    temperature: 0.3,
    thinkingLevel: "medium",
    capabilities: { text: true, audioIn: true, audioOut: false, nativeToolCalling: true, streaming: true },
    builtin: true,
  },
  {
    id: "ollama",
    name: "Ollama (Local)",
    kind: "openai_chat_completions",
    auth: "api_key",
    endpoint: "http://localhost:11434/v1/chat/completions",
    model: "llama3.1",
    contextLength: 8192,
    maxTokens: 4096,
    temperature: 0.3,
    thinkingLevel: "medium",
    capabilities: { text: true, audioIn: false, audioOut: false, nativeToolCalling: false, streaming: true },
    builtin: true,
  },
  {
    id: "custom",
    name: "Custom Endpoint",
    kind: "openai_chat_completions",
    auth: "api_key",
    endpoint: "",
    model: "",
    contextLength: 1_000_000,
    maxTokens: 16_000,
    temperature: 0.3,
    thinkingLevel: "medium",
    capabilities: { text: true, audioIn: false, audioOut: false, nativeToolCalling: false, streaming: false },
    builtin: true,
  },
];

/** An install with no saved provider: the catalogue is a template list, not a
 * row in the user's configuration. */
const DEFAULT_CONFIG: AiConfig = {
  activeProviderId: "",
  providers: [],
};

/** Stands in for the active provider while none is configured, so callers that
 * read a field keep a total function without inventing a catalogue entry. */
export const NO_PROVIDER: AiProvider = {
  id: "",
  name: "",
  kind: "openai_chat_completions",
  auth: "api_key",
  endpoint: "",
  model: "",
  contextLength: 1_000_000,
  maxTokens: 16_000,
  temperature: 0.3,
  thinkingLevel: DEFAULT_THINKING_LEVEL,
  capabilities: {
    text: true,
    audioIn: false,
    audioOut: false,
    nativeToolCalling: false,
    streaming: false,
  },
};

// ---------------------------------------------------------------------------
// Conversion between frontend (camelCase) and Rust (snake_case) formats
// ---------------------------------------------------------------------------

/** The protocol and sign-in a saved entry names, or null when it names a tag
 * this build does not know (an integration that no longer exists). */
function parseTags(p: RustProvider): { wire: Wire; authProvider?: AuthProvider } | null {
  // An absent tag may arrive as a missing key or as null.
  let wire: Wire | undefined;
  if (p.wire != null) wire = knownWire(p.wire);
  else if (p.kind == null) wire = WIRE_FOR_KIND.openai_chat_completions;
  else {
    const kind = knownKind(p.kind);
    wire = kind && WIRE_FOR_KIND[kind];
  }
  if (!wire) return null;
  if (p.auth_provider == null) return { wire };
  const authProvider = knownAuthProvider(p.auth_provider);
  return authProvider ? { wire, authProvider } : null;
}

function fromRust(rc: RustAiConfig): AiConfig {
  const providers: AiProvider[] = [];
  for (const p of rc.providers) {
    // An entry with an unknown protocol or sign-in cannot be spoken to or
    // edited, so it is left out rather than coerced into another provider.
    const tags = parseTags(p);
    if (!tags) {
      log.warn("ai", `Ignoring saved AI provider "${p.id}": unsupported protocol or sign-in`);
      continue;
    }
    providers.push(fromRustProvider(p, tags.wire, tags.authProvider));
  }
  return { activeProviderId: rc.active_provider, providers };
}

function fromRustProvider(p: RustProvider, wire: Wire, authProvider: AuthProvider | undefined): AiProvider {
  return {
    id: p.id,
    name: p.name,
    kind: KIND_FOR_WIRE[wire],
    wire,
    auth: p.auth === "oauth" ? "oauth" : "api_key",
    authProvider,
    oauthAllowedHosts: p.oauth_allowed_hosts ?? [],
    endpoint: p.endpoint,
    model: p.model,
    ...(p.models?.length ? { models: p.models } : {}),
    contextLength: p.context_length,
    maxTokens: p.max_tokens,
    temperature: p.temperature,
    thinkingLevel: p.thinking_level ?? DEFAULT_THINKING_LEVEL,
    capabilities: {
      text: p.capabilities.text,
      audioIn: p.capabilities.audio_in,
      audioOut: p.capabilities.audio_out,
      nativeToolCalling: p.capabilities.native_tool_calling,
      streaming: p.capabilities.streaming,
    },
    // Built-in identity is catalogue knowledge, so it is derived here rather
    // than written to the file. A saved entry that shares a catalogue id is
    // that catalogue entry: it cannot be removed, only reconfigured.
    ...(DEFAULT_PROVIDERS.some(def => def.id === p.id) ? { builtin: true } : {}),
  };
}

function toRust(cfg: AiConfig): RustAiConfig {
  return {
    version: 2,
    active_provider: cfg.activeProviderId,
    providers: cfg.providers.map(p => ({
      id: p.id,
      name: p.name,
      kind: KIND_FOR_WIRE[wireOf(p)],
      wire: wireOf(p),
      auth: p.auth ?? "api_key",
      ...(p.authProvider ? { auth_provider: p.authProvider } : {}),
      ...(p.oauthAllowedHosts?.length ? { oauth_allowed_hosts: p.oauthAllowedHosts } : {}),
      endpoint: p.endpoint,
      model: p.model,
      ...(p.models?.length ? { models: p.models } : {}),
      context_length: p.contextLength,
      max_tokens: p.maxTokens,
      temperature: p.temperature,
      thinking_level: p.thinkingLevel ?? DEFAULT_THINKING_LEVEL,
      capabilities: {
        text: p.capabilities.text,
        audio_in: p.capabilities.audioIn,
        audio_out: p.capabilities.audioOut,
        native_tool_calling: p.capabilities.nativeToolCalling,
        streaming: p.capabilities.streaming,
      },
    })),
  };
}

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

const [config, setConfig] = createStore<AiConfig>(structuredClone(DEFAULT_CONFIG));
const [configLoaded, setConfigLoaded] = createSignal(false);
export const aiConfigLoaded = configLoaded;
const [savedConfig, setSavedConfig] = createSignal("");
const [configWarnings, setConfigWarnings] = createSignal<ProviderWarning[]>([]);
/** Why the last automatic write failed, or null. */
const [saveError, setSaveError] = createSignal<string | null>(null);
export const hasUnsavedConfig = () => JSON.stringify(toRust(config)) !== savedConfig();
export const configSaveError = saveError;

/** Writes the current settings now. The header's model and effort pickers call
 * this so a pick is never a draft; the settings dialog keeps its own Save so an
 * invalid form is not written. Never throws: a failure lands in `saveError`. */
export async function flushSave(): Promise<boolean> {
  try {
    await saveConfig();
    setSaveError(null);
    return true;
  } catch (error) {
    setSaveError(error instanceof Error ? error.message : String(error));
    return false;
  }
}

/** Drops the failure banner once the user has seen it. */
export function clearSaveError(): void {
  setSaveError(null);
}

/** A dismissal only clears the line until the next load or save reports again. */
function dismissWarnings(): void {
  setConfigWarnings([]);
}

/** Keys typed but not saved, one per provider. Memory only, never persisted: a
 * closed dialog must not silently drop what the operator pasted, and a draft
 * typed for one provider must never be written to another. */
const [keyDrafts, setKeyDrafts] = createStore<Record<string, string>>({});

export function keyDraftFor(providerId: string): string {
  return keyDrafts[providerId] ?? "";
}

export function setKeyDraftFor(providerId: string, value: string): void {
  setKeyDrafts(providerId, value);
}

/** True while any provider has a typed but unsaved key. */
export function hasAnyKeyDraft(): boolean {
  return Object.values(keyDrafts).some((value) => value.length > 0);
}

/** Drops every typed key; the OS store is untouched. */
export function clearKeyDrafts(): void {
  setKeyDrafts({});
}

/** Asks the backend to unlock one provider's key now, so no turn ever opens the
 * OS prompt. Fire and forget: Settings reports the state it produced. */
export function warmCredential(providerId: string): void {
  void invoke("ai_warm_key", { providerId }).catch(() => {});
}

// ---------------------------------------------------------------------------
// Load / Save via Tauri IPC
// ---------------------------------------------------------------------------

/** Reloads the file, dropping every unsaved provider edit and typed key. */
export async function discardConfigChanges(): Promise<void> {
  await loadConfig();
  clearKeyDrafts();
}

export async function loadConfig(): Promise<void> {
  try {
    const rustConfig = await invoke<RustAiConfig>("load_ai_config");
    setConfigWarnings(rustConfig.warnings ?? []);
    if (rustConfig.providers.length > 0) {
      const parsed = fromRust(rustConfig);
      const merged = normaliseSaved(parsed);
      setConfig(merged);
      // Snapshot what is in memory: normalising a built-in label still has to
      // compare equal to the file, or the panel reports unsaved edits forever
      // and Send stays disabled.
      setSavedConfig(JSON.stringify(toRust(merged)));
      // A readable file supersedes an older write failure: the notice describes
      // the current state, not the last attempt.
      setSaveError(null);
      log.info("ai", "Loaded AI config from backend");
    } else {
      // No file yet. The panel offers the catalogue through Add provider; it
      // does not claim those entries as saved, and writes nothing until the
      // user creates one.
      setConfig(structuredClone(DEFAULT_CONFIG));
      setSavedConfig(JSON.stringify(toRust(config)));
      setSaveError(null);
      log.info("ai", "No saved AI provider; the panel offers the catalogue");
    }
  } catch (error) {
    // The backend's own words name the file and the fault; replacing them with
    // "desktop backend is required" sent the operator after the wrong problem.
    throw new Error(
      `Could not load AI settings: ${error instanceof Error ? error.message : String(error)}`,
    );
  }
  setConfigLoaded(true);
  // Unlock the key before anything is asked of it: macOS asks once per build,
  // and the question must not arrive in the middle of a turn.
  if (hasProvider()) warmCredential(getActiveProvider().id);
}

export async function saveConfig(): Promise<void> {
  const result = await invoke<SaveResult>("save_ai_config", { config: toRust(config) });
  setConfigWarnings(result.warnings ?? []);
  setSavedConfig(JSON.stringify(toRust(config)));
  // A write that landed clears an older failure line, so the banner only ever
  // describes the current state of the file.
  setSaveError(null);
}

/** Repair an untouched built-in's label/kind/capabilities after a load.
 *
 * It never substitutes a model and never adds a provider the file does not
 * list: the catalogue is a template list for Add provider. */
export function normaliseSaved(saved: AiConfig): AiConfig {
  // Normalise an untouched built-in's label/kind/capabilities, never its model:
  // the catalog is metadata now, so a saved id stays exactly as the user saved
  // it and the panel marks an id it cannot describe instead of moving the user
  // silently. Stable IDs retain account keys and active selection.
  const legacy: Record<string, string> = {
    "deepseek-v4-flash": "deepseek-v4-flash",
    "openai-gpt4o": "gpt-4o",
    "gemini-2.5-pro": "gemini-2.5-pro-preview-06-05",
  };
  const providers = saved.providers.map(p => {
    const def = DEFAULT_PROVIDERS.find(d => d.id === p.id);
    if (!def || p.model !== legacy[p.id] || p.endpoint !== def.endpoint) return p;
    return { ...p, name: def.name, kind: def.kind, wire: WIRE_FOR_KIND[def.kind],
      maxTokens: Math.max(p.maxTokens, def.maxTokens), capabilities: { ...def.capabilities } };
  });
  // Missing catalogue entries are not added: an unconfigured provider must not
  // read as one of the user's own.
  return {
    activeProviderId: saved.activeProviderId || providers[0]?.id || "",
    providers,
  };
}

// ---------------------------------------------------------------------------
// Actions
// ---------------------------------------------------------------------------

export function getActiveProvider(): AiProvider {
  return config.providers.find(p => p.id === config.activeProviderId)
    ?? config.providers[0]
    ?? NO_PROVIDER;
}

/** Whether the user has configured a provider at all. */
export function hasProvider(): boolean {
  return config.providers.length > 0;
}

export function setActiveProvider(id: string): void {
  setConfig("activeProviderId", id);
}

export function updateProvider(id: string, updates: Partial<AiProvider>): void {
  const idx = config.providers.findIndex(p => p.id === id);
  if (idx < 0) return;
  setConfig("providers", idx, (p) => ({ ...p, ...updates }));
}

export function addProvider(provider: AiProvider): void {
  setConfig("providers", (prev) => [...prev, provider]);
}

export function removeProvider(id: string): void {
  const provider = config.providers.find(p => p.id === id);
  if (provider?.builtin) return;
  setConfig("providers", (prev) => prev.filter(p => p.id !== id));
  setKeyDrafts(id, "");
  if (config.activeProviderId === id) {
    setConfig("activeProviderId", config.providers[0]?.id ?? "");
  }
}

// ---------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------

export const aiConfig = {
  config,
  configLoaded,
  warnings: configWarnings,
  dismissWarnings,
  loadConfig,
  saveConfig,
  flushSave,
  saveError: configSaveError,
  clearSaveError,
  keyDraftFor,
  setKeyDraftFor,
  warmCredential,
  getActiveProvider,
  setActiveProvider,
  updateProvider,
  addProvider,
  removeProvider,
};
