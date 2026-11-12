/**
 * One grouped model list for both selectors: the panel header's and the
 * settings dialog's. A row carries the provider it belongs to, so picking one
 * switches provider and model through a single update path.
 */
import {
  aiConfig,
  clampThinkingLevel,
  getActiveProvider,
  wireOf,
  type Wire,
} from "./config";
import {
  catalogProviderFor,
  isCustomEndpoint,
  modelFor,
  type Catalog,
  type CatalogModel,
} from "./catalog";
import { i18n } from "../stores/i18n";

/** Dropdown values must be unique: a model id is not, so the group travels
 * with it. Two entries for one catalog provider share the catalog group. */
const MODEL_KEY_SEPARATOR = "::";
const CATALOG_GROUP = "catalog:";
const PROVIDER_GROUP = "provider:";

export interface ModelChoice {
  value: string;
  label: string;
  /** A short state word on the row, such as the deprecation badge. */
  badge?: string;
  /** Hover text for the row, such as the date the model is removed. */
  hint?: string;
}

const modelGroup = (catalogProviderId: string) => `${CATALOG_GROUP}${catalogProviderId}`;
const providerGroup = (providerId: string) => `${PROVIDER_GROUP}${providerId}`;

export function modelKey(group: string, modelId: string): string {
  return `${group}${MODEL_KEY_SEPARATOR}${modelId}`;
}

export function parseModelKey(value: string): [string, string] {
  const at = value.indexOf(MODEL_KEY_SEPARATOR);
  return at < 0 ? [value, ""] : [value.slice(0, at), value.slice(at + MODEL_KEY_SEPARATOR.length)];
}

/** The row that represents the active provider's current model. */
export function activeModelGroup(catalog: Catalog | null): string {
  const provider = getActiveProvider();
  const entry = catalogProviderFor(catalog, provider);
  return entry ? modelGroup(entry.id) : providerGroup(provider.id);
}

/** The levels the catalog declares for the model. */
export function modelLevels(
  catalog: Catalog | null,
  providerId: string,
  modelId: string,
): string[] {
  const provider = aiConfig.config.providers.find((entry) => entry.id === providerId);
  const known = provider ? modelFor(catalog, wireOf(provider), modelId) : undefined;
  return known?.thinking_levels ?? [];
}

/** The catalog is metadata, not a gate: a model it has no entry for is marked,
 * never treated as an error. */
export function activeModelKnown(catalog: Catalog | null): boolean {
  const provider = getActiveProvider();
  return !!modelFor(catalog, wireOf(provider), provider.model)
    || isCustomEndpoint(catalog, provider.endpoint);
}

/**
 * The identity of one selectable model: its provider and its id together. The
 * header and the option list both show this form, so a clipped label is never
 * the only thing that names the model.
 */
export function modelIdentity(providerId: string, modelId: string): string {
  return modelId ? `${providerId}/${modelId}` : i18n.t("ai.model.none");
}

/** The badge and its tooltip. The date comes from the catalog row, so a model
 * retired on another day reads its own date. */
function deprecation(known: CatalogModel | undefined): { badge?: string; hint?: string } {
  if (known?.status !== "deprecated") return {};
  return {
    badge: i18n.t("ai.model.deprecated"),
    hint: known.deprecated_on
      ? i18n.t("ai.model.deprecatedHint", { date: known.deprecated_on })
      : i18n.t("ai.model.deprecatedHintUndated"),
  };
}

/** Every model the config can reach, in provider order. Two saved entries that
 * point at the same catalog provider share one group, so the list is grouped by
 * provider and each model appears once. */
export function listModelChoices(catalog: Catalog | null): ModelChoice[] {
  const rows: { group: string; providerId: string; modelId: string; wire: Wire }[] = [];
  for (const provider of aiConfig.config.providers) {
    const entry = catalogProviderFor(catalog, provider);
    const group = entry ? modelGroup(entry.id) : providerGroup(provider.id);
    const wire = wireOf(provider);
    if (entry && !rows.some((row) => row.group === group)) {
      for (const model of entry.models) {
        rows.push({ group, providerId: provider.id, modelId: model.id, wire });
      }
    }
    for (const modelId of provider.models ?? []) {
      if (!rows.some((row) => row.group === group && row.modelId === modelId)) {
        rows.push({ group, providerId: provider.id, modelId, wire });
      }
    }
  }
  const active = getActiveProvider();
  const activeGroup = activeModelGroup(catalog);
  if (!rows.some((row) => row.group === activeGroup && row.modelId === active.model)) {
    rows.push({ group: activeGroup, providerId: active.id, modelId: active.model, wire: wireOf(active) });
  }
  const known = activeModelKnown(catalog);
  return rows.map((row) => {
    const label = modelIdentity(row.providerId, row.modelId);
    const marked = !known && row.group === activeGroup && row.modelId === active.model;
    const catalogRow = modelFor(catalog, row.wire, row.modelId);
    return {
      value: modelKey(row.group, row.modelId),
      label: marked ? `${label} · ${i18n.t("ai.model.notInCatalog")}` : label,
      ...deprecation(catalogRow),
    };
  });
}

/** One entry represents a catalog provider in the model list: the active one
 * when it serves that provider, so switching models keeps its key, else the
 * first entry that does. */
function catalogGroupProvider(catalog: Catalog | null, catalogProviderId: string) {
  const candidates = aiConfig.config.providers.filter(
    (entry) => catalogProviderFor(catalog, entry)?.id === catalogProviderId,
  );
  const active = getActiveProvider();
  return candidates.find((entry) => entry.id === active.id) ?? candidates[0];
}

/**
 * Picking a model from another provider switches both in one action, and a
 * catalog entry's context, output cap, capabilities and thinking levels move
 * with it. Returns false when the value names no known provider.
 */
export function applyModelChoice(
  catalog: Catalog | null,
  value: string,
  save = true,
): boolean {
  const [group, modelId] = parseModelKey(value);
  const provider = group.startsWith(CATALOG_GROUP)
    ? catalogGroupProvider(catalog, group.slice(CATALOG_GROUP.length))
    : aiConfig.config.providers.find((entry) => entry.id === group.slice(PROVIDER_GROUP.length));
  if (!provider) return false;
  // Re-picking the shown model must not rewrite its limits: that would be an
  // unsaved edit for a click that changed nothing, and unsaved edits disable Send.
  if (provider.id === getActiveProvider().id && provider.model === modelId) return true;
  const model: CatalogModel | undefined = modelFor(catalog, wireOf(provider), modelId);
  const thinkingLevel = clampThinkingLevel(provider.thinkingLevel, model?.thinking_levels);
  const switched = provider.id !== getActiveProvider().id;
  aiConfig.setActiveProvider(provider.id);
  // A pick that changes provider unlocks that provider's key now, so the first
  // send does not open the OS prompt in the middle of a turn.
  if (switched) aiConfig.warmCredential(provider.id);
  if (!model) {
    // An id the catalog does not describe keeps the provider's own limits and
    // capabilities.
    aiConfig.updateProvider(provider.id, { model: modelId, thinkingLevel });
    if (save) void aiConfig.flushSave();
    return true;
  }
  aiConfig.updateProvider(provider.id, {
    model: model.id,
    contextLength: model.context,
    maxTokens: Math.min(model.max_output, 262144),
    capabilities: {
      text: model.capabilities.text,
      audioIn: model.capabilities.audio_in,
      audioOut: model.capabilities.audio_out,
      nativeToolCalling: model.capabilities.native_tool_calling,
      streaming: model.capabilities.streaming,
    },
    thinkingLevel,
  });
  if (save) void aiConfig.flushSave();
  return true;
}
