/**
 * One automation target per OpenSynth parameter: the lane name, the mapping
 * between the control's display value and the engine's normalized 0..1 domain,
 * and a stable human label.
 *
 * Lane names reuse the hosted-plugin encoding (`pluginparam_<hex>`), so a
 * project lane drives a built-in synth parameter through the same engine path
 * as a plugin parameter.
 *
 * The curves below mirror `SynthPatch::set_normalized_param`; a parameter is
 * only automatable when the engine's normalized domain covers the control's
 * whole range. Parameters the engine cannot address are reported unavailable
 * with the reason, never silently clamped.
 */
import { groupLabel, paramLabel, specForConcreteKey } from "../data/synthCatalog";
import type { DeviceDef, DeviceParamDef } from "../types/devices";
import type { AutomationLane, Effect, Project } from "../types/project";

const LANE_PREFIX = "pluginparam_";

export interface AutomationCurveSpec {
  /** Display value -> engine-normalized 0..1 value. */
  encode(physical: number): number;
  /** Engine-normalized 0..1 value -> display value. */
  decode(normalized: number): number;
  /** Physical range the engine's normalized domain covers. */
  domain: readonly [number, number];
  /** Discrete parameter count/enum: decoded values are whole numbers. */
  stepped?: boolean;
}

export interface AutomateTarget {
  available: true;
  key: string;
  lane: string;
  label: string;
  stepped: boolean;
  /** Physical range the normalized 0..1 domain covers. */
  domain: readonly [number, number];
  encode(physical: number): number;
  decode(normalized: number): number;
}

export interface UnavailableTarget {
  available: false;
  key: string;
  reason: string;
}

export type AutomationTarget = AutomateTarget | UnavailableTarget;

const clamp01 = (value: number) => value < 0 ? 0 : value > 1 ? 1 : value;

function identity(): AutomationCurveSpec {
  return { encode: clamp01, decode: clamp01, domain: [0, 1] };
}

function linear(min: number, max: number, stepped = false, spanOverride?: number): AutomationCurveSpec {
  const span = spanOverride ?? (max - min);
  return {
    encode: (value) => clamp01((value - min) / span),
    decode: (normalized) => {
      const scaled = clamp01(normalized) * span;
      const value = min === 0 && spanOverride === undefined ? scaled : min + scaled;
      return stepped ? Math.round(value) : value;
    },
    domain: [min, max],
    stepped,
  };
}

/** `physical = min * (max / min) ^ normalized`, used by cutoff and rates. */
function exponential(min: number, max: number, base: "log" | "log10"): AutomationCurveSpec {
  const ratio = max / min;
  return {
    encode: (value) => value <= 0 ? 0 : clamp01(base === "log10"
      ? Math.log10(value / min) / Math.log10(ratio)
      : Math.log(value / min) / Math.log(ratio)),
    decode: (normalized) => min * ratio ** clamp01(normalized),
    domain: [min, max],
  };
}

/** `physical = normalized ^ exponent * max`, used by envelopes and glide. */
function powered(max: number, exponent: 2 | 3): AutomationCurveSpec {
  return {
    encode: (value) =>
      clamp01((Math.max(0, value) / max) ** (1 / exponent)),
    decode: (normalized) => clamp01(normalized) ** exponent * max,
    domain: [0, max],
  };
}

const WARP_MODES = linear(0, 69, true);
const WAVEFORM_ID = linear(0, 6, true);
const FILTER_MODELS = linear(0, 11, true);
const SUB_SHAPES = linear(0, 5, true);
const BIPOLAR = linear(-1, 1);
const UNISON_VOICES = linear(1, 16, true);
const LFO_SHAPES = linear(0, 4, true);
const LFO_SYNC_DIVISIONS = linear(0, 42, true);
const DIVISIONS = linear(0, 5, true);

/** Exact-key curves; aliases (`cutoff`, `detune`, ...) are listed too. */
const CURVES: Record<string, AutomationCurveSpec> = {
  "osc.waveform": WAVEFORM_ID,
  waveform: WAVEFORM_ID,
  "osc.a.waveform": WAVEFORM_ID,
  "osc.b.waveform": WAVEFORM_ID,
  "osc.c.waveform": WAVEFORM_ID,
  "lfo.waveform": WAVEFORM_ID,
  "sub.waveform": SUB_SHAPES,
  "sub.shape": SUB_SHAPES,
  "osc.a.warp_mode": WARP_MODES,
  "osc.b.warp_mode": WARP_MODES,
  "osc.c.warp_mode": WARP_MODES,
  "osc.a.warp2_mode": WARP_MODES,
  "osc.b.warp2_mode": WARP_MODES,
  "osc.c.warp2_mode": WARP_MODES,
  "filter.mode": linear(0, 3, true),
  "filter2.mode": linear(0, 3, true),
  "filter.model": FILTER_MODELS,
  "filter2.model": FILTER_MODELS,
  "quality.oversampling": linear(0, 2, true),
  "unison.voices": UNISON_VOICES,
  "osc.a.unison": UNISON_VOICES,
  "osc.b.unison": UNISON_VOICES,
  "osc.c.unison": UNISON_VOICES,
  "lfo.division": DIVISIONS,
  "step.division": DIVISIONS,
  "step.sync": identity(),
  "lfo.shape": LFO_SHAPES,
  "lfo.sync_div": LFO_SYNC_DIVISIONS,
  "lfo.pitch": linear(0, 200),
  "lfo.rate_x10": identity(),
  "voice.polyphony": linear(1, 32, true),
  "tuning.reference_hz": linear(400, 480),
  "tuning.transpose": linear(-24, 24),
  "tuning.fine": linear(-100, 100),
};

