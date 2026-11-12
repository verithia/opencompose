/**
 * Per-model limits dialog: context length, output token limit, input modalities
 * and optional prices. The bundled values are the baseline; saving writes the
 * user layer and the merged catalog applies to the next request.
 */
import { For, Show, createEffect, createMemo, createSignal } from "solid-js";
import { i18n, type I18nKey } from "../stores/i18n";
import {
  aiLimits,
  type InputModalities,
  type LimitsError,
  type ModelLimits,
  type ModelLimitsView,
} from "../stores/aiLimits";
import { TokenCountInput } from "./TokenCountInput";

interface Props {
  providerId: string;
  modelId: string;
  /** Estimated tokens of the outgoing conversation (characters / 4). */
  used?: number;
  onSaved?: () => void | Promise<void>;
}

const MODALITIES: (keyof InputModalities)[] = ["text", "image", "video", "audio"];

/** The readout is a budget, so group digits while staying locale-aware. */
const tokenCount = (value: number): string => new Intl.NumberFormat(i18n.locale()).format(value);

const MODALITY_KEYS: Record<keyof InputModalities, I18nKey> = {
  text: "ai.limits.text",
  image: "ai.limits.image",
  video: "ai.limits.video",
  audio: "ai.limits.audio",
};

function cloneLimits(limits: ModelLimits): ModelLimits {
  return {
    context: limits.context,
    max_output: limits.max_output,
    input: { ...limits.input },
    price: limits.price ? { ...limits.price } : null,
  };
}

