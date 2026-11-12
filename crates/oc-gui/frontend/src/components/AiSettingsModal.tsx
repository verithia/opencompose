/**
 * AI settings modal: the whole settings surface, wide enough for two columns so
 * the narrow right sidebar only keeps the provider and model pickers, a
 * summary, the toolset thumbnail and the buttons that open this dialog.
 *
 * Connection lives on the left; the right column is the model group (the same
 * selector the header carries, the effort, the provider defaults and the
 * per-model limits), then capabilities, edit target and the toolset
 * switchboard. The body scrolls inside the dialog.
 */
import { For, Show, createEffect, createMemo, createSignal, on, onCleanup, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import { TokenCountInput } from "./TokenCountInput";
import { keyStatus, readKey as readStoredKey, removeKey as removeStoredKey, setKey } from "../ai/api";
import { ask } from "@tauri-apps/plugin-dialog";
import {
  aiConfig,
  authProvidersFor,
  DEFAULT_THINKING_LEVEL,
  getActiveProvider,
  hasProvider,
  hasUnsavedConfig,
  PROVIDER_KINDS,
  WIRE_FOR_KIND,
  wireOf,
  type AuthKind,
  type AuthProvider,
  type AiProvider,
  type ProviderKind,
} from "../ai/config";
import { catalogProviderFor, modelFor, type Catalog } from "../ai/catalog";
import {
  activeModelGroup,
  activeModelKnown,
  applyModelChoice,
  listModelChoices,
  modelKey,
  modelLevels,
  type ModelChoice,
} from "../ai/modelPicker";
import { audioAccepted } from "../ai/media";
import { noteIsOverLimit, readProjectInstructions, writeProjectInstructions } from "../ai/projectInstructions";
import { PROJECT_NOTE_MAX_CHARS, PROJECT_NOTE_TOKEN_BUDGET } from "../ai/system-prompt";
import { errorMessage } from "../ai/errors";
import {
  oauthBegin,
  oauthCancel,
  oauthPoll,
  oauthSignOut,
  oauthStatus,
  type OAuthAttempt,
  type OAuthStatus,
} from "../ai/oauth";
import type { ToolsetDef } from "../ai/toolsets";
import { DEFAULT_PERMISSION, type PermissionMode } from "../ai/permission";
import { AiPermissionSelect, permissionHint } from "./AiPermissionSelect";
import { store } from "../stores/project";
import { i18n, type I18nKey } from "../stores/i18n";
import { aiLimits } from "../stores/aiLimits";
import { useTooltip } from "./Tooltip";
import { Dropdown } from "./Dropdown";
import { AiModelLimits } from "./AiModelLimits";
import { useTopmostEscape } from "./modalEscape";

const KIND_LABEL_KEYS = {
  openai_chat_completions: "ai.kind.openaiChatCompletions",
  openai_responses: "ai.kind.openaiResponses",
  anthropic_messages: "ai.kind.anthropicMessages",
  gemini_interactions: "ai.kind.geminiInteractions",
  gemini_generate: "ai.kind.geminiGenerate",
} as const;

const kindLabel = (kind: ProviderKind) => i18n.t(KIND_LABEL_KEYS[kind]);

/** Label for a saved sign-in tag; this build offers none. */
const authProviderLabel = (_provider: AuthProvider) => i18n.t("ai.auth.providerChatGpt");

/** The words for an attempt that ended without a credential: a lapsed window
 * reads as one by its tag, anything else in the backend's own words. */
const attemptFailure = (attempt: OAuthAttempt): string =>
  attempt.error?.kind === "expired" || (!attempt.error && attempt.state === "expired")
    ? i18n.t("ai.auth.timedOut")
    : attempt.error?.message || attempt.message || attempt.state;

/** A tooltip lists this many tools before the "and N more" line. */
const TOOLTIP_TOOL_LIMIT = 6;
const TOOL_DESCRIPTION_CHARS = 110;

const oneLine = (text: string): string => {
  const line = text.replace(/\s+/g, " ").trim();
  return line.length > TOOL_DESCRIPTION_CHARS
    ? `${line.slice(0, TOOL_DESCRIPTION_CHARS - 1).trimEnd()}…`
    : line;
};

/** How the status line reads: a save/add success, a failure, or a request in
 * flight. The text is the same either way. */
export type NoticeTone = "idle" | "success" | "error";

/** Dropdown value that reveals the typed-id field instead of picking a model. */
const TYPE_AN_ID = "__type_an_id__";

/** What the OS credential store last said about the active provider's key.
 * `refused` keeps the store's own words so the dialog can show the fix. */
interface KeyState {
  state: "unchecked" | "ready" | "missing" | "refused";
  message: string;
}

interface Props {
  catalog: Catalog | null;
  /** A request is in flight; every control stays disabled like the old fieldset. */
  loading: boolean;
  /** Estimated tokens of the outgoing conversation, for the limits budget. */
  contextEstimate: number;
  testResult: string | null;
  noticeTone: NoticeTone;
  targetPath: string;
  /** The open conversation's DSL permission, and its setter. */
  permission?: PermissionMode;
  onPermission?: (mode: PermissionMode) => void;
  toolsets: ToolsetDef[];
  selectedSets: string[] | null;
  /** Field the opener points at: the connection column by default. */
  focus?: "connection" | "toolsets" | "auth";
  toolDescriptions: ReadonlyMap<string, string>;
  onClose: () => void;
  onTestResult: (text: string | null, tone: NoticeTone) => void;
  onTargetPath: (path: string) => void;
  onToggleSet: (id: string) => void;
  onTestConnection: () => void;
  onCatalogReload: () => void | Promise<void>;
  /** The provider dialog opens over this one; its entry point lives here. */
  onAddProvider: () => void;
  /** Rendered inside Preferences: no overlay, header or footer. */
  embedded?: boolean;
  /** A connection test is in flight; a turn alone must not lock the page. */
  testing?: boolean;
  /** Reports whether a save or test is in flight, so a shell can refuse to close. */
  onBusyChange?: (busy: boolean) => void;
}

export function AiSettingsPanel(props: Props) {
  const [saving, setSaving] = createSignal(false);
  /** What the OS store last said about this provider's key, in its own words. */
  const [keyState, setKeyState] = createSignal<KeyState>({ state: "unchecked", message: "" });
  const [contextProblem, setContextProblem] = createSignal<I18nKey | null>(null);
  const [outputProblem, setOutputProblem] = createSignal<I18nKey | null>(null);
  const [temperatureProblem, setTemperatureProblem] = createSignal(false);
  const [oauth, setOauth] = createSignal<OAuthStatus | null>(null);
  const [oauthBusy, setOauthBusy] = createSignal(false);
  const [oauthError, setOauthError] = createSignal<string | null>(null);
  const [typingId, setTypingId] = createSignal(false);
  const [typedId, setTypedId] = createSignal("");
  /** The project note, its cap from the store, and the last save's outcome. */
  const [note, setNote] = createSignal("");
  const [noteLimit, setNoteLimit] = createSignal(PROJECT_NOTE_MAX_CHARS);
  const [noteSaving, setNoteSaving] = createSignal(false);
  const [noteSaved, setNoteSaved] = createSignal("");
  const [noteNotice, setNoteNotice] = createSignal<{ text: string; error: boolean } | null>(null);
  /** The store measures code points, so the counter must too. */
  const noteCount = () => [...note().trim()].length;
  const noteOverLimit = () => noteIsOverLimit(note(), noteLimit());
  const noteDirty = () => note().trim() !== noteSaved().trim();

  let protocolSelect: HTMLSelectElement | undefined;
  let authSelect: HTMLSelectElement | undefined;
  let firstToolset: HTMLInputElement | undefined;
  let idInput: HTMLInputElement | undefined;
  let restoreFocus: Element | null = null;
  let signInCancelled = false;
  let attemptId: string | null = null;
  /** A late key or OAuth read must not describe a provider the page left. */
  let keyRead = 0;
  let authRead = 0;

  const disabled = () => props.loading || saving();
  const outputOverContext = () => getActiveProvider().maxTokens > getActiveProvider().contextLength;
  /** Why Save must stay disabled: a problem the controls already explain. */
  const settingsProblem = (): I18nKey | null =>
    contextProblem() ?? outputProblem()
    ?? (outputOverContext() ? "ai.limits.error.overContext" : null)
    ?? (temperatureProblem() ? "ai.settings.error.temperature" : null);
  const authAvailable = createMemo(() => authProvidersFor(wireOf(getActiveProvider())));
  const currentModel = createMemo(() => modelFor(props.catalog, wireOf(getActiveProvider()), getActiveProvider().model));
  /** The window a request for the selected model uses. The stored provider
   * default is a fallback, so it only applies where nothing describes the model. */
  const [effectiveContext, setEffectiveContext] = createSignal<number | null>(null);
  const canTakeAudio = createMemo(() => audioAccepted(currentModel(), getActiveProvider()));
  const thinkingLevels = createMemo(() =>
    modelLevels(props.catalog, getActiveProvider().id, getActiveProvider().model));
  /** The badge the selected row carries, on the trigger as well as in the list. */
  const modelBadge = () => currentModel()?.status === "deprecated" ? i18n.t("ai.model.deprecated") : undefined;
  /** The header's list, unchanged: same groups, same order, same labels. */
  const modelChoices = createMemo(() => listModelChoices(props.catalog));
  /** A provider the catalog and its own row say nothing about names its ids. */
  const listsNoModels = createMemo(() => {
    const provider = getActiveProvider();
    const entry = catalogProviderFor(props.catalog, provider);
    return !entry?.models.length && !provider.models?.length;
  });
  const modelOptions = createMemo<ModelChoice[]>(() => listsNoModels()
    ? [...modelChoices(), { value: TYPE_AN_ID, label: i18n.t("ai.model.typeAnId") }]
    : modelChoices());
  const selectModel = (value: string) => {
    if (value === TYPE_AN_ID) {
      setTypingId(true);
      queueMicrotask(() => idInput?.focus());
      return;
    }
    if (applyModelChoice(props.catalog, value, false)) setTypingId(false);
  };
  const commitTypedId = () => {
    const id = typedId().trim();
    setTypingId(false);
    setTypedId("");
    if (id) applyModelChoice(props.catalog, modelKey(activeModelGroup(props.catalog), id), false);
  };

  const targets = createMemo(() => {
    const paths = ["project", "project.properties", "master"];
    for (let i = 0; i < store.project.tracks.length; i++) {
      const t = store.project.tracks[i];
      paths.push(`track:${t.name}`);
      for (const ph of t.phrases) paths.push(`track:${t.name}.phrase:${ph.name}`);
      for (const pat of t.patterns) paths.push(`track:${t.name}.pattern:${pat.name}`);
    }
    for (const b of store.project.buses) paths.push(`bus:${b.name}`);
    for (const p of store.project.presets) paths.push(`preset:${p.name}`);
    for (const p of store.project.patterns) paths.push(`pattern:${p.name}`);
    return paths;
  });

  const refreshAuth = async () => {
    const provider = getActiveProvider();
    const read = ++authRead;
    if (provider.auth !== "oauth") {
      setOauth(null);
      return;
    }
    try {
      const status = await oauthStatus(provider.id);
      if (read === authRead && getActiveProvider().id === provider.id) setOauth(status);
    } catch {
      if (read === authRead && getActiveProvider().id === provider.id) setOauth(null);
    }
  };
  /** Reads the key verdict. `again` is the explicit retry: it asks the OS even
   * when a refusal is cached, which is what fixes a dismissed prompt. */
  const readKey = async (again: boolean) => {
    const providerId = getActiveProvider().id;
    const read = ++keyRead;
    try {
      const saved = again ? await readStoredKey(providerId) : await keyStatus(providerId);
      if (read !== keyRead || getActiveProvider().id !== providerId) return;
      setKeyState({ state: saved ? "ready" : "missing", message: "" });
    } catch (error) {
      if (read !== keyRead || getActiveProvider().id !== providerId) return;
      // The store's own words name the fix; a generic line would hide it.
      setKeyState({ state: "refused", message: errorMessage(error) });
    }
  };
  const checkKey = () => readKey(false);
  const keyDraft = () => aiConfig.keyDraftFor(getActiveProvider().id);
  const setKeyDraft = (value: string) => aiConfig.setKeyDraftFor(getActiveProvider().id, value);
  const keyStatusText = () => {
    if (keyDraft()) return i18n.t("ai.auth.keyDraft");
    switch (keyState().state) {
      case "ready": return i18n.t("ai.auth.keySaved");
      case "missing": return i18n.t("ai.auth.keyMissing");
      case "refused": return keyState().message || i18n.t("ai.auth.storeUnavailable");
      default: return i18n.t("ai.auth.notChecked");
    }
  };
  /** The column edits the active provider; this picker chooses which one. The
   * choice is a draft like every other field in this form, and the unsaved line
   * below Save says so; the header's picker has no Save, so it writes at once. */
  const selectProvider = (id: string) => {
    aiConfig.setActiveProvider(id);
    aiConfig.warmCredential(id);
    setOauthError(null);
    setOauth(null);
    setKeyState({ state: "unchecked", message: "" });
  };

  createEffect(on(
    () => getActiveProvider().id,
    () => {
      setTemperatureProblem(false);
      setKeyState({ state: "unchecked", message: "" });
      setOauth(null);
      void checkKey();
      void refreshAuth();
    },
    { defer: true },
  ));

  createEffect(on(
    () => [getActiveProvider().id, getActiveProvider().model] as const,
    ([providerId, modelId]) => {
      if (!providerId || !modelId) { setEffectiveContext(null); return; }
      let stale = false;
      onCleanup(() => { stale = true; });
      void aiLimits.loadModelLimits(providerId, modelId)
        .then((view) => { if (!stale) setEffectiveContext(view?.effective_context ?? null); })
        .catch(() => { if (!stale) setEffectiveContext(null); });
    },
  ));

  /** Focus belongs to the field the opener pointed at, not to the header's
   * close button. */
  const focusField = (focus: Props["focus"]) => {
    if (focus === "toolsets") firstToolset?.focus();
    else if (focus === "auth") authSelect?.focus();
    else protocolSelect?.focus();
  };

  onMount(() => {
    restoreFocus = document.activeElement;
    focusField(props.focus);
    void (async () => {
      await checkKey();
      await refreshAuth();
      // The note belongs to the project file, so the dialog loads it on open.
      const loaded = await readProjectInstructions(store.filePath());
      setNote(loaded.text);
      setNoteSaved(loaded.text);
      setNoteLimit(loaded.max_chars);
    })();
  });
  onCleanup(() => {
    signInCancelled = true;
    if (attemptId) void oauthCancel(attemptId).catch(() => {});
    // A typed key stays in memory per provider, so closing the dialog is not a
    // silent loss; the status line says it is unsaved until it is saved.
    if (!props.embedded && restoreFocus instanceof HTMLElement) {
      restoreFocus.focus({ preventScroll: true });
    }
    props.onBusyChange?.(false);
  });

  createEffect(() => props.onBusyChange?.(saving() || !!props.testing));
  // A surface can move the open page to another field while this panel stays
  // mounted; follow that request instead of keeping the first focus.
  createEffect(on(() => props.focus, (focus) => {
    if (focus) queueMicrotask(() => focusField(focus));
  }, { defer: true }));

  const saveSettings = async () => {
    if (saving() || settingsProblem()) return;
    setSaving(true);
    try {
      await aiConfig.saveConfig();
      if (getActiveProvider().auth === "api_key" && keyDraft()) {
        await setKey(getActiveProvider().id, keyDraft());
        setKeyDraft("");
      }
      await checkKey();
      await refreshAuth();
      props.onTestResult(i18n.t("ai.settings.saved"), "success");
    } catch (e) { props.onTestResult(errorMessage(e), "error"); }
    finally { setSaving(false); }
  };
  const removeKey = async () => {
    if (saving() || keyState().state === "missing") return;
    const confirmed = await ask(i18n.t("ai.auth.removeKeyConfirm"), {
      title: i18n.t("ai.auth.removeKey"),
      kind: "warning",
    });
    if (!confirmed) return;
    setSaving(true);
    try { await removeStoredKey(getActiveProvider().id); setKeyDraft(""); await checkKey(); }
    catch(e) { props.onTestResult(errorMessage(e), "error"); }
    finally { setSaving(false); }
  };
  const removeProvider = async () => {
    const provider = getActiveProvider();
    if (saving() || provider.builtin) return;
    const confirmed = await ask(i18n.t("ai.settings.removeProviderConfirm", { name: provider.name }), {
      title: i18n.t("ai.settings.removeProvider"),
      kind: "warning",
    });
    if (!confirmed) return;
    setSaving(true);
    try {
      aiConfig.removeProvider(provider.id);
      await aiConfig.saveConfig();
      aiConfig.clearSaveError();
      props.onTestResult(i18n.t("ai.settings.removed", { name: provider.name }), "success");
    } catch (e) { props.onTestResult(errorMessage(e), "error"); }
    finally { setSaving(false); }
  };
  /** Saves the project note; an over-limit note is refused here, not stored. */
  const saveNote = async () => {
    if (noteSaving()) return;
    if (noteOverLimit()) {
      setNoteNotice({ text: i18n.t("ai.settings.instructions.tooLong", {
        count: noteCount(), limit: noteLimit(), tokens: PROJECT_NOTE_TOKEN_BUDGET,
      }), error: true });
      return;
    }
    setNoteSaving(true);
    try {
      const saved = await writeProjectInstructions(store.filePath(), note());
      setNote(saved.text);
      setNoteSaved(saved.text);
      setNoteLimit(saved.max_chars);
      setNoteNotice({ text: i18n.t("ai.settings.instructions.saved"), error: false });
    } catch (e) {
      setNoteNotice({ text: errorMessage(e), error: true });
    } finally {
      setNoteSaving(false);
    }
  };

  const setAuth = (auth: AuthKind) => {
    const provider = getActiveProvider();
    const providers = authProvidersFor(wireOf(provider));
    const authProvider = provider.authProvider && providers.includes(provider.authProvider)
      ? provider.authProvider
      : providers[0];
    aiConfig.updateProvider(provider.id, {
      auth,
      ...(auth === "oauth" && authProvider ? { authProvider } : {}),
    });
    setOauthError(null);
    void refreshAuth();
  };
  const startSignIn = async () => {
    setOauthError(null);
    setOauthBusy(true);
    signInCancelled = false;
    const providerId = getActiveProvider().id;
    try {
      const start = await oauthBegin(providerId);
      // Cancel (or a provider switch) can land while the start is in flight;
      // the attempt exists on the backend, so it still has to be revoked.
      if (signInCancelled || getActiveProvider().id !== providerId) {
        await oauthCancel(start.attempt_id).catch(() => {});
        return;
      }
      attemptId = start.attempt_id;
      if (!start.opened) window.open(start.url, "_blank", "noopener");
      const deadline = start.expires_at + 5000;
      while (!signInCancelled && Date.now() < deadline) {
        await new Promise(resolve => setTimeout(resolve, 1500));
        if (signInCancelled) break;
        const attempt = await oauthPoll(start.attempt_id);
        if (signInCancelled) break;
        if (getActiveProvider().id !== providerId) {
          await oauthCancel(start.attempt_id).catch(() => {});
          attemptId = null;
          return;
        }
        if (attempt.state === "pending") continue;
        attemptId = null;
        if (attempt.state === "complete") {
          await refreshAuth();
        } else if (attempt.state !== "cancelled") {
          setOauthError(i18n.t("ai.auth.failed", { error: attemptFailure(attempt) }));
        }
        return;
      }
      if (!signInCancelled) {
        setOauthError(i18n.t("ai.auth.failed", { error: i18n.t("ai.auth.timedOut") }));
      }
    } catch (e) {
      setOauthError(i18n.t("ai.auth.failed", { error: errorMessage(e) }));
    } finally {
      setOauthBusy(false);
    }
  };
  const cancelSignIn = async () => {
    signInCancelled = true;
    if (attemptId) await oauthCancel(attemptId).catch(() => {});
    attemptId = null;
    setOauthBusy(false);
  };
  const signOut = async () => {
    setOauthBusy(true);
    try {
      await oauthSignOut(getActiveProvider().id);
      setOauthError(null);
      await refreshAuth();
    } catch (e) { setOauthError(i18n.t("ai.auth.failed", { error: errorMessage(e) })); }
    finally { setOauthBusy(false); }
  };
  const authStateLabel = () => {
    switch (oauth()?.state) {
      case "connected": return i18n.t("ai.auth.statusConnected");
      case "expired": return i18n.t("ai.auth.statusExpired");
      case "needs_reauth": case "missing": return i18n.t("ai.auth.statusNeedsAuth");
      default: return "";
    }
  };
  const needsReauth = () => oauth()?.state === "needs_reauth" || oauth()?.state === "missing";
  const minutesLeft = () => {
    const expires = oauth()?.expires_at;
    return expires ? Math.max(0, Math.round((expires - Date.now()) / 60000)) : 0;
  };

  /** The set's own line plus its tools' one-line descriptions, all local. */
  const toolsetTooltip = (set: ToolsetDef) => ({
    title: i18n.t("ai.toolsets.tooltipTitle", { title: set.title, count: set.tools.length }),
    subtitle: set.description,
    rows: [
      ...set.tools.slice(0, TOOLTIP_TOOL_LIMIT).map((name) => ({
        label: name,
        value: props.toolDescriptions.get(name)
          ? oneLine(props.toolDescriptions.get(name)!)
          : i18n.t("ai.toolsets.noDescription"),
      })),
      ...(set.tools.length > TOOLTIP_TOOL_LIMIT
        ? [{ value: i18n.t("ai.toolsets.more", { count: set.tools.length - TOOLTIP_TOOL_LIMIT }) }]
        : []),
    ],
  });

  return (
    <fieldset class="ai-settings-body" disabled={disabled()}>
            <div class="ai-settings-columns">
              <div class="ai-settings-column">
                <Show
                  when={hasProvider()}
                  fallback={<div class="ai-settings-empty">
                    <p class="ai-settings-empty-text">{i18n.t("ai.provider.none")}</p>
                    <button
                      type="button"
                      class="toolbar-btn text-btn"
                      disabled={props.loading}
                      onClick={() => props.onAddProvider()}
                    >{i18n.t("ai.provider.add")}</button>
                  </div>}
                >
                <span class="ai-settings-column-title">{i18n.t("ai.settings.connection")}</span>
                <Dropdown
                  label={i18n.t("ai.provider.title")}
                  value={getActiveProvider().id}
                  options={aiConfig.config.providers.map((provider) => ({ value: provider.id, label: provider.name }))}
                  onSelect={selectProvider}
                  class="ai-settings-provider"
                  disabled={disabled() || oauthBusy()}
                />

                <label class="ai-settings-label">{i18n.t("ai.providerKind")}</label>
                <div class="ai-settings-field-row">
                  <select
                    ref={(element) => { protocolSelect = element; }}
                    class="ai-settings-select"
                    value={getActiveProvider().kind}
                    disabled={disabled()}
                    onChange={(e) => {
                      const kind = e.currentTarget.value as ProviderKind;
                      const provider = getActiveProvider();
                      const wire = WIRE_FOR_KIND[kind];
                      const providers = authProvidersFor(wire);
                      const update: Partial<AiProvider> = { kind, wire };
                      if (provider.auth === "oauth" && !providers.length) update.auth = "api_key";
                      else if (provider.auth === "oauth" && provider.authProvider && !providers.includes(provider.authProvider)) update.authProvider = providers[0];
                      aiConfig.updateProvider(provider.id, update);
                      void refreshAuth();
                    }}
                  >
                    <For each={PROVIDER_KINDS}>
                      {(kind) => <option value={kind}>{kindLabel(kind)}</option>}
                    </For>
                  </select>
                  <button
                    type="button"
                    class="toolbar-btn text-btn"
                    disabled={disabled()}
                    onClick={() => props.onAddProvider()}
                  >{i18n.t("ai.provider.add")}</button>
                </div>

                <label class="ai-settings-label">{i18n.t("ai.settings.endpoint")}</label>
                <input
                  class="ai-settings-input"
                  value={getActiveProvider().endpoint}
                  onInput={(e) => aiConfig.updateProvider(getActiveProvider().id, { endpoint: e.currentTarget.value })}
                  placeholder="https://api.openai.com/v1/chat/completions"
                />
                <p class="ai-endpoint-hint">{i18n.t("ai.endpoint.hint")}</p>

                <label class="ai-settings-label">{i18n.t("ai.auth.title")}</label>
                <select
                  ref={(element) => { authSelect = element; }}
                  class="ai-settings-select"
                  value={getActiveProvider().auth}
                  onChange={(e) => setAuth(e.currentTarget.value as AuthKind)}
                >
                  <option value="api_key">{i18n.t("ai.auth.apiKey")}</option>
                  <option value="oauth" disabled={!authAvailable().length}>{i18n.t("ai.auth.oauth")}</option>
                </select>
                <Show when={!authAvailable().length}>
                  <small>{i18n.t("ai.auth.oauthUnavailable")}</small>
                </Show>
                <Show when={getActiveProvider().auth === "api_key"} fallback={
                  <div class="ai-auth-section">
                    <Show when={authAvailable().length > 1}>
                      <select
                        class="ai-settings-select"
                        value={getActiveProvider().authProvider ?? ""}
                        onChange={(e) => aiConfig.updateProvider(getActiveProvider().id, { authProvider: e.currentTarget.value as AuthProvider })}
                      >
                        <For each={authAvailable()}>{(provider) => <option value={provider}>{authProviderLabel(provider)}</option>}</For>
                      </select>
                    </Show>
                    <small>{i18n.t("ai.auth.experimentalWarning")}</small>
                    <div class="ai-auth-status selectable">
                      <span>{authStateLabel()}</span>
                      <Show when={oauth()?.account}>
                        <span>{i18n.t("ai.auth.signedInAs", { account: oauth()!.account! })}</span>
                      </Show>
                      <Show when={oauth()?.expires_at}>
                        <span>{i18n.t("ai.auth.expiresIn", { minutes: minutesLeft() })}</span>
                      </Show>
                      <Show when={oauth()?.state === "connected" && !oauth()?.expires_at}>
                        <span>{i18n.t("ai.auth.nonExpiring")}</span>
                      </Show>
                    </div>
                    <Show when={oauthBusy()}><small>{i18n.t("ai.auth.waiting")}</small></Show>
                    <Show when={oauthError()}><div class="ai-auth-error selectable">{oauthError()}</div></Show>
                    <Show when={hasUnsavedConfig()}><small>{i18n.t("ai.auth.saveFirst")}</small></Show>
                    <div class="ai-settings-actions">
                      <button
                        class="toolbar-btn text-btn"
                        onClick={startSignIn}
                        disabled={oauthBusy() || props.loading || hasUnsavedConfig() || !authAvailable().length}
                      >{needsReauth() ? i18n.t("ai.auth.reauth") : i18n.t("ai.auth.signIn")}</button>
                      <Show when={oauthBusy()}>
                        <button class="toolbar-btn text-btn" onClick={cancelSignIn}>{i18n.t("ai.auth.cancel")}</button>
                      </Show>
                      <button class="toolbar-btn text-btn" onClick={signOut} disabled={oauthBusy() || !oauth() || needsReauth()}>
                        {i18n.t("ai.auth.signOut")}
                      </button>
                    </div>
                  </div>
                }>
                  <input
                    class="ai-settings-input"
                    type="password"
                    value={keyDraft()}
                    onInput={(e) => setKeyDraft(e.currentTarget.value)}
                    autocomplete="off"
                    placeholder={i18n.t("ai.auth.keyPlaceholder")}
                  />
                  <small class="selectable">{keyStatusText()}</small>
                  <Show when={keyState().state === "refused" || keyState().state === "missing" || keyState().state === "unchecked"}>
                    <button
                      type="button"
                      class="toolbar-btn text-btn"
                      onClick={() => void readKey(true)}
                      disabled={saving() || props.loading}
                    >{i18n.t("ai.auth.readKey")}</button>
                  </Show>
                </Show>
                <div class="ai-settings-actions">
                  <button class="toolbar-btn text-btn" onClick={saveSettings}
                    disabled={saving() || props.loading || !!settingsProblem() || (!hasUnsavedConfig() && !keyDraft())}>
                    {i18n.t(getActiveProvider().auth === "api_key" ? "ai.settings.saveWithKey" : "ai.settings.save")}
                  </button>
                  <Show when={getActiveProvider().auth === "api_key"}>
                    <button class="toolbar-btn text-btn" onClick={removeKey} disabled={saving() || props.loading || keyState().state === "missing" || hasUnsavedConfig()}>{i18n.t("ai.auth.removeKey")}</button>
                  </Show>
                  <Show when={!getActiveProvider().builtin}>
                    <button class="toolbar-btn text-btn" onClick={removeProvider} disabled={saving() || props.loading}>
                      {i18n.t("ai.settings.removeProvider")}
                    </button>
                  </Show>
                </div>
                {/* A failed automatic write (a model or provider pick) lands
                    here; without this line the pick looked applied while the
                    file still held the old value. */}
                <Show when={aiConfig.saveError()}>
                  <small class="ai-auth-error selectable">
                    {i18n.t("ai.saveNotice", { message: aiConfig.saveError() ?? "" })}
                  </small>
                </Show>
                <Show when={hasUnsavedConfig()}>
                  <small>{i18n.t("ai.settings.unsaved")}</small>
                </Show>
                <small>{i18n.t("ai.settings.testHint")}</small>

                <button
                  class="ai-test-btn"
                  disabled={props.loading || saving() || hasUnsavedConfig() || !!keyDraft()}
                  onClick={props.onTestConnection}
                >{i18n.t("ai.settings.test")}</button>
                <Show when={props.testResult}>
                  <div
                    class="ai-settings-notice selectable"
                    classList={{
                      "ai-settings-notice-success": props.noticeTone === "success",
                      "ai-settings-notice-error": props.noticeTone === "error",
                    }}
                  >
                    {props.testResult}
                  </div>
                </Show>
                </Show>
              </div>

              <div class="ai-settings-column">
                <span class="ai-settings-column-title">{i18n.t("ai.model.title")}</span>

                {/* Same list, order and update path as the header's selector. */}
                <div class="ai-settings-model-row">
                  <Dropdown
                    label={i18n.t("ai.model.title")}
                    value={modelKey(activeModelGroup(props.catalog), getActiveProvider().model)}
                    options={modelOptions()}
                    onSelect={selectModel}
                    hideLabel
                    class="ai-settings-model"
                    triggerAttrs={{ "data-model-known": activeModelKnown(props.catalog) ? "true" : "false" }}
                    badge={modelBadge()}
                    disabled={disabled()}
                  />
                  <Show when={thinkingLevels().length}>
                    <Dropdown
                      label={i18n.t("ai.header.thinking")}
                      value={getActiveProvider().thinkingLevel ?? DEFAULT_THINKING_LEVEL}
                      options={thinkingLevels().map((level) => ({ value: level, label: level }))}
                      onSelect={(level) => aiConfig.updateProvider(getActiveProvider().id, { thinkingLevel: level })}
                      hideLabel
                      class="ai-settings-effort"
                      disabled={disabled()}
                    />
                  </Show>
                </div>
                <Show when={typingId()}>
                  <input
                    ref={(element) => { idInput = element; }}
                    class="ai-settings-input"
                    aria-label={i18n.t("ai.model.typeAnId")}
                    placeholder={i18n.t("ai.model.typeAnId")}
                    value={typedId()}
                    disabled={disabled()}
                    onInput={(e) => setTypedId(e.currentTarget.value)}
                    onKeyDown={(e) => {
                      if (e.key === "Enter") { e.preventDefault(); commitTypedId(); }
                      if (e.key === "Escape") { setTypingId(false); setTypedId(""); }
                    }}
                    onBlur={commitTypedId}
                  />
                </Show>

                {/* Provider defaults: they feed requests only where the model's
                    own merged limits say nothing. */}
                <div class="ai-settings-defaults">
                  <small class="ai-settings-defaults-hint">{i18n.t("ai.settings.providerDefaults")}</small>
                  <div class="ai-settings-triple">
                    <div>
                      <label class="ai-settings-label">{i18n.t("ai.settings.contextTokens")}</label>
                      <TokenCountInput
                        class="ai-settings-input"
                        ariaLabel={i18n.t("ai.settings.contextTokens")}
                        errorKey="ai.limits.error.context"
                        value={getActiveProvider().contextLength}
                        onCommit={(value) => aiConfig.updateProvider(aiConfig.config.activeProviderId, { contextLength: value })}
                        onProblem={setContextProblem}
                      />
                      <Show
                        when={effectiveContext() !== null && effectiveContext() !== getActiveProvider().contextLength}
                      >
                        <small class="ai-settings-defaults-hint selectable">
                          {i18n.t("ai.settings.effectiveContext", { context: effectiveContext() ?? 0 })}
                        </small>
                      </Show>
                    </div>
                    <div>
                      <label class="ai-settings-label">{i18n.t("ai.settings.maximumOutput")}</label>
                      <TokenCountInput
                        class="ai-settings-input"
                        ariaLabel={i18n.t("ai.settings.maximumOutput")}
                        errorKey="ai.limits.error.maxOutput"
                        value={getActiveProvider().maxTokens}
                        onCommit={(value) => aiConfig.updateProvider(aiConfig.config.activeProviderId, { maxTokens: value })}
                        onProblem={setOutputProblem}
                      />
                      <Show when={outputOverContext()}>
                        <small class="ai-auth-error selectable">{i18n.t("ai.limits.error.overContext")}</small>
                      </Show>
                    </div>
                    <div>
                      <label class="ai-settings-label">{i18n.t("ai.settings.temperature")}</label>
                      <input
                        class="ai-settings-input"
                        type="number"
                        step="0.1"
                        min="0"
                        max="2"
                        value={getActiveProvider().temperature}
                        onInput={(e) => {
                          const raw = e.currentTarget.value;
                          const value = Number(raw);
                          if (raw.trim() === "" || !Number.isFinite(value) || value < 0 || value > 2) {
                            setTemperatureProblem(true);
                            return;
                          }
                          setTemperatureProblem(false);
                          aiConfig.updateProvider(aiConfig.config.activeProviderId, { temperature: value });
                        }}
                      />
                      <Show when={temperatureProblem()}>
                        <small class="ai-auth-error selectable">{i18n.t("ai.settings.error.temperature")}</small>
                      </Show>
                    </div>
                  </div>
                </div>

                {/* Limits belong to the settings surface: closing it hides both. */}
                <AiModelLimits
                  providerId={getActiveProvider().id}
                  modelId={getActiveProvider().model}
                  used={props.contextEstimate}
                  onSaved={props.onCatalogReload}
                />

                {/* What the open conversation may do to the DSL without asking. */}
                <label class="ai-settings-label">{i18n.t("ai.permission.section")}</label>
                <AiPermissionSelect
                  value={props.permission ?? DEFAULT_PERMISSION}
                  onChange={(mode) => props.onPermission?.(mode)}
                  disabled={disabled()}
                />
                <small class="ai-permission-hint">{i18n.t(permissionHint(props.permission ?? DEFAULT_PERMISSION))}</small>

                <label class="ai-settings-label">{i18n.t("ai.settings.capabilities")}</label>
                <div class="ai-capabilities">
                  <label style={{ display: "flex", "align-items": "center", gap: "4px" }}>
                    <input
                      type="checkbox"
                      checked={canTakeAudio()}
                      disabled={!!currentModel()}
                      onChange={(e) => {
                        const cap = { ...getActiveProvider().capabilities, audioIn: e.currentTarget.checked };
                        aiConfig.updateProvider(aiConfig.config.activeProviderId, { capabilities: cap });
                      }}
                    /> {i18n.t("ai.media.capability")}
                  </label>
                  <label style={{ display: "flex", "align-items": "center", gap: "4px" }}>
                    <input
                      type="checkbox"
                      checked={getActiveProvider().capabilities.nativeToolCalling}
                      onChange={(e) => {
                        const cap = { ...getActiveProvider().capabilities, nativeToolCalling: e.currentTarget.checked };
                        aiConfig.updateProvider(aiConfig.config.activeProviderId, { capabilities: cap });
                      }}
                    /> {i18n.t("ai.settings.nativeTools")}
                  </label>
                  <label><input type="checkbox" checked={getActiveProvider().capabilities.streaming}
                    onChange={e => aiConfig.updateProvider(aiConfig.config.activeProviderId, {capabilities:{...getActiveProvider().capabilities,streaming:e.currentTarget.checked}})} /> {i18n.t("ai.settings.streaming")}</label>
                </div>

                <label class="ai-settings-label">{i18n.t("ai.settings.editTarget")}</label>
                <select
                  class="ai-settings-select"
                  value={props.targetPath}
                  onChange={(e) => props.onTargetPath(e.currentTarget.value)}
                >
                  <Show when={!targets().includes(props.targetPath)}>
                    <option value={props.targetPath}>{i18n.t("ai.settings.targetMissing", { path: props.targetPath })}</option>
                  </Show>
                  <For each={targets()}>
                    {(t) => <option value={t}>{t}</option>}
                  </For>
                </select>

                {/* The project's own note: sent once in every turn's prompt for
                    this project, and never for another one. */}
                <label class="ai-settings-label" for="ai-project-instructions">
                  {i18n.t("ai.settings.instructions")}
                </label>
                <textarea
                  id="ai-project-instructions"
                  class="ai-settings-textarea"
                  rows={4}
                  value={note()}
                  placeholder={i18n.t("ai.settings.instructions.placeholder")}
                  onInput={(e) => { setNote(e.currentTarget.value); setNoteNotice(null); }}
                  onKeyDown={(e) => {
                    if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) {
                      e.preventDefault();
                      void saveNote();
                    }
                  }}
                />
                <div class="ai-settings-field-row">
                  <small
                    class="ai-instructions-count"
                    classList={{ "ai-instructions-over": noteOverLimit() }}
                    data-testid="ai-instructions-count"
                  >{i18n.t("ai.settings.instructions.count", { count: noteCount(), limit: noteLimit() })}</small>
                  <button
                    type="button"
                    class="toolbar-btn text-btn"
                    disabled={noteSaving() || (!noteDirty() && !noteOverLimit())}
                    onClick={() => void saveNote()}
                  >{i18n.t("ai.settings.instructions.save")}</button>
                </div>
                <Show when={noteNotice()}>
                  <small class={noteNotice()!.error ? "ai-auth-error selectable" : "ai-settings-notice ai-settings-notice-success"}>
                    {noteNotice()!.text}
                  </small>
                </Show>
                <small>{i18n.t("ai.settings.instructions.hint")}</small>

                {/* Toolset switchboard: one line per set, with its tools on hover. */}
                <label class="ai-settings-label">{i18n.t("ai.settings.toolsets")}</label>
                <Show when={props.toolsets.length} fallback={
                  <small>{i18n.t("ai.settings.toolsetsUnavailable")}</small>
                }>
                  <div class="ai-toolset-list">
                    <For each={props.toolsets}>
                      {(set) => (
                        <label class="ai-toolset-row" {...useTooltip(() => toolsetTooltip(set))}>
                          <input
                            type="checkbox"
                            ref={(element) => { firstToolset ??= element; }}
                            checked={(props.selectedSets ?? []).includes(set.id)}
                            onChange={() => props.onToggleSet(set.id)}
                          />
                          <span class="ai-toolset-title">{set.title}</span>
                          <span class="ai-toolset-description">{set.description}</span>
                          <span class="ai-toolset-count">{i18n.t("ai.toolsets.toolCount", { count: set.tools.length })}</span>
                        </label>
                      )}
                    </For>
                  </div>
                  <Show when={props.selectedSets?.length === 0}>
                    <small>{i18n.t("ai.toolsets.noneHint")}</small>
                  </Show>
                </Show>
              </div>
            </div>
    </fieldset>
  );
}

