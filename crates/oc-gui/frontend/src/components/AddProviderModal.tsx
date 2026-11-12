/**
 * Add-provider modal. Provider fields share one row group and every model is a
 * single-line row with its own toggle, context, output and input modalities, so
 * the narrow right sidebar only keeps a summary and the button that opens this.
 */
import { For, Index, Show, createSignal, onCleanup, onMount } from "solid-js";
import { Portal } from "solid-js/web";
import { addProvider, fetchModels as fetchModelListing, setKey as saveKey, type ListedModel, type ModelListing } from "../ai/api";
import { KIND_FOR_WIRE, PROVIDER_KINDS, WIRE_FOR_KIND, type AuthKind, type ProviderKind } from "../ai/config";
import { catalogWire, modelFor, type Catalog } from "../ai/catalog";
import { errorMessage } from "../ai/errors";
import {
  defaultLimits,
  validateLimitsDraft,
  type InputModalities,
  type ModelLimits,
} from "../stores/aiLimits";
import { i18n, type I18nKey } from "../stores/i18n";
import { TokenCountInput } from "./TokenCountInput";
import { useTooltip } from "./Tooltip";
import { Dropdown } from "./Dropdown";
import { useTopmostEscape } from "./modalEscape";

const KIND_LABEL_KEYS = {
  openai_chat_completions: "ai.kind.openaiChatCompletions",
  openai_responses: "ai.kind.openaiResponses",
  anthropic_messages: "ai.kind.anthropicMessages",
  gemini_interactions: "ai.kind.geminiInteractions",
  gemini_generate: "ai.kind.geminiGenerate",
} as const;

const MODALITIES: (keyof InputModalities)[] = ["text", "image", "video", "audio"];
const MODALITY_LABEL_KEYS = {
  text: "ai.limits.text",
  image: "ai.limits.image",
  video: "ai.limits.video",
  audio: "ai.limits.audio",
} as const;
const MAX_MODELS = 32;

interface ModelRow {
  id: string;
  enabled: boolean;
  limits: ModelLimits;
}

interface Props {
  catalog: Catalog | null;
  disabled?: boolean;
  onClose: () => void;
  /** The provider is saved; the panel reloads the config and shows its name. */
  onAdded: (name: string, providerId: string) => void;
}