export function AiModelLimits(props: Props) {
  const [open, setOpen] = createSignal(false);
  const [view, setView] = createSignal<ModelLimitsView | null>(null);
  const [draft, setDraft] = createSignal<ModelLimits>(aiLimits.defaultLimits());
  const [error, setError] = createSignal<LimitsError | null>(null);
  const [invalid, setInvalid] = createSignal({ context: false, max_output: false });
  const [message, setMessage] = createSignal<string | null>(null);
  const [saving, setSaving] = createSignal(false);

  const refresh = async () => {
    if (!props.providerId || !props.modelId) {
      setView(null);
      aiLimits.setActiveContext(null);
      return;
    }
    try {
      const next = await aiLimits.loadModelLimits(props.providerId, props.modelId);
      setView(next);
      const effective = aiLimits.effectiveLimits(next);
      aiLimits.setActiveContext(effective?.context ?? null);
      // The price toggle edits the user layer only. The catalogue price is the
      // baseline readout, so saving limits never pins today's price.
      setDraft(
        effective
          ? { ...cloneLimits(effective), price: next?.overridden?.price ?? null }
          : aiLimits.defaultLimits(),
      );
    } catch {
      setView(null);
      aiLimits.setActiveContext(null);
    }
  };

  createEffect(() => {
    void refresh();
  });

  const budget = createMemo(() => aiLimits.contextBudget(props.used ?? 0, aiLimits.activeContext()));
  const effective = createMemo(() => aiLimits.effectiveLimits(view()));
  const priceEnabled = createMemo(() => draft().price !== null && draft().price !== undefined);

  const markInvalid = (field: "context" | "max_output") => (problem: I18nKey | null) => {
    setInvalid((current) => ({ ...current, [field]: problem !== null }));
  };
  const hasInvalid = () => invalid().context || invalid().max_output;

  const setModality = (key: keyof InputModalities, checked: boolean) => {
    setDraft((current) => ({ ...current, input: { ...current.input, [key]: checked } }));
  };

  const setPrice = (key: "input" | "output" | "cached_input", raw: string) => {
    const value = raw.trim() === "" ? 0 : Number(raw);
    setDraft((current) => {
      const price = current.price ?? { input: 0, output: 0, cached_input: null };
      return { ...current, price: { ...price, [key]: Number.isFinite(value) ? value : 0 } };
    });
  };

  const togglePrice = (enabled: boolean) => {
    setDraft((current) => ({
      ...current,
      price: enabled ? (current.price ?? { input: 0, output: 0, cached_input: null }) : null,
    }));
  };

  const save = async () => {
    if (saving() || hasInvalid()) return;
    const limits: ModelLimits = {
      ...draft(),
      price: priceEnabled() ? draft().price : null,
    };
    const problem = aiLimits.validateLimitsDraft(limits);
    if (problem) {
      setError(problem);
      setMessage(null);
      return;
    }
    setSaving(true);
    try {
      await aiLimits.saveModelLimits(props.providerId, props.modelId, limits);
      setError(null);
      await refresh();
      setMessage(i18n.t("ai.limits.saved"));
      await props.onSaved?.();
    } catch (e) {
      setMessage(String(e));
    } finally {
      setSaving(false);
    }
  };

  const reset = async () => {
    if (saving()) return;
    setSaving(true);
    try {
      await aiLimits.resetModelLimits(props.providerId, props.modelId);
      setError(null);
      await refresh();
      setMessage(i18n.t("ai.limits.resetDone"));
      await props.onSaved?.();
    } catch (e) {
      setMessage(String(e));
    } finally {
      setSaving(false);
    }
  };

  const close = () => {
    setOpen(false);
    setError(null);
    setMessage(null);
    void refresh();
  };

  return (
    <div class="ai-limits">
      <div style={{ display: "flex", "align-items": "center", gap: "6px", "flex-wrap": "wrap" }}>
        <button
          class="toolbar-btn text-btn"
          aria-label={i18n.t("ai.limits.edit")}
          onClick={() => (open() ? close() : setOpen(true))}
          disabled={!props.providerId || !props.modelId}
        >
          {i18n.t("ai.limits.edit")}
        </button>
        <Show when={budget()}>
          {(shown) => (
            <span
              class="selectable"
              style={{
                "font-size": "10px",
                color: shown().over ? "var(--stop-red)" : "var(--text-muted)",
              }}
            >
              {i18n.t("ai.limits.budget", {
                used: tokenCount(shown().used),
                context: tokenCount(shown().context),
              })}
            </span>
          )}
        </Show>
      </div>
      <Show when={budget()?.over}>
        <div class="ai-auth-error selectable" style={{ "font-size": "10px" }}>
          {i18n.t("ai.limits.over")}
        </div>
      </Show>

      <Show when={open()}>
        <div class="ai-device-dialog" role="dialog" aria-label={i18n.t("ai.limits.title")}>
          <div class="ai-settings-label">{i18n.t("ai.limits.title")}</div>
          <Show
            when={view()?.bundled}
            fallback={<small>{i18n.t("ai.limits.create")}</small>}
          >
            {(bundled) => (
              <small>
                {i18n.t("ai.limits.bundled", {
                  context: bundled().context,
                  output: bundled().max_output,
                })}
              </small>
            )}
          </Show>
          <Show when={view()?.overridden}>
            <small class="selectable">{i18n.t("ai.limits.overridden")}</small>
          </Show>
          <small>{i18n.t("ai.limits.heuristic")}</small>

          <label class="ai-settings-label" for="ai-limits-context">
            {i18n.t("ai.limits.context")}
          </label>
          <TokenCountInput
            id="ai-limits-context"
            class="ai-settings-input"
            ariaLabel={i18n.t("ai.limits.context")}
            errorKey="ai.limits.error.context"
            value={draft().context}
            onCommit={(value) => setDraft((current) => ({ ...current, context: value }))}
            onProblem={markInvalid("context")}
          />
          <Show when={error()?.field === "context"}>
            <small class="ai-auth-error selectable">{i18n.t(error()!.key)}</small>
          </Show>

          <label class="ai-settings-label" for="ai-limits-output">
            {i18n.t("ai.limits.maxOutput")}
          </label>
          <TokenCountInput
            id="ai-limits-output"
            class="ai-settings-input"
            ariaLabel={i18n.t("ai.limits.maxOutput")}
            errorKey="ai.limits.error.maxOutput"
            value={draft().max_output}
            onCommit={(value) => setDraft((current) => ({ ...current, max_output: value }))}
            onProblem={markInvalid("max_output")}
          />
          <Show when={error()?.field === "max_output"}>
            <small class="ai-auth-error selectable">{i18n.t(error()!.key)}</small>
          </Show>

          <div class="ai-settings-label">{i18n.t("ai.limits.input")}</div>
          <div class="ai-capabilities">
            <For each={MODALITIES}>
              {(modality) => (
                <label style={{ display: "flex", "align-items": "center", gap: "4px" }}>
                  <input
                    type="checkbox"
                    checked={draft().input[modality]}
                    onChange={(event) => setModality(modality, event.currentTarget.checked)}
                  />
                  {i18n.t(MODALITY_KEYS[modality])}
                </label>
              )}
            </For>
          </div>
          <Show when={error()?.field === "input"}>
            <small class="ai-auth-error selectable">{i18n.t(error()!.key)}</small>
          </Show>

          <Show when={view()?.bundled?.price}>
            {(price) => (
              <small class="selectable">
                {i18n.t("ai.limits.listPrice", {
                  input: price().input,
                  output: price().output,
                })}
              </small>
            )}
          </Show>
          <label style={{ display: "flex", "align-items": "center", gap: "4px" }}>
            <input
              type="checkbox"
              checked={priceEnabled()}
              onChange={(event) => togglePrice(event.currentTarget.checked)}
            />
            {i18n.t("ai.limits.price")}
          </label>
          <Show when={priceEnabled()}>
            <div style={{ display: "flex", gap: "8px" }}>
              <For
                each={[
                  ["input", "ai.limits.priceInput"],
                  ["output", "ai.limits.priceOutput"],
                  ["cached_input", "ai.limits.priceCached"],
                ] as const}
              >
                {([field, key]) => (
                  <div style={{ flex: "1" }}>
                    <label class="ai-settings-label">{i18n.t(key)}</label>
                    <input
                      class="ai-settings-input"
                      type="number"
                      min="0"
                      step="0.01"
                      aria-label={i18n.t(key)}
                      value={draft().price?.[field] ?? 0}
                      onInput={(event) => setPrice(field, event.currentTarget.value)}
                    />
                  </div>
                )}
              </For>
            </div>
          </Show>
          <Show when={error()?.field === "price"}>
            <small class="ai-auth-error selectable">{i18n.t(error()!.key)}</small>
          </Show>

          <div class="ai-settings-actions">
            <button
              class="toolbar-btn text-btn"
              onClick={save}
              disabled={saving() || hasInvalid() || !props.providerId || !props.modelId}
            >
              {saving() ? i18n.t("ai.limits.saving") : i18n.t("ai.limits.save")}
            </button>
            <button
              class="toolbar-btn text-btn"
              onClick={reset}
              disabled={saving() || !view()?.overridden}
            >
              {i18n.t("ai.limits.reset")}
            </button>
            <button class="toolbar-btn text-btn" onClick={close} disabled={saving()}>
              {i18n.t("ai.limits.close")}
            </button>
          </div>
          <Show when={message()}>
            <small class="selectable">{message()}</small>
          </Show>
          <Show when={effective()}>
            {(limits) => (
              <small class="selectable">
                {i18n.t("ai.limits.budget", {
                  used: tokenCount(props.used ?? 0),
                  context: tokenCount(limits().context),
                })}
              </small>
            )}
          </Show>
        </div>
      </Show>
    </div>
  );
}