const MOD_FIELDS: Record<string, AutomationCurveSpec> = {
  source: linear(0, 28, true),
  destination: linear(0, 34, true),
  bipolar: identity(),
  aux: linear(0, 29, true),
  aux_invert: identity(),
};

const LFO_FIELDS: Record<string, AutomationCurveSpec> = {
  shape: LFO_SHAPES,
  waveform: WAVEFORM_ID,
  sync_div: LFO_SYNC_DIVISIONS,
  division: DIVISIONS,
  sync: identity(),
  retrigger: identity(),
  rate_x10: identity(),
};

/** Host automation laws come from the generated DSP authority, not UI travel. */
function schemaCurve(key: string): AutomationCurveSpec | undefined {
  const spec = specForConcreteKey(key);
  if (!spec?.dspNormalization) return undefined;
  switch (spec.dspNormalization) {
    case "identity": return identity();
    case "cubic": return powered(spec.max, 3);
    case "square": return powered(spec.max, 2);
    case "linear": return linear(spec.min, spec.max, spec.kind === "enum" || spec.kind === "bool", spec.dspNormalizationSpan);
    case "log":
    case "log10": return exponential(spec.dspNormalizationFloor ?? spec.min, spec.max, spec.dspNormalization);
  }
}

function curveFor(key: string): AutomationCurveSpec | undefined {
  const declared = schemaCurve(key);
  if (declared) return declared;
  if (Object.prototype.hasOwnProperty.call(CURVES, key)) return CURVES[key];
  const route = /^mod\.\d+\.([a-z0-9_]+)$/.exec(key);
  if (route) return MOD_FIELDS[route[1]];
  const lfo = /^lfo(\d+)\.([a-z0-9_]+)$/.exec(key);
  if (lfo) return LFO_FIELDS[lfo[2]];
  if (/^lfo\.curve\.\d+$/.test(key)) return BIPOLAR;
  return undefined;
}

/** The automation lanes one panel target owns; a preset chain owns none. */
export function ownerAutomationLanes(project: Project, target: string): AutomationLane[] {
  if (target === "master") return project.master?.automation ?? [];
  if (target.startsWith("bus:")) {
    const name = target.slice(4);
    return project.buses.find((bus) => bus.name === name)?.automation ?? [];
  }
  if (target.startsWith("track:")) {
    const index = Number(target.slice(6));
    return project.tracks[index]?.automation ?? [];
  }
  return [];
}

/** The effect chain one panel target owns; a preset chain owns none. */
export function ownerEffects(project: Project, target: string): Effect[] {
  if (target === "master") return project.master?.effects ?? [];
  if (target.startsWith("bus:")) {
    const name = target.slice(4);
    return project.buses.find((bus) => bus.name === name)?.effects ?? [];
  }
  if (target.startsWith("track:")) {
    const index = Number(target.slice(6));
    return project.tracks[index]?.effects ?? [];
  }
  return [];
}

/** Split `<effect>.<parameter>`; a lane with no dot targets no effect. */
export function effectLaneTarget(
  laneParam: string,
): { target: string; parameter: string } | null {
  const dot = laneParam.indexOf(".");
  if (dot <= 0 || dot === laneParam.length - 1) return null;
  return { target: laneParam.slice(0, dot), parameter: laneParam.slice(dot + 1) };
}

/**
 * The lane a device-panel knob writes: `fx<N>.<param>`, the chain-position form
 * that stays unambiguous when two entries of one type sit in the same chain.
 *
 * A preset chain owns no lanes, and a parameter with no numeric range cannot be
 * written as a point, so both are reported unavailable with their reason rather
 * than silently doing nothing.
 */
export type EffectAutomateTarget =
  | { available: true; lane: string; label: string }
  | { available: false; reason: string };

export function effectAutomationTarget(
  owner: string,
  device: DeviceDef,
  index: number,
  param: DeviceParamDef,
): EffectAutomateTarget {
  if (owner.startsWith("preset:")) {
    return { available: false, reason: "A preset chain owns no automation lanes." };
  }
  if (!Number.isFinite(param.min) || !Number.isFinite(param.max) || param.max <= param.min) {
    return { available: false, reason: "This parameter has no automation range." };
  }
  return {
    available: true,
    lane: `fx${index + 1}.${param.id}`,
    label: `${device.id}.${param.id}`,
  };
}

