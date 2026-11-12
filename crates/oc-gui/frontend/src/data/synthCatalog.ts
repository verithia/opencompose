import {
  SYNTH_CATALOG,
  type FxTypeSpec,
  type SynthEnum,
  type SynthParamSpec,
} from "./synthCatalog.gen";
import { i18n, type I18nKey } from "../stores/i18n";

export { SYNTH_CATALOG };
export type { FxTypeSpec, SynthEnum, SynthParamSpec };

/** Enum vocabulary is technical and stays English in every locale, including
 * the status marker; parameter labels below do go through i18n. */
const STATUS_SUFFIX: Record<string, string> = {
  noOp: " (no effect)",
  partial: " (partial)",
};

function statusSuffix(status: string): string {
  return STATUS_SUFFIX[status] ?? "";
}

export function findEnum(id: string): SynthEnum | undefined {
  const catalog = SYNTH_CATALOG;
  return [
    catalog.modSources,
    catalog.modDestinations,
    catalog.modAux,
    catalog.filterModels,
    catalog.filterModes,
    catalog.filterRoutings,
    catalog.warpModes,
    catalog.waveforms,
    catalog.subShapes,
    catalog.lfoShapes,
    catalog.qualityModes,
    catalog.fxParamKinds,
  ].find((entry) => entry.id === id);
}

export function enumOptions(id: string, markStatus = false): { value: number; label: string }[] {
  const spec = findEnum(id);
  if (!spec) return [];
  return spec.options.map((option) => ({
    value: option.id,
    label: markStatus ? `${option.label}${statusSuffix(option.status)}` : option.label,
  }));
}

export function enumLabel(id: string, value: number): string {
  const spec = findEnum(id);
  const option = spec?.options.find((entry) => entry.id === Math.round(value));
  if (!option) return String(value);
  return `${option.label}${statusSuffix(option.status)}`;
}

function familyFieldSpec(key: string): SynthParamSpec | undefined {
  for (const family of SYNTH_CATALOG.families) {
    if (!key.startsWith(family.prefix)) continue;
    const rest = key.slice(family.prefix.length);
    const scalar = family.fields.length === 1 && family.fields[0].suffix === "";
    let suffix = "";
    let indexText = rest;
    if (!scalar) {
      const dot = rest.indexOf(".");
      if (dot < 0) continue;
      indexText = rest.slice(0, dot);
      suffix = rest.slice(dot + 1);
    }
    const index = Number(indexText);
    if (!Number.isInteger(index) || index < family.first || index > family.last) continue;
    const field = family.fields.find((entry) => entry.suffix === suffix);
    if (!field) continue;
    return {
      ids: [key],
      label: field.label,
      group: family.group,
      unit: field.unit,
      min: field.min,
      max: field.max,
      step: field.step,
      default: field.default,
      curve: field.curve,
      dspNormalization: field.dspNormalization,
      dspNormalizationFloor: field.dspNormalizationFloor,
      dspNormalizationSpan: field.dspNormalizationSpan,
      dspRuntimeBounds: field.dspRuntimeBounds,
      kind: field.kind,
      control: field.control,
      status: field.status,
      enumId: field.enumId,
      aliases: [],
    };
  }
  return undefined;
}

export function paramSpec(key: string): SynthParamSpec | undefined {
  for (const spec of SYNTH_CATALOG.params) {
    if (spec.ids.includes(key) || spec.aliases.includes(key)) return spec;
  }
  return familyFieldSpec(key);
}

/** Whether the engine accepts `key`; mirrors the Rust catalog matcher. */
export function isKnownParam(key: string): boolean {
  return paramSpec(key) !== undefined || SYNTH_CATALOG.families.some((family) => {
    if (!family.openFields || !key.startsWith(family.prefix)) return false;
    const rest = key.slice(family.prefix.length);
    const dot = rest.indexOf(".");
    if (dot < 0) return false;
    const index = Number(rest.slice(0, dot));
    return Number.isInteger(index) && index >= family.first && index <= family.last;
  });
}

export function paramLabel(key: string): string {
  const spec = paramSpec(key);
  return spec ? i18n.t(spec.label as I18nKey) : key;
}

export function groupLabel(id: string): string {
  const group = SYNTH_CATALOG.groups.find((entry) => entry.id === id);
  return group ? i18n.t(group.label as I18nKey) : id;
}

export function fxTypeSpec(id: string): FxTypeSpec | undefined {
  return SYNTH_CATALOG.fxTypes.find((entry) => entry.id === id);
}

/** Control metadata for every catalog parameter in a group, in table order. */
export function paramsInGroup(group: string): SynthParamSpec[] {
  return SYNTH_CATALOG.params.filter((spec) => spec.group === group);
}

/** Catalog entry for one concrete key, expanded from a multi-key row. */
export function specForConcreteKey(key: string): SynthParamSpec | undefined {
  const spec = paramSpec(key);
  if (!spec || spec.ids.includes(key)) return spec;
  return { ...spec, ids: [key] };
}