/** The standalone dialog shell, kept for a surface that still opens it alone. */
function AiSettingsModalShell(props: Props) {
  const [busy, setBusy] = createSignal(false);
  useTopmostEscape(() => props.onClose(), () => !busy());

  return (
    <Portal>
      <div
        class="ai-settings-overlay"
        data-tauri-drag-region="false"
        onClick={() => { if (!busy()) props.onClose(); }}
      >
        <div
          class="ai-settings-modal"
          role="dialog"
          aria-modal="true"
          aria-label={i18n.t("ai.settings.title")}
          onClick={(event) => event.stopPropagation()}
        >
          <header class="ai-settings-head">
            <span class="ai-settings-title">{i18n.t("ai.settings.title")}</span>
            <button
              type="button"
              class="ai-provider-close"
              aria-label={i18n.t("ai.settings.close")}
              onClick={() => { if (!busy()) props.onClose(); }}
              {...useTooltip(() => i18n.t("ai.settings.close"))}
            >×</button>
          </header>
          <AiSettingsPanel {...props} onBusyChange={setBusy} />
          <footer class="ai-settings-foot">
            <button
              type="button"
              class="toolbar-btn text-btn"
              onClick={() => { if (!busy()) props.onClose(); }}
            >
              {i18n.t("ai.settings.done")}
            </button>
          </footer>
        </div>
      </div>
    </Portal>
  );
}

export function AiSettingsModal(props: Props) {
  return props.embedded ? <AiSettingsPanel {...props} /> : <AiSettingsModalShell {...props} />;
}
