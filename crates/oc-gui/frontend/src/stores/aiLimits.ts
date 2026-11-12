/**
 * Per-model limits on top of the bundled catalog: context length, output token
 * limit, input modalities and optional prices. The backend stores them in
 * `~/.opencompose/ai/models.json` and merges them when the catalog is read; this
 * store only carries the dialog view and the panel's context estimate.
 */
import { createSignal } from "solid-js";
import { invoke } from "@tauri-apps/api/core";
import type { I18nKey } from "./i18n";

export interface InputModalities {
  text: boolean;
  image: boolean;
  video: boolean;
  audio: boolean;
}

/** Price per 1M tokens, matching provider billing. */
export interface ModelPrice {
  input: number;
  output: number;
  cached_input?: number | null;
  /** ISO 4217 code; the backend writes USD when a price does not name one. */
  currency?: string;
}

export interface ModelLimits {
  context: number;
  max_output: number;
  input: InputModalities;
  price?: ModelPrice | null;
}

export interface ModelLimitsView {
  provider_id: string;
  model_id: string;
  /** Declared by the bundled catalog; null when this is the user's own model. */
  bundled: ModelLimits | null;
  /** Present when the user layer overrides this model. */
  overridden: ModelLimits | null;
  /** The window a request uses: the override, the catalog, the known list, and
   * only then the saved provider default. Older backends omit it. */
  effective_context?: number;
}

/** Documented estimate: 4 characters per token for narrow (ASCII) text and one
 * token per wide (CJK) character. No tokenizer runs in the panel, so the
 * estimate is never presented as exact. */
export const CHARACTERS_PER_TOKEN = 4;
const WIDE = /[\u3040-\u30ff\u3400-\u4dbf\u4e00-\u9fff\uf900-\ufaff\uac00-\ud7af]/;

export function estimateTokens(text: string): number {
  let narrow = 0;
  let wide = 0;
  for (const character of text) {
    if (WIDE.test(character)) wide += 1;
    else narrow += 1;
  }
  return Math.ceil(narrow / CHARACTERS_PER_TOKEN) + wide;
}

export interface ContextBudget {
  used: number;
  context: number;
  over: boolean;
  ratio: number;
}

export function contextBudget(used: number, context: number | null | undefined): ContextBudget | null {
  if (!context || context <= 0) return null;
  return { used, context, over: used > context, ratio: Math.min(1, used / context) };
}

/** Merged limits for a model the catalog does not describe: the conservative
 * defaults rather than no budget at all. The backend's effective window wins
 * over all three, because it is the number a request and auto-compaction use. */
export function effectiveLimits(view: ModelLimitsView | null): ModelLimits | null {
  if (!view) return null;
  const limits = view.overridden ?? view.bundled ?? defaultLimits();
  return view.effective_context ? { ...limits, context: view.effective_context } : limits;
}

export function defaultLimits(): ModelLimits {
  return {
    context: 1_000_000,
    max_output: 16_000,
    input: { text: true, image: false, video: false, audio: false },
    price: null,
  };
}

export type LimitsField = "context" | "max_output" | "input" | "price";

export interface LimitsError {
  field: LimitsField;
  /** i18n key with the readable reason. */
  key: I18nKey;
}

/** Mirrors the backend rules so the dialog can name the field immediately. */
export function validateLimitsDraft(draft: ModelLimits): LimitsError | null {
  if (!Number.isInteger(draft.context) || draft.context <= 0) {
    return { field: "context", key: "ai.limits.error.context" };
  }
  if (!Number.isInteger(draft.max_output) || draft.max_output <= 0) {
    return { field: "max_output", key: "ai.limits.error.maxOutput" };
  }
  if (draft.max_output > draft.context) {
    return { field: "max_output", key: "ai.limits.error.overContext" };
  }
  const { text, image, video, audio } = draft.input;
  if (!text && !image && !video && !audio) {
    return { field: "input", key: "ai.limits.error.modalities" };
  }
  const prices: [string, number | null | undefined][] = [
    ["input", draft.price?.input],
    ["output", draft.price?.output],
    ["cached_input", draft.price?.cached_input],
  ];
  for (const [, value] of prices) {
    if (value !== null && value !== undefined && (!Number.isFinite(value) || value < 0)) {
      return { field: "price", key: "ai.limits.error.price" };
    }
  }
  return null;
}

export function loadModelLimits(providerId: string, modelId: string): Promise<ModelLimitsView> {
  return invoke<ModelLimitsView>("ai_model_limits", { providerId, modelId });
}

export function saveModelLimits(
  providerId: string,
  modelId: string,
  limits: ModelLimits,
): Promise<string> {
  return invoke<string>("ai_save_model_limits", { providerId, modelId, limits });
}

export function resetModelLimits(providerId: string, modelId: string): Promise<void> {
  return invoke<void>("ai_reset_model_limits", { providerId, modelId });
}

/** Context limit of the model the panel is currently showing, so the pre-send
 * guard reads the same merged value the dialog shows. */
const [activeContext, setActiveContext] = createSignal<number | null>(null);

export const aiLimits = {
  CHARACTERS_PER_TOKEN,
  estimateTokens,
  contextBudget,
  effectiveLimits,
  defaultLimits,
  validateLimitsDraft,
  loadModelLimits,
  saveModelLimits,
  resetModelLimits,
  activeContext,
  setActiveContext,
};