/** The one lane encoding; identical bytes to the hosted-plugin path. */
export function automationLaneName(key: string): string {
  let encoded = LANE_PREFIX;
  for (const byte of new TextEncoder().encode(key)) {
    encoded += byte.toString(16).padStart(2, "0");
  }
  return encoded;
}

/** Decode a lane back to its parameter key; null for non-pluginparam lanes. */
export function automationLaneKey(lane: string): string | null {
  if (!lane.startsWith(LANE_PREFIX)) return null;
  const encoded = lane.slice(LANE_PREFIX.length);
  if (encoded.length === 0 || encoded.length % 2 !== 0) return null;
  const bytes = new Uint8Array(encoded.length / 2);
  for (let index = 0; index < bytes.length; index += 1) {
    const byte = Number.parseInt(encoded.slice(index * 2, index * 2 + 2), 16);
    if (!Number.isInteger(byte)) return null;
    bytes[index] = byte;
  }
  return new TextDecoder().decode(bytes);
}

const MOD_FIELD_LABELS: Record<string, string> = {
  source: "source",
  destination: "destination",
  amount: "amount",
  curve: "curve",
  slew_up: "slew up",
  slew_down: "slew down",
  bipolar: "bipolar",
  aux: "aux source",
  aux_invert: "invert aux",
};

function fieldLabel(field: string): string {
  return MOD_FIELD_LABELS[field] ?? field.replaceAll("_", " ");
}

/** Stable, human-readable lane target; falls back to the key when unknown. */
export function automationLabel(key: string): string {
  const route = /^mod\.(\d+)\.([a-z0-9_]+)$/.exec(key);
  if (route) return `Route ${route[1]} ${fieldLabel(route[2])}`;
  const lfoCurve = /^lfo\.curve\.(\d+)$/.exec(key);
  if (lfoCurve) return `LFO curve ${lfoCurve[1]}`;
  const lfo = /^lfo(\d*)\.([a-z0-9_]+)$/.exec(key);
  if (lfo) return `LFO ${lfo[1] === "" ? 1 : lfo[1]} ${fieldLabel(lfo[2])}`;
  const step = /^step\.(\d+)$/.exec(key);
  if (step) return `Step ${step[1]}`;
  const macro = /^macro\.(\d+)$/.exec(key);
  if (macro) return `Macro ${macro[1]}`;
  const spec = specForConcreteKey(key);
  if (!spec) return key;
  const label = paramLabel(key);
  const group = groupLabel(spec.group);
  return label === group || label.startsWith(`${group} `) ? label : `${group} ${label}`;
}

function unavailable(key: string, reason: string): UnavailableTarget {
  return { available: false, key, reason };
}

function formatNumber(value: number): string {
  return Number.isInteger(value) ? String(value) : value.toFixed(3).replace(/0+$/, "");
}

/** Resolve one parameter key to its automation target. */
export function automationTarget(key: string): AutomationTarget {
  const trimmed = key.trim();
  if (trimmed === "") return unavailable(trimmed, "Parameter name is empty.");
  const spec = specForConcreteKey(trimmed);
  if (
    spec &&
    (spec.kind === "string" || spec.kind === "metadata" || spec.kind === "array")
  ) {
    return unavailable(trimmed, "This parameter is not a numeric value, so it has no automation lane.");
  }
  const curve = curveFor(trimmed);
  if (!curve) {
    if (spec && spec.min >= 0 && spec.max <= 1) {
      return availableTarget(trimmed, identity());
    }
    return unavailable(trimmed, "The engine exposes no normalized automation range for this parameter.");
  }
  if (spec && spec.step > 0) {
    const [min, max] = curve.domain;
    if (min > spec.min + spec.step || max < spec.max - spec.step) {
      return unavailable(
        trimmed,
        `Automation covers ${formatNumber(min)}–${formatNumber(max)}, but this control spans ${formatNumber(spec.min)}–${formatNumber(spec.max)}.`,
      );
    }
  }
  return availableTarget(trimmed, curve);
}

function availableTarget(key: string, curve: AutomationCurveSpec): AutomateTarget {
  return {
    available: true,
    key,
    lane: automationLaneName(key),
    label: automationLabel(key),
    stepped: curve.stepped ?? false,
    domain: curve.domain,
    encode: curve.encode,
    decode: curve.decode,
  };
}

/** The plugin's own range, so a hosted parameter rides the same lane encoding. */
export function pluginAutomationTarget(param: {
  name: string;
  unit: string;
  min: number;
  max: number;
  steps: number | null;
  automatable: boolean;
  group: string | null;
}): AutomationTarget {
  const key = param.name.trim();
  if (key === "") return unavailable(key, "Parameter name is empty.");
  if (!param.automatable) {
    return unavailable(key, "The plugin does not accept automation for this parameter.");
  }
  if (!Number.isFinite(param.min) || !Number.isFinite(param.max) || param.max < param.min) {
    return unavailable(key, "The plugin reports no value range for this parameter.");
  }
  const curve = linear(param.min, param.max, param.steps !== null && param.steps > 1);
  return {
    ...availableTarget(key, curve),
    label: param.group ? `${param.group} ${key}` : key,
  };
}