export function AddProviderModal(props: Props) {
  const [presetId, setPresetId] = createSignal("");
  const [name, setName] = createSignal("");
  const [kind, setKind] = createSignal<ProviderKind>("openai_chat_completions");
  const [endpoint, setEndpoint] = createSignal("");
  const [auth, setAuth] = createSignal<AuthKind>("api_key");
  const [key, setKey] = createSignal("");
  const [rows, setRows] = createSignal<ModelRow[]>([]);
  const [modelId, setModelId] = createSignal("");
  const [fetching, setFetching] = createSignal(false);
  const [saving, setSaving] = createSignal(false);
  /** Id of the provider the last submit created, so a retry never adds a second
   * one when only the key failed. */
  const [created, setCreated] = createSignal<string | null>(null);
  const [error, setError] = createSignal<string | null>(null);
  const [notice, setNotice] = createSignal<string | null>(null);
  let nameInput: HTMLInputElement | undefined;
  let restoreFocus: Element | null = null;

  /** Catalog entries that name a base URL can fill the whole form. */
  const presets = () => (props.catalog?.providers ?? [])
    .filter((provider) => !!provider.base_url)
    .map((provider) => ({ provider, wire: catalogWire(provider) }));

  /** Fill name, protocol, endpoint and the declared models in one choice. A
   * preset never overwrites the key field. */
  const applyPreset = (id: string) => {
    setPresetId(id);
    if (!id) return;
    const preset = presets().find((candidate) => candidate.provider.id === id);
    if (!preset) return;
    const { provider, wire } = preset;
    setName(provider.name);
    setKind(KIND_FOR_WIRE[wire]);
    setEndpoint(provider.base_url ?? "");
    setAuth(provider.auth_providers.length ? "oauth" : "api_key");
    setRows(provider.models.map((model) => rowFor({ id: model.id }, true)));
    setModelId("");
    setError(null);
    setNotice(null);
  };

  /** A fetched or typed id starts from the bundled values when the catalog
   * declares it, so adding a known model never narrows its limits. A model the
   * catalog does not declare keeps the provider-reported window when it has one. */
  const rowFor = (model: ListedModel, enabled: boolean): ModelRow => {
    const known = modelFor(props.catalog, WIRE_FOR_KIND[kind()], model.id);
    if (known) {
      return {
        id: model.id,
        enabled,
        limits: {
          context: known.context,
          max_output: known.max_output,
          input: {
            text: known.capabilities.text,
            image: false,
            video: false,
            audio: known.capabilities.audio_in,
          },
          price: null,
        },
      };
    }
    const limits = defaultLimits();
    return {
      id: model.id,
      enabled,
      limits: {
        ...limits,
        context: model.context ?? limits.context,
        max_output: model.max_output ?? limits.max_output,
      },
    };
  };

  const enabledRows = () => rows().filter((row) => row.enabled);

  const mergeRows = (listing: ModelListing) => {
    setRows((current) => {
      const seen = new Set(current.map((row) => row.id));
      const added = listing.models
        .filter((model) => !seen.has(model.id))
        .map((model) => rowFor(model, false));
      // The first fetched id is the default model; enabling a whole catalog of
      // ids by default would exceed the provider's model cap.
      const first = added[0];
      if (first && !current.some((row) => row.enabled)) first.enabled = true;
      return [...current, ...added];
    });
  };

  const fetchModels = async () => {
    if (fetching() || saving() || !endpoint().trim()) return;
    setFetching(true);
    setError(null);
    setNotice(null);
    try {
      const listing: ModelListing = await fetchModelListing({
        endpoint: endpoint().trim(),
        wire: WIRE_FOR_KIND[kind()],
        auth: auth(),
        key: key().trim() || null,
      });
      mergeRows(listing);
      const shown = listing.models.length;
      setNotice(listing.total > shown
        ? i18n.t("ai.provider.fetchedTruncated", { shown, total: listing.total })
        : i18n.t("ai.provider.fetched", { count: shown }));
    } catch (e) {
      setError(errorMessage(e));
    } finally {
      setFetching(false);
    }
  };

  const addModel = () => {
    const id = modelId().trim();
    if (!id) return;
    setError(null);
    setModelId("");
    setRows((current) => {
      if (current.some((row) => row.id === id)) {
        return current.map((row) => (row.id === id ? { ...row, enabled: true } : row));
      }
      return [...current, rowFor({ id }, true)];
    });
  };

  const toggle = (id: string, enabled: boolean) => {
    setRows((current) => current.map((row) => (row.id === id ? { ...row, enabled } : row)));
  };

  const setLimit = (id: string, field: "context" | "max_output", value: number) => {
    setRows((current) => current.map((row) => (row.id === id
      ? { ...row, limits: { ...row.limits, [field]: value } }
      : row)));
  };

  const [problems, setProblems] = createSignal<
    Record<string, Partial<Record<"context" | "max_output", I18nKey>>>
  >({});

  /** Text that does not parse stays out of the row; the modal reports it here. */
  const noteProblem = (id: string, field: "context" | "max_output") => (key: I18nKey | null) => {
    setProblems((current) => {
      const row = { ...current[id] };
      if (key) row[field] = key;
      else delete row[field];
      const next = { ...current };
      if (Object.keys(row).length) next[id] = row;
      else delete next[id];
      return next;
    });
  };

  const problemMessage = () => {
    const row = enabledRows().find((candidate) => !!problems()[candidate.id]);
    if (!row) return null;
    const problem = problems()[row.id];
    return i18n.t("ai.provider.rowInvalid", {
      model: row.id,
      error: i18n.t(problem.context ?? problem.max_output!),
    });
  };

  /** The first reason the enabled rows cannot be created: an unparseable field,
   * a row the backend would refuse, or too many rows for one provider. */
  const rowProblem = () => {
    const problem = problemMessage();
    if (problem) return problem;
    const models = enabledRows();
    if (models.length > MAX_MODELS) return i18n.t("ai.provider.tooMany", { count: MAX_MODELS });
    for (const row of models) {
      const invalid = validateLimitsDraft(row.limits);
      if (invalid) {
        return i18n.t("ai.provider.rowInvalid", { model: row.id, error: i18n.t(invalid.key) });
      }
    }
    return null;
  };

  const setModality = (id: string, modality: keyof InputModalities, checked: boolean) => {
    setRows((current) => current.map((row) => (row.id === id
      ? { ...row, limits: { ...row.limits, input: { ...row.limits.input, [modality]: checked } } }
      : row)));
  };

  const canSubmit = () => !props.disabled && !saving() && auth() === "api_key"
    && (!!created()
      || (!!name().trim() && !!endpoint().trim() && enabledRows().length > 0))
    && !rowProblem();

  const submit = async () => {
    if (!canSubmit()) return;
    const models = enabledRows();
    for (const row of models) {
      const problem = validateLimitsDraft(row.limits);
      if (problem) {
        setError(i18n.t("ai.provider.rowInvalid", { model: row.id, error: i18n.t(problem.key) }));
        return;
      }
    }
    setSaving(true);
    setError(null);
    setNotice(null);
    try {
      let providerId = created();
      if (!providerId) {
        const limits: Record<string, ModelLimits> = {};
        for (const row of models) limits[row.id] = { ...row.limits, price: null };
        const added = await addProvider({
          name: name().trim(),
          wire: WIRE_FOR_KIND[kind()],
          endpoint: endpoint().trim(),
          auth: "api_key",
          models: models.map((row) => row.id),
          limits,
        });
        providerId = added.provider_id;
        setCreated(providerId);
      }
      const typedKey = key().trim();
      if (typedKey) {
        await saveKey(providerId, typedKey);
      }
      setKey("");
      props.onAdded(name().trim(), providerId);
    } catch (e) {
      setError(created()
        ? i18n.t("ai.provider.createdNoKey", {
          id: name().trim() || created()!,
          error: errorMessage(e),
        })
        : errorMessage(e));
    } finally {
      setSaving(false);
    }
  };

  onMount(() => {
    restoreFocus = document.activeElement;
    nameInput?.focus();
  });
  // Only Escape is consumed, and only while this is the topmost dialog: typing
  // must not swallow the panel's shortcuts.
  useTopmostEscape(() => props.onClose(), () => !saving());
  onCleanup(() => {
    setKey("");
    if (restoreFocus instanceof HTMLElement) restoreFocus.focus({ preventScroll: true });
  });

  return (
    <Portal>
      <div class="ai-provider-overlay"
        data-tauri-drag-region="false" onClick={() => { if (!saving()) props.onClose(); }}>
        <div
          class="ai-provider-modal"
          role="dialog"
          aria-modal="true"
          aria-label={i18n.t("ai.provider.addTitle")}
          onClick={(event) => event.stopPropagation()}
        >
          <header class="ai-provider-modal-header">
            <span class="ai-provider-modal-title">{i18n.t("ai.provider.addTitle")}</span>
            <button
              type="button"
              class="ai-provider-close"
              aria-label={i18n.t("ai.provider.close")}
              onClick={() => { if (!saving()) props.onClose(); }}
              {...useTooltip(() => i18n.t("ai.provider.close"))}
            >×</button>
          </header>

          <div class="ai-provider-fields">
            <Show when={presets().length}>
              <Dropdown
                label={i18n.t("ai.provider.preset")}
                stack
                value={presetId()}
                options={[
                  { value: "", label: i18n.t("ai.provider.presetCustom") },
                  ...presets().map(({ provider }) => ({ value: provider.id, label: provider.name })),
                ]}
                onSelect={(value) => applyPreset(value)}
              />
            </Show>
            <label class="ai-provider-field">
              <span>{i18n.t("ai.provider.name")}</span>
              <input
                ref={(element) => { nameInput = element; }}
                class="ai-settings-input"
                value={name()}
                onInput={(event) => setName(event.currentTarget.value)}
              />
            </label>
            <Dropdown
              label={i18n.t("ai.providerKind")}
              stack
              value={kind()}
              options={PROVIDER_KINDS.map((value) => ({ value, label: i18n.t(KIND_LABEL_KEYS[value]) }))}
              onSelect={(value) => setKind(value as ProviderKind)}
            />
            <label class="ai-provider-field ai-provider-field-wide">
              <span>{i18n.t("ai.provider.baseUrl")}</span>
              <input
                class="ai-settings-input"
                value={endpoint()}
                onInput={(event) => setEndpoint(event.currentTarget.value)}
                placeholder="https://api.example.com/v1/chat/completions"
              />
              <span class="ai-endpoint-hint">{i18n.t("ai.endpoint.hint")}</span>
            </label>
            <Dropdown
              label={i18n.t("ai.auth.title")}
              stack
              value={auth()}
              options={[
                { value: "api_key", label: i18n.t("ai.auth.apiKey") },
                { value: "oauth", label: i18n.t("ai.auth.oauth") },
              ]}
              onSelect={(value) => setAuth(value as AuthKind)}
            />
            <label class="ai-provider-field">
              <span>{i18n.t("ai.auth.apiKey")}</span>
              <input
                class="ai-settings-input"
                type="password"
                autocomplete="off"
                value={key()}
                onInput={(event) => setKey(event.currentTarget.value)}
                placeholder="sk-…"
              />
            </label>
          </div>
          <Show when={auth() !== "api_key"}>
            <div class="ai-provider-hint">{i18n.t("ai.provider.oauthHint")}</div>
          </Show>
          <div class="ai-provider-hint">{i18n.t("ai.provider.keyHint")}</div>

          <div class="ai-provider-models-bar">
            <span class="ai-provider-models-title">
              {i18n.t("ai.provider.models")}
              <Show when={rows().length}>
                <span class="ai-provider-count">
                  {i18n.t("ai.provider.enabled", { enabled: enabledRows().length, total: rows().length })}
                </span>
              </Show>
            </span>
            <span class="ai-provider-gap" />
            <input
              class="ai-settings-input ai-provider-id-input"
              aria-label={i18n.t("ai.provider.modelId")}
              placeholder={i18n.t("ai.provider.modelId")}
              value={modelId()}
              onInput={(event) => setModelId(event.currentTarget.value)}
              onKeyDown={(event) => { if (event.key === "Enter") { event.preventDefault(); addModel(); } }}
            />
            <button type="button" class="toolbar-btn text-btn" onClick={addModel} disabled={!modelId().trim()}>
              {i18n.t("ai.provider.addModel")}
            </button>
            <button
              type="button"
              class="toolbar-btn text-btn"
              onClick={() => void fetchModels()}
              disabled={fetching() || saving() || !endpoint().trim()}
            >{fetching() ? i18n.t("ai.provider.fetching") : i18n.t("ai.provider.fetch")}</button>
          </div>

          <Show when={rows().length} fallback={
            <div class="ai-provider-empty">{i18n.t("ai.provider.empty")}</div>
          }>
            <div class="ai-provider-models" role="table" aria-label={i18n.t("ai.provider.models")}>
              <div class="ai-provider-model-row ai-provider-model-head" role="row">
                <span class="ai-provider-col-on" role="columnheader" />
                <span class="ai-provider-col-name" role="columnheader">{i18n.t("ai.provider.headers.model")}</span>
                <span class="ai-provider-col-num" role="columnheader">{i18n.t("ai.provider.headers.context")}</span>
                <span class="ai-provider-col-num" role="columnheader">{i18n.t("ai.provider.headers.output")}</span>
                <span class="ai-provider-col-mod" role="columnheader">{i18n.t("ai.provider.headers.input")}</span>
              </div>
              <Index each={rows()}>
                {(row) => (
                  <div class="ai-provider-model-row" classList={{ "ai-provider-model-off": !row().enabled }} role="row">
                    <input
                      type="checkbox"
                      class="ai-provider-col-on"
                      aria-label={i18n.t("ai.provider.rowToggle", { model: row().id })}
                      checked={row().enabled}
                      onChange={(event) => toggle(row().id, event.currentTarget.checked)}
                    />
                    <span class="ai-provider-col-name" role="cell" title={row().id}>{row().id}</span>
                    <TokenCountInput
                      class="ai-settings-input ai-provider-col-num"
                      ariaLabel={i18n.t("ai.provider.rowContext", { model: row().id })}
                      errorKey="ai.limits.error.context"
                      inlineError={false}
                      value={row().limits.context}
                      onCommit={(value) => setLimit(row().id, "context", value)}
                      onProblem={noteProblem(row().id, "context")}
                    />
                    <TokenCountInput
                      class="ai-settings-input ai-provider-col-num"
                      ariaLabel={i18n.t("ai.provider.rowOutput", { model: row().id })}
                      errorKey="ai.limits.error.maxOutput"
                      inlineError={false}
                      value={row().limits.max_output}
                      onCommit={(value) => setLimit(row().id, "max_output", value)}
                      onProblem={noteProblem(row().id, "max_output")}
                    />
                    <span class="ai-provider-col-mod" role="cell">
                      <For each={MODALITIES}>
                        {(modality) => (
                          <button
                            type="button"
                            class="ai-provider-mod"
                            classList={{ "ai-provider-mod-on": row().limits.input[modality] }}
                            aria-pressed={row().limits.input[modality]}
                            aria-label={i18n.t("ai.provider.rowModality", {
                              modality: i18n.t(MODALITY_LABEL_KEYS[modality]),
                              model: row().id,
                            })}
                            onClick={() => setModality(row().id, modality, !row().limits.input[modality])}
                          >{modality[0].toUpperCase()}</button>
                        )}
                      </For>
                    </span>
                  </div>
                )}
              </Index>
            </div>
          </Show>

          <div class="ai-provider-notice" role="status">
            <Show when={error()}>
              <span class="ai-auth-error selectable">{error()}</span>
            </Show>
            <Show when={!error() && rowProblem()}>
              {(message) => <span class="ai-auth-error selectable">{message()}</span>}
            </Show>
            <Show when={!error() && !rowProblem() && notice()}>
              <span class="selectable">{notice()}</span>
            </Show>
          </div>

          <div class="ai-provider-modal-actions">
            <Show when={enabledRows().length}>
              <span class="ai-provider-hint">{i18n.t("ai.provider.modelsHint")}</span>
            </Show>
            <span class="ai-provider-gap" />
            <button type="button" class="toolbar-btn text-btn" onClick={props.onClose} disabled={saving()}>
              {i18n.t("ai.provider.cancel")}
            </button>
            <button
              type="button"
              class="toolbar-btn text-btn"
              onClick={() => void submit()}
              disabled={!canSubmit()}
            >{saving() ? i18n.t("ai.provider.saving") : i18n.t("ai.provider.save")}</button>
          </div>
        </div>
      </div>
    </Portal>
  );
}
