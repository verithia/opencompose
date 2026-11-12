/**
 * Schema-driven parameter math and formatting shared by the panel and its tests.
 */
import type { Effect, ParamValue } from "../types/project";
import { unitDefinition } from "../data/units.gen";
import type { DeviceParamDef } from "../types/devices";

const EPSILON = 1e-9;

const PARAM_ALIASES: Record<string, Record<string, string[]>> = {
  eq: { mid_freq: ["freq"], mid_gain: ["gain"] },
};

export function clamp01(value: number): number {
  return Math.min(1, Math.max(0, value));
}

/** Clamp to the schema range, then snap to the optional discrete positions. */
export function clampParam(def: DeviceParamDef, value: number): number {
  if (!Number.isFinite(value)) return def.default;
  const clamped = Math.min(def.max, Math.max(def.min, value));
  const steps = def.steps ?? null;
  if (steps !== null && steps >= 2) {
    const step = (def.max - def.min) / (steps - 1);
    if (step > 0) {
      const snapped = def.min + Math.round((clamped - def.min) / step) * step;
      return Math.min(def.max, Math.max(def.min, snapped));
    }
  }
  return clamped;
}

export function normalizedToValue(def: DeviceParamDef, position: number): number {
  const t = clamp01(position);
  if (def.curve === "log" && def.min > 0 && def.max > def.min) {
    return def.min * Math.pow(def.max / def.min, t);
  }
  return def.min + t * (def.max - def.min);
}

export function valueToNormalized(def: DeviceParamDef, value: number): number {
  const clamped = clampParam(def, value);
  if (def.curve === "log" && def.min > 0 && def.max > def.min) {
    return Math.log(clamped / def.min) / Math.log(def.max / def.min);
  }
  if (def.max === def.min) return 0;
  return (clamped - def.min) / (def.max - def.min);
}

/** Read a parameter from the effect, honoring legacy aliases and typed values. */
export function readEffectParam(effect: Effect, deviceId: string, def: DeviceParamDef): number {
  const aliases = PARAM_ALIASES[deviceId]?.[def.id] ?? [];
  for (const id of [def.id, ...aliases]) {
    const entry = effect.params.find(([name]) => name === id);
    if (!entry) continue;
    const value = paramValueToNumber(entry[1], def);
    if (value !== null) return value;
  }
  return def.default;
}

/** Serialize a schema value into the string form accepted by set_effect_param. */
export function toParamString(def: DeviceParamDef, value: number): string {
  const clamped = clampParam(def, value);
  if (def.kind === "bool") return clamped >= 0.5 ? "true" : "false";
  if (def.kind === "enum") {
    const index = Math.round(clamped);
    const option = def.options?.[index];
    // The DSL and the DSP use snake_case filter names; labels are display only.
    return option ? normalizeChoice(option) : String(index);
  }
  const number = trimmed(clamped, def.unit === "Hz" ? 1 : def.unit === "ms" ? 2 : 3);
  return `${number}${unitDefinition(def.unit).writeSuffix}`;
}

export function formatParamValue(def: DeviceParamDef, value: number): string {
  if (def.kind === "bool") return value >= 0.5 ? "On" : "Off";
  if (def.kind === "enum") {
    return def.options?.[Math.round(value)] ?? trimmed(value, 0);
  }
  if (def.unit === "Hz") {
    if (value >= 1000) return `${trimmed(value / 1000, 2)} kHz`;
    return `${trimmed(value, 1)} Hz`;
  }
  if (def.unit === "dB") {
    const sign = value > 0 ? "+" : "";
    return `${sign}${value.toFixed(1)} dB`;
  }
  if (def.unit === "ms") {
    if (value >= 1000) return `${trimmed(value / 1000, 2)} s`;
    return `${trimmed(value, 2)} ms`;
  }
  if (def.unit === "%" || def.unit === "fraction") {
    const unit = unitDefinition(def.unit);
    return `${trimmed(value * unit.displayMultiplier, 1)}${unit.displayUnit}`;
  }
  if (def.unit === "ratio") return `${trimmed(value, 2)}:1`;
  return trimmed(value, 2);
}

function trimmed(value: number, decimals: number): string {
  const rounded = Number(value.toFixed(decimals));
  return String(rounded);
}

function paramValueToNumber(value: ParamValue, def: DeviceParamDef): number | null {
  if ("Number" in value) return value.Number;
  if ("Db" in value) return value.Db;
  if ("Percentage" in value) return def.unit === "fraction" ? value.Percentage / 100 : value.Percentage;
  if ("Frequency" in value) return value.Frequency;
  if ("Cents" in value) return value.Cents;
  if ("Time" in value) return def.unit === "ms" ? value.Time * 1000 : value.Time;
  if ("Bool" in value) return value.Bool ? 1 : 0;
  if ("String" in value) {
    if (def.kind === "enum") {
      const index = optionIndex(def.options, value.String);
      if (index !== null) return index;
    }
    const ratio = /^\s*([0-9.]+)\s*:\s*([0-9.]+)\s*$/.exec(value.String);
    if (ratio) {
      const numerator = Number(ratio[1]);
      const denominator = Number(ratio[2]);
      if (denominator > EPSILON) return numerator / denominator;
    }
    const numeric = Number(value.String);
    return Number.isFinite(numeric) ? numeric : null;
  }
  return null;
}

function optionIndex(options: string[] | undefined, text: string): number | null {
  if (!options?.length) return null;
  const normalized = normalizeChoice(text);
  const index = options.findIndex((option) => normalizeChoice(option) === normalized);
  return index >= 0 ? index : null;
}

function normalizeChoice(text: string): string {
  return text.trim().toLowerCase().replace(/[\s-]+/g, "_");
}
